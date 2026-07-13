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
use std::io::Write;
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
    Ok(build_mcp_info(&state.mcp.token, port, healthy))
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
    /// Harbor is registered in Claude Code (user scope).
    #[serde(rename = "codeConnected")]
    pub code_connected: bool,
    /// Claude Desktop appears installed (its config dir exists).
    #[serde(rename = "desktopInstalled")]
    pub desktop_installed: bool,
    /// Harbor is present in claude_desktop_config.json.
    #[serde(rename = "desktopConnected")]
    pub desktop_connected: bool,
    /// `codex` CLI found, or a ~/.codex config exists.
    #[serde(rename = "codexInstalled")]
    pub codex_installed: bool,
    /// Harbor is present in ~/.codex/config.toml.
    #[serde(rename = "codexConnected")]
    pub codex_connected: bool,
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

fn toml_nested_str<'a>(item: Option<&'a toml_edit::Item>, key: &str) -> Option<&'a str> {
    let item = item?;
    item.as_inline_table()
        .and_then(|table| table.get(key))
        .and_then(|value| value.as_str())
        .or_else(|| {
            item.as_table_like()
                .and_then(|table| table.get(key))
                .and_then(|value| value.as_str())
        })
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

const MCP_BRIDGE_SCRIPT: &str = r#"#!/bin/sh
set -eu

