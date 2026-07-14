//! Tauri command handlers (DESIGN.md §6). Thin wrappers over `AppState`; they
//! return `Result<T, String>` so failures surface as rejected JS promises.
//!
//! The same operations are exposed to Claude over MCP (`mcp.rs`) — both call
//! into the shared `ops` helpers so behavior can't drift between surfaces.

use crate::detect::{self, Detection};
use crate::model::{AppConfig, AppRunSnapshot, LocalServerInventory, LogLine};
use crate::state::AppState;
use crate::{ops, store};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

#[derive(Serialize)]
pub struct AppListItem {
    pub config: AppConfig,
    pub running: bool,
    pub run: Option<AppRunSnapshot>,
}

#[derive(Serialize)]
pub struct McpInfo {
    pub url: String,
    pub port: u16,
    pub token: String,
    pub healthy: bool,
    pub version: String,
    #[serde(rename = "claudeAddCommand")]
    pub claude_add_command: String,
    #[serde(rename = "desktopJson")]
    pub desktop_json: String,
    #[serde(rename = "bridgeCommand")]
    pub bridge_command: String,
}

#[tauri::command]
pub async fn list_apps(state: State<'_, Arc<AppState>>) -> Result<Vec<AppListItem>, String> {
    let mut out = Vec::new();
    for config in state.list_configs().await {
        // Reflect a server started outside Harbor (e.g. a terminal) on this app's
        // port; no-op + cheap when the app is already tracked as running.
        state.supervisor.reflect_external_if_idle(&config).await;
        let run = state.supervisor.snapshot(&config.name).await;
        let running = state.supervisor.is_running(&config.name).await;
        out.push(AppListItem {
            config,
            running,
            run,
        });
    }
    Ok(out)
}

#[tauri::command]
pub async fn app_status(
    state: State<'_, Arc<AppState>>,
    app: String,
) -> Result<Option<AppRunSnapshot>, String> {
    if let Some(cfg) = state.get_config(&app).await {
        state.supervisor.reflect_external_if_idle(&cfg).await;
    }
    Ok(state.supervisor.snapshot(&app).await)
}

