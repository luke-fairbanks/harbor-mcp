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

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A registered project folder and the services it runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AppConfig {
    pub name: String,
    /// Absolute path to the project root.
    pub root: String,
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    /// Named service sets, e.g. `{"default": ["server"], "dev": ["server","web"]}`.
    #[serde(default)]
    pub profiles: BTreeMap<String, Vec<String>>,
    /// Auto-restart Harbor-spawned services that exit unexpectedly (bounded
    /// backoff, gives up after a few tries). Never applies to servers started
    /// outside Harbor. Off by default.
    #[serde(
        default,
        rename = "autoRestart",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub auto_restart: bool,
    /// Local approval gate for executing this app's commands. Existing and
    /// user-imported configs default to trusted for backwards compatibility;
    /// configs registered by an MCP client are forced to `false` until the
    /// person approves them in Harbor.
    #[serde(default = "default_trusted")]
    pub trusted: bool,
}

fn default_trusted() -> bool {
    true
}

impl AppConfig {
    /// The service list for an exact named profile. `default` selects every
    /// service when no explicit default profile exists; unknown names select
    /// nothing so callers cannot silently launch the wrong stack.
    pub fn services_for_profile(&self, profile: &str) -> Vec<ServiceConfig> {
        match self.profiles.get(profile) {
            Some(names) => self
                .services
                .iter()
                .filter(|s| names.contains(&s.name))
                .cloned()
                .collect(),
            None if profile == "default" => self.services.clone(),
            None => Vec::new(),
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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
    #[serde(
        default,
        rename = "healthCheck",
        skip_serializing_if = "Option::is_none"
    )]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
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

/// One spawned service, persisted to `runs.json` so a restarted Harbor can
/// re-adopt processes it left running (a dev server that outlived the app).
///
/// Written on spawn, removed when the monitor reaps a clean exit (or when an
/// adopted process is later found dead). Adoption only ever *reads* these — it
/// never signals a pid off a bare record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedRun {
    pub app: String,
    pub service: String,
    /// Leader pid == process-group id (the child is `setsid`'d in `spawn_service`).
    /// This is exactly the value `killpg` needs.
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The exact resolved command we spawned (`sh -c <command>`), used as a
    /// defense-in-depth identity check (the leader's argv contains it).
    pub command: String,
    /// Absolute cwd we spawned in (defense-in-depth / future use).
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The process start time as `ps`'s `lstart` field (e.g. `"Mon Jun 29 14:23:01 2026"`).
    /// An absolute, boot-stable identity token: a reused pid started at a
    /// different instant yields a different string, so exact equality is the
    /// PID-reuse defense (no date math, no boot-id needed).
    pub started_at: String,
    /// True when this record describes a server Harbor did NOT spawn — detected
    /// on its port and corroborated as this app (started in a terminal, etc.).
    /// Identity/stop-safety are unchanged: `pid` is the group leader and
    /// `command` is its observed argv, so the standard `still_ours` gate applies.
    #[serde(default)]
    pub foreign: bool,
    /// For `foreign` records, the app root used to re-corroborate the process at
    /// Stop time (so adoption can't drift into killing an unrelated group).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
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
    /// True when re-adopted from a prior Harbor session: Harbor holds the verified
    /// pid/port and can Stop/Open it, but live stdout/stderr can't be re-attached.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub adopted: bool,
    /// True when discovered running OUTSIDE Harbor (started in a terminal, etc.):
    /// matched to the app by its port + project folder via the group-leader walk.
    /// Implies `adopted`; drives the "external" badge and Stop wording.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub external: bool,
    /// Recent CPU% summed over the whole process group (`ps pcpu`). Live-sampled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<f32>,
    /// Resident memory summed over the process group, in bytes (`ps rss` KiB × 1024).
    #[serde(rename = "memBytes", skip_serializing_if = "Option::is_none")]
    pub mem_bytes: Option<u64>,
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

/// One service's latest resource sample — element of the `harbor://stats` batch.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceStat {
    pub app: String,
    pub service: String,
    pub cpu: f32,
    #[serde(rename = "memBytes")]
    pub mem_bytes: u64,
}

// ---------------------------------------------------------------------------
// Machine-wide local-server discovery (UI + MCP facing)
// ---------------------------------------------------------------------------

/// One TCP listener observed on the local machine. Discovery is deliberately
/// separate from ownership: an entry can be matched to a Harbor app without
/// Harbor having permission to stop it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalServer {
    /// PID that owns the listening socket (often a child of npm/vite).
    pub pid: u32,
    /// Process-group leader used for stable identity and safe cleanup checks.
    pub leader_pid: u32,
    pub port: u16,
    pub addresses: Vec<String>,
    /// True when at least one socket is bound beyond loopback (for example `*`
    /// or a LAN address). This is a visibility warning, not a firewall verdict.
    pub network_exposed: bool,
    pub process: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Nearest ancestor containing a project marker such as package.json/.git.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    pub display_name: String,
    /// Best-effort technology label derived from argv and the HTTP response.
    pub kind: String,
    /// `ps lstart` token for the group leader. Together with `leaderPid`, this
    /// prevents a stale cleanup action from hitting a reused PID.
    pub started_at: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_reason: Option<String>,
    /// True when this exact process is already represented in Supervisor state.
    pub tracked: bool,
    /// True when the tracked process was started outside Harbor.
    pub external: bool,
    /// Cleanup is offered only for isolated, untracked process groups whose
    /// identity can be revalidated immediately before signalling.
    pub safe_to_stop: bool,
    pub likely_dev: bool,
    /// Number of distinct process groups with the same project + runtime
    /// fingerprint. Values >1 are probable duplicate dev-server launches.
    pub duplicate_count: usize,
    /// Harbor's own MCP listener: visible for clarity, never cleanable here.
    pub harbor_internal: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalServerInventory {
    pub scanned_at: u64,
    pub servers: Vec<LocalServer>,
    pub dev_count: usize,
    pub other_count: usize,
    pub mapped_count: usize,
    pub duplicate_count: usize,
}
