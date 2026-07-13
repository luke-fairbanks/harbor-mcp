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
    if !cfg.trusted {
        return Err(format!(
            "approval required: review and trust '{app}' in Harbor before its commands can run"
        ));
    }
    let profile = profile.unwrap_or("default");
    if profile != "default" && !cfg.profiles.contains_key(profile) {
        let available = if cfg.profiles.is_empty() {
            "default".to_string()
        } else {
            cfg.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
        };
        return Err(format!(
            "unknown profile '{profile}' for {app}; available profiles: {available}"
        ));
    }
    state
        .supervisor
        .start(&cfg, profile)
        .await
        .map_err(|e| e.to_string())
}

pub async fn stop_app(state: &AppState, app: &str) -> Result<(), String> {
    state.supervisor.stop(app).await.map_err(|e| e.to_string())
}

/// Stop then start an app. Re-derives the running profile if none is given, so a
/// `dev` run restarts as `dev`. The stop phase sets the intentional-stop marker,
/// so a Restart is never mistaken for a crash.
pub async fn restart_app(
    state: &AppState,
    app: &str,
    profile: Option<&str>,
) -> Result<AppRunSnapshot, String> {
    let profile = match profile {
        Some(p) => Some(p.to_string()),
        None => state.supervisor.snapshot(app).await.and_then(|s| s.profile),
    };
    stop_app(state, app).await?;
    start_app(state, app, profile.as_deref()).await
}

/// Stop every running app (including servers started outside Harbor — the user
/// wants their ports freed). Best-effort: a per-app failure never aborts the sweep.
pub async fn stop_all(state: &AppState) -> Result<(), String> {
    for cfg in state.list_configs().await {
        if state.supervisor.is_running(&cfg.name).await {
            let _ = state.supervisor.stop(&cfg.name).await;
        }
    }
    Ok(())
}

/// Start every not-already-running app under its default profile, sequentially
/// (to avoid a spawn storm). Best-effort.
pub async fn start_all(state: &AppState) -> Result<(), String> {
    for cfg in state.list_configs().await {
        if cfg.trusted && !state.supervisor.is_running(&cfg.name).await {
            let _ = state.supervisor.start(&cfg, "default").await;
        }
    }
    Ok(())
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
    let url = format!("http://localhost:{port}");
    open_url(&url)?;
    Ok(url)
}

#[cfg(target_os = "macos")]
pub fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn open_url(url: &str) -> Result<(), String> {
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
