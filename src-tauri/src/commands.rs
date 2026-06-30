//! Tauri command handlers (DESIGN.md §6). Thin wrappers over `AppState`; they
//! return `Result<T, String>` so failures surface as rejected JS promises.
//!
//! The same operations are exposed to Claude over MCP (`mcp.rs`) — both call
//! into the shared `ops` helpers so behavior can't drift between surfaces.

use crate::detect::{self, Detection};
use crate::model::{AppConfig, AppRunSnapshot, LogLine};
use crate::state::AppState;
use crate::{ops, store};
use serde::Serialize;
use std::collections::BTreeMap;
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
pub async fn register_app(state: State<'_, Arc<AppState>>, config: AppConfig) -> Result<(), String> {
    state.upsert(config).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_app(
    state: State<'_, Arc<AppState>>,
    app: String,
    config: AppConfig,
) -> Result<(), String> {
    // Allow rename: remove the old key if the name changed.
    if app != config.name {
        let _ = state.remove(&app).await;
    }
    state.upsert(config).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remove_app(state: State<'_, Arc<AppState>>, app: String) -> Result<bool, String> {
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
        return Err(format!("not a folder: {path} — drop a project folder, not a file"));
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
    Ok(build_mcp_info(&state.mcp.token, state.mcp.port))
}

/// Import a shareable `harbor.json`. `path` may be the file itself or the folder
/// containing it; the app is registered under its `name`.
#[tauri::command]
pub async fn import_app(state: State<'_, Arc<AppState>>, path: String) -> Result<AppConfig, String> {
    let p = PathBuf::from(&path);
    let (file, root) = if p.is_dir() {
        (p.join("harbor.json"), p.clone())
    } else {
        let root = p.parent().map(|x| x.to_path_buf()).unwrap_or_else(|| p.clone());
        (p.clone(), root)
    };
    if !file.exists() {
        return Err(format!("no harbor.json at {}", file.display()));
    }
    let cfg = store::import_harbor_json(&file, &root).map_err(|e| e.to_string())?;
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

fn codex_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let base = std::env::var("CODEX_HOME").unwrap_or_else(|_| format!("{home}/.codex"));
    Some(PathBuf::from(base).join("config.toml"))
}

#[tauri::command]
pub async fn agents_status(state: State<'_, Arc<AppState>>) -> Result<AgentStatus, String> {
    let _ = &state; // status doesn't need live state
    let claude = crate::sysenv::resolve_bin("claude");
    let code_cli = claude.is_some();
    let mut code_connected = false;
    if let Some(bin) = &claude {
        let out = tokio::process::Command::new(bin)
            .args(["mcp", "get", "harbor"])
            .env("PATH", crate::sysenv::enriched_path().unwrap_or_default())
            .output()
            .await;
        if let Ok(o) = out {
            code_connected = o.status.success();
        }
    }

    let cfg = claude_desktop_config_path();
    let desktop_installed = cfg
        .as_ref()
        .and_then(|p| p.parent().map(|d| d.exists()))
        .unwrap_or(false);
    let mut desktop_connected = false;
    if let Some(p) = &cfg {
        if let Ok(text) = std::fs::read_to_string(p) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                desktop_connected = v
                    .get("mcpServers")
                    .and_then(|m| m.get("harbor"))
                    .is_some();
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
                    .map(|t| t.contains_key("harbor"))
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
    let bin = crate::sysenv::resolve_bin("claude")
        .ok_or("Claude Code CLI (`claude`) not found.")?;
    let url = format!("http://127.0.0.1:{}/mcp", state.mcp.port);
    let header = format!("Authorization: Bearer {}", state.mcp.token);
    let path = crate::sysenv::enriched_path().unwrap_or_default();

    // Remove any prior entry first so reconnecting refreshes the token/port.
    let _ = tokio::process::Command::new(&bin)
        .args(["mcp", "remove", "-s", "user", "harbor"])
        .env("PATH", &path)
        .output()
        .await;

    let out = tokio::process::Command::new(&bin)
        .args([
            "mcp", "add", "-s", "user", "--transport", "http", "harbor", &url, "--header",
            &header,
        ])
        .env("PATH", &path)
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if out.status.success() {
        Ok("Connected to Claude Code.".to_string())
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
        let _ = std::fs::write(p.with_extension("json.harbor-bak"), &text);
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if !root.is_object() {
        root = serde_json::json!({});
    }

    // Claude Desktop's config is stdio-only, so bridge to our HTTP server with
    // `mcp-remote`. Use an absolute `npx` (Claude Desktop launches commands with
    // a minimal PATH), and pass the bearer header via an env var to dodge the
    // mcp-remote `--header` space-splitting bug.
    let npx = crate::sysenv::resolve_bin("npx")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "npx".to_string());
    let url = format!("http://127.0.0.1:{}/mcp", state.mcp.port);
    // `--allow-http` because the endpoint is plain http on loopback;
    // `--transport http-only` skips an SSE-fallback probe (we serve /mcp).
    let entry = serde_json::json!({
        "command": npx,
        "args": [
            "-y", "mcp-remote", url,
            "--header", "Authorization:${HARBOR_AUTH}",
            "--allow-http", "--transport", "http-only"
        ],
        "env": { "HARBOR_AUTH": format!("Bearer {}", state.mcp.token) }
    });

    let obj = root.as_object_mut().unwrap();
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        *servers = serde_json::json!({});
    }
    servers
        .as_object_mut()
        .unwrap()
        .insert("harbor".to_string(), entry);

    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    std::fs::write(&p, text).map_err(|e| e.to_string())?;
    Ok("Added to Claude Desktop — fully quit and reopen Claude Desktop to use Harbor.".to_string())
}

#[tauri::command]
pub async fn connect_codex(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let p = codex_config_path().ok_or("could not resolve ~/.codex/config.toml")?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let original = std::fs::read_to_string(&p).unwrap_or_default();
    if !original.is_empty() {
        let _ = std::fs::write(p.with_extension("toml.harbor-bak"), &original);
    }
    let mut doc = original
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("parsing ~/.codex/config.toml: {e}"))?;

    // Compatibility flag for streamable-HTTP MCP on older Codex (harmless on new).
    doc["experimental_use_rmcp_client"] = toml_edit::value(true);

    let url = format!("http://127.0.0.1:{}/mcp", state.mcp.port);
    let mut headers = toml_edit::InlineTable::new();
    headers.insert(
        "Authorization",
        toml_edit::Value::from(format!("Bearer {}", state.mcp.token)),
    );

    let mut tbl = toml_edit::Table::new();
    tbl["url"] = toml_edit::value(url);
    tbl["http_headers"] = toml_edit::value(headers);

    if !doc.get("mcp_servers").map(|i| i.is_table()).unwrap_or(false) {
        doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["mcp_servers"]["harbor"] = toml_edit::Item::Table(tbl);

    std::fs::write(&p, doc.to_string()).map_err(|e| e.to_string())?;
    Ok("Connected to Codex — restart Codex to use Harbor.".to_string())
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
    let root = state.get_config(&app).await.map(|c| c.root).unwrap_or_default();
    let dir = if root.is_empty() {
        ".".to_string()
    } else {
        root
    };
    let timeout = std::time::Duration::from_secs(150);

    if let Some(codex) = crate::sysenv::resolve_bin("codex") {
        let fut = tokio::process::Command::new(codex)
            .args(["exec", "--sandbox", "read-only", "--skip-git-repo-check", &prompt])
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

pub fn build_mcp_info(token: &str, port: u16) -> McpInfo {
    let url = format!("http://127.0.0.1:{port}/mcp");
    let claude_add_command = format!(
        "claude mcp add --transport http harbor {url} --header \"Authorization: Bearer {token}\""
    );
    let desktop_json = format!(
        "{{\n  \"mcpServers\": {{\n    \"harbor\": {{\n      \"command\": \"npx\",\n      \"args\": [\"-y\", \"mcp-remote\", \"{url}\", \"--header\", \"Authorization:${{HARBOR_AUTH}}\", \"--allow-http\", \"--transport\", \"http-only\"],\n      \"env\": {{ \"HARBOR_AUTH\": \"Bearer {token}\" }}\n    }}\n  }}\n}}"
    );
    McpInfo {
        url,
        port,
        token: token.to_string(),
        claude_add_command,
        desktop_json,
    }
}
