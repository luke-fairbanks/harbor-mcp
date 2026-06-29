//! Process supervisor (DESIGN.md §3.1, M1).
//!
//! Each service is spawned via `sh -c` in **its own process group** (`setsid` in
//! a `pre_exec` hook → the child becomes a group leader, so `pgid == pid`).
//! That lets us tear down the whole tree (shell + node + vite grandchildren)
//! with a single `killpg`, SIGTERM then SIGKILL after a grace period.
//!
//! Concurrency model (avoids sharing `&mut child`):
//!   - a **monitor task** owns the `Child` and is the only thing that `wait()`s
//!     it (→ reaps the zombie, records the exit code, frees the port);
//!   - **reader tasks** own the piped stdout/stderr and stream lines to the UI +
//!     ring buffer, and flip `starting → ready` on a `readyLogPattern` match;
//!   - `stop()` never touches the `Child`; it signals the process *group* by pid
//!     via `nix::killpg`, then polls for the monitor to record the exit.

use crate::health;
use crate::model::*;
use crate::ports;
use anyhow::{anyhow, bail, Result};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;

const RING_CAP: usize = 3000;
const READY_TIMEOUT: Duration = Duration::from_secs(40);
const STOP_GRACE: Duration = Duration::from_secs(5);
const HEALTH_POLL: Duration = Duration::from_millis(350);

pub const LOG_EVENT: &str = "harbor://log";
pub const STATUS_EVENT: &str = "harbor://status";
pub const REGISTRY_EVENT: &str = "harbor://registry";

/// Shared maps, cloned into the per-service tasks.
type Runs = Arc<Mutex<BTreeMap<String, AppRun>>>;
type Reserved = Arc<Mutex<HashSet<u16>>>;

pub struct Supervisor {
    app: AppHandle,
    runs: Runs,
    reserved: Reserved,
    seq: Arc<AtomicU64>,
    /// The user's real login-shell `PATH`, so services find `node`/`npm` even
    /// when Harbor is launched from Finder (where the inherited PATH is just
    /// `/usr/bin:/bin:…` and misses nvm/asdf/Homebrew). `None` → inherit as-is.
    user_path: Option<String>,
}

/// Live state of one app instance.
struct AppRun {
    profile: Option<String>,
    port_plan: Vec<PortPlanEntry>,
    services: BTreeMap<String, ServiceProc>,
}

/// Live state of one service process.
struct ServiceProc {
    name: String,
    status: ServiceStatus,
    /// Leader pid == process-group id.
    pid: Option<u32>,
    port: Option<u16>,
    resolved_command: Option<String>,
    exit_code: Option<i32>,
    logs: VecDeque<LogLine>,
}

