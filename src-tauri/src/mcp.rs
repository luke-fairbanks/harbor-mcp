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
    streamable_http_server::{session::never::NeverSessionManager, tower::StreamableHttpService},
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
#[derive(serde::Serialize)]
struct JsonOut {
    result: Value,
}

impl JsonSchema for JsonOut {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "HarborToolResult".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // `serde_json::Value` normally generates the boolean JSON Schema
        // `true`. Although valid JSON Schema, Claude Desktop's MCP validator
        // requires every property schema to be an object and discards the
        // entire tool catalog when it encounters that boolean. Every Harbor
        // tool returns an object under `result`, so describe that contract
        // explicitly instead of advertising an unconstrained value.
        schemars::json_schema!({
            "type": "object",
            "properties": {
                "result": {
                    "type": "object",
                    "description": "Harbor tool result"
                }
            },
            "required": ["result"]
        })
    }
}

fn out(v: Value) -> Json<JsonOut> {
    debug_assert!(v.is_object(), "Harbor tool results must be JSON objects");
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

fn transport_config() -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default()
        // Harbor's handlers keep all durable state in AppState, not in the MCP
        // session. Stateless JSON avoids handing bridge clients a session ID
        // that makes them open and indefinitely reconnect a secondary SSE GET.
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_allowed_origins(["http://localhost", "http://127.0.0.1", "http://[::1]"])
}