#[tauri::command]
pub async fn list_local_servers(
    state: State<'_, Arc<AppState>>,
) -> Result<LocalServerInventory, String> {
    let configs = state.list_configs().await;
    // Reuse exact, high-confidence configured matches before producing the
    // inventory. All remaining entries stay observation-only.
    state.supervisor.scan_and_adopt_external(&configs).await;
    let tracked = state.supervisor.tracked_servers().await;
    crate::discovery::scan(&configs, &tracked, state.mcp.port)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_local_server(
    state: State<'_, Arc<AppState>>,
    pid: u32,
    port: u16,
    #[allow(non_snake_case)] startedAt: String,
) -> Result<(), String> {
    if state.supervisor.owns_pid(pid).await {
        return Err(
            "this process is managed by Harbor; stop it from the app card so state stays consistent"
                .to_string(),
        );
    }
    crate::discovery::stop_untracked(pid, &startedAt, port)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_app(
    state: State<'_, Arc<AppState>>,
    app: String,
    profile: Option<String>,
) -> Result<AppRunSnapshot, String> {
    ops::start_app(&state, &app, profile.as_deref()).await
}

#[tauri::command]
pub async fn stop_app(state: State<'_, Arc<AppState>>, app: String) -> Result<(), String> {
    ops::stop_app(&state, &app).await
}

#[tauri::command]
pub async fn restart_app(
    state: State<'_, Arc<AppState>>,
    app: String,
    profile: Option<String>,
) -> Result<AppRunSnapshot, String> {
    ops::restart_app(&state, &app, profile.as_deref()).await
}

#[tauri::command]
pub async fn start_all(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    ops::start_all(&state).await
}

#[tauri::command]
pub async fn stop_all(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    ops::stop_all(&state).await
}

#[tauri::command]
pub async fn get_logs(
    state: State<'_, Arc<AppState>>,
    app: String,
    service: String,
    lines: Option<usize>,
) -> Result<Vec<LogLine>, String> {
    Ok(state
        .supervisor
        .logs(&app, &service, lines.unwrap_or(200))
        .await)
}

#[tauri::command]
pub async fn register_app(
    state: State<'_, Arc<AppState>>,
    mut config: AppConfig,
) -> Result<(), String> {
    let _lifecycle = state.supervisor.lock_lifecycle(&config.name).await;
    if state.supervisor.is_running(&config.name).await {
        return Err("stop the existing app before replacing its configuration".to_string());
    }
    // Reaching this command requires an explicit interaction in Harbor's UI.
    config.trusted = true;
    state.upsert(config).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn approve_app(
    state: State<'_, Arc<AppState>>,
    app: String,
    expected: AppConfig,
) -> Result<(), String> {
    state
        .approve_if_unchanged(&app, &expected)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_app(
    state: State<'_, Arc<AppState>>,
    app: String,
    config: AppConfig,
) -> Result<(), String> {
    let _lifecycle = state.supervisor.lock_lifecycle(&app).await;
    if state.supervisor.is_running(&app).await {
        return Err("stop the app before editing its services, root, or name".to_string());
    }
    state.replace(&app, config).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remove_app(state: State<'_, Arc<AppState>>, app: String) -> Result<bool, String> {
    let _lifecycle = state.supervisor.lock_lifecycle(&app).await;
    if state.supervisor.is_running(&app).await {
        return Err("stop the app before removing it".to_string());
    }
    state.remove(&app).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_env(
    state: State<'_, Arc<AppState>>,
    app: String,
    service: String,
    env: BTreeMap<String, String>,
) -> Result<bool, String> {
    let _lifecycle = state.supervisor.lock_lifecycle(&app).await;
    state
        .mutate(&app, |cfg| {
            if let Some(svc) = cfg.service_mut(&service) {
                svc.env = env;
            }
        })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_port(
    state: State<'_, Arc<AppState>>,
    app: String,
    service: String,
    port: u16,
) -> Result<bool, String> {
    let _lifecycle = state.supervisor.lock_lifecycle(&app).await;
    state
        .mutate(&app, |cfg| {
            if let Some(svc) = cfg.service_mut(&service) {
                svc.port = Some(port);
            }
        })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn detect_app(path: String) -> Result<Detection, String> {
    let p = std::path::PathBuf::from(&path);
    if !p.exists() {
        return Err(format!("path does not exist: {path}"));
    }
    if !p.is_dir() {
        return Err(format!(
            "not a folder: {path} — drop a project folder, not a file"
        ));
    }
    Ok(detect::detect(&p))
}

/// Classify a dropped path so the UI can resolve folder-vs-file without a throw.
#[tauri::command]
pub fn path_kind(path: String) -> &'static str {
    let p = std::path::Path::new(&path);
    if p.is_dir() {
        "dir"
    } else if p.is_file() {
        "file"
    } else {
        "missing"
    }
}

/// Parse a `.env` file into KEY→VALUE (handles `export `, `#` comments, and
/// single/double-quoted values). Used by the env editor's "Import .env".
#[tauri::command]
pub fn read_dotenv(path: String) -> Result<BTreeMap<String, String>, String> {
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let l = l.strip_prefix("export ").unwrap_or(l);
        if let Some((k, v)) = l.split_once('=') {
            let k = k.trim();
            if k.is_empty() {
                continue;
            }
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(v);
            out.insert(k.to_string(), v.to_string());
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn open_app(state: State<'_, Arc<AppState>>, app: String) -> Result<String, String> {
    ops::open_app(&state, &app).await
}

/// Open an arbitrary http(s) URL (e.g. a service's localhost address) in the
/// default browser.
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("only http(s) URLs can be opened".to_string());
    }
    ops::open_url(&url)
}

#[tauri::command]
pub async fn mcp_info(state: State<'_, Arc<AppState>>) -> Result<McpInfo, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let port = state.mcp.port;
    let request = format!(
        "GET /health HTTP/1.0\r\nHost: localhost\r\nAuthorization: Bearer {}\r\n\r\n",
        state.mcp.token
    );
    let healthy = tokio::time::timeout(std::time::Duration::from_millis(500), async move {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .ok()?;
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut response = Vec::with_capacity(512);
        stream.read_to_end(&mut response).await.ok()?;
        String::from_utf8_lossy(&response)
            .contains("Harbor MCP OK")
            .then_some(())
    })
    .await
    .ok()
    .flatten()
    .is_some();
    let bridge = state.store.bridge_path().to_string_lossy().into_owned();
    Ok(build_mcp_info(&state.mcp.token, port, healthy, &bridge))
}

/// Import a shareable `harbor.json`. `path` may be the file itself or the folder
/// containing it; the app is registered under its `name`.
#[tauri::command]
pub async fn import_app(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<AppConfig, String> {
    let p = PathBuf::from(&path);
    let (file, root) = if p.is_dir() {
        (p.join("harbor.json"), p.clone())
    } else {
        let root = p
            .parent()
            .map(|x| x.to_path_buf())
            .unwrap_or_else(|| p.clone());
        (p.clone(), root)
    };
    if !file.exists() {
        return Err(format!("no harbor.json at {}", file.display()));
    }
    let cfg = store::import_harbor_json(&file, &root).map_err(|e| e.to_string())?;
    let _lifecycle = state.supervisor.lock_lifecycle(&cfg.name).await;
    if state.supervisor.is_running(&cfg.name).await {
        return Err("stop the existing app before importing a replacement config".to_string());
    }
    state.upsert(cfg.clone()).await.map_err(|e| e.to_string())?;
    Ok(cfg)
}

// ---------------------------------------------------------------------------
// Claude connection — make "Connect your Claude" one click.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct AgentStatus {
    /// `claude` CLI is installed and found.
    #[serde(rename = "codeCli")]
    pub code_cli: bool,
    /// Harbor's Claude Code configuration and observed runtime state.
    pub code: AgentConnection,
    /// Claude Desktop appears installed (its config dir exists).
    #[serde(rename = "desktopInstalled")]
    pub desktop_installed: bool,
    /// Harbor's Claude Desktop configuration and observed runtime state.
    pub desktop: AgentConnection,
    /// `codex` CLI found, or a ~/.codex config exists.
    #[serde(rename = "codexInstalled")]
    pub codex_installed: bool,
    /// Harbor's Codex configuration and observed runtime state.
    pub codex: AgentConnection,
}

/// Configuration and runtime are deliberately separate. An entry in a config
/// file does not mean the client launched Harbor's bridge, and a bridge process
/// alone cannot prove the client accepted Harbor's tool catalog.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnection {
    pub configured: bool,
    /// A current Harbor bridge process is running below this client.
    pub bridge_running: bool,
    /// The client predates its config or Harbor's current endpoint descriptor.
    pub restart_required: bool,
    /// A running client has not launched its current Harbor bridge.
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
struct ProcessInfo {
    pid: u32,
    parent: u32,
    age: std::time::Duration,
    command: String,
}

fn claude_desktop_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/Claude/claude_desktop_config.json"))
}

fn claude_code_config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join(".claude.json"));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".claude.json"))
}

fn codex_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let base = std::env::var("CODEX_HOME").unwrap_or_else(|_| format!("{home}/.codex"));
    Some(PathBuf::from(base).join("config.toml"))
}

/// Agent configs and their backups can contain unrelated credentials as well as
/// Harbor's token. Preserve them atomically with owner-only permissions.
fn write_private_atomic(path: &std::path::Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension(format!(
        "{}.{}.harbor-tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("cfg"),
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    drop(file);
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Return Harbor's installed native stdio bridge. The GUI installs this signed
/// executable at a stable path before any connection command can be invoked.
fn has_native_executable_header(path: &std::path::Path) -> bool {
    let mut header = [0_u8; 4];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok()
        && matches!(
            header,
            // Mach-O universal/thin, ELF, or PE. Harbor currently ships Mach-O,
            // while the other headers keep development builds portable.
            [0xca, 0xfe, 0xba, 0xbe]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xbf, 0xba, 0xfe, 0xca]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0x7f, b'E', b'L', b'F']
                | [b'M', b'Z', _, _]
        )
}

fn ensure_mcp_bridge(state: &AppState) -> Result<String, String> {
    let bridge = state.store.bridge_path();
    let metadata = std::fs::symlink_metadata(&bridge).map_err(|_| {
        "Harbor's native MCP bridge is not installed. Restart Harbor and try again."
    })?;
    if !metadata.file_type().is_file() {
        return Err("Harbor's native MCP bridge path is not a regular file.".to_string());
    }
    if !has_native_executable_header(&bridge) {
        return Err(
            "Harbor's native MCP bridge has not been installed yet. Restart Harbor and try again."
                .to_string(),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("Harbor's native MCP bridge is not executable.".to_string());
        }
    }
    Ok(bridge.to_string_lossy().into_owned())
}

fn claude_code_add_args(bridge: &str) -> Vec<String> {
    vec![
        "mcp".into(),
        "add".into(),
        "harbor".into(),
        "--scope".into(),
        "user".into(),
        "--transport".into(),
        "stdio".into(),
        "--".into(),
        bridge.into(),
    ]
}

fn json_entry_uses_bridge(
    entry: &serde_json::Value,
    expected_bridge: &str,
    bridge_available: bool,
) -> bool {
    bridge_available
        && entry.get("command").and_then(serde_json::Value::as_str) == Some(expected_bridge)
}

fn toml_entry_uses_bridge(
    entry: &toml_edit::Item,
    expected_bridge: &str,
    bridge_available: bool,
) -> bool {
    let Some(harbor) = entry.as_table_like() else {
        return false;
    };
    let enabled = match harbor.get("enabled") {
        None => true,
        Some(value) => value.as_bool() == Some(true),
    };
    enabled
        && bridge_available
        && harbor.get("command").and_then(toml_edit::Item::as_str) == Some(expected_bridge)
}

#[derive(Clone, Copy)]
enum AgentKind {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
}

fn parse_process_age(value: &str) -> Option<std::time::Duration> {
    let (days, clock) = match value.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, value),
    };
    let parts = clock
        .split(':')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let seconds = match parts.as_slice() {
        [minutes, seconds] => minutes * 60 + seconds,
        [hours, minutes, seconds] => hours * 3600 + minutes * 60 + seconds,
        _ => return None,
    };
    Some(std::time::Duration::from_secs(days * 86_400 + seconds))
}

