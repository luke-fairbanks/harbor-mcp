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
use crate::store::{RunsFile, Store};
use anyhow::{anyhow, bail, Result};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, RwLock};

const RING_CAP: usize = 3000;
const READY_TIMEOUT: Duration = Duration::from_secs(40);
const STOP_GRACE: Duration = Duration::from_secs(5);
const HEALTH_POLL: Duration = Duration::from_millis(350);

/// Auto-restart policy. A crash-on-start loop exhausts `RESTART_WINDOW_MAX`
/// attempts (~38s with the backoff below) then gives up; the counter only resets
/// after a respawn stays `Ready` ≥ `RESTART_STABLE_RESET`, so a tight loop can
/// never reset itself.
const RESTART_WINDOW_MAX: u32 = 5;
const RESTART_STABLE_RESET: Duration = Duration::from_secs(60);
const RESTART_BACKOFF: [u64; 5] = [1, 2, 5, 10, 20];
/// Don't post more than one crash notification per service within this window.
const NOTIFY_COOLDOWN: Duration = Duration::from_secs(60);

pub const LOG_EVENT: &str = "harbor://log";
pub const STATUS_EVENT: &str = "harbor://status";
pub const REGISTRY_EVENT: &str = "harbor://registry";
pub const STATS_EVENT: &str = "harbor://stats";

/// How often the per-service resource sampler runs.
const STATS_INTERVAL: Duration = Duration::from_secs(2);

/// Shared maps, cloned into the per-service tasks.
type Runs = Arc<Mutex<BTreeMap<String, AppRun>>>;
type Reserved = Arc<Mutex<HashSet<u16>>>;

type Registry = Arc<RwLock<BTreeMap<String, AppConfig>>>;

pub struct Supervisor {
    app: AppHandle,
    runs: Runs,
    reserved: Reserved,
    seq: Arc<AtomicU64>,
    /// The user's real login-shell `PATH`, so services find `node`/`npm` even
    /// when Harbor is launched from Finder (where the inherited PATH is just
    /// `/usr/bin:/bin:…` and misses nvm/asdf/Homebrew). `None` → inherit as-is.
    user_path: Option<String>,
    /// Persists spawned processes to `runs.json` so a relaunched Harbor can
    /// re-adopt the ones still running (see [`Supervisor::adopt_persisted`]).
    store: Arc<Store>,
    /// Shared live (possibly user-edited) config, read at crash time so
    /// auto-restart honours the current `autoRestart` flag and env.
    registry: Registry,
    /// Per-(app,service) cooldown so a flapping service can't spam notifications.
    last_notified: Arc<Mutex<HashMap<(String, String), Instant>>>,
    /// Flipped true on app quit; restart + notify paths no-op when set.
    shutting_down: Arc<AtomicBool>,
    /// Weak self-handle so a detached monitor task can call back into the
    /// supervisor to auto-restart a crashed service.
    me: Weak<Supervisor>,
}

/// Live state of one app instance.
struct AppRun {
    profile: Option<String>,
    port_plan: Vec<PortPlanEntry>,
    services: BTreeMap<String, ServiceProc>,
    /// Services whose NEXT monitor-observed exit is intentional (user Stop,
    /// Restart's stop phase, Stop-all). Set under the `runs` lock before any
    /// killpg; the monitor removes its own name on exit. This is what tells a
    /// crash apart from a deliberate Stop — the whole auto-restart correctness
    /// hinges on it being read under the same lock it's written.
    intentional_stop: HashSet<String>,
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
    /// `ps` `lstart` identity token captured at spawn (and carried on adoption),
    /// used to refuse signalling a reused pid.
    started_at: Option<String>,
    /// True when re-adopted from a prior session (no live log stream).
    adopted: bool,
    /// True when discovered running outside Harbor (started elsewhere).
    external: bool,
    /// App root, carried for `external` services so Stop can re-corroborate the
    /// process group before signalling. `None` for Harbor-spawned/orphan-adopted.
    root: Option<String>,
    /// Exact env this process was spawned with (resolved `${...}`), so an
    /// auto-restart re-runs it identically. `None` for adopted/external.
    resolved_env: Option<BTreeMap<String, String>>,
    /// Consecutive auto-restart attempts in the current crash episode.
    restart_count: u32,
    /// When this proc last reached `Ready`; used to reset `restart_count` once a
    /// respawn has stayed alive past the stability window.
    ready_since: Option<Instant>,
    /// Latest sampled group CPU%; written only by the resource sampler.
    cpu: Option<f32>,
    /// Latest sampled group RSS in bytes; written only by the resource sampler.
    mem_bytes: Option<u64>,
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
            adopted: self.adopted,
            external: self.external,
            cpu: self.cpu,
            mem_bytes: self.mem_bytes,
        }
    }
}

/// A few facts about a live process, read from stock `ps`. `None` ⇒ no such pid.
struct ProcFacts {
    /// Process-group id. For a service Harbor spawned this equals the leader pid.
    pgid: u32,
    /// `ps` `lstart` field — an absolute, boot-stable start-time string.
    started_at: String,
    /// Full argv of the process (contains the resolved command for the leader).
    command: String,
}

