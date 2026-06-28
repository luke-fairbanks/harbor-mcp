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
    Ok(detect::detect(&p))
}

#[tauri::command]
pub async fn open_app(state: State<'_, Arc<AppState>>, app: String) -> Result<String, String> {
    ops::open_app(&state, &app).await
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
pub struct ClaudeStatus {
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
}

fn claude_desktop_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join("Library/Application Support/Claude/claude_desktop_config.json"))
}

#[tauri::command]
pub async fn claude_status(state: State<'_, Arc<AppState>>) -> Result<ClaudeStatus, String> {
    let _ = &state; // touched for symmetry; status doesn't need live state
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

    Ok(ClaudeStatus {
        code_cli,
        code_connected,
        desktop_installed,
        desktop_connected,
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

    let entry = serde_json::json!({
        "type": "http",
        "url": format!("http://127.0.0.1:{}/mcp", state.mcp.port),
        "headers": { "Authorization": format!("Bearer {}", state.mcp.token) }
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
    Ok("Added to Claude Desktop — restart it to use Harbor.".to_string())
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
        "{{\n  \"mcpServers\": {{\n    \"harbor\": {{\n      \"type\": \"http\",\n      \"url\": \"{url}\",\n      \"headers\": {{ \"Authorization\": \"Bearer {token}\" }}\n    }}\n  }}\n}}"
    );
    McpInfo {
        url,
        port,
        token: token.to_string(),
        claude_add_command,
        desktop_json,
    }
}