/// Build the axum router: bearer authentication protects both `/health` and
/// `/mcp`, so a process merely occupying the remembered port cannot impersonate
/// Harbor's readiness route.
pub fn build_router(state: Arc<AppState>, token: String) -> Router {
    let factory_state = state.clone();
    let mcp_service: StreamableHttpService<HarborMcp, NeverSessionManager> =
        StreamableHttpService::new(
            move || Ok(HarborMcp::new(factory_state.clone())),
            NeverSessionManager::default().into(),
            transport_config(),
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
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Method};

    #[derive(Clone)]
    struct LifecycleTestMcp;

    impl ServerHandler for LifecycleTestMcp {
        fn get_info(&self) -> rmcp::model::ServerInfo {
            use rmcp::model::*;
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
                .with_server_info(Implementation::new("harbor-lifecycle-test", "1"))
        }
    }

    fn lifecycle_test_service() -> StreamableHttpService<LifecycleTestMcp, NeverSessionManager> {
        StreamableHttpService::new(
            || Ok(LifecycleTestMcp),
            NeverSessionManager::default().into(),
            transport_config(),
        )
    }

    fn mcp_post(message: Value) -> Request {
        Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-11-25")
            .body(Body::from(serde_json::to_vec(&message).unwrap()))
            .unwrap()
    }

    async fn assert_json_result(response: axum::response::Response, expected_id: u64) -> Value {
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert!(response.headers().get("mcp-session-id").is_none());
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["id"], expected_id);
        assert!(
            value.get("result").is_some(),
            "unexpected MCP body: {value}"
        );
        value
    }

    fn initialize(id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "harbor-test", "version": "1" }
            }
        })
    }

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

    #[tokio::test]
    async fn stateless_json_transport_does_not_create_an_sse_session() {
        let service = lifecycle_test_service();
        let response = service.handle(mcp_post(initialize(1))).await;
        assert_json_result(response.map(Body::new), 1).await;

        let initialized = service
            .handle(mcp_post(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })))
            .await;
        assert_eq!(initialized.status(), StatusCode::ACCEPTED);
        assert!(initialized.headers().get("mcp-session-id").is_none());

        // mcp-remote performs this GET after its initialized notification. A
        // 405 tells it that the server has no standalone SSE stream; because
        // initialize returned no session ID, it does not enter reconnect mode.
        let get = Request::builder()
            .method(Method::GET)
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1")
            .header(header::ACCEPT, "text/event-stream")
            .body(Body::empty())
            .unwrap();
        let response = service.handle(get).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response
                .headers()
                .get(header::ALLOW)
                .and_then(|value| value.to_str().ok()),
            Some("POST")
        );
    }

    #[tokio::test]
    async fn stateless_json_transport_handles_repeated_fresh_requests() {
        let service = lifecycle_test_service();

        for round in 0..2 {
            let initialize_id = 10 + round * 2;
            assert_json_result(
                service
                    .handle(mcp_post(initialize(initialize_id)))
                    .await
                    .map(Body::new),
                initialize_id,
            )
            .await;

            let list_id = initialize_id + 1;
            let result = assert_json_result(
                service
                    .handle(mcp_post(json!({
                        "jsonrpc": "2.0",
                        "id": list_id,
                        "method": "tools/list",
                        "params": {}
                    })))
                    .await
                    .map(Body::new),
                list_id,
            )
            .await;
            assert_eq!(result["result"]["tools"], json!([]));
        }
    }

    fn find_boolean_schema(value: &Value, path: &str) -> Option<String> {
        if value.is_boolean() {
            return Some(path.to_string());
        }
        let object = value.as_object()?;

        // Only descend into keywords whose values are schemas. Boolean data
        // such as `default: false` is not a boolean-form schema.
        for key in [
            "additionalItems",
            "additionalProperties",
            "contains",
            "contentSchema",
            "else",
            "if",
            "items",
            "not",
            "propertyNames",
            "then",
            "unevaluatedItems",
            "unevaluatedProperties",
        ] {
            if let Some(child) = object.get(key) {
                if let Some(items) = child.as_array() {
                    for (index, item) in items.iter().enumerate() {
                        if let Some(found) =
                            find_boolean_schema(item, &format!("{path}.{key}[{index}]"))
                        {
                            return Some(found);
                        }
                    }
                } else if let Some(found) = find_boolean_schema(child, &format!("{path}.{key}")) {
                    return Some(found);
                }
            }
        }

        for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
            if let Some(children) = object.get(key).and_then(Value::as_array) {
                for (index, child) in children.iter().enumerate() {
                    if let Some(found) =
                        find_boolean_schema(child, &format!("{path}.{key}[{index}]"))
                    {
                        return Some(found);
                    }
                }
            }
        }

        for key in [
            "$defs",
            "definitions",
            "dependentSchemas",
            "patternProperties",
            "properties",
        ] {
            if let Some(children) = object.get(key).and_then(Value::as_object) {
                for (name, child) in children {
                    if let Some(found) = find_boolean_schema(child, &format!("{path}.{key}.{name}"))
                    {
                        return Some(found);
                    }
                }
            }
        }

        None
    }

    #[test]
    fn every_tool_schema_is_claude_compatible() {
        let tools = HarborMcp::tool_router().list_all();
        assert!(!tools.is_empty());

        for tool in tools {
            let input = Value::Object(tool.input_schema.as_ref().clone());
            assert_eq!(
                find_boolean_schema(&input, &format!("{}.inputSchema", tool.name)),
                None,
                "{} emitted a boolean-form input schema",
                tool.name
            );

            let output = tool
                .output_schema
                .as_ref()
                .unwrap_or_else(|| panic!("{} is missing outputSchema", tool.name));
            let output_value = Value::Object(output.as_ref().clone());
            assert_eq!(
                find_boolean_schema(&output_value, &format!("{}.outputSchema", tool.name)),
                None,
                "{} emitted a boolean-form output schema",
                tool.name
            );
            let result_schema = output
                .get("properties")
                .and_then(Value::as_object)
                .and_then(|properties| properties.get("result"))
                .unwrap_or_else(|| {
                    panic!("{} is missing outputSchema.properties.result", tool.name)
                });

            assert!(
                result_schema.is_object(),
                "{} emitted a boolean property schema: {result_schema}",
                tool.name
            );
            assert_eq!(result_schema["type"], "object", "tool: {}", tool.name);
        }
    }
}
