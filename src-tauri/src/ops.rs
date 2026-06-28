//! Operations shared by the Tauri command layer and the MCP server, so the two
//! Claude-facing surfaces behave identically.

use crate::model::AppRunSnapshot;
use crate::state::AppState;

pub async fn start_app(
    state: &AppState,
    app: &str,
    profile: Option<&str>,
) -> Result<AppRunSnapshot, String> {
    let cfg = state
        .get_config(app)
        .await
        .ok_or_else(|| format!("no such app: {app}"))?;
    let profile = profile.unwrap_or("default");
    state
        .supervisor
        .start(&cfg, profile)
        .await
        .map_err(|e| e.to_string())
}

pub async fn stop_app(state: &AppState, app: &str) -> Result<(), String> {
    state.supervisor.stop(app).await.map_err(|e| e.to_string())
}

/// Open the served URL of a running app. Picks the most "front-door" service
/// that has a port: prefer `web`, then `server`, then any.
pub async fn open_app(state: &AppState, app: &str) -> Result<String, String> {
    let snap = state
        .supervisor
        .snapshot(app)
        .await
        .ok_or_else(|| format!("{app} is not running"))?;
    let pick = snap
        .services
        .iter()
        .filter(|s| s.port.is_some())
        .max_by_key(|s| match s.name.as_str() {
            "web" => 2,
            "server" => 1,
            _ => 0,
        });
    let port = pick
        .and_then(|s| s.port)
        .ok_or_else(|| "no running service exposes a port to open".to_string())?;
    let url = format!("http://127.0.0.1:{port}");
    open_url(&url)?;
    Ok(url)
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
fn open_url(url: &str) -> Result<(), String> {
    // Fallback for other platforms; xdg-open on Linux, start on Windows.
    #[cfg(target_os = "windows")]
    let (prog, args): (&str, Vec<&str>) = ("cmd", vec!["/C", "start", url]);
    #[cfg(not(target_os = "windows"))]
    let (prog, args): (&str, Vec<&str>) = ("xdg-open", vec![url]);
    std::process::Command::new(prog)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