fn process_snapshot() -> Vec<ProcessInfo> {
    let output = match std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,etime=,command="])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            let age = parse_process_age(fields.next()?)?;
            let command = fields.collect::<Vec<_>>().join(" ");
            Some(ProcessInfo {
                pid,
                parent,
                age,
                command,
            })
        })
        .collect()
}

fn is_agent_root(process: &ProcessInfo, kind: AgentKind, resolved_cli: Option<&str>) -> bool {
    let command = process.command.to_ascii_lowercase();
    match kind {
        AgentKind::ClaudeDesktop => {
            command.contains("/claude.app/contents/macos/claude")
                && !command.contains("claude helper")
        }
        AgentKind::Codex => {
            command.contains("/contents/resources/codex") && command.contains("app-server")
                || command.contains("/codex.app/contents/macos/codex")
                || resolved_cli.is_some_and(|cli| process.command.starts_with(cli))
        }
        AgentKind::ClaudeCode => {
            resolved_cli.is_some_and(|cli| process.command.starts_with(cli))
                && !command.contains("/claude.app/contents/macos/claude")
        }
    }
}

fn has_ancestor(
    start: u32,
    roots: &std::collections::HashSet<u32>,
    parents: &std::collections::HashMap<u32, u32>,
) -> bool {
    let mut current = start;
    for _ in 0..64 {
        if roots.contains(&current) {
            return true;
        }
        let Some(parent) = parents.get(&current).copied() else {
            return false;
        };
        if parent == 0 || parent == current {
            return false;
        }
        current = parent;
    }
    false
}