impl ServiceProc {
    fn to_run(&self) -> ServiceRun {
        ServiceRun {
            name: self.name.clone(),
            status: self.status,
            pid: self.pid,
            port: self.port,
            resolved_command: self.resolved_command.clone(),
            exit_code: self.exit_code,
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Supervisor {
    pub fn new(app: AppHandle) -> Self {
        let user_path = crate::sysenv::enriched_path();
        Supervisor {
            app,
            runs: Arc::new(Mutex::new(BTreeMap::new())),
            reserved: Arc::new(Mutex::new(HashSet::new())),
            seq: Arc::new(AtomicU64::new(0)),
            user_path,
        }
    }

    /// Tell the UI the registry changed (e.g. an app was registered over MCP).
    pub fn notify_registry_changed(&self) {
        let _ = self.app.emit(REGISTRY_EVENT, ());
    }

    pub async fn is_running(&self, app_name: &str) -> bool {
        self.runs
            .lock()
            .await
            .get(app_name)
            .map(|r| r.services.values().any(|s| s.status.is_live()))
            .unwrap_or(false)
    }

    /// Snapshot one app's run (or `None` if it was never started this session).
    pub async fn snapshot(&self, app_name: &str) -> Option<AppRunSnapshot> {
        let runs = self.runs.lock().await;
        runs.get(app_name).map(|r| AppRunSnapshot {
            app: app_name.to_string(),
            profile: r.profile.clone(),
            running: r.services.values().any(|s| s.status.is_live()),
            services: r.services.values().map(|s| s.to_run()).collect(),
            port_plan: r.port_plan.clone(),
        })
    }

    /// Tail captured logs for a service (most recent `lines`).
    pub async fn logs(&self, app_name: &str, service: &str, lines: usize) -> Vec<LogLine> {
        let runs = self.runs.lock().await;
        let Some(run) = runs.get(app_name) else {
            return vec![];
        };
        let Some(svc) = run.services.get(service) else {
            return vec![];
        };
        let n = svc.logs.len().min(lines);
        svc.logs.iter().skip(svc.logs.len() - n).cloned().collect()
    }

    /// Start an app under a profile. Idempotent: a no-op (returns current
    /// snapshot) if already running.
    pub async fn start(&self, cfg: &AppConfig, profile: &str) -> Result<AppRunSnapshot> {
        if self.is_running(&cfg.name).await {
            return self
                .snapshot(&cfg.name)
                .await
                .ok_or_else(|| anyhow!("already running but no snapshot"));
        }

        let services = cfg.services_for_profile(profile);
        if services.is_empty() {
            bail!("profile '{}' selects no services", profile);
        }
        let ordered = ports::topo_sort(&services)?;

        // Allocate ports, avoiding ones already held by other live runs.
        let alloc = {
            let reserved = self.reserved.lock().await;
            ports::allocate(&ordered, &reserved)?
        };
        {
            let mut reserved = self.reserved.lock().await;
            for p in alloc.ports.values() {
                reserved.insert(*p);
            }
        }

        // Fresh run record (replaces any prior exited run).
        {
            let mut runs = self.runs.lock().await;
            runs.insert(
                cfg.name.clone(),
                AppRun {
                    profile: Some(profile.to_string()),
                    port_plan: alloc.plan.clone(),
                    services: BTreeMap::new(),
                },
            );
        }

        let root = PathBuf::from(&cfg.root);
        for svc in &ordered {
            let port = alloc.ports.get(&svc.name).copied();
            let (resolved_command, resolved_env) = ports::resolve_service(svc, &alloc.ports);

            match self
                .spawn_service(cfg, svc, &root, port, &resolved_command, &resolved_env)
                .await
            {
                Ok(()) => {}
                Err(e) => {
                    self.system_log(&cfg.name, &svc.name, &format!("failed to start: {e}"))
                        .await;
                    self.set_status(&cfg.name, &svc.name, ServiceStatus::Exited, Some(-1))
                        .await;
                    if let Some(p) = port {
                        self.reserved.lock().await.remove(&p);
                    }
                    bail!("service '{}' failed to start: {e}", svc.name);
                }
            }

            // Gate dependents: wait for this service to become ready before the
            // next (topo order guarantees deps precede dependents).
            self.await_ready(&cfg.name, &svc.name).await;
        }

        self.snapshot(&cfg.name)
            .await
            .ok_or_else(|| anyhow!("run vanished after start"))
    }

    async fn spawn_service(
        &self,
        cfg: &AppConfig,
        svc: &ServiceConfig,
        root: &Path,
        port: Option<u16>,
        resolved_command: &str,
        resolved_env: &BTreeMap<String, String>,
    ) -> Result<()> {
        let cwd = resolve_cwd(root, &svc.cwd);

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(resolved_command);
        cmd.current_dir(&cwd);
        // Toolchain PATH first, so a service-pinned env PATH can still override.
        if let Some(path) = &self.user_path {
            if !resolved_env.contains_key("PATH") {
                cmd.env("PATH", path);
            }
        }
        for (k, v) in resolved_env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(false);
        // New session/process group so the whole tree shares one pgid (== pid).
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid()
                    .map(|_| ())
                    .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
            });
        }

        let mut child = cmd
            .spawn()
            .with_context_path(&cwd, resolved_command)?;
        let pid = child.id();

        // Register the service as starting.
        {
            let mut runs = self.runs.lock().await;
            let run = runs
                .get_mut(&cfg.name)
                .ok_or_else(|| anyhow!("run missing"))?;
            run.services.insert(
                svc.name.clone(),
                ServiceProc {
                    name: svc.name.clone(),
                    status: ServiceStatus::Starting,
                    pid,
                    port,
                    resolved_command: Some(resolved_command.to_string()),
                    exit_code: None,
                    logs: VecDeque::new(),
                },
            );
        }
        self.emit_status(&cfg.name, svc.name.clone(), ServiceStatus::Starting, port, pid, None);
        self.system_log(
            &cfg.name,
            &svc.name,
            &format!(
                "$ {resolved_command}   (cwd {}{})",
                cwd.display(),
                port.map(|p| format!(", port {p}")).unwrap_or_default()
            ),
        )
        .await;

        // Reader tasks for stdout/stderr.
        let ready_pattern = svc.ready_log_pattern.clone();
        if let Some(out) = child.stdout.take() {
            self.spawn_reader(
                cfg.name.clone(),
                svc.name.clone(),
                "stdout",
                out,
                ready_pattern.clone(),
            );
        }
        if let Some(err) = child.stderr.take() {
            self.spawn_reader(cfg.name.clone(), svc.name.clone(), "stderr", err, ready_pattern);
        }

        // Monitor task owns the Child: waits, reaps, records exit, frees port.
        {
            let runs = self.runs.clone();
            let reserved = self.reserved.clone();
            let app = self.app.clone();
            let app_name = cfg.name.clone();
            let svc_name = svc.name.clone();
            tokio::spawn(async move {
                let status = child.wait().await;
                let code = status.ok().and_then(|s| s.code());
                set_status_inner(&app, &runs, &app_name, &svc_name, ServiceStatus::Exited, code)
                    .await;
                if let Some(p) = port {
                    reserved.lock().await.remove(&p);
                }
            });
        }

        // Health polling for http/tcp checks.
        if let Some(hc) = svc.health_check.clone() {
            if matches!(hc, HealthCheck::Http { .. } | HealthCheck::Tcp) {
                let runs = self.runs.clone();
                let app = self.app.clone();
                let app_name = cfg.name.clone();
                let svc_name = svc.name.clone();
                tokio::spawn(async move {
                    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
                    loop {
                        if tokio::time::Instant::now() >= deadline {
                            break;
                        }
                        // Stop polling if the service already left `starting`.
                        if !is_starting(&runs, &app_name, &svc_name).await {
                            break;
                        }
                        if health::check_once(&hc, port).await {
                            set_status_inner(
                                &app,
                                &runs,
                                &app_name,
                                &svc_name,
                                ServiceStatus::Ready,
                                None,
                            )
                            .await;
                            break;
                        }
                        tokio::time::sleep(HEALTH_POLL).await;
                    }
                });
            }
        }

        // Services with no readiness mechanism are "ready when alive".
        let has_probe = matches!(
            svc.health_check,
            Some(HealthCheck::Http { .. }) | Some(HealthCheck::Tcp)
        );
        if svc.ready_log_pattern.is_none() && !has_probe {
            self.set_status(&cfg.name, &svc.name, ServiceStatus::Ready, None)
                .await;
        }

        Ok(())
    }

