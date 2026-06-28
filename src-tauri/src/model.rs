//! Core data types for Harbor.
//!
//! Two families live here:
//!  - **Config** types (`AppConfig`, `ServiceConfig`, …) — persisted in the
//!    registry and shareable as a per-project `harbor.json`.
//!  - **Run** types (`ServiceRun`, `AppRunSnapshot`, `PortPlanEntry`, …) — live,
//!    in-memory snapshots streamed to the UI and returned by MCP tools.
//!
//! Field names use `camelCase` on the wire (via `rename`) so they read naturally
//! from both TypeScript and the `harbor.json` schema in DESIGN.md §5.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A registered project folder and the services it runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub name: String,
    /// Absolute path to the project root.
    pub root: String,
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    /// Named service sets, e.g. `{"default": ["server"], "dev": ["server","web"]}`.
    #[serde(default)]
    pub profiles: BTreeMap<String, Vec<String>>,
}

impl AppConfig {
    /// The service list for a profile, or all services if the profile is unknown
    /// / unspecified. Falls back to `default` then to every service.
    pub fn services_for_profile(&self, profile: &str) -> Vec<ServiceConfig> {
        let names = self
            .profiles
            .get(profile)
            .or_else(|| self.profiles.get("default"));
        match names {
            Some(names) => self
                .services
                .iter()
                .filter(|s| names.contains(&s.name))
                .cloned()
                .collect(),
            None => self.services.clone(),
        }
    }

    #[allow(dead_code)] // companion to service_mut; used by config tooling
    pub fn service(&self, name: &str) -> Option<&ServiceConfig> {
        self.services.iter().find(|s| s.name == name)
    }

    pub fn service_mut(&mut self, name: &str) -> Option<&mut ServiceConfig> {
        self.services.iter_mut().find(|s| s.name == name)
    }
}

/// One long-running process within an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    /// Working directory, relative to the app root (or absolute). Defaults to `.`.
    #[serde(default = "default_cwd")]
    pub cwd: String,
    /// Shell command line. May contain `${PORT}` and `${services.X.port}`.
    pub command: String,
    /// Preferred port; the allocator uses this, bumping upward if taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Extra environment; values may contain the same `${...}` placeholders.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Services that must reach `ready` before this one starts.
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<String>,
    #[serde(default, rename = "healthCheck", skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheck>,
    /// Regex on stdout/stderr that flips the service to `ready`.
    #[serde(
        default,
        rename = "readyLogPattern",
        skip_serializing_if = "Option::is_none"
    )]
    pub ready_log_pattern: Option<String>,
}

fn default_cwd() -> String {
    ".".to_string()
}

/// How Harbor decides a service is `ready`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum HealthCheck {
    /// HTTP GET `path` on the service's port; ready on a 2xx/3xx response.
    Http {
        path: String,
        /// e.g. `"2xx-3xx"`. Informational; default acceptance is 2xx/3xx.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<String>,
    },
    /// TCP connect to the service's port succeeds.
    Tcp,
    /// A line matching `pattern` appears in the logs.
    Log { pattern: String },
    /// Process is simply alive (default when no check is given).
    Process,
}

// ---------------------------------------------------------------------------
// Run-time snapshot types (UI + MCP facing; not persisted)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceStatus {
    Stopped,
    Starting,
    Ready,
    Unhealthy,
    Exited,
}

impl ServiceStatus {
    pub fn is_live(self) -> bool {
        matches!(
            self,
            ServiceStatus::Starting | ServiceStatus::Ready | ServiceStatus::Unhealthy
        )
    }
}

/// Live state of a single service in a run.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceRun {
    pub name: String,
    pub status: ServiceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The command actually spawned, with all `${...}` resolved.
    #[serde(rename = "resolvedCommand", skip_serializing_if = "Option::is_none")]
    pub resolved_command: Option<String>,
    #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// One row of the port plan: what each service asked for and what it got.
#[derive(Debug, Clone, Serialize)]
pub struct PortPlanEntry {
    pub service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred: Option<u16>,
    pub resolved: u16,
    /// Human note, e.g. `"4321 was busy → 4322"` or `"web proxy → api:4322"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Snapshot of a whole app run — what `app_status` / `list_apps` return.
#[derive(Debug, Clone, Serialize)]
pub struct AppRunSnapshot {
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub running: bool,
    pub services: Vec<ServiceRun>,
    #[serde(rename = "portPlan")]
    pub port_plan: Vec<PortPlanEntry>,
}

/// A single captured log line — the payload of the `harbor://log` event and the
/// element returned by `get_logs`.
#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub app: String,
    pub service: String,
    /// `"stdout"`, `"stderr"`, or `"system"` (Harbor's own lifecycle messages).
    pub stream: String,
    pub line: String,
    /// Epoch millis.
    pub ts: u64,
    /// Monotonic per-run sequence number, for stable ordering in the UI.
    pub seq: u64,
}

/// Payload of the `harbor://status` event: a service changed state.
#[derive(Debug, Clone, Serialize)]
pub struct StatusEvent {
    pub app: String,
    pub service: String,
    pub status: ServiceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}
