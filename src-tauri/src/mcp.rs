//! In-process MCP server (DESIGN.md §3.2, M3).
//!
//! Hosted by the running Harbor app on an axum server bound to `127.0.0.1`,
//! sharing the live `Arc<AppState>` so tools act on real running state. Transport
//! is MCP **Streamable HTTP** via `rmcp`'s `StreamableHttpService`, which is a
//! `tower::Service` we nest at `/mcp`. A bearer-token middleware guards it.

use crate::detect;
use crate::model::ServiceStatus;
use crate::ops;
use crate::state::AppState;
use anyhow::Result;
use axum::{
    extract::{Request, State as AxState},
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Router,
};
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::transport::{
    streamable_http_server::{session::local::LocalSessionManager, tower::StreamableHttpService},
    StreamableHttpServerConfig,
};
use rmcp::{Json, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

// ---- tool argument types (input schemas auto-derived) ---------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct AppArg {
    /// The registered app name.
    app: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StartArg {
    /// The registered app name.
    app: String,
    /// Profile to launch (e.g. "default" or "dev"). Defaults to "default".
    #[serde(default)]
    profile: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LogsArg {
    app: String,
    service: String,
    /// How many recent lines to return (default 200).
    #[serde(default)]
    lines: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DetectArg {
    /// Absolute path to a project folder to scan.
    path: String,
}

/// Uniform object-rooted output wrapper. rmcp requires a tool's `outputSchema`
/// root to be type `"object"`; a bare `serde_json::Value` schema is `"any"` and
/// is rejected at runtime. Wrapping the payload under `result` guarantees a
/// valid object root for every tool.
#[derive(serde::Serialize, JsonSchema)]
struct JsonOut {
    result: Value,
}

fn out(v: Value) -> Json<JsonOut> {
    Json(JsonOut { result: v })
}

// ---- the MCP server, holding live shared state ----------------------------

#[derive(Clone)]
pub struct HarborMcp {
    state: Arc<AppState>,
    // Consumed by the `#[tool_handler]`-generated code, not read directly.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[rmcp::tool_router]
impl HarborMcp {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }

    #[rmcp::tool(description = "List all registered apps with their current run status and ports")]
    async fn list_apps(&self) -> Result<Json<JsonOut>, String> {
        let mut apps = Vec::new();
        for cfg in self.state.list_configs().await {
            let run = self.state.supervisor.snapshot(&cfg.name).await;
            let running = self.state.supervisor.is_running(&cfg.name).await;
            apps.push(json!({
                "name": cfg.name,
                "root": cfg.root,
                "running": running,
                "profiles": cfg.profiles,
                "services": cfg.services.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                "run": run.and_then(|r| serde_json::to_value(r).ok()),
            }));
        }
        Ok(out(json!({ "apps": apps })))
    }

    #[rmcp::tool(description = "Per-service state, resolved ports, and the port plan for one app")]
    async fn app_status(&self, Parameters(AppArg { app }): Parameters<AppArg>) -> Result<Json<JsonOut>, String> {
        match self.state.supervisor.snapshot(&app).await {
            Some(snap) => Ok(out(serde_json::to_value(snap).map_err(|e| e.to_string())?)),
            None => {
                let known = self.state.get_config(&app).await.is_some();
                Ok(out(json!({
                    "app": app,
                    "running": false,
                    "registered": known,
                    "services": [],
                    "portPlan": [],
                })))
            }
        }
    }

    #[rmcp::tool(description = "Start an app under a profile; resolves ports, spawns services in dependency order, returns the port plan")]
    async fn start_app(&self, Parameters(StartArg { app, profile }): Parameters<StartArg>) -> Result<Json<JsonOut>, String> {
        let snap = ops::start_app(&self.state, &app, profile.as_deref()).await?;
        Ok(out(serde_json::to_value(snap).map_err(|e| e.to_string())?))
    }

    #[rmcp::tool(description = "Stop an app: SIGTERM then SIGKILL its whole process tree, freeing ports")]
    async fn stop_app(&self, Parameters(AppArg { app }): Parameters<AppArg>) -> Result<Json<JsonOut>, String> {
        ops::stop_app(&self.state, &app).await?;
        Ok(out(json!({ "app": app, "stopped": true })))
    }

    #[rmcp::tool(description = "Tail recent captured logs for a service")]
    async fn get_logs(&self, Parameters(LogsArg { app, service, lines }): Parameters<LogsArg>) -> Result<Json<JsonOut>, String> {
        let logs = self
            .state
            .supervisor
            .logs(&app, &service, lines.unwrap_or(200))
            .await;
        Ok(out(json!({
            "app": app,
            "service": service,
            "lines": logs.iter().map(|l| serde_json::to_value(l).unwrap_or(Value::Null)).collect::<Vec<_>>(),
        })))
    }

    #[rmcp::tool(description = "Scan a project folder and propose a service config (does not save)")]
    async fn detect_app(&self, Parameters(DetectArg { path }): Parameters<DetectArg>) -> Result<Json<JsonOut>, String> {
        let p = std::path::PathBuf::from(&path);
        if !p.exists() {
            return Err(format!("path does not exist: {path}"));
        }
        let det = detect::detect(&p);
        Ok(out(serde_json::to_value(det).map_err(|e| e.to_string())?))
    }
}

#[rmcp::tool_handler]
impl ServerHandler for HarborMcp {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        use rmcp::model::*;
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Harbor: registers local apps and runs their services with automatic port \
                 allocation. Use list_apps/app_status to inspect, detect_app to scan a folder, \
                 start_app/stop_app to control lifecycle, get_logs to debug."
                    .to_string(),
            )
    }
}

// Helper so status enums round-trip if needed elsewhere.
#[allow(dead_code)]
fn status_str(s: ServiceStatus) -> &'static str {
    match s {
        ServiceStatus::Stopped => "stopped",
        ServiceStatus::Starting => "starting",
        ServiceStatus::Ready => "ready",
        ServiceStatus::Unhealthy => "unhealthy",
        ServiceStatus::Exited => "exited",
    }
}

// ---- bearer-token auth middleware -----------------------------------------

#[derive(Clone)]
struct Auth {
    token: String,
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

async fn auth_mw(
    AxState(auth): AxState<Auth>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    match bearer(&headers) {
        Some(t) if t == auth.token => Ok(next.run(request).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

// ---- router + serve -------------------------------------------------------

/// Build the axum router: `/health` (open) + `/mcp` (bearer-guarded MCP).
pub fn build_router(state: Arc<AppState>, token: String) -> Router {
    let factory_state = state.clone();
    let mcp_service: StreamableHttpService<HarborMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(HarborMcp::new(factory_state.clone())),
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default(),
        );

    let protected = Router::new()
        .nest_service("/mcp", mcp_service)
        .layer(middleware::from_fn_with_state(Auth { token }, auth_mw));

    Router::new()
        .route("/health", get(|| async { "Harbor MCP OK" }))
        .merge(protected)
}

/// Bind `127.0.0.1:port` and serve forever. The caller picks a free port up
/// front (see `pick_free_port`).
pub async fn serve(state: Arc<AppState>, token: String, port: u16) -> Result<()> {
    let router = build_router(state, token);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}

/// Choose a bindable port for the MCP server, starting at `preferred` and
/// scanning upward. Used at startup so the persisted port is always live.
pub fn pick_free_port(preferred: u16) -> u16 {
    for p in preferred..preferred.saturating_add(100) {
        if std::net::TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return p;
        }
    }
    preferred
}
