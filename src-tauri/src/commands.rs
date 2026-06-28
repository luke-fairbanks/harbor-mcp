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