    fn spawn_reader<R>(
        &self,
        app_name: String,
        svc_name: String,
        stream: &'static str,
        reader: R,
        ready_pattern: Option<String>,
    ) where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        let runs = self.runs.clone();
        let app = self.app.clone();
        let seq = self.seq.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let seq_n = seq.fetch_add(1, Ordering::Relaxed);
                let log = LogLine {
                    app: app_name.clone(),
                    service: svc_name.clone(),
                    stream: stream.to_string(),
                    line: line.clone(),
                    ts: now_millis(),
                    seq: seq_n,
                };
                push_log(&runs, &app_name, &svc_name, log.clone()).await;
                let _ = app.emit(LOG_EVENT, &log);

                // readyLogPattern: flip starting → ready on a substring match.
                if let Some(pat) = &ready_pattern {
                    if !pat.is_empty() && line.contains(pat.as_str())
                        && is_starting(&runs, &app_name, &svc_name).await
                    {
                        set_status_inner(
                            &app,
                            &runs,
                            &app_name,
                            &svc_name,
                            ServiceStatus::Ready,
                            None,
                        )
                        .await;
                    }
                }
            }
        });
    }

    /// Stop every service in an app: SIGTERM the group, then SIGKILL after grace.
    pub async fn stop(&self, app_name: &str) -> Result<()> {
        let targets: Vec<(String, u32)> = {
            let runs = self.runs.lock().await;
            let Some(run) = runs.get(app_name) else {
                return Ok(());
            };
            run.services
                .values()
                .filter(|s| s.status.is_live())
                .filter_map(|s| s.pid.map(|p| (s.name.clone(), p)))
                .collect()
        };
        if targets.is_empty() {
            // Nothing live; clear any stale exited run.
            self.runs.lock().await.remove(app_name);
            return Ok(());
        }

        for (name, pid) in &targets {
            self.system_log(app_name, name, "SIGTERM → process group").await;
            let _ = killpg(Pid::from_raw(*pid as i32), Signal::SIGTERM);
        }

        // Poll for monitors to record exits, up to the grace period.
        let deadline = tokio::time::Instant::now() + STOP_GRACE;
        loop {
            if !self.is_running(app_name).await {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Escalate to SIGKILL for anything still alive.
        let still: Vec<(String, u32)> = {
            let runs = self.runs.lock().await;
            runs.get(app_name)
                .map(|run| {
                    run.services
                        .values()
                        .filter(|s| s.status.is_live())
                        .filter_map(|s| s.pid.map(|p| (s.name.clone(), p)))
                        .collect()
                })
                .unwrap_or_default()
        };
        for (name, pid) in &still {
            self.system_log(app_name, name, "SIGKILL → process group").await;
            let _ = killpg(Pid::from_raw(*pid as i32), Signal::SIGKILL);
        }
        if !still.is_empty() {
            // Give monitors a beat to reap.
            tokio::time::sleep(Duration::from_millis(300)).await;
        }

        // Drop the run record; ports are freed by the monitors as they exit.
        self.runs.lock().await.remove(app_name);
        Ok(())
    }

    // ---- small helpers ----------------------------------------------------

    async fn await_ready(&self, app_name: &str, svc_name: &str) {
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        loop {
            let status = {
                let runs = self.runs.lock().await;
                runs.get(app_name)
                    .and_then(|r| r.services.get(svc_name))
                    .map(|s| s.status)
            };
            match status {
                Some(ServiceStatus::Starting) => {}
                _ => return, // ready / exited / unhealthy / gone
            }
            if tokio::time::Instant::now() >= deadline {
                self.system_log(app_name, svc_name, "readiness timeout → marking unhealthy")
                    .await;
                self.set_status(app_name, svc_name, ServiceStatus::Unhealthy, None)
                    .await;
                return;
            }
            tokio::time::sleep(HEALTH_POLL).await;
        }
    }

    async fn set_status(
        &self,
        app_name: &str,
        svc_name: &str,
        status: ServiceStatus,
        exit_code: Option<i32>,
    ) {
        set_status_inner(&self.app, &self.runs, app_name, svc_name, status, exit_code).await;
    }

    fn emit_status(
        &self,
        app_name: &str,
        svc_name: String,
        status: ServiceStatus,
        port: Option<u16>,
        pid: Option<u32>,
        exit_code: Option<i32>,
    ) {
        let _ = self.app.emit(
            STATUS_EVENT,
            StatusEvent {
                app: app_name.to_string(),
                service: svc_name,
                status,
                port,
                pid,
                exit_code,
            },
        );
    }

    async fn system_log(&self, app_name: &str, svc_name: &str, msg: &str) {
        let seq_n = self.seq.fetch_add(1, Ordering::Relaxed);
        let log = LogLine {
            app: app_name.to_string(),
            service: svc_name.to_string(),
            stream: "system".to_string(),
            line: msg.to_string(),
            ts: now_millis(),
            seq: seq_n,
        };
        push_log(&self.runs, app_name, svc_name, log.clone()).await;
        let _ = self.app.emit(LOG_EVENT, &log);
    }
}

// ---- free functions usable from spawned tasks (no &self) ------------------

async fn is_starting(runs: &Runs, app_name: &str, svc_name: &str) -> bool {
    let runs = runs.lock().await;
    runs.get(app_name)
        .and_then(|r| r.services.get(svc_name))
        .map(|s| s.status == ServiceStatus::Starting)
        .unwrap_or(false)
}

async fn push_log(runs: &Runs, app_name: &str, svc_name: &str, log: LogLine) {
    let mut runs = runs.lock().await;
    if let Some(svc) = runs
        .get_mut(app_name)
        .and_then(|r| r.services.get_mut(svc_name))
    {
        svc.logs.push_back(log);
        while svc.logs.len() > RING_CAP {
            svc.logs.pop_front();
        }
    }
}

async fn set_status_inner(
    app: &AppHandle,
    runs: &Runs,
    app_name: &str,
    svc_name: &str,
    status: ServiceStatus,
    exit_code: Option<i32>,
) {
    let (port, pid) = {
        let mut runs = runs.lock().await;
        let Some(svc) = runs
            .get_mut(app_name)
            .and_then(|r| r.services.get_mut(svc_name))
        else {
            return;
        };
        // Don't resurrect an exited service.
        if svc.status == ServiceStatus::Exited {
            return;
        }
        svc.status = status;
        if exit_code.is_some() {
            svc.exit_code = exit_code;
        }
        (svc.port, svc.pid)
    };
    let _ = app.emit(
        STATUS_EVENT,
        StatusEvent {
            app: app_name.to_string(),
            service: svc_name.to_string(),
            status,
            port,
            pid,
            exit_code,
        },
    );
}

fn resolve_cwd(root: &Path, cwd: &str) -> PathBuf {
    let p = Path::new(cwd);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

/// Tiny extension to attach context to spawn errors without pulling the whole
/// `anyhow::Context` into scope here.
trait SpawnContext<T> {
    fn with_context_path(self, cwd: &Path, cmd: &str) -> Result<T>;
}
impl<T> SpawnContext<T> for std::io::Result<T> {
    fn with_context_path(self, cwd: &Path, cmd: &str) -> Result<T> {
        self.map_err(|e| anyhow!("spawn `{}` in {}: {}", cmd, cwd.display(), e))
    }
}