fn process_runs_executable(process: &ProcessInfo, executable: &str) -> bool {
    process
        .command
        .strip_prefix(executable)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

struct ConnectionTarget<'a> {
    configured: bool,
    config_path: Option<&'a std::path::Path>,
    kind: AgentKind,
    resolved_cli: Option<&'a str>,
    client_name: &'a str,
}

fn connection_state(
    target: ConnectionTarget<'_>,
    descriptor_age: Option<std::time::Duration>,
    bridge_age: Option<std::time::Duration>,
    processes: &[ProcessInfo],
    bridge: &str,
) -> AgentConnection {
    if !target.configured {
        return AgentConnection::default();
    }

    let roots = processes
        .iter()
        .filter(|process| is_agent_root(process, target.kind, target.resolved_cli))
        .map(|process| process.pid)
        .collect::<std::collections::HashSet<_>>();
    let parents = processes
        .iter()
        .map(|process| (process.pid, process.parent))
        .collect::<std::collections::HashMap<_, _>>();
    let belongs_to_client = |process: &ProcessInfo| has_ancestor(process.pid, &roots, &parents);

    // The native bridge re-reads mcp.json and reconnects in place, so rotating
    // Harbor's endpoint no longer makes it stale. It only needs a client restart
    // when Harbor has replaced the executable with a newer bridge build.
    let native_processes = processes
        .iter()
        .filter(|process| belongs_to_client(process) && process_runs_executable(process, bridge))
        .collect::<Vec<_>>();
    let native_found = !native_processes.is_empty();
    let native_running = native_processes
        .iter()
        .any(|process| bridge_age.is_some_and(|age| process.age <= age));

    // Recognize the pre-native launcher for a seamless upgrade. That bridge
    // cached its token/port, so it still predates and is invalidated by a newer
    // descriptor. The shell parent contains the stable bridge path. After the
    // script's final `exec`, its path can disappear from `ps`, so also recognize
    // the old mcp-remote process by its loopback Streamable HTTP target.
    let legacy_processes = processes
        .iter()
        .filter(|process| {
            let old_remote = process.command.contains("mcp-remote")
                && process.command.contains("mcp-remote@0.1.38")
                && process.command.contains("Authorization:${HARBOR_AUTH}")
                && process.command.contains("http://127.0.0.1:")
                && process.command.contains("/mcp");
            belongs_to_client(process)
                && !process_runs_executable(process, bridge)
                && (old_remote || (!native_found && process.command.contains(bridge)))
        })
        .collect::<Vec<_>>();
    let legacy_found = !legacy_processes.is_empty();
    let legacy_running = legacy_processes
        .iter()
        .any(|process| descriptor_age.is_some_and(|age| process.age <= age));

    let bridge_running = native_running || legacy_running;
    let stale_bridge = (native_found && !native_running) || (legacy_found && !legacy_running);

    let config_age = target
        .config_path
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok());
    let restart_required = !bridge_running
        && (stale_bridge
            || (!roots.is_empty()
                && config_age.is_some_and(|age| {
                    processes
                        .iter()
                        .filter(|process| roots.contains(&process.pid))
                        .all(|process| process.age > age)
                })));
    let error = (!roots.is_empty() && !bridge_running && !restart_required).then(|| {
        format!(
            "{} is running but has not launched Harbor's bridge. Fully quit and reopen it.",
            target.client_name
        )
    });

    AgentConnection {
        configured: target.configured,
        bridge_running,
        restart_required,
        error,
    }
}