/// `ps -o pid=,pgid=,lstart=,command= -p <pid>` → facts, or `None` if the pid is
/// gone. Columns: pid, pgid, then `lstart` (always 5 whitespace tokens —
/// `Www Mmm dd hh:mm:ss yyyy`), then the full command. Token-based parsing is
/// robust to the right-justified numeric column padding `ps` uses.
fn ps_facts(pid: u32) -> Option<ProcFacts> {
    let out = std::process::Command::new("ps")
        .args(["-o", "pid=,pgid=,lstart=,command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let line = raw.lines().next()?.trim();
    let toks: Vec<&str> = line.split_whitespace().collect();
    // 2 numeric cols + 5 lstart tokens + at least one command token.
    if toks.len() < 8 {
        return None;
    }
    let pgid: u32 = toks[1].parse().ok()?;
    let started_at = toks[2..7].join(" "); // "Www Mmm dd hh:mm:ss yyyy"
    let command = toks[7..].join(" ");
    Some(ProcFacts {
        pgid,
        started_at,
        command,
    })
}

/// PID of the process LISTENing on `port`, via `lsof`. `None` if nothing is
/// listening or `lsof` is unavailable (best-effort).
fn port_listener_pid(port: u16) -> Option<u32> {
    // `/usr/sbin` is on the default Finder PATH, but try an absolute fallback too.
    for prog in ["lsof", "/usr/sbin/lsof"] {
        let Ok(out) = std::process::Command::new(prog)
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpn"])
            .output()
        else {
            continue; // this lsof path isn't available — try the next
        };
        for l in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(rest) = l.strip_prefix('p') {
                if let Ok(pid) = rest.trim().parse::<u32>() {
                    return Some(pid);
                }
            }
        }
        return None; // lsof ran and reported no LISTEN holder
    }
    None
}

/// The single ownership gate for adoption. A persisted record is still "ours"
/// iff **every** guard passes. NEVER signals a process.
///
///  - G1 the pid exists **and** is its own group leader (`pgid == pid`) — the
///    `setsid` invariant; a reused pid is almost never a group leader.
///  - G2 the `ps` start-time string matches exactly — the PID-reuse defense.
///  - G3 the leader's argv still contains the command we spawned.
///  - G4 (only if a port was recorded) the port is held, and its listener is our
///    pid or a member of our process group.
fn still_ours(e: &PersistedRun) -> bool {
    let Some(f) = ps_facts(e.pid) else {
        return false; // G1a: gone
    };
    if f.pgid != e.pid {
        return false; // G1b: not the leader → almost certainly a reused pid
    }
    if f.started_at != e.started_at {
        return false; // G2: different process reusing the pid
    }
    if !f.command.contains(&e.command) {
        return false; // G3: argv no longer matches
    }
    match e.port {
        None => true, // portless service: G1–G3 suffice
        Some(p) => {
            if ports::is_port_free(p) {
                return false; // G4 neg: nothing is listening
            }
            match port_listener_pid(p) {
                Some(lp) => lp == e.pid || ps_facts(lp).map_or(false, |g| g.pgid == e.pid),
                None => false,
            }
        }
    }
}

/// Cheap re-check used immediately before `killpg` on an adopted service, so a
/// pid that died (and was possibly reused) between adoption and Stop is never
/// signalled. Leader + start-time identity is the decisive guard.
fn safe_to_signal(pid: u32, started_at: Option<&str>) -> bool {
    match ps_facts(pid) {
        Some(f) => f.pgid == pid && started_at.map_or(true, |s| f.started_at == s),
        None => false,
    }
}

// ---- external-process detection (servers Harbor did NOT spawn) -------------
//
// A dev server the user started in a terminal isn't in `runs.json`, but Harbor
// can still recognize it: walk the LISTEN socket on the service's effective port
// to the process **group leader** (which satisfies `pgid == pid`, exactly what
// the rest of the machinery assumes), corroborate that the group really is this
// app (cwd or argv under the app root), and then route it through the unchanged
// adoption path. Four independent barriers make killing a terminal impossible.

/// `lsof -a -p <pid> -d cwd -Fn` → the process's cwd, or `None` (unreadable /
/// dead pid / lsof absent). Absence is treated as no signal, never a match.
fn pid_cwd(pid: u32) -> Option<String> {
    for prog in ["lsof", "/usr/sbin/lsof"] {
        let Ok(out) = std::process::Command::new(prog)
            .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
            .output()
        else {
            continue;
        };
        for l in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(rest) = l.strip_prefix('n') {
                let p = rest.trim();
                if !p.is_empty() {
                    return Some(p.to_string());
                }
            }
        }
        return None;
    }
    None
}

/// `ps -g <pgid> -o command=` → one argv string per group member.
fn group_argv_lines(pgid: u32) -> Vec<String> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-g", &pgid.to_string(), "-o", "command="])
        .output()
    else {
        return vec![];
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Boundary-safe "is `cwd` at or under `root`" — not a bare substring, so
/// `/…/opal-sandbox` does NOT match root `/…/opal`.
fn path_under(cwd: &str, root: &str) -> bool {
    let cwd = cwd.trim_end_matches('/');
    let root = root.trim_end_matches('/');
    !root.is_empty() && (cwd == root || cwd.starts_with(&format!("{root}/")))
}

/// The leader's argv names an interactive shell / login / multiplexer → refuse
/// to adopt or signal (the kill-the-terminal guard). Applied only on the
/// foreign path. Strips a login shell's leading `-` and takes the basename.
fn leader_is_shell(command: &str) -> bool {
    let base = command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_start_matches('-')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(':'); // e.g. "sshd:" in "sshd: user@pts/1"
    matches!(
        base,
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "tcsh" | "csh" | "login" | "tmux"
            | "screen" | "sshd" | "mosh-server"
    )
}

/// Walk an effective port → its LISTEN socket's pid → that process's group
/// **leader**. `None` if nothing listens, the chain is missing, the leader isn't
/// its own group leader (`pgid == pid`, the invariant the machinery assumes), or
/// the leader is launchd-ish (`pid <= 1`).
fn resolve_listener_leader(port: u16) -> Option<(u32, ProcFacts)> {
    let listener = port_listener_pid(port)?;
    let lf = ps_facts(listener)?;
    let leader_pid = lf.pgid;
    if leader_pid <= 1 {
        return None; // launchd / daemonized → refuse
    }
    let leader = ps_facts(leader_pid)?;
    if leader.pgid != leader_pid {
        return None; // not a group leader → can't safely killpg it
    }
    Some((leader_pid, leader))
}

/// Does the leader's process group really belong to THIS app, and is it safe to
/// adopt/signal? Requires (a) the leader is not a shell/login/multiplexer, AND
/// (b) the leader's cwd is under `root`, OR some group member's argv contains the
/// absolute `root` (a long, specific, low-collision path). Absence of evidence
/// ⇒ not this app ⇒ no adoption.
fn group_belongs_to_app(leader_pid: u32, leader_cmd: &str, root: &str) -> bool {
    if leader_is_shell(leader_cmd) {
        return false; // never claim/kill a terminal
    }
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return false;
    }
    // `lsof` reports the *canonical* cwd (macOS resolves `/var`→`/private/var`,
    // and a user may symlink a project dir), so compare against the canonical
    // root for the cwd check. Fall back to the raw root if it can't be resolved.
    let canon = std::fs::canonicalize(root)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    if let Some(cwd) = pid_cwd(leader_pid) {
        if path_under(&cwd, canon.as_deref().unwrap_or(root)) {
            return true;
        }
    }
    // argv usually contains the root as the user typed it; accept either form.
    group_argv_lines(leader_pid).iter().any(|argv| {
        argv.contains(root) || canon.as_deref().map_or(false, |c| argv.contains(c))
    })
}