settings="$HARBOR_SETTINGS"
npx="$HARBOR_NPX"
# npx installations managed by nvm/asdf often use `#!/usr/bin/env node`.
# A GUI-launched agent has a minimal PATH, so include npx's own directory.
npx_dir=${HARBOR_NPX%/*}
export PATH="$npx_dir:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
port=$(/usr/bin/plutil -extract port raw -o - "$settings")
token=$(/usr/bin/plutil -extract token raw -o - "$settings")
export HARBOR_AUTH="Bearer $token"

# A configured agent should work after a reboot without asking the user to
# remember to open Harbor first. Launch it quietly and wait for its health route.
listener_owned_by_me() {
  pid=$(/usr/sbin/lsof -nP "-iTCP:$port" -sTCP:LISTEN -t 2>/dev/null | /usr/bin/head -n 1)
  [ -n "$pid" ] || return 1
  owner=$(/bin/ps -o uid= -p "$pid" 2>/dev/null | /usr/bin/tr -d ' ')
  [ "$owner" = "$(/usr/bin/id -u)" ]
}

is_harbor() {
  listener_owned_by_me || return 1
  response=$(/usr/bin/curl -fsS --max-time 1 \
    -H "Authorization: $HARBOR_AUTH" \
    "http://127.0.0.1:$port/health" 2>/dev/null || true)
  [ "$response" = "Harbor MCP OK" ]
}

if ! is_harbor; then
  /usr/bin/open -gj -a Harbor >/dev/null 2>&1 || true
  attempts=0
  while [ "$attempts" -lt 40 ]; do
    /bin/sleep 0.25
    port=$(/usr/bin/plutil -extract port raw -o - "$settings")
    token=$(/usr/bin/plutil -extract token raw -o - "$settings")
    export HARBOR_AUTH="Bearer $token"
    if is_harbor; then
      break
    fi
    attempts=$((attempts + 1))
  done
fi

if ! is_harbor; then
  echo "Harbor did not start on a listener owned by this user; refusing to send its MCP token." >&2
  exit 1
fi

exec "$npx" -y mcp-remote@0.1.38 \
  "http://127.0.0.1:$port/mcp" \
  --header 'Authorization:${HARBOR_AUTH}' \
  --allow-http \
  --transport http-only
"#;

/// Install a stable stdio launcher whose config contains no token or runtime
/// port. It reads Harbor's protected descriptor whenever an agent starts it.
fn ensure_mcp_bridge(state: &AppState) -> Result<(String, String, String), String> {
    let bridge = state.store.bridge_path();
    write_private_atomic(&bridge, MCP_BRIDGE_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bridge, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let npx = crate::sysenv::resolve_bin("npx")
        .ok_or("Node.js/npx is required for the MCP desktop bridge")?
        .to_string_lossy()
        .into_owned();
    Ok((
        bridge.to_string_lossy().into_owned(),
        state.store.settings_path().to_string_lossy().into_owned(),
        npx,
    ))
}

fn claude_code_add_args(settings: &str, npx: &str, bridge: &str) -> Vec<String> {
    vec![
        "mcp".into(),
        "add".into(),
        "harbor".into(),
        "--scope".into(),
        "user".into(),
        "--transport".into(),
        "stdio".into(),
        "--env".into(),
        format!("HARBOR_SETTINGS={settings}"),
        "--env".into(),
        format!("HARBOR_NPX={npx}"),
        "--".into(),
        bridge.into(),
    ]
}

#[tauri::command]
pub async fn agents_status(state: State<'_, Arc<AppState>>) -> Result<AgentStatus, String> {
    let expected_url = format!("http://127.0.0.1:{}/mcp", state.mcp.port);
    let expected_auth = format!("Bearer {}", state.mcp.token);
    let expected_bridge = state.store.bridge_path().to_string_lossy().into_owned();
    let expected_settings = state.store.settings_path().to_string_lossy().into_owned();
    let expected_npx =
        crate::sysenv::resolve_bin("npx").map(|path| path.to_string_lossy().into_owned());
    let bridge_current = std::fs::read_to_string(&expected_bridge)
        .map(|script| script == MCP_BRIDGE_SCRIPT)
        .unwrap_or(false);
    let claude = crate::sysenv::resolve_bin("claude");
    let code_cli = claude.is_some();
    let mut code_connected = false;
    if let Some(path) = claude_code_config_path() {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(entry) = config
                    .get("mcpServers")
                    .and_then(|servers| servers.get("harbor"))
                {
                    let native_http = entry.get("url").and_then(|value| value.as_str())
                        == Some(expected_url.as_str())
                        && entry
                            .get("headers")
                            .and_then(|headers| headers.get("Authorization"))
                            .and_then(|value| value.as_str())
                            == Some(expected_auth.as_str());
                    let npx_matches = expected_npx.as_deref().is_some_and(|expected| {
                        entry
                            .get("env")
                            .and_then(|env| env.get("HARBOR_NPX"))
                            .and_then(|value| value.as_str())
                            == Some(expected)
                    });
                    let stable_bridge = bridge_current
                        && entry.get("command").and_then(|value| value.as_str())
                            == Some(expected_bridge.as_str())
                        && entry
                            .get("env")
                            .and_then(|env| env.get("HARBOR_SETTINGS"))
                            .and_then(|value| value.as_str())
                            == Some(expected_settings.as_str())
                        && npx_matches;
                    code_connected = native_http || stable_bridge;
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
    let mut desktop_connected = false;
    if let Some(p) = &cfg {
        if let Ok(text) = std::fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                desktop_connected = v
                    .get("mcpServers")
                    .and_then(|m| m.get("harbor"))
                    .map(|entry| {
                        let has_url = entry
                            .get("args")
                            .and_then(|a| a.as_array())
                            .map(|args| {
                                args.iter()
                                    .any(|v| v.as_str() == Some(expected_url.as_str()))
                            })
                            .unwrap_or(false);
                        let has_token = entry
                            .get("env")
                            .and_then(|e| e.get("HARBOR_AUTH"))
                            .and_then(|v| v.as_str())
                            == Some(expected_auth.as_str());
                        let npx_matches = expected_npx.as_deref().is_some_and(|expected| {
                            entry
                                .get("env")
                                .and_then(|e| e.get("HARBOR_NPX"))
                                .and_then(|v| v.as_str())
                                == Some(expected)
                        });
                        let has_bridge = bridge_current
                            && entry.get("command").and_then(|v| v.as_str())
                                == Some(expected_bridge.as_str())
                            && entry
                                .get("env")
                                .and_then(|e| e.get("HARBOR_SETTINGS"))
                                .and_then(|v| v.as_str())
                                == Some(expected_settings.as_str())
                            && npx_matches;
                        (has_url && has_token) || has_bridge
                    })
                    .unwrap_or(false);
            }
        }
    }

    // Codex
    let codex_cli = crate::sysenv::resolve_bin("codex").is_some();
    let cxp = codex_config_path();
    let codex_installed = codex_cli
        || cxp
            .as_ref()
            .map(|p| p.exists() || p.parent().map(|d| d.exists()).unwrap_or(false))
            .unwrap_or(false);
    let mut codex_connected = false;
    if let Some(p) = &cxp {
        if let Ok(text) = std::fs::read_to_string(p) {
            if let Ok(doc) = text.parse::<toml_edit::DocumentMut>() {
                codex_connected = doc
                    .get("mcp_servers")
                    .and_then(|t| t.as_table_like())
                    .and_then(|t| t.get("harbor"))
                    .and_then(|item| item.as_table_like())
                    .map(|harbor| {
                        let enabled = match harbor.get("enabled") {
                            None => true,
                            Some(value) => value.as_bool() == Some(true),
                        };
                        let url_ok = harbor.get("url").and_then(|v| v.as_str())
                            == Some(expected_url.as_str());
                        let auth_ok = toml_nested_str(harbor.get("http_headers"), "Authorization")
                            == Some(expected_auth.as_str());
                        let npx_matches = expected_npx.as_deref().is_some_and(|expected| {
                            toml_nested_str(harbor.get("env"), "HARBOR_NPX") == Some(expected)
                        });
                        let bridge_ok = bridge_current
                            && harbor.get("command").and_then(|v| v.as_str())
                                == Some(expected_bridge.as_str())
                            && toml_nested_str(harbor.get("env"), "HARBOR_SETTINGS")
                                == Some(expected_settings.as_str())
                            && npx_matches;
                        enabled && ((url_ok && auth_ok) || bridge_ok)
                    })
                    .unwrap_or(false);
            }
        }
    }

    Ok(AgentStatus {
        code_cli,
        code_connected,
        desktop_installed,
        desktop_connected,
        codex_installed,
        codex_connected,
    })
}

#[tauri::command]
pub async fn connect_claude_code(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let bin =
        crate::sysenv::resolve_bin("claude").ok_or("Claude Code CLI (`claude`) not found.")?;
    let (bridge, settings, npx) = ensure_mcp_bridge(&state)?;
    let add_args = claude_code_add_args(&settings, &npx, &bridge);
    let path = crate::sysenv::enriched_path().unwrap_or_default();

    // Remove any prior entry first so reconnecting refreshes the token/port.
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
        Ok("Connected to Claude Code with Harbor's restart-safe launcher.".to_string())
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

    // Claude Desktop is stdio-only. Its config points to Harbor's stable
    // launcher, which reads the protected live endpoint descriptor at startup.
    let (bridge, settings, npx) = ensure_mcp_bridge(&state)?;
    let entry = serde_json::json!({
        "command": bridge,
        "args": [],
        "env": {
            "HARBOR_SETTINGS": settings,
            "HARBOR_NPX": npx
        }
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

    // Streamable HTTP is stable in current Codex. Remove the old compatibility
    // flag Harbor used to inject instead of mutating an unrelated global setting.
    doc.remove("experimental_use_rmcp_client");

    let (bridge, settings, npx) = ensure_mcp_bridge(&state)?;
    let mut env = toml_edit::InlineTable::new();
    env.insert("HARBOR_SETTINGS", toml_edit::Value::from(settings));
    env.insert("HARBOR_NPX", toml_edit::Value::from(npx));
    let mut tbl = toml_edit::Table::new();
    tbl["command"] = toml_edit::value(bridge);
    tbl["env"] = toml_edit::value(env);
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
        "Connected to Codex with Harbor's restart-safe launcher — restart Codex to use it."
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

pub fn build_mcp_info(token: &str, port: u16, healthy: bool) -> McpInfo {
    let url = format!("http://127.0.0.1:{port}/mcp");
    let claude_add_command = format!(
        "claude mcp add harbor --scope user --transport http {url} --header \"Authorization: Bearer {token}\""
    );
    let desktop_json = format!(
        "{{\n  \"mcpServers\": {{\n    \"harbor\": {{\n      \"command\": \"npx\",\n      \"args\": [\"-y\", \"mcp-remote@0.1.38\", \"{url}\", \"--header\", \"Authorization:${{HARBOR_AUTH}}\", \"--allow-http\", \"--transport\", \"http-only\"],\n      \"env\": {{ \"HARBOR_AUTH\": \"Bearer {token}\" }}\n    }}\n  }}\n}}"
    );
    McpInfo {
        url,
        port,
        token: token.to_string(),
        healthy,
        version: env!("CARGO_PKG_VERSION").to_string(),
        claude_add_command,
        desktop_json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_code_args_keep_name_before_options_and_command_separator() {
        let args = claude_code_add_args("/tmp/mcp.json", "/opt/node/bin/npx", "/tmp/bridge");
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
                "--env",
                "HARBOR_SETTINGS=/tmp/mcp.json",
                "--env",
                "HARBOR_NPX=/opt/node/bin/npx",
                "--",
                "/tmp/bridge",
            ]
        );
    }

    #[test]
    fn manual_claude_command_uses_current_cli_order() {
        let info = build_mcp_info("abc", 7777, true);
        assert!(info
            .claude_add_command
            .starts_with("claude mcp add harbor --scope user"));
    }

    #[test]
    fn bridge_is_valid_shell_and_gates_token_forwarding() {
        let status = std::process::Command::new("sh")
            .args(["-n", "-c", MCP_BRIDGE_SCRIPT])
            .status()
            .expect("run sh syntax check");
        assert!(status.success());
        assert!(MCP_BRIDGE_SCRIPT.contains("listener_owned_by_me"));
        assert!(MCP_BRIDGE_SCRIPT.contains("if ! is_harbor; then"));
        assert!(MCP_BRIDGE_SCRIPT.contains("mcp-remote@0.1.38"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bridge_smoke_test_works_with_minimal_environment() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut served = 0;
            while served < 2 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut buf = [0u8; 512];
                        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                            let count = stream.read(&mut buf).unwrap();
                            if count == 0 {
                                break;
                            }
                            request.extend_from_slice(&buf[..count]);
                        }
                        let request = String::from_utf8_lossy(&request);
                        assert!(request.contains("Authorization: Bearer smoke-token"));
                        stream
                            .write_all(
                                b"HTTP/1.0 200 OK\r\nContent-Length: 13\r\n\r\nHarbor MCP OK",
                            )
                            .unwrap();
                        served += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => panic!("accepting smoke-test health request: {error}"),
                }
            }
            served
        });

        let settings = std::env::temp_dir().join(format!(
            "harbor-bridge-smoke-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(
            &settings,
            serde_json::json!({ "port": port, "token": "smoke-token" }).to_string(),
        )
        .unwrap();
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", MCP_BRIDGE_SCRIPT])
            .env_clear()
            .env("HARBOR_SETTINGS", &settings)
            .env("HARBOR_NPX", "/usr/bin/true")
            .output()
            .unwrap();
        let _ = std::fs::remove_file(settings);
        assert!(
            output.status.success(),
            "bridge failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(server.join().unwrap(), 2);
    }
}