#[tauri::command]
pub async fn agents_status(state: State<'_, Arc<AppState>>) -> Result<AgentStatus, String> {
    let expected_bridge = state.store.bridge_path().to_string_lossy().into_owned();
    let bridge_current = ensure_mcp_bridge(&state).is_ok();
    let claude = crate::sysenv::resolve_bin("claude");
    let code_cli = claude.is_some();
    let code_config = claude_code_config_path();
    let mut code_configured = false;
    if let Some(path) = &code_config {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(entry) = config
                    .get("mcpServers")
                    .and_then(|servers| servers.get("harbor"))
                {
                    code_configured =
                        json_entry_uses_bridge(entry, &expected_bridge, bridge_current);
                }
            }
        }
    }

    let cfg = claude_desktop_config_path();
    let desktop_installed = cfg
        .as_ref()
        .and_then(|p| p.parent().map(|d| d.exists()))
        .unwrap_or(false)
        || std::path::Path::new("/Applications/Claude.app").exists()
        || std::env::var("HOME")
            .ok()
            .is_some_and(|home| PathBuf::from(home).join("Applications/Claude.app").exists());
    let mut desktop_configured = false;
    if let Some(p) = &cfg {
        if let Ok(text) = std::fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                desktop_configured = v
                    .get("mcpServers")
                    .and_then(|m| m.get("harbor"))
                    .map(|entry| json_entry_uses_bridge(entry, &expected_bridge, bridge_current))
                    .unwrap_or(false);
            }
        }
    }

    // Codex
    let codex_bin = crate::sysenv::resolve_bin("codex");
    let codex_cli = codex_bin.is_some();
    let cxp = codex_config_path();
    let codex_installed = codex_cli
        || cxp
            .as_ref()
            .map(|p| p.exists() || p.parent().map(|d| d.exists()).unwrap_or(false))
            .unwrap_or(false);
    let mut codex_configured = false;
    if let Some(p) = &cxp {
        if let Ok(text) = std::fs::read_to_string(p) {
            if let Ok(doc) = text.parse::<toml_edit::DocumentMut>() {
                codex_configured = doc
                    .get("mcp_servers")
                    .and_then(|t| t.as_table_like())
                    .and_then(|t| t.get("harbor"))
                    .map(|entry| toml_entry_uses_bridge(entry, &expected_bridge, bridge_current))
                    .unwrap_or(false);
            }
        }
    }

    let processes = process_snapshot();
    let descriptor_age = std::fs::metadata(state.store.settings_path())
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok());
    let bridge_age = std::fs::metadata(state.store.bridge_path())
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok());
    let code = connection_state(
        ConnectionTarget {
            configured: code_configured,
            config_path: code_config.as_deref(),
            kind: AgentKind::ClaudeCode,
            resolved_cli: claude.as_deref().and_then(std::path::Path::to_str),
            client_name: "Claude Code",
        },
        descriptor_age,
        bridge_age,
        &processes,
        &expected_bridge,
    );
    let desktop = connection_state(
        ConnectionTarget {
            configured: desktop_configured,
            config_path: cfg.as_deref(),
            kind: AgentKind::ClaudeDesktop,
            resolved_cli: None,
            client_name: "Claude Desktop",
        },
        descriptor_age,
        bridge_age,
        &processes,
        &expected_bridge,
    );
    let codex = connection_state(
        ConnectionTarget {
            configured: codex_configured,
            config_path: cxp.as_deref(),
            kind: AgentKind::Codex,
            resolved_cli: codex_bin.as_deref().and_then(std::path::Path::to_str),
            client_name: "Codex",
        },
        descriptor_age,
        bridge_age,
        &processes,
        &expected_bridge,
    );

    Ok(AgentStatus {
        code_cli,
        code,
        desktop_installed,
        desktop,
        codex_installed,
        codex,
    })
}

#[tauri::command]
pub async fn connect_claude_code(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let bin =
        crate::sysenv::resolve_bin("claude").ok_or("Claude Code CLI (`claude`) not found.")?;
    let bridge = ensure_mcp_bridge(&state)?;
    let add_args = claude_code_add_args(&bridge);
    let path = crate::sysenv::enriched_path().unwrap_or_default();

    // Replace any prior HTTP or Node-backed entry with the stable native bridge.
    let _ = tokio::process::Command::new(&bin)
        .args(["mcp", "remove", "harbor", "--scope", "user"])
        .env("PATH", &path)
        .output()
        .await;

    let out = tokio::process::Command::new(&bin)
        .args(add_args)
        .env("PATH", &path)
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if out.status.success() {
        Ok("Configured Claude Code with Harbor's reconnecting native bridge.".to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[tauri::command]
pub async fn connect_claude_desktop(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let p = claude_desktop_config_path().ok_or("could not resolve Claude Desktop config path")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }

    let mut root: serde_json::Value = if p.exists() {
        let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
        // Back up the original before we touch it.
        write_private_atomic(&p.with_extension("json.harbor-bak"), &text)?;
        serde_json::from_str(&text)
            .map_err(|e| format!("Claude Desktop config is invalid JSON; left it unchanged: {e}"))?
    } else {
        serde_json::json!({})
    };
    if !root.is_object() {
        return Err(
            "Claude Desktop config must contain a JSON object; left it unchanged".to_string(),
        );
    }

    // Claude Desktop is stdio-only. Its config points to Harbor's stable native
    // bridge, which follows protected endpoint descriptor updates in place.
    let bridge = ensure_mcp_bridge(&state)?;
    let entry = serde_json::json!({
        "command": bridge,
        "args": []
    });

    let obj = root.as_object_mut().unwrap();
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        return Err(
            "Claude Desktop config has a non-object mcpServers value; left it unchanged"
                .to_string(),
        );
    }
    servers
        .as_object_mut()
        .unwrap()
        .insert("harbor".to_string(), entry);

    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    write_private_atomic(&p, &text)?;
    Ok("Added to Claude Desktop — fully quit and reopen Claude Desktop to use Harbor.".to_string())
}

#[tauri::command]
pub async fn connect_codex(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let p = codex_config_path().ok_or("could not resolve ~/.codex/config.toml")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let original = match std::fs::read_to_string(&p) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("reading {}: {error}", p.display())),
    };
    if !original.is_empty() {
        write_private_atomic(&p.with_extension("toml.harbor-bak"), &original)?;
    }
    let mut doc = original
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("parsing ~/.codex/config.toml: {e}"))?;

    let bridge = ensure_mcp_bridge(&state)?;
    let mut tbl = toml_edit::Table::new();
    tbl["command"] = toml_edit::value(bridge);
    tbl["startup_timeout_sec"] = toml_edit::value(45);
    // Current Codex understands MCP tool annotations; prompt for state-changing
    // Harbor tools while allowing inventory/status/log reads without friction.
    tbl["default_tools_approval_mode"] = toml_edit::value("writes");

    if let Some(item) = doc.get("mcp_servers") {
        if !item.is_table() {
            let entries = item
                .as_inline_table()
                .ok_or("~/.codex/config.toml has a non-table mcp_servers value; left it unchanged")?
                .iter()
                .map(|(name, value)| (name.to_string(), value.clone()))
                .collect::<Vec<_>>();
            let mut table = toml_edit::Table::new();
            for (name, value) in entries {
                table.insert(&name, toml_edit::Item::Value(value));
            }
            doc["mcp_servers"] = toml_edit::Item::Table(table);
        }
    } else {
        doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["mcp_servers"]["harbor"] = toml_edit::Item::Table(tbl);

    write_private_atomic(&p, &doc.to_string())?;
    Ok(
        "Configured Codex with Harbor's reconnecting native bridge — restart Codex to load it."
            .to_string(),
    )
}

