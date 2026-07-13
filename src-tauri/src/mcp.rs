//! In-process MCP server (DESIGN.md §3.2, M3).
//!
//! Hosted by the running Harbor app on an axum server bound to `127.0.0.1`,
//! sharing the live `Arc<AppState>` so tools act on real running state. Transport
//! is MCP **Streamable HTTP** via `rmcp`'s `StreamableHttpService`, which is a
//! `tower::Service` we nest at `/mcp`. A bearer-token middleware guards it.

use crate::detect;
use crate::model::{AppConfig, ServiceStatus};
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

#[derive(Debug, Deserialize, JsonSchema)]
struct RegisterArg {
    /// Full app config JSON: { name, root, services: [{ name, cwd, command,
    /// port?, env, dependsOn, healthCheck?, readyLogPattern? }], profiles }.
    /// Tip: call detect_app first and pass (a corrected) `proposed` here.
    config: AppConfig,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct StopLocalArg {
    /// Process-group leader PID returned by list_local_servers.
    pid: u32,
    /// Listening port returned by list_local_servers.
    port: u16,
    /// Exact startedAt identity token returned by list_local_servers.
    started_at: String,
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

    #[rmcp::tool(
        description = "List all registered apps with their current run status and ports",
        annotations(
            title = "List Harbor apps",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
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

    #[rmcp::tool(
        description = "Per-service state, resolved ports, and the port plan for one app",
        annotations(
            title = "Inspect app status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn app_status(
        &self,
        Parameters(AppArg { app }): Parameters<AppArg>,
    ) -> Result<Json<JsonOut>, String> {
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

    #[rmcp::tool(
        description = "Start an approved app under a profile; reuses a matching external server before allocating, then returns the port plan",
        annotations(
            title = "Start app",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn start_app(
        &self,
        Parameters(StartArg { app, profile }): Parameters<StartArg>,
    ) -> Result<Json<JsonOut>, String> {
        let snap = ops::start_app(&self.state, &app, profile.as_deref()).await?;
        Ok(out(serde_json::to_value(snap).map_err(|e| e.to_string())?))
    }

    #[rmcp::tool(
        description = "Stop an app: SIGTERM then SIGKILL its managed process tree, freeing ports",
        annotations(
            title = "Stop app",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn stop_app(
        &self,
        Parameters(AppArg { app }): Parameters<AppArg>,
    ) -> Result<Json<JsonOut>, String> {
        self.state
            .supervisor
            .stop_managed_only(&app)
            .await
            .map_err(|error| error.to_string())?;
        Ok(out(json!({ "app": app, "stopped": true })))
    }

    #[rmcp::tool(
        description = "Restart an app under the same (or given) profile",
        annotations(
            title = "Restart app",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn restart_app(
        &self,
        Parameters(StartArg { app, profile }): Parameters<StartArg>,
    ) -> Result<Json<JsonOut>, String> {
        let selected_profile = match profile {
            Some(profile) => Some(profile),
            None => self
                .state
                .supervisor
                .snapshot(&app)
                .await
                .and_then(|snapshot| snapshot.profile),
        };
        self.state
            .supervisor
            .stop_managed_only(&app)
            .await
            .map_err(|error| error.to_string())?;
        let snap = ops::start_app(&self.state, &app, selected_profile.as_deref()).await?;
        Ok(out(serde_json::to_value(snap).map_err(|e| e.to_string())?))
    }

    #[rmcp::tool(
        description = "Tail recent captured logs for a service",
        annotations(
            title = "Read service logs",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_logs(
        &self,
        Parameters(LogsArg {
            app,
            service,
            lines,
        }): Parameters<LogsArg>,
    ) -> Result<Json<JsonOut>, String> {
        let logs = self
            .state
            .supervisor
            .logs(&app, &service, lines.unwrap_or(200).min(2_000))
            .await;
        Ok(out(json!({
            "app": app,
            "service": service,
            "lines": logs.iter().map(|l| serde_json::to_value(l).unwrap_or(Value::Null)).collect::<Vec<_>>(),
        })))
    }

    #[rmcp::tool(
        description = "Scan a project folder and propose a service config (does not save)",
        annotations(
            title = "Detect project config",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn detect_app(
        &self,
        Parameters(DetectArg { path }): Parameters<DetectArg>,
    ) -> Result<Json<JsonOut>, String> {
        let p = std::path::PathBuf::from(&path);
        if !p.exists() {
            return Err(format!("path does not exist: {path}"));
        }
        if !p.is_dir() {
            return Err(format!("path is not a directory: {path}"));
        }
        let det = detect::detect(&p);
        Ok(out(serde_json::to_value(det).map_err(|e| e.to_string())?))
    }

    #[rmcp::tool(
        description = "Register (or update) an app config in Harbor. Saves it to the registry; \
                       does NOT start it. Pass the full config JSON (e.g. detect_app's proposed, \
                       corrected as needed). Configs supplied by an agent require a person to \
                       approve their commands in Harbor before start_app can execute them.",
        annotations(
            title = "Register app config",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn register_app(
        &self,
        Parameters(RegisterArg { config }): Parameters<RegisterArg>,
    ) -> Result<Json<JsonOut>, String> {
        let mut cfg = config;
        let _lifecycle = self.state.supervisor.lock_lifecycle(&cfg.name).await;
        if self.state.supervisor.is_running(&cfg.name).await {
            return Err("stop the running app before replacing its config".to_string());
        }
        // Trust is never accepted from an MCP payload; only the local UI can
        // approve executable commands.
        cfg.trusted = false;
        let name = cfg.name.clone();
        self.state.upsert(cfg).await.map_err(|e| e.to_string())?;
        Ok(out(json!({ "registered": name, "approvalRequired": true })))
    }

    #[rmcp::tool(
        description = "Inventory local TCP servers, including unknown and duplicate project runs. Returns PID/start identity, command, cwd, HTTP title, Harbor match evidence, and whether safe cleanup is available. Observation only.",
        annotations(
            title = "List local servers",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn list_local_servers(&self) -> Result<Json<JsonOut>, String> {
        let configs = self.state.list_configs().await;
        let tracked = self.state.supervisor.tracked_servers().await;
        let inventory = crate::discovery::scan(&configs, &tracked, self.state.mcp.port)
            .await
            .map_err(|e| e.to_string())?;
        Ok(out(
            serde_json::to_value(inventory).map_err(|e| e.to_string())?
        ))
    }

    #[rmcp::tool(
        description = "Stop one untracked local server previously returned with safeToStop=true. Requires the exact leader PID, listening port, and startedAt token; refuses stale identity, shells, terminals, IDEs, coding agents, Harbor, and Harbor-managed processes.",
        annotations(
            title = "Stop untracked local server",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn stop_local_server(
        &self,
        Parameters(StopLocalArg {
            pid,
            port,
            started_at,
        }): Parameters<StopLocalArg>,
    ) -> Result<Json<JsonOut>, String> {
        if self.state.supervisor.owns_pid(pid).await {
            return Err("process is managed by Harbor; use stop_app instead".to_string());
        }
        crate::discovery::stop_untracked(pid, &started_at, port)
            .await
            .map_err(|e| e.to_string())?;
        Ok(out(json!({ "pid": pid, "stopped": true })))
    }

    #[rmcp::tool(
        description = "Open a running app's primary local URL in the default browser",
        annotations(
            title = "Open app in browser",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn open_app(
        &self,
        Parameters(AppArg { app }): Parameters<AppArg>,
    ) -> Result<Json<JsonOut>, String> {
        let url = ops::open_app(&self.state, &app).await?;
        Ok(out(json!({ "app": app, "url": url })))
    }
}

#[rmcp::tool_handler]
impl ServerHandler for HarborMcp {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        use rmcp::model::*;
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("harbor", env!("CARGO_PKG_VERSION"))
                    .with_title("Harbor")
                    .with_description(
                        "Local development server discovery, orchestration, cleanup, and diagnostics",
                    )
                    .with_website_url("https://github.com/luke-fairbanks/harbor-mcp"),
            )
            .with_instructions(
                "Harbor is the local runtime control plane. Start with list_local_servers to \
                 identify already-running and duplicate project servers, then list_apps/app_status. \
                 Use detect_app + register_app for new folders; agent-registered commands require \
                 local approval in Harbor before start_app. Prefer stop_app for managed apps. Only \
                 use stop_local_server when list_local_servers returned safeToStop=true, passing \
                 its exact pid, port, and startedAt identity. Use get_logs to debug."
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

/// Build the axum router: bearer authentication protects both `/health` and
/// `/mcp`, so a process merely occupying the remembered port cannot impersonate
/// Harbor's readiness route.
pub fn build_router(state: Arc<AppState>, token: String) -> Router {
    let factory_state = state.clone();
    let transport_config = StreamableHttpServerConfig::default().with_allowed_origins([
        "http://localhost",
        "http://127.0.0.1",
        "http://[::1]",
    ]);
    let mcp_service: StreamableHttpService<HarborMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(HarborMcp::new(factory_state.clone())),
            LocalSessionManager::default().into(),
            transport_config,
        );

    let protected = Router::new()
        .route("/health", get(|| async { "Harbor MCP OK" }))
        .nest_service("/mcp", mcp_service)
        .layer(middleware::from_fn_with_state(Auth { token }, auth_mw));

    Router::new().merge(protected)
}

/// Serve on the loopback listener reserved during app setup. Holding the socket
/// from selection through axum startup removes the check-then-bind race.
pub async fn serve_on(
    state: Arc<AppState>,
    token: String,
    listener: std::net::TcpListener,
) -> Result<()> {
    let router = build_router(state, token);
    let listener = tokio::net::TcpListener::from_std(listener)?;
    axum::serve(listener, router).await?;
    Ok(())
}

/// Bind and retain a loopback listener, scanning upward from the preferred port.
pub fn bind_listener(preferred: u16) -> Result<(u16, std::net::TcpListener)> {
    if preferred == 0 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let selected = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        return Ok((selected, listener));
    }

    for offset in 0..100u16 {
        let Some(p) = preferred.checked_add(offset) else {
            break;
        };
        if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", p)) {
            listener.set_nonblocking(true)?;
            return Ok((p, listener));
        }
    }
    Err(anyhow::anyhow!(
        "no loopback port available for Harbor MCP near {preferred}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_retains_the_selected_socket_and_skips_busy_port() {
        let busy = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = busy.local_addr().unwrap().port();
        if port == u16::MAX {
            return;
        }
        let (selected, reserved) = bind_listener(port).unwrap();
        assert_ne!(selected, port);
        assert_eq!(reserved.local_addr().unwrap().port(), selected);
        assert!(std::net::TcpListener::bind(("127.0.0.1", selected)).is_err());
    }

    #[test]
    fn binding_port_zero_reports_the_kernel_selected_port() {
        let (selected, reserved) = bind_listener(0).unwrap();
        assert_ne!(selected, 0);
        assert_eq!(reserved.local_addr().unwrap().port(), selected);
    }

    #[test]
    fn bearer_parser_requires_the_exact_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer secret-token".parse().unwrap());
        assert_eq!(bearer(&headers), Some("secret-token"));
        headers.insert(AUTHORIZATION, "Basic secret-token".parse().unwrap());
        assert_eq!(bearer(&headers), None);
    }
}