/// Foreign Stop guard: re-resolve the leader and re-corroborate immediately
/// before each `killpg`, so a pid that died/was-reused or a process that is no
/// longer this app's tree is never signalled.
fn safe_to_signal_foreign(leader_pid: u32, started_at: &str, root: &str) -> bool {
    match ps_facts(leader_pid) {
        Some(f) => {
            f.pgid == leader_pid
                && f.started_at == started_at
                && group_belongs_to_app(leader_pid, &f.command, root)
        }
        None => false,
    }
}

/// Stop-time safety for any adopted service: the stricter foreign guard when a
/// `root` is present (externally-detected), else the leader+lstart guard.
fn adopted_signal_ok(pid: u32, started: Option<&str>, root: Option<&str>) -> bool {
    match root {
        Some(r) => safe_to_signal_foreign(pid, started.unwrap_or(""), r),
        None => safe_to_signal(pid, started),
    }
}

/// Detect a server running on `port` that Harbor did not spawn but which is
/// corroborated as `app`'s service. Returns a `foreign` [`PersistedRun`] keyed on
/// the group **leader** (so `still_ours`/`killpg`/persistence all apply), or
/// `None` if nothing is there or it doesn't look like this app. Ends with a
/// `still_ours` re-check so detection can never mint a record the rest of the
/// system would reject — one trust path, no parallel gate.
fn detect_external(
    app: &str,
    svc_name: &str,
    port: u16,
    root: &str,
    profile: Option<String>,
) -> Option<PersistedRun> {
    let (leader_pid, leader) = resolve_listener_leader(port)?;
    if !group_belongs_to_app(leader_pid, &leader.command, root) {
        return None;
    }
    let rec = PersistedRun {
        app: app.to_string(),
        service: svc_name.to_string(),
        pid: leader_pid, // == pgid → exactly what killpg needs
        port: Some(port),
        command: leader.command.clone(), // OBSERVED leader argv → G3 self-consistent
        cwd: pid_cwd(leader_pid).unwrap_or_else(|| root.to_string()),
        profile,
        started_at: leader.started_at.clone(), // the LEADER's lstart, not the listener's
        foreign: true,
        root: Some(root.to_string()),
    };
    if still_ours(&rec) {
        Some(rec)
    } else {
        None
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One whole-machine `ps`, grouped by pgid → (summed RSS bytes, summed CPU%).
/// `ps -axo pgid=,rss=,pcpu=` is a single ~30ms shell-out regardless of how many
/// services are live, and is inherently safe against stale pgids (a dead group is
/// simply absent). Rows are `PGID RSS_KiB PCPU`; the token split is padding-robust.
fn sample_all_groups() -> HashMap<u32, (u64, f32)> {
    let mut map: HashMap<u32, (u64, f32)> = HashMap::new();
    let Ok(out) = std::process::Command::new("ps")
        .args(["-axo", "pgid=,rss=,pcpu="])
        .output()
    else {
        return map; // ps missing/failed → empty map → no stats this tick
    };
    if !out.status.success() {
        return map;
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut t = line.split_whitespace();
        let (Some(pg), Some(rss), Some(cpu)) = (t.next(), t.next(), t.next()) else {
            continue;
        };
        let (Ok(pgid), Ok(rss_kib), Ok(pcpu)) =
            (pg.parse::<u32>(), rss.parse::<u64>(), cpu.parse::<f32>())
        else {
            continue;
        };
        let e = map.entry(pgid).or_insert((0, 0.0));
        e.0 += rss_kib * 1024; // KiB → bytes
        e.1 += pcpu; // group CPU% sum (can exceed 100 on multicore)
    }
    map
}

/// Whether a non-deliberate exit is a *failure* worth notifying / auto-restarting:
/// a signal death, any non-zero code, or an exit-0 that happened BEFORE the
/// service ever became `Ready` (a crash-on-start). An exit-0 after `Ready` — or a
/// one-shot running to completion — is clean and is left alone, so a build/lint
/// task registered as a service never gets restarted in a loop.
fn is_failure_exit(exit_code: Option<i32>, reached_ready: bool) -> bool {
    match exit_code {
        None => true,
        Some(0) => !reached_ready,
        Some(_) => true,
    }
}

impl Supervisor {
    pub fn new(app: AppHandle, store: Arc<Store>, registry: Registry) -> Arc<Self> {
        let user_path = crate::sysenv::enriched_path();
        Arc::new_cyclic(|me| Supervisor {
            app,
            runs: Arc::new(Mutex::new(BTreeMap::new())),
            reserved: Arc::new(Mutex::new(HashSet::new())),
            seq: Arc::new(AtomicU64::new(0)),
            user_path,
            store,
            registry,
            last_notified: Arc::new(Mutex::new(HashMap::new())),
            shutting_down: Arc::new(AtomicBool::new(false)),
            me: me.clone(),
        })
    }

    /// Mark the supervisor as shutting down: any in-flight auto-restart backoff
    /// and crash notification becomes a no-op. Called on app quit.
    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    /// Spawn the single periodic resource sampler (group CPU% + RSS, ~2s). Call
    /// once from setup, after adoption so adopted/external services are sampled
    /// too. Holds a `Weak<Self>` so it dies with the supervisor.
    pub fn spawn_sampler(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(STATS_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let Some(sup) = me.upgrade() else { break };
                if sup.shutting_down.load(Ordering::SeqCst) {
                    break;
                }
                sup.sample_once().await;
            }
        });
    }

    /// One sampling pass: collect live (app, svc, pgid) under the lock and drop
    /// it; idle with no `ps` when nothing is live; run the single `ps` off the
    /// lock; re-lock briefly to write cpu/mem back; emit one batch event.
    async fn sample_once(&self) {
        let targets: Vec<(String, String, u32)> = {
            let runs = self.runs.lock().await;
            let mut v = Vec::new();
            for (app, run) in runs.iter() {
                for s in run.services.values() {
                    if s.status.is_live() {
                        if let Some(pid) = s.pid {
                            v.push((app.clone(), s.name.clone(), pid)); // pid == pgid
                        }
                    }
                }
            }
            v
        };
        if targets.is_empty() {
            return; // nothing running → no shell-out at all
        }

        let groups = tokio::task::spawn_blocking(sample_all_groups)
            .await
            .unwrap_or_default();

        let mut batch: Vec<ServiceStat> = Vec::with_capacity(targets.len());
        {
            let mut runs = self.runs.lock().await;
            for (app, svc, pgid) in &targets {
                // Absent pgid = group died between collect and ps → skip.
                let Some(&(mem_bytes, cpu)) = groups.get(pgid) else {
                    continue;
                };
                if let Some(s) = runs.get_mut(app).and_then(|r| r.services.get_mut(svc)) {
                    if !s.status.is_live() {
                        continue; // stopped mid-tick → don't write a stale sample
                    }
                    s.cpu = Some(cpu);
                    s.mem_bytes = Some(mem_bytes);
                    batch.push(ServiceStat {
                        app: app.clone(),
                        service: svc.clone(),
                        cpu,
                        mem_bytes,
                    });
                }
            }
        }
        if !batch.is_empty() {
            let _ = self.app.emit(STATS_EVENT, &batch);
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

        // Preflight: a chosen port that's already bound would crash the child
        // with EADDRINUSE. But if an externally-started server is sitting on it
        // and corroborates as THIS app (port + project folder), adopt it instead
        // of spawning a duplicate. Only bail when the holder isn't this app. (Our
        // own already-running run was short-circuited by `is_running` above.)
        let mut external_recs: Vec<PersistedRun> = Vec::new();
        for (svc_name, p) in &alloc.ports {
            if ports::is_port_free(*p) {
                continue;
            }
            if let Some(rec) =
                detect_external(&cfg.name, svc_name, *p, &cfg.root, Some(profile.to_string()))
            {
                external_recs.push(rec);
                continue;
            }
            let who = port_listener_pid(*p)
                .map(|pid| format!(" by pid {pid}"))
                .unwrap_or_default();
            bail!(
                "port {p} (service '{svc_name}') is already in use{who}, but it doesn't look \
                 like {} — its working directory and command don't match this project. Stop \
                 that process (or change {svc_name}'s port), then start again.",
                cfg.name
            );
        }

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
                    intentional_stop: HashSet::new(),
                },
            );
        }

        // Reflect any externally-started services as `external` adoptions; the
        // spawn loop below skips them (they're already running + reserved).
        let mut adopted_external: HashSet<String> = HashSet::new();
        for rec in external_recs {
            adopted_external.insert(rec.service.clone());
            self.adopt_external(cfg, Some(profile.to_string()), rec).await;
        }

        let root = PathBuf::from(&cfg.root);
        for svc in &ordered {
            if adopted_external.contains(&svc.name) {
                continue; // already running outside Harbor — adopted, not respawned
            }
            let port = alloc.ports.get(&svc.name).copied();
            let (resolved_command, resolved_env) = ports::resolve_service(svc, &alloc.ports);

            match self
                .spawn_service(cfg, svc, &root, profile, port, &resolved_command, &resolved_env, 0)
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
        profile: &str,
        port: Option<u16>,
        resolved_command: &str,
        resolved_env: &BTreeMap<String, String>,
        restart_count: u32,
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

        // Capture the kernel start-time now (for PID-reuse-safe adoption) and
        // persist the run so a relaunched Harbor can re-adopt it.
        let started_at = pid.and_then(ps_facts).map(|f| f.started_at);
        if let Some(pidv) = pid {
            let _ = self.store.upsert_run(PersistedRun {
                app: cfg.name.clone(),
                service: svc.name.clone(),
                pid: pidv,
                port,
                command: resolved_command.to_string(),
                cwd: cwd.to_string_lossy().into_owned(),
                profile: Some(profile.to_string()),
                started_at: started_at.clone().unwrap_or_default(),
                foreign: false,
                root: None,
            });
        }

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
                    started_at,
                    adopted: false,
                    external: false,
                    root: None,
                    resolved_env: Some(resolved_env.clone()),
                    restart_count,
                    ready_since: None,
                    cpu: None,
                    mem_bytes: None,
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

        // Monitor task owns the Child: waits, reaps, records exit, frees port,
        // drops the persisted record, and — if the exit was NOT a deliberate
        // Stop — hands off to the crash path (notify / auto-restart).
        {
            let runs = self.runs.clone();
            let reserved = self.reserved.clone();
            let store = self.store.clone();
            let app = self.app.clone();
            let app_name = cfg.name.clone();
            let svc_name = svc.name.clone();
            let me = self.me.clone();
            tokio::spawn(async move {
                let status = child.wait().await;
                let code = status.ok().and_then(|s| s.code());

                // Consume the intentional-stop marker UNDER the runs lock (the
                // same lock stop() writes it under) — this is what makes the
                // stop-vs-crash decision race-free. A missing run means stop()
                // already removed it ⇒ intentional.
                let intended = {
                    let mut r = runs.lock().await;
                    r.get_mut(&app_name)
                        .map(|run| run.intentional_stop.remove(&svc_name))
                        .unwrap_or(true)
                };

                set_status_inner(&app, &runs, &app_name, &svc_name, ServiceStatus::Exited, code)
                    .await;
                if let Some(p) = port {
                    reserved.lock().await.remove(&p);
                }
                let _ = store.remove_run(&app_name, &svc_name);

                if !intended {
                    if let Some(sup) = me.upgrade() {
                        // `on_unexpected_exit` returns a boxed `Send` future,
                        // crossing a type-erased boundary that breaks the Send
                        // auto-trait inference cycle from the async recursion
                        // (spawn_service → monitor → on_unexpected_exit → …).
                        sup.on_unexpected_exit(&app_name, &svc_name, code).await;
                    }
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
        // Mark every live service intentional BEFORE any signalling, latched on
        // the AppRun for the whole stop sequence (covers SIGTERM, the grace
        // window, SIGKILL escalation). Each monitor consumes its own name on
        // exit, so a deliberate Stop is never mistaken for a crash.
        {
            let mut runs = self.runs.lock().await;
            if let Some(run) = runs.get_mut(app_name) {
                let names: Vec<String> = run
                    .services
                    .values()
                    .filter(|s| s.status.is_live())
                    .map(|s| s.name.clone())
                    .collect();
                for n in names {
                    run.intentional_stop.insert(n);
                }
            }
        }

        let targets = self.live_signal_targets(app_name).await;
        if targets.is_empty() {
            // Nothing (still) live; clear any stale records.
            self.runs.lock().await.remove(app_name);
            let _ = self.store.remove_app_runs(app_name);
            return Ok(());
        }

        for (name, pid) in &targets {
            self.system_log(app_name, name, "SIGTERM → process group").await;
            let _ = killpg(Pid::from_raw(*pid as i32), Signal::SIGTERM);
        }

        // Poll until every targeted process is actually gone, up to the grace
        // period. (Adopted services have no monitor task, so we verify via `ps`
        // rather than waiting for a monitor-recorded exit.)
        let deadline = tokio::time::Instant::now() + STOP_GRACE;
        loop {
            if !self.any_live_process(app_name).await {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Escalate to SIGKILL for anything still alive (re-verified safe).
        let still = self.live_signal_targets(app_name).await;
        for (name, pid) in &still {
            self.system_log(app_name, name, "SIGKILL → process group").await;
            let _ = killpg(Pid::from_raw(*pid as i32), Signal::SIGKILL);
        }
        if !still.is_empty() {
            // Give monitors a beat to reap.
            tokio::time::sleep(Duration::from_millis(300)).await;
        }

        // Drop the run record + persisted entries; ports are freed by the
        // monitors as they exit (adopted ports are freed below).
        {
            let mut runs = self.runs.lock().await;
            if let Some(run) = runs.get(app_name) {
                let adopted_ports: Vec<u16> = run
                    .services
                    .values()
                    .filter(|s| s.adopted)
                    .filter_map(|s| s.port)
                    .collect();
                if !adopted_ports.is_empty() {
                    let mut reserved = self.reserved.lock().await;
                    for p in adopted_ports {
                        reserved.remove(&p);
                    }
                }
            }
            runs.remove(app_name);
        }
        let _ = self.store.remove_app_runs(app_name);
        Ok(())
    }

    /// Live services to signal, filtering out adopted entries whose pid is no
    /// longer provably ours (died and possibly reused since adoption).
    async fn live_signal_targets(&self, app_name: &str) -> Vec<(String, u32)> {
        type Cand = (String, u32, bool, Option<String>, Option<String>);
        let candidates: Vec<Cand> = {
            let runs = self.runs.lock().await;
            match runs.get(app_name) {
                Some(run) => run
                    .services
                    .values()
                    .filter(|s| s.status.is_live())
                    .filter_map(|s| {
                        s.pid.map(|p| {
                            (s.name.clone(), p, s.adopted, s.started_at.clone(), s.root.clone())
                        })
                    })
                    .collect(),
                None => return vec![],
            }
        };
        candidates
            .into_iter()
            .filter(|(_, pid, adopted, started, root)| {
                !*adopted || adopted_signal_ok(*pid, started.as_deref(), root.as_deref())
            })
            .map(|(name, pid, _, _, _)| (name, pid))
            .collect()
    }

    /// True if any live service of `app_name` still has a running process.
    /// Monitored services are trusted (their monitor flips status on exit);
    /// adopted services are verified directly with `ps`.
    async fn any_live_process(&self, app_name: &str) -> bool {
        let snap: Vec<(u32, bool, Option<String>, Option<String>)> = {
            let runs = self.runs.lock().await;
            match runs.get(app_name) {
                Some(run) => run
                    .services
                    .values()
                    .filter(|s| s.status.is_live())
                    .filter_map(|s| {
                        s.pid
                            .map(|p| (p, s.adopted, s.started_at.clone(), s.root.clone()))
                    })
                    .collect(),
                None => return false,
            }
        };
        for (pid, adopted, started, root) in snap {
            if !adopted {
                return true; // monitored & still marked live → trust the monitor
            }
            if adopted_signal_ok(pid, started.as_deref(), root.as_deref()) {
                return true; // adopted process still alive per `ps`
            }
        }
        false
    }

    /// Handle a Harbor-spawned service that exited WITHOUT a deliberate Stop.
    /// Decides crash-vs-clean, notifies, and (if the app opted in) auto-restarts
    /// with bounded backoff. Adopted/external services never reach here.
    ///
    /// Returns a **boxed `Send` future** on purpose: this method is mutually
    /// recursive with `spawn_service` (it respawns, whose monitor calls back
    /// here), and the type-erased boundary is what lets the compiler resolve
    /// `Send` for the spawned monitor task.
    fn on_unexpected_exit<'a>(
        &'a self,
        app: &'a str,
        svc: &'a str,
        exit_code: Option<i32>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }

        // Snapshot the (now-Exited) proc + run profile under the runs lock.
        let snap = {
            let runs = self.runs.lock().await;
            runs.get(app).and_then(|r| {
                let profile = r.profile.clone();
                r.services.get(svc).map(|s| {
                    (
                        s.adopted,
                        s.external,
                        s.status,
                        s.port,
                        s.resolved_command.clone(),
                        s.resolved_env.clone(),
                        s.restart_count,
                        s.ready_since,
                        profile,
                    )
                })
            })
        };
        let Some((adopted, external, status, port, cmd, env, count, ready_since, profile)) = snap
        else {
            return; // run gone (a Stop won the race) ⇒ nothing to do
        };
        // Belt-and-suspenders: adopted/external have no monitor and can't reach
        // here, but never restart something Harbor didn't launch.
        if adopted || external || status != ServiceStatus::Exited {
            return;
        }

        // Read possibly-edited live config for the opt-in + service definition.
        let cfg = self.registry.read().await.get(app).cloned();
        let Some(cfg) = cfg else { return };
        let Some(svc_cfg) = cfg.service(svc).cloned() else {
            return;
        };

        // Crash predicate (hard-req: exit-0). A failure is: signal death, any
        // non-zero code, OR exit-0 that happened BEFORE the service ever became
        // Ready (a crash-on-start that exits 0). Exit-0 after Ready / a one-shot
        // running to completion is clean — never restart it.
        let reached_ready = ready_since.is_some();
        if !is_failure_exit(exit_code, reached_ready) {
            self.system_log(app, svc, "exited cleanly (code 0)").await;
            return;
        }

        if !cfg.auto_restart {
            self.notify_crash(app, svc, exit_code, false).await;
            return;
        }

        // Stability reset: a respawn that stayed Ready ≥ window starts fresh.
        let count = match ready_since {
            Some(t) if t.elapsed() >= RESTART_STABLE_RESET => 0,
            _ => count,
        };
        if count >= RESTART_WINDOW_MAX {
            self.system_log(
                app,
                svc,
                &format!(
                    "auto-restart gave up after {RESTART_WINDOW_MAX} attempts — service keeps \
                     crashing. Fix the error and Start again."
                ),
            )
            .await;
            self.notify_crash(app, svc, exit_code, true).await;
            return;
        }

        let delay = RESTART_BACKOFF[count as usize];
        self.system_log(
            app,
            svc,
            &format!(
                "auto-restart {}/{RESTART_WINDOW_MAX} — service crashed ({}), restarting in {delay}s…",
                count + 1,
                exit_code.map(|c| format!("exit {c}")).unwrap_or_else(|| "signal".into())
            ),
        )
        .await;
        tokio::time::sleep(Duration::from_secs(delay)).await;

        // Re-validate after the backoff: shutdown / a Stop / a manual Start may
        // have raced in. Only proceed if the service is still the Exited one.
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        {
            let runs = self.runs.lock().await;
            match runs.get(app).and_then(|r| r.services.get(svc)) {
                Some(s) if s.status == ServiceStatus::Exited && !s.adopted && !s.external => {}
                _ => return,
            }
        }

        // The crashed server should have released its port.
        if let Some(p) = port {
            if !ports::is_port_free(p) {
                self.system_log(app, svc, "port no longer free — auto-restart gave up")
                    .await;
                self.notify_crash(app, svc, exit_code, true).await;
                return;
            }
            self.reserved.lock().await.insert(p);
        }

        let root = PathBuf::from(&cfg.root);
        let profile = profile.unwrap_or_else(|| "default".to_string());
        let env = env.unwrap_or_default();
        let cmd = cmd.unwrap_or_default();
        if let Err(e) = self
            .spawn_service(&cfg, &svc_cfg, &root, &profile, port, &cmd, &env, count + 1)
            .await
        {
            self.system_log(app, svc, &format!("auto-restart failed to spawn: {e}"))
                .await;
            if let Some(p) = port {
                self.reserved.lock().await.remove(&p);
            }
        }
        }) // end Box::pin(async move { … })
    }

    /// Post a native crash notification (best-effort, rate-limited). Never fires
    /// for intentional stops or adopted/external services (callers gate that).
    async fn notify_crash(&self, app: &str, svc: &str, code: Option<i32>, gave_up: bool) {
        use tauri_plugin_notification::NotificationExt;
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        // Cooldown — but a give-up is terminal & important, so it's exempt.
        if !gave_up {
            let mut m = self.last_notified.lock().await;
            let key = (app.to_string(), svc.to_string());
            let now = Instant::now();
            if m
                .get(&key)
                .map(|t| now.duration_since(*t) < NOTIFY_COOLDOWN)
                .unwrap_or(false)
            {
                return;
            }
            m.insert(key, now);
        }
        let (title, body) = if gave_up {
            (
                format!("{app} — {svc} keeps crashing"),
                format!("Gave up after {RESTART_WINDOW_MAX} restart attempts. Click to open Harbor."),
            )
        } else if code.is_none() {
            (
                format!("{app} — {svc} crashed"),
                "The process was killed (out of memory?). Click to open Harbor.".to_string(),
            )
        } else {
            (
                format!("{app} — {svc} crashed"),
                format!("Exited with code {}. Click to open Harbor.", code.unwrap()),
            )
        };
        let _ = self
            .app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show();
    }

    /// Re-adopt processes a previous Harbor session left running. Called once at
    /// launch (before the window is shown) so the UI reflects already-running
    /// servers and a duplicate Start is short-circuited by `is_running`.
    ///
    /// Only records this Harbor wrote are considered, and each must pass the full
    /// [`still_ours`] identity gate. Survivors are rebuilt as `adopted`, `Ready`
    /// services (pid/port held, but no live log stream); everything stale is
    /// pruned. **Nothing is ever signalled here.**
    pub async fn adopt_persisted(&self) {
        let file = self.store.load_runs().unwrap_or_default();
        if file.runs.is_empty() {
            return;
        }

        // Keep only records whose process is provably still ours, grouped by app.
        let mut by_app: BTreeMap<String, Vec<PersistedRun>> = BTreeMap::new();
        for e in file.runs {
            if still_ours(&e) {
                by_app.entry(e.app.clone()).or_default().push(e);
            }
        }
        if by_app.is_empty() {
            let _ = self.store.save_runs(&RunsFile::default());
            return;
        }

        let mut survivors: Vec<PersistedRun> = Vec::new();

        for (app, entries) in by_app {
            let mut services: BTreeMap<String, ServiceProc> = BTreeMap::new();
            let mut plan: Vec<PortPlanEntry> = Vec::new();
            let mut profile: Option<String> = None;

            for e in &entries {
                profile = profile.or_else(|| e.profile.clone());
                if let Some(p) = e.port {
                    self.reserved.lock().await.insert(p);
                    plan.push(PortPlanEntry {
                        service: e.service.clone(),
                        preferred: Some(p),
                        resolved: p,
                        note: Some(if e.foreign {
                            format!("external — running outside Harbor (pid {})", e.pid)
                        } else {
                            "adopted from previous session".to_string()
                        }),
                    });
                }
                let mut logs = VecDeque::new();
                let seq_n = self.seq.fetch_add(1, Ordering::Relaxed);
                logs.push_back(LogLine {
                    app: app.clone(),
                    service: e.service.clone(),
                    stream: "system".to_string(),
                    line: if e.foreign {
                        format!(
                            "detected running outside Harbor (pid {}{}). Live output isn't \
                             captured for a process Harbor didn't launch — Stop it here, then \
                             Start to run it under Harbor with logs.",
                            e.pid,
                            e.port.map(|p| format!(", port {p}")).unwrap_or_default()
                        )
                    } else {
                        format!(
                            "adopted from a previous Harbor session (pid {}{}). Live output isn't \
                             captured for an adopted process — Stop and Start to recapture logs.",
                            e.pid,
                            e.port.map(|p| format!(", port {p}")).unwrap_or_default()
                        )
                    },
                    ts: now_millis(),
                    seq: seq_n,
                });
                services.insert(
                    e.service.clone(),
                    ServiceProc {
                        name: e.service.clone(),
                        status: ServiceStatus::Ready,
                        pid: Some(e.pid),
                        port: e.port,
                        resolved_command: Some(e.command.clone()),
                        exit_code: None,
                        logs,
                        started_at: Some(e.started_at.clone()),
                        adopted: true,
                        external: e.foreign,
                        root: e.root.clone(),
                        resolved_env: None,
                        restart_count: 0,
                        ready_since: None,
                        cpu: None,
                        mem_bytes: None,
                    },
                );
            }

            if services.is_empty() {
                continue;
            }

            let to_emit: Vec<(String, Option<u16>, u32)> = services
                .values()
                .map(|s| (s.name.clone(), s.port, s.pid.unwrap_or(0)))
                .collect();
            {
                let mut runs = self.runs.lock().await;
                runs.insert(
                    app.clone(),
                    AppRun {
                        profile: profile.clone(),
                        port_plan: plan,
                        services,
                        intentional_stop: HashSet::new(),
                    },
                );
            }
            for (name, port, pid) in to_emit {
                self.emit_status(&app, name, ServiceStatus::Ready, port, Some(pid), None);
            }
            eprintln!(
                "[harbor] adopted {} running service(s) for {app}",
                entries.len()
            );
            for e in &entries {
                self.spawn_adopted_monitor(e.clone());
            }
            survivors.extend(entries);
        }

        let _ = self.store.save_runs(&RunsFile { runs: survivors });
    }

    /// Reflect one externally-detected service as a running, `external` adoption.
    /// Inserts it into the app's run (creating the run if needed), reserves the
    /// port, persists the record, emits `Ready`, and starts the liveness poll —
    /// the same path orphan adoptions take, plus `external`/`root` markers.
    async fn adopt_external(&self, cfg: &AppConfig, profile: Option<String>, rec: PersistedRun) {
        let app = cfg.name.clone();
        let svc = rec.service.clone();
        let port = rec.port;
        let pid = rec.pid;
        if let Some(p) = port {
            self.reserved.lock().await.insert(p);
        }

        let seq_n = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut logs = VecDeque::new();
        logs.push_back(LogLine {
            app: app.clone(),
            service: svc.clone(),
            stream: "system".to_string(),
            line: format!(
                "detected running outside Harbor (pid {pid}{}). Live output isn't captured for a \
                 process Harbor didn't launch — Stop it here, then Start to run it under Harbor \
                 with logs.",
                port.map(|p| format!(", port {p}")).unwrap_or_default()
            ),
            ts: now_millis(),
            seq: seq_n,
        });

        let proc = ServiceProc {
            name: svc.clone(),
            status: ServiceStatus::Ready,
            pid: Some(pid),
            port,
            resolved_command: Some(rec.command.clone()),
            exit_code: None,
            logs,
            started_at: Some(rec.started_at.clone()),
            adopted: true,
            external: true,
            root: Some(cfg.root.clone()),
            resolved_env: None,
            restart_count: 0,
            ready_since: None,
            cpu: None,
            mem_bytes: None,
        };

        {
            let mut runs = self.runs.lock().await;
            let run = runs.entry(app.clone()).or_insert_with(|| AppRun {
                profile: profile.clone(),
                port_plan: Vec::new(),
                services: BTreeMap::new(),
                intentional_stop: HashSet::new(),
            });
            if run.profile.is_none() {
                run.profile = profile.clone();
            }
            run.port_plan.retain(|e| e.service != svc);
            if let Some(p) = port {
                run.port_plan.push(PortPlanEntry {
                    service: svc.clone(),
                    preferred: Some(p),
                    resolved: p,
                    note: Some(format!("external — running outside Harbor (pid {pid})")),
                });
            }
            run.services.insert(svc.clone(), proc);
        }

        let _ = self.store.upsert_run(rec.clone());
        eprintln!(
            "[harbor] reflected external server for {app}/{svc} (leader pid {pid}{})",
            port.map(|p| format!(", port {p}")).unwrap_or_default()
        );
        self.emit_status(&app, svc, ServiceStatus::Ready, port, Some(pid), None);
        self.spawn_adopted_monitor(rec);
    }

    /// If `cfg` isn't already tracked as running, probe each of its ported
    /// (default-profile) services for an externally-started server and reflect
    /// any that corroborate as this app. Cheap: a free port costs one `lsof`.
    pub async fn reflect_external_if_idle(&self, cfg: &AppConfig) {
        if self.is_running(&cfg.name).await {
            return;
        }
        for svc in cfg.services_for_profile("default") {
            let Some(port) = ports::effective_port(&svc) else {
                continue;
            };
            if ports::is_port_free(port) {
                continue; // nothing listening — no external process to reflect
            }
            let app = cfg.name.clone();
            let svc_name = svc.name.clone();
            let root = cfg.root.clone();
            let rec = tokio::task::spawn_blocking(move || {
                detect_external(&app, &svc_name, port, &root, Some("default".to_string()))
            })
            .await
            .ok()
            .flatten();
            if let Some(rec) = rec {
                self.adopt_external(cfg, Some("default".to_string()), rec).await;
            }
        }
    }

    /// Launch-time sweep: reflect externally-running servers for every registered
    /// app, so they show as running the instant Harbor opens (no Start needed).
    pub async fn scan_and_adopt_external(&self, configs: &[AppConfig]) {
        for cfg in configs {
            self.reflect_external_if_idle(cfg).await;
        }
    }

    /// Poll an adopted process for liveness — it has no monitor task owning a
    /// `Child`. When it disappears, mark it `Exited`, free its port, and drop the
    /// persisted record.
    fn spawn_adopted_monitor(&self, e: PersistedRun) {
        let runs = self.runs.clone();
        let reserved = self.reserved.clone();
        let store = self.store.clone();
        let app = self.app.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(4)).await;
                let live = {
                    let r = runs.lock().await;
                    r.get(&e.app)
                        .and_then(|a| a.services.get(&e.service))
                        .map(|s| s.status.is_live())
                        .unwrap_or(false)
                };
                if !live {
                    break; // stopped / removed / already exited
                }
                let entry = e.clone();
                let alive = tokio::task::spawn_blocking(move || still_ours(&entry))
                    .await
                    .unwrap_or(false);
                if !alive {
                    set_status_inner(&app, &runs, &e.app, &e.service, ServiceStatus::Exited, None)
                        .await;
                    if let Some(p) = e.port {
                        reserved.lock().await.remove(&p);
                    }
                    let _ = store.remove_run(&e.app, &e.service);
                    break;
                }
            }
        });
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
        // Stamp the moment a service comes up, so a respawn that stays Ready past
        // the stability window resets its auto-restart counter.
        if status == ServiceStatus::Ready && svc.ready_since.is_none() {
            svc.ready_since = Some(Instant::now());
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

#[cfg(test)]
mod tests {
    //! Integration tests for the OS-introspection adoption gate. These shell out
    //! to the real `ps`/`lsof` and spawn real processes, so they validate the
    //! exact primitives the running app depends on on this platform.
    use super::*;
    use crate::model::PersistedRun;
    use std::os::unix::process::CommandExt;

    /// Spawn `sh -c <cmd>` in its own session/group, exactly like the supervisor.
    fn spawn_grouped(cmd: &str) -> std::process::Child {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        unsafe {
            c.pre_exec(|| {
                nix::unistd::setsid()
                    .map(|_| ())
                    .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
            });
        }
        c.spawn().expect("spawn test process")
    }

    #[test]
    fn ps_facts_reads_self_and_is_stable() {
        let me = std::process::id();
        let a = ps_facts(me).expect("our own pid must resolve");
        let b = ps_facts(me).expect("second read");
        assert!(!a.started_at.is_empty(), "lstart should be non-empty");
        assert_eq!(a.started_at, b.started_at, "lstart must be stable per process");
    }

    #[test]
    fn ps_facts_none_for_dead_pid() {
        // A pid this high is effectively never live.
        assert!(ps_facts(900_000_000).is_none());
    }

    #[test]
    fn still_ours_true_for_live_child_then_false_after_kill() {
        let child = spawn_grouped("sleep 30");
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(250));

        let facts = ps_facts(pid).expect("child should be visible to ps");
        assert_eq!(facts.pgid, pid, "setsid child must lead its own group");

        let rec = PersistedRun {
            app: "t".into(),
            service: "s".into(),
            pid,
            port: None,
            command: "sleep 30".into(),
            cwd: ".".into(),
            profile: None,
            started_at: facts.started_at.clone(),
            foreign: false,
            root: None,
        };
        assert!(still_ours(&rec), "a live, matching, leader process is ours");

        // A start-time mismatch (PID-reuse simulation) must fail the gate.
        let mut reused = rec.clone();
        reused.started_at = "Mon Jan  1 00:00:00 2001".into();
        assert!(!still_ours(&reused), "different start-time → not ours");

        // Tear the group down; the gate must then report it gone.
        let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
        let mut waitable = child;
        let _ = waitable.wait();
        std::thread::sleep(Duration::from_millis(150));
        assert!(!still_ours(&rec), "after kill the process is no longer ours");
    }

    #[test]
    fn port_listener_pid_finds_our_listener() {
        use std::net::{Ipv4Addr, TcpListener};
        let l = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind ephemeral");
        let port = l.local_addr().unwrap().port();
        // lsof should attribute the LISTENing socket to this test process.
        let found = port_listener_pid(port);
        assert_eq!(found, Some(std::process::id()), "lsof should name our pid");
        drop(l);
    }

    #[test]
    fn failure_exit_classification() {
        // Signal death (no code) is always a failure.
        assert!(is_failure_exit(None, true));
        assert!(is_failure_exit(None, false));
        // Any non-zero code is a failure regardless of readiness.
        assert!(is_failure_exit(Some(1), true));
        assert!(is_failure_exit(Some(137), false));
        // Exit-0 BEFORE Ready (crash-on-start) is a failure…
        assert!(is_failure_exit(Some(0), false));
        // …but exit-0 AFTER Ready (clean stop / one-shot done) is NOT.
        assert!(!is_failure_exit(Some(0), true));
    }

    #[test]
    fn restart_backoff_is_bounded() {
        // The backoff table must cover every attempt up to the give-up cap, and
        // a crash-on-start loop must terminate quickly (sum well under a minute).
        assert_eq!(RESTART_BACKOFF.len() as u32, RESTART_WINDOW_MAX);
        let total: u64 = RESTART_BACKOFF.iter().sum();
        assert!(total < 60, "a crash loop should give up in < 60s, got {total}s");
    }

    #[test]
    fn path_under_is_boundary_safe() {
        assert!(path_under("/Users/x/opal", "/Users/x/opal"));
        assert!(path_under("/Users/x/opal/web", "/Users/x/opal"));
        assert!(path_under("/Users/x/opal/", "/Users/x/opal"));
        // The classic prefix-collision must NOT match.
        assert!(!path_under("/Users/x/opal-sandbox", "/Users/x/opal"));
        assert!(!path_under("/Users/x/other", "/Users/x/opal"));
        assert!(!path_under("/anything", ""));
    }

    #[test]
    fn leader_is_shell_catches_terminals() {
        for s in ["zsh", "-zsh", "/bin/bash", "/usr/bin/fish -i", "login -pf x", "tmux", "sshd: u"]
        {
            assert!(leader_is_shell(s), "{s:?} should read as a shell/login");
        }
        for s in ["node /x/yarn.js run dev", "next dev -p 3002", "/usr/bin/python3 -m http.server"]
        {
            assert!(!leader_is_shell(s), "{s:?} should NOT read as a shell");
        }
    }

    #[test]
    fn group_belongs_to_app_refuses_shell_leader() {
        // Even with a real, live pid, a shell-looking leader command is rejected
        // outright (the kill-the-terminal guard) before any cwd/argv inspection.
        assert!(!group_belongs_to_app(std::process::id(), "/bin/zsh -i", "/"));
    }

    #[test]
    fn detect_external_corroborates_by_cwd_then_rejects_wrong_root() {
        use std::net::{Ipv4Addr, TcpListener};
        // A likely-free port (drop the probe listener, then bind it in node).
        let port = {
            let l = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
            let p = l.local_addr().unwrap().port();
            drop(l);
            p
        };
        let dir = std::env::temp_dir();
        let dir_s = dir.to_string_lossy().trim_end_matches('/').to_string();

        // A grouped (setsid) node http server, cwd = temp dir, bound to `port`.
        // Spawn node DIRECTLY (no shell) so the JS needs no shell quoting and
        // node itself is the setsid group leader. Resolve node the way the app
        // does so nvm/Homebrew installs are found regardless of test PATH.
        let js = format!(
            "require('http').createServer((q,r)=>r.end('ok')).listen({port},()=>{{}})"
        );
        let node = crate::sysenv::resolve_bin("node")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "node".to_string());
        let mut c = std::process::Command::new(node);
        c.arg("-e").arg(&js).current_dir(&dir);
        unsafe {
            c.pre_exec(|| {
                nix::unistd::setsid()
                    .map(|_| ())
                    .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
            });
        }
        let child = c.spawn().expect("spawn node server");
        let pid = child.id();

        // Wait for it to listen (node startup).
        let mut listening = false;
        for _ in 0..60 {
            if port_listener_pid(port).is_some() {
                listening = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let good = if listening {
            detect_external("App", "web", port, &dir_s, None)
        } else {
            None
        };
        let bad = if listening {
            detect_external("App", "web", port, "/no/such/project/root", None)
        } else {
            None
        };

        // Cleanup before asserting so a failure never leaks the process.
        let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
        let mut waitable = child;
        let _ = waitable.wait();

        assert!(listening, "node test server never bound the port");
        let good = good.expect("cwd under root must corroborate");
        assert_eq!(good.pid, pid, "identity must be the group leader");
        assert!(good.foreign, "must be flagged foreign");
        assert_eq!(good.root.as_deref(), Some(dir_s.as_str()));
        assert!(bad.is_none(), "a non-matching root must NOT corroborate");
    }
}