// ---- "Fix with AI": diagnose a failed service via Claude/Codex --------------

#[derive(Serialize)]
pub struct FixResult {
    pub agent: String,
    pub response: String,
}

async fn build_fix_prompt(state: &AppState, app: &str, service: &str) -> String {
    let cfg = state.get_config(app).await;
    let logs = state.supervisor.logs(app, service, 60).await;
    let snap = state.supervisor.snapshot(app).await;
    let svc = cfg.as_ref().and_then(|c| c.service(service).cloned());
    let sr = snap
        .as_ref()
        .and_then(|s| s.services.iter().find(|x| x.name == service).cloned());
    let root = cfg.as_ref().map(|c| c.root.clone()).unwrap_or_default();

    let mut p = String::new();
    p.push_str(
        "A local dev service failed to start (managed by Harbor, a local server orchestrator). \
         Diagnose the root cause and give the exact fix — commands to run and/or config changes. \
         Be concise and specific.\n\n",
    );
    p.push_str(&format!("App: {app}\nService: {service}\n"));
    if let Some(svc) = &svc {
        p.push_str(&format!("Command: {}\n", svc.command));
        let cwd = if svc.cwd == "." {
            root.clone()
        } else {
            format!("{root}/{}", svc.cwd)
        };
        p.push_str(&format!("Working directory: {cwd}\n"));
    } else {
        p.push_str(&format!("Working directory: {root}\n"));
    }
    if let Some(sr) = &sr {
        if let Some(code) = sr.exit_code {
            p.push_str(&format!("Exit code: {code}\n"));
        }
    }
    p.push_str("\nRecent output:\n");
    for l in logs.iter() {
        p.push_str(&l.line);
        p.push('\n');
    }
    p.push_str("\nWhat went wrong, and exactly how do I fix it?");
    p
}

#[tauri::command]
pub async fn fix_prompt(
    state: State<'_, Arc<AppState>>,
    app: String,
    service: String,
) -> Result<String, String> {
    Ok(build_fix_prompt(&state, &app, &service).await)
}

#[tauri::command]
pub async fn run_fix(
    state: State<'_, Arc<AppState>>,
    app: String,
    service: String,
) -> Result<FixResult, String> {
    let prompt = build_fix_prompt(&state, &app, &service).await;
    let path = crate::sysenv::enriched_path().unwrap_or_default();
    let root = state
        .get_config(&app)
        .await
        .map(|c| c.root)
        .unwrap_or_default();
    let dir = if root.is_empty() {
        ".".to_string()
    } else {
        root
    };
    let timeout = std::time::Duration::from_secs(150);

    if let Some(codex) = crate::sysenv::resolve_bin("codex") {
        let fut = tokio::process::Command::new(codex)
            .args([
                "exec",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                &prompt,
            ])
            .current_dir(&dir)
            .env("PATH", &path)
            .output();
        if let Ok(Ok(out)) = tokio::time::timeout(timeout, fut).await {
            let resp = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !resp.is_empty() {
                return Ok(FixResult {
                    agent: "Codex".to_string(),
                    response: resp,
                });
            }
        }
    }
    if let Some(claude) = crate::sysenv::resolve_bin("claude") {
        let fut = tokio::process::Command::new(claude)
            .args([
                "--bare",
                "-p",
                &prompt,
                "--allowedTools",
                "Read",
                "--output-format",
                "text",
            ])
            .current_dir(&dir)
            .env("PATH", &path)
            .output();
        if let Ok(Ok(out)) = tokio::time::timeout(timeout, fut).await {
            let resp = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !resp.is_empty() {
                return Ok(FixResult {
                    agent: "Claude".to_string(),
                    response: resp,
                });
            }
        }
    }
    Err("no-agent".to_string())
}

/// Bring the main window to the front (from the tray panel), optionally selecting
/// an app there.
#[tauri::command]
pub fn show_main_window(app: tauri::AppHandle, select: Option<String>) -> Result<(), String> {
    use tauri::{Emitter, Manager};
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    if let Some(name) = select {
        let _ = app.emit("harbor://select", name);
    }
    Ok(())
}

/// Export a registered app to `<root>/harbor.json` so the config is committable
/// and shareable. Returns the written path.
#[tauri::command]
pub async fn export_app(state: State<'_, Arc<AppState>>, app: String) -> Result<String, String> {
    let cfg = state
        .get_config(&app)
        .await
        .ok_or_else(|| format!("no such app: {app}"))?;
    let path = PathBuf::from(&cfg.root).join("harbor.json");
    store::export_harbor_json(&cfg, &path).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn build_mcp_info(token: &str, port: u16, healthy: bool, bridge: &str) -> McpInfo {
    let url = format!("http://127.0.0.1:{port}/mcp");
    let claude_add_command = format!(
        "claude mcp add harbor --scope user --transport stdio -- {}",
        shell_single_quote(bridge)
    );
    let desktop_json = serde_json::to_string_pretty(&serde_json::json!({
        "mcpServers": {
            "harbor": {
                "command": bridge,
                "args": []
            }
        }
    }))
    .expect("serializing static MCP configuration cannot fail");
    McpInfo {
        url,
        port,
        token: token.to_string(),
        healthy,
        version: env!("CARGO_PKG_VERSION").to_string(),
        claude_add_command,
        desktop_json,
        bridge_command: bridge.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_test_config() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "harbor-agent-status-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, "configured").unwrap();
        path
    }

    fn desktop_process(pid: u32, parent: u32, age_secs: u64, command: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent,
            age: std::time::Duration::from_secs(age_secs),
            command: command.to_string(),
        }
    }

    #[test]
    fn parses_ps_elapsed_time() {
        assert_eq!(parse_process_age("05:09").unwrap().as_secs(), 309);
        assert_eq!(parse_process_age("02:03:04").unwrap().as_secs(), 7_384);
        assert_eq!(parse_process_age("2-03:04:05").unwrap().as_secs(), 183_845);
        assert!(parse_process_age("not-an-age").is_none());
    }

    #[test]
    fn finds_a_runtime_process_through_its_ancestors() {
        let roots = std::collections::HashSet::from([10]);
        let parents = std::collections::HashMap::from([(30, 20), (20, 10), (10, 1)]);
        assert!(has_ancestor(30, &roots, &parents));
        assert!(!has_ancestor(40, &roots, &parents));
    }

    #[test]
    fn running_bridge_suppresses_restart_required() {
        let config = status_test_config();
        let processes = vec![
            desktop_process(10, 1, 60, "/Applications/Claude.app/Contents/MacOS/Claude"),
            desktop_process(20, 10, 30, "/tmp/harbor-mcp-bridge"),
        ];
        let state = connection_state(
            ConnectionTarget {
                configured: true,
                config_path: Some(&config),
                kind: AgentKind::ClaudeDesktop,
                resolved_cli: None,
                client_name: "Claude Desktop",
            },
            Some(std::time::Duration::from_secs(120)),
            Some(std::time::Duration::from_secs(120)),
            &processes,
            "/tmp/harbor-mcp-bridge",
        );
        let _ = std::fs::remove_file(config);
        assert!(state.bridge_running);
        assert!(!state.restart_required);
        assert!(state.error.is_none());
    }

    #[test]
    fn config_newer_than_running_client_requires_restart() {
        let config = status_test_config();
        let processes = vec![desktop_process(
            10,
            1,
            60,
            "/Applications/Claude.app/Contents/MacOS/Claude",
        )];
        let state = connection_state(
            ConnectionTarget {
                configured: true,
                config_path: Some(&config),
                kind: AgentKind::ClaudeDesktop,
                resolved_cli: None,
                client_name: "Claude Desktop",
            },
            Some(std::time::Duration::from_secs(120)),
            Some(std::time::Duration::from_secs(120)),
            &processes,
            "/tmp/harbor-mcp-bridge",
        );
        let _ = std::fs::remove_file(config);
        assert!(!state.bridge_running);
        assert!(state.restart_required);
        assert!(state.error.is_none());
    }

    #[test]
    fn running_current_client_without_bridge_reports_error() {
        let config = status_test_config();
        let processes = vec![desktop_process(
            10,
            1,
            0,
            "/Applications/Claude.app/Contents/MacOS/Claude",
        )];
        let state = connection_state(
            ConnectionTarget {
                configured: true,
                config_path: Some(&config),
                kind: AgentKind::ClaudeDesktop,
                resolved_cli: None,
                client_name: "Claude Desktop",
            },
            Some(std::time::Duration::from_secs(120)),
            Some(std::time::Duration::from_secs(120)),
            &processes,
            "/tmp/harbor-mcp-bridge",
        );
        let _ = std::fs::remove_file(config);
        assert!(!state.restart_required);
        assert!(state
            .error
            .as_deref()
            .is_some_and(|error| error.contains("has not launched")));
    }

    #[test]
    fn native_bridge_survives_descriptor_rotation() {
        let config = status_test_config();
        let processes = vec![
            desktop_process(10, 1, 300, "/Applications/Claude.app/Contents/MacOS/Claude"),
            desktop_process(20, 10, 180, "/tmp/harbor-mcp-bridge"),
        ];
        let state = connection_state(
            ConnectionTarget {
                configured: true,
                config_path: Some(&config),
                kind: AgentKind::ClaudeDesktop,
                resolved_cli: None,
                client_name: "Claude Desktop",
            },
            Some(std::time::Duration::from_secs(60)),
            Some(std::time::Duration::from_secs(300)),
            &processes,
            "/tmp/harbor-mcp-bridge",
        );
        let _ = std::fs::remove_file(config);
        assert!(state.bridge_running);
        assert!(!state.restart_required);
        assert!(state.error.is_none());
    }

    #[test]
    fn native_bridge_ignores_client_launcher_wrapper_after_rotation() {
        let config = status_test_config();
        let processes = vec![
            desktop_process(10, 1, 300, "/Applications/Claude.app/Contents/MacOS/Claude"),
            desktop_process(
                20,
                10,
                180,
                "/Applications/Claude.app/Contents/Helpers/disclaimer /tmp/harbor-mcp-bridge",
            ),
            desktop_process(21, 20, 180, "/tmp/harbor-mcp-bridge"),
        ];
        let state = connection_state(
            ConnectionTarget {
                configured: true,
                config_path: Some(&config),
                kind: AgentKind::ClaudeDesktop,
                resolved_cli: None,
                client_name: "Claude Desktop",
            },
            Some(std::time::Duration::from_secs(60)),
            Some(std::time::Duration::from_secs(300)),
            &processes,
            "/tmp/harbor-mcp-bridge",
        );
        let _ = std::fs::remove_file(config);
        assert!(state.bridge_running);
        assert!(!state.restart_required);
    }

    #[test]
    fn native_bridge_older_than_installed_binary_requires_restart() {
        let config = status_test_config();
        let processes = vec![
            desktop_process(10, 1, 300, "/Applications/Claude.app/Contents/MacOS/Claude"),
            desktop_process(20, 10, 180, "/tmp/harbor-mcp-bridge"),
        ];
        let state = connection_state(
            ConnectionTarget {
                configured: true,
                config_path: Some(&config),
                kind: AgentKind::ClaudeDesktop,
                resolved_cli: None,
                client_name: "Claude Desktop",
            },
            Some(std::time::Duration::from_secs(60)),
            Some(std::time::Duration::from_secs(30)),
            &processes,
            "/tmp/harbor-mcp-bridge",
        );
        let _ = std::fs::remove_file(config);
        assert!(!state.bridge_running);
        assert!(state.restart_required);
        assert!(state.error.is_none());
    }

    #[test]
    fn legacy_bridge_older_than_descriptor_requires_restart() {
        let config = status_test_config();
        let processes = vec![
            desktop_process(10, 1, 300, "/Applications/Claude.app/Contents/MacOS/Claude"),
            desktop_process(
                20,
                10,
                175,
                "npx -y mcp-remote@0.1.38 http://127.0.0.1:7000/mcp --header Authorization:${HARBOR_AUTH}",
            ),
        ];
        let state = connection_state(
            ConnectionTarget {
                configured: true,
                config_path: Some(&config),
                kind: AgentKind::ClaudeDesktop,
                resolved_cli: None,
                client_name: "Claude Desktop",
            },
            Some(std::time::Duration::from_secs(60)),
            Some(std::time::Duration::from_secs(300)),
            &processes,
            "/tmp/harbor-mcp-bridge",
        );
        let _ = std::fs::remove_file(config);
        assert!(!state.bridge_running);
        assert!(state.restart_required);
        assert!(state.error.is_none());
    }

    #[test]
    fn claude_code_args_keep_name_before_options_and_command_separator() {
        let args = claude_code_add_args("/tmp/bridge");
        assert_eq!(
            args,
            vec![
                "mcp",
                "add",
                "harbor",
                "--scope",
                "user",
                "--transport",
                "stdio",
                "--",
                "/tmp/bridge",
            ]
        );
    }

    #[test]
    fn legacy_environment_keys_do_not_invalidate_stable_bridge_config() {
        let json = serde_json::json!({
            "command": "/tmp/bridge",
            "env": {
                "HARBOR_SETTINGS": "/tmp/mcp.json",
                "HARBOR_NPX": "/opt/node/bin/npx"
            }
        });
        assert!(json_entry_uses_bridge(&json, "/tmp/bridge", true));

        let toml = r#"
command = "/tmp/bridge"
env = { HARBOR_SETTINGS = "/tmp/mcp.json", HARBOR_NPX = "/opt/node/bin/npx" }
"#
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
        assert!(toml_entry_uses_bridge(toml.as_item(), "/tmp/bridge", true));
    }

    #[test]
    fn native_bridge_header_rejects_the_legacy_shell_launcher() {
        let root = std::env::temp_dir().join(format!(
            "harbor-native-header-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let legacy = root.join("legacy");
        let native = root.join("native");
        std::fs::write(&legacy, b"#!/bin/sh\nexec npx mcp-remote\n").unwrap();
        std::fs::write(&native, [0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2]).unwrap();
        assert!(!has_native_executable_header(&legacy));
        assert!(has_native_executable_header(&native));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manual_claude_command_uses_current_cli_order() {
        let info = build_mcp_info("abc", 7777, true, "/tmp/Application Support/bridge");
        assert!(info
            .claude_add_command
            .starts_with("claude mcp add harbor --scope user"));
        assert!(info.claude_add_command.contains("--transport stdio"));
        assert!(info
            .claude_add_command
            .contains("'/tmp/Application Support/bridge'"));
        assert!(!info.claude_add_command.contains("abc"));
        assert!(!info.desktop_json.contains("mcp-remote"));
        assert!(!info.desktop_json.contains("HARBOR_NPX"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&info.desktop_json).unwrap()["mcpServers"]
                ["harbor"]["command"],
            "/tmp/Application Support/bridge"
        );
    }
}
