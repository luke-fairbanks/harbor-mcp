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
const KILL_VERIFY_GRACE: Duration = Duration::from_secs(2);
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
    /// Serializes lifecycle entry for each app so simultaneous GUI/MCP starts
    /// cannot both pass the initial running check and spawn duplicates.
    lifecycle_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Serializes cross-app port planning until chosen ports are recorded in
    /// `reserved`; per-app locks alone do not prevent two apps choosing 5173.
    allocation_lock: Arc<Mutex<()>>,
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

#[derive(Debug, Clone)]
struct StopIdentity {
    name: String,
    pid: u32,
    started_at: Option<String>,
    port: Option<u16>,
}

/// Generation token captured by a monitor when it is spawned. Monitor tasks
/// outlive individual run entries, so every delayed mutation must prove the
/// `(app, service)` slot still contains this exact process generation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MonitorIdentity {
    pid: Option<u32>,
    started_at: Option<String>,
    adopted: bool,
}

impl MonitorIdentity {
    fn matches(&self, service: &ServiceProc) -> bool {
        service.pid == self.pid
            && service.started_at == self.started_at
            && service.adopted == self.adopted
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopPolicy {
    /// The local UI may stop a process the person has explicitly seen and
    /// confirmed, including a corroborated externally-started service.
    AllowExternal,
    /// Remote/control-plane callers may stop only Harbor-spawned processes.
    /// The check is performed while holding the app lifecycle lock so adoption
    /// cannot race between an out-of-lock preflight and signalling.
    ManagedOnly,
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
                Some(lp) => lp == e.pid || ps_facts(lp).is_some_and(|g| g.pgid == e.pid),
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
        Some(f) => f.pgid == pid && started_at.is_none_or(|s| f.started_at == s),
        None => false,
    }
}

/// Outcome verification is deliberately separate from signal authorization.
/// An adopted process whose cwd no longer corroborates must not be signalled,
/// but it is still a survivor and therefore prevents Harbor from claiming Stop
/// succeeded or discarding its state.
fn stop_identity_alive(identity: &StopIdentity) -> bool {
    if let Some(facts) = ps_facts(identity.pid) {
        return facts.pgid == identity.pid
            && identity
                .started_at
                .as_deref()
                .is_none_or(|started| facts.started_at == started);
    }

    // A process-group leader can exit before a listening grandchild. Retain the
    // run conservatively while any member still reports the original pgid.
    if !group_argv_lines(identity.pid).is_empty() {
        return true;
    }
    // A child may daemonize into a new process group while retaining the port.
    // Harbor must report that the server survived, but must not signal the new
    // group without a fresh identity/corroboration pass.
    identity
        .port
        .is_some_and(|port| port_listener_pid(port).is_some())
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

/// Boundary-safe path corroboration inside an argv string. A raw substring is
/// unsafe here: project `/x/app` must not claim `/x/app-old` (and later signal
/// its process group). Descendants such as `/x/app/node_modules/vite` remain a
/// match, as do quoted paths and `--cwd=/x/app`-style arguments.
fn argv_mentions_path(argv: &str, path: &str) -> bool {
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return false;
    }

    let before_boundary = |byte: Option<u8>| {
        byte.is_none_or(|byte| {
            byte.is_ascii_whitespace()
                || matches!(byte, b'\'' | b'"' | b'=' | b':' | b',' | b';' | b'(')
        })
    };
    let after_boundary = |byte: Option<u8>| {
        byte.is_none_or(|byte| {
            byte.is_ascii_whitespace()
                || matches!(
                    byte,
                    b'/' | b'\\' | b'\'' | b'"' | b'=' | b':' | b',' | b';' | b')'
                )
        })
    };

    let mut from = 0;
    while let Some(relative) = argv[from..].find(path) {
        let start = from + relative;
        let end = start + path.len();
        let before = start
            .checked_sub(1)
            .and_then(|index| argv.as_bytes().get(index).copied());
        let after = argv.as_bytes().get(end).copied();
        if before_boundary(before) && after_boundary(after) {
            return true;
        }
        from = end;
    }
    false
}

/// The leader's argv names an interactive shell, terminal, IDE, or coding agent
/// host → refuse to adopt or signal. An externally-launched server may share
/// that host's process group; killing it would otherwise take the user's agent
/// or editor down too. Applied only on the foreign path.
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
    let blocked_base = matches!(
        base,
        "sh" | "bash"
            | "zsh"
            | "fish"
            | "dash"
            | "ksh"
            | "tcsh"
            | "csh"
            | "login"
            | "tmux"
            | "screen"
            | "sshd"
            | "mosh-server"
            | "codex"
            | "claude"
            | "cursor"
            | "electron"
            | "code"
            | "zed"
            | "warp"
            | "terminal"
            | "iterm2"
    );
    let lower = command.to_ascii_lowercase();
    blocked_base
        || lower.contains("claude-code")
        || lower.contains("visual studio code")
        || lower.contains("cursor.app")
        || lower.contains("codex.app")
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
        argv_mentions_path(argv, root)
            || canon
                .as_deref()
                .is_some_and(|canonical| argv_mentions_path(argv, canonical))
    })
}

/// Observation-only root corroboration for a listener. Unlike adoption this
/// does not require a safely signalable group leader. It exists so a server
/// sharing Codex/Claude/an IDE's process group still blocks a duplicate Harbor
/// launch, while remaining monitor-only in the Local servers view.
fn listener_belongs_to_app_observation(port: u16, root: &str) -> bool {
    let Some(listener_pid) = port_listener_pid(port) else {
        return false;
    };
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return false;
    }
    let canon = std::fs::canonicalize(root)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let matches_cwd =
        |pid| pid_cwd(pid).is_some_and(|cwd| path_under(&cwd, canon.as_deref().unwrap_or(root)));
    if matches_cwd(listener_pid) {
        return true;
    }
    let Some(facts) = ps_facts(listener_pid) else {
        return false;
    };
    if matches_cwd(facts.pgid) {
        return true;
    }
    group_argv_lines(facts.pgid).iter().any(|argv| {
        argv_mentions_path(argv, root)
            || canon
                .as_deref()
                .is_some_and(|canonical| argv_mentions_path(argv, canonical))
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

/// Treat `:`, `-`, and `_` as part of a command atom. In particular, this keeps
/// `npm run dev` from matching the different script `npm run dev:docs` while
/// still recognizing framework names inside paths such as `node_modules/vite/`.
fn command_boundary(byte: Option<u8>) -> bool {
    byte.is_none_or(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':')))
}

fn contains_boundary_phrase(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(relative) = haystack[from..].find(needle) {
        let start = from + relative;
        let end = start + needle.len();
        if command_boundary(
            start
                .checked_sub(1)
                .and_then(|i| haystack.as_bytes().get(i).copied()),
        ) && command_boundary(haystack.as_bytes().get(end).copied())
        {
            return true;
        }
        from = end;
    }
    false
}

fn normalized_command_token(token: &str) -> String {
    token
        .trim_matches(|c: char| matches!(c, '\'' | '"' | ';' | ',' | '(' | ')'))
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn contains_token_sequence(observed: &str, configured: &[String]) -> bool {
    if configured.is_empty() {
        return false;
    }
    observed.lines().any(|line| {
        let tokens: Vec<String> = line
            .split_whitespace()
            .map(normalized_command_token)
            .collect();
        tokens
            .windows(configured.len())
            .any(|window| window == configured)
    })
}

/// Higher-confidence command corroboration used only by adoption/control. The
/// inventory can remain suggestive, but Supervisor must not take ownership on a
/// loose substring such as `dev` inside `dev:docs`.
fn command_matches_for_adoption(configured: &str, observed: &str) -> bool {
    // Keep discovery's broad framework/entry-point vocabulary as the first
    // pass, then apply the stricter boundary checks below before ownership.
    if !crate::discovery::command_matches(configured, observed) {
        return false;
    }
    let configured_lower = configured.to_ascii_lowercase();
    let observed_lower = observed.to_ascii_lowercase();
    let configured_tokens: Vec<String> = configured_lower
        .split_whitespace()
        .take_while(|token| *token != "--")
        .filter(|token| !token.contains("${"))
        .map(normalized_command_token)
        .filter(|token| !token.is_empty())
        .collect();
    let command_core = configured_tokens.join(" ");

    if command_core.len() >= 5
        && (contains_boundary_phrase(&observed_lower, &command_core)
            || contains_token_sequence(&observed_lower, &configured_tokens))
    {
        return true;
    }

    let distinctive = [
        "vite",
        "next",
        "nuxt",
        "astro",
        "svelte",
        "remix",
        "webpack",
        "angular",
        "uvicorn",
        "fastapi",
        "flask",
        "django",
        "manage.py",
        "rails",
        "gatsby",
        "http.server",
    ];
    if distinctive.iter().any(|needle| {
        contains_boundary_phrase(&configured_lower, needle)
            && contains_boundary_phrase(&observed_lower, needle)
    }) {
        return true;
    }

    let configured_entries: HashSet<String> = configured_lower
        .split_whitespace()
        .map(normalized_command_token)
        .filter(|token| {
            token.len() > 3
                && [".js", ".ts", ".py", ".rb"]
                    .iter()
                    .any(|suffix| token.ends_with(suffix))
        })
        .collect();
    let observed_entries: HashSet<String> = observed_lower
        .split_whitespace()
        .map(normalized_command_token)
        .collect();
    !configured_entries.is_disjoint(&observed_entries)
}

fn service_log_ready_pattern(svc: &ServiceConfig) -> Option<String> {
    svc.ready_log_pattern
        .clone()
        .or_else(|| match svc.health_check.as_ref() {
            Some(HealthCheck::Log { pattern }) => Some(pattern.clone()),
            _ => None,
        })
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
    expected_command: &str,
    port: u16,
    root: &str,
    profile: Option<String>,
) -> Option<PersistedRun> {
    let (leader_pid, leader) = resolve_listener_leader(port)?;
    if !group_belongs_to_app(leader_pid, &leader.command, root) {
        return None;
    }
    let observed = format!(
        "{}\n{}",
        leader.command,
        group_argv_lines(leader_pid).join("\n")
    );
    if !command_matches_for_adoption(expected_command, &observed) {
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
            lifecycle_locks: Arc::new(Mutex::new(HashMap::new())),
            allocation_lock: Arc::new(Mutex::new(())),
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

    /// Acquire the same per-app lifecycle lock used by Start, Stop, discovery,
    /// and auto-restart. Registry mutation surfaces use this to make their
    /// `is_running` precondition atomic with the subsequent config write.
    pub async fn lock_lifecycle(&self, app_name: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lifecycle = {
            let mut locks = self.lifecycle_locks.lock().await;
            locks
                .entry(app_name.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        lifecycle.lock_owned().await
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

    /// Stable identities currently represented by Supervisor state, used by the
    /// machine-wide discovery view to distinguish observed from managed servers.
    pub async fn tracked_servers(&self) -> Vec<crate::discovery::TrackedServer> {
        let runs = self.runs.lock().await;
        let mut tracked = Vec::new();
        for (app, run) in runs.iter() {
            for service in run.services.values() {
                if !service.status.is_live() {
                    continue;
                }
                if let Some(leader_pid) = service.pid {
                    tracked.push(crate::discovery::TrackedServer {
                        app: app.clone(),
                        service: service.name.clone(),
                        leader_pid,
                        port: service.port,
                        external: service.external,
                    });
                }
            }
        }
        tracked
    }

    pub async fn owns_pid(&self, leader_pid: u32) -> bool {
        self.runs.lock().await.values().any(|run| {
            run.services
                .values()
                .any(|service| service.status.is_live() && service.pid == Some(leader_pid))
        })
    }

    /// Start an app under a profile. Idempotent: a no-op (returns current
    /// snapshot) if already running.
    pub async fn start(&self, cfg: &AppConfig, profile: &str) -> Result<AppRunSnapshot> {
        let _lifecycle_guard = self.lock_lifecycle(&cfg.name).await;
        match self.registry.read().await.get(&cfg.name) {
            Some(current) if current == cfg => {}
            Some(_) => {
                bail!(
                    "configuration for '{}' changed before Start acquired its lifecycle lock; retry with the latest config",
                    cfg.name
                );
            }
            None => {
                bail!(
                    "configuration for '{}' was removed before Start acquired its lifecycle lock",
                    cfg.name
                );
            }
        }
        let services = cfg.services_for_profile(profile);
        if services.is_empty() {
            bail!("profile '{}' selects no services", profile);
        }
        let ordered = ports::topo_sort(&services)?;

        // A launch-time discovery pass may have adopted only part of a profile.
        // Reuse those live services and continue starting the missing siblings;
        // return early only when the requested profile is fully represented.
        let (existing_live, existing_ports): (HashSet<String>, BTreeMap<String, u16>) = {
            let runs = self.runs.lock().await;
            let live: HashSet<String> = runs
                .get(&cfg.name)
                .map(|run| {
                    run.services
                        .values()
                        .filter(|service| service.status.is_live())
                        .map(|service| service.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            let ports = runs
                .get(&cfg.name)
                .map(|run| {
                    run.services
                        .values()
                        .filter(|service| service.status.is_live())
                        .filter_map(|service| service.port.map(|port| (service.name.clone(), port)))
                        .collect()
                })
                .unwrap_or_default();
            (live, ports)
        };
        if ordered
            .iter()
            .all(|service| existing_live.contains(&service.name))
        {
            for service in &ordered {
                self.await_ready(&cfg.name, &service.name).await?;
            }
            return self
                .snapshot(&cfg.name)
                .await
                .ok_or_else(|| anyhow!("already running but no snapshot"));
        }

        // Held only through discovery/allocation/reservation, not process spawn.
        let _allocation_guard = self.allocation_lock.lock().await;

        // Discover an externally-started copy on each configured/preferred port
        // BEFORE allocation. Without this pass, a relocatable service such as
        // Vite on 5173 looks merely "busy", gets bumped to 5174, and Harbor
        // creates exactly the duplicate it is meant to prevent.
        let reserved_snapshot = self.reserved.lock().await.clone();
        let root_for_scan = cfg.root.clone();
        let observed = tokio::task::spawn_blocking(move || {
            crate::discovery::listeners_for_root(&root_for_scan)
        })
        .await
        .unwrap_or_default();
        let mut external_recs: Vec<PersistedRun> = Vec::new();
        let mut external_claims: BTreeMap<String, u16> = existing_ports.clone();
        let mut claimed_groups: HashSet<u32> = HashSet::new();
        for svc in &ordered {
            if existing_live.contains(&svc.name) {
                continue;
            }
            let preferred = ports::discovery_port(svc);
            let exact: Vec<u16> = observed
                .iter()
                .filter(|listener| Some(listener.port) == preferred)
                .map(|listener| listener.port)
                .collect();
            let mut candidates: Vec<u16> = if exact.is_empty() {
                observed
                    .iter()
                    .filter(|listener| {
                        command_matches_for_adoption(&svc.command, &listener.command)
                    })
                    .map(|listener| listener.port)
                    .collect()
            } else {
                exact
            };
            candidates.sort_unstable();
            candidates.dedup();
            candidates.retain(|port| !reserved_snapshot.contains(port));

            if candidates.len() > 1 {
                bail!(
                    "found multiple already-running candidates for {}/{} on ports {}. Harbor \
                     will not guess or start another copy; open Local servers to choose which \
                     duplicate to keep.",
                    cfg.name,
                    svc.name,
                    candidates
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            let p = match candidates.first().copied() {
                Some(p) => p,
                None => {
                    let Some(p) = preferred else {
                        continue;
                    };
                    if reserved_snapshot.contains(&p) || ports::is_port_free(p) {
                        continue;
                    }
                    p
                }
            };
            if let Some(rec) = detect_external(
                &cfg.name,
                &svc.name,
                &svc.command,
                p,
                &cfg.root,
                Some(profile.to_string()),
            ) {
                // One process group may expose more than one socket. Never map
                // the same external group onto two configured service names.
                if claimed_groups.insert(rec.pid) {
                    external_claims.insert(svc.name.clone(), p);
                    external_recs.push(rec);
                }
            } else if listener_belongs_to_app_observation(p, &cfg.root) {
                bail!(
                    "{} already has a project-related listener on port {p}, but its command or \
                     process group does not safely match service '{}'. Harbor will not guess, \
                     take control, or start a duplicate. Open Local servers to inspect it, or \
                     stop it from the tool that launched it first.",
                    cfg.name,
                    svc.name
                );
            }
        }

        // Allocate everything else, preserving observed ports as explicit
        // claims so cross-service placeholders resolve to the existing server.
        let mut alloc =
            ports::allocate_with_claims(&ordered, &reserved_snapshot, &external_claims)?;
        for entry in &mut alloc.plan {
            if existing_live.contains(&entry.service) {
                entry.note = Some("already running — reused".to_string());
            }
        }

        // Preflight: a chosen port that's already bound would crash the child
        // with EADDRINUSE. But if an externally-started server is sitting on it
        // and corroborates as THIS app (port + project folder), adopt it instead
        // of spawning a duplicate. Only bail when the holder isn't this app. (Our
        // own already-running run was short-circuited by `is_running` above.)
        for (svc_name, p) in &alloc.ports {
            if external_claims.contains_key(svc_name) {
                continue; // already corroborated in the pre-allocation pass
            }
            if ports::is_port_free(*p) {
                continue;
            }
            let expected_command = ordered
                .iter()
                .find(|service| service.name == *svc_name)
                .map(|service| service.command.as_str())
                .unwrap_or("");
            if let Some(rec) = detect_external(
                &cfg.name,
                svc_name,
                expected_command,
                *p,
                &cfg.root,
                Some(profile.to_string()),
            ) {
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
        drop(_allocation_guard);

        // Create a run or extend a partially-adopted one. Never replace live
        // services discovered just before Start.
        {
            let mut runs = self.runs.lock().await;
            let run = runs.entry(cfg.name.clone()).or_insert_with(|| AppRun {
                profile: Some(profile.to_string()),
                port_plan: Vec::new(),
                services: BTreeMap::new(),
                intentional_stop: HashSet::new(),
            });
            run.profile = Some(profile.to_string());
            run.port_plan = alloc.plan.clone();
        }

        // Reflect any externally-started services as `external` adoptions; the
        // spawn loop below skips them (they're already running + reserved).
        let mut adopted_external: HashSet<String> = HashSet::new();
        for rec in external_recs {
            adopted_external.insert(rec.service.clone());
            self.adopt_external(cfg, Some(profile.to_string()), rec)
                .await;
        }

        let root = PathBuf::from(&cfg.root);
        for svc in &ordered {
            if existing_live.contains(&svc.name) {
                if let Err(error) = self.await_ready(&cfg.name, &svc.name).await {
                    self.release_unclaimed_reservations(&cfg.name, &alloc.ports)
                        .await;
                    return Err(error);
                }
                continue;
            }
            if adopted_external.contains(&svc.name) {
                continue; // already running outside Harbor — adopted, not respawned
            }
            let port = alloc.ports.get(&svc.name).copied();
            let (resolved_command, resolved_env) = ports::resolve_service(svc, &alloc.ports);

            match self
                .spawn_service(
                    cfg,
                    svc,
                    &root,
                    profile,
                    port,
                    &resolved_command,
                    &resolved_env,
                    0,
                )
                .await
            {
                Ok(()) => {}
                Err(e) => {
                    self.system_log(&cfg.name, &svc.name, &format!("failed to start: {e}"))
                        .await;
                    self.set_status(&cfg.name, &svc.name, ServiceStatus::Exited, Some(-1))
                        .await;
                    self.release_unclaimed_reservations(&cfg.name, &alloc.ports)
                        .await;
                    bail!("service '{}' failed to start: {e}", svc.name);
                }
            }

            // Gate dependents: wait for this service to become ready before the
            // next (topo order guarantees deps precede dependents).
            if let Err(error) = self.await_ready(&cfg.name, &svc.name).await {
                self.release_unclaimed_reservations(&cfg.name, &alloc.ports)
                    .await;
                return Err(error);
            }
        }

        match self.snapshot(&cfg.name).await {
            Some(snapshot) => Ok(snapshot),
            None => {
                self.release_unclaimed_reservations(&cfg.name, &alloc.ports)
                    .await;
                Err(anyhow!("run vanished after start"))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
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

        let mut child = cmd.spawn().with_context_path(&cwd, resolved_command)?;
        let pid = child.id();

        // Capture the kernel start-time now (for PID-reuse-safe adoption) and
        // persist the run so a relaunched Harbor can re-adopt it.
        let started_at = pid.and_then(ps_facts).map(|f| f.started_at);
        let monitor_identity = MonitorIdentity {
            pid,
            started_at: started_at.clone(),
            adopted: false,
        };
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
                    started_at: started_at.clone(),
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
        self.emit_status(
            &cfg.name,
            svc.name.clone(),
            ServiceStatus::Starting,
            port,
            pid,
            None,
        );
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
        let ready_pattern = service_log_ready_pattern(svc);
        let has_log_readiness = ready_pattern.is_some();
        if let Some(out) = child.stdout.take() {
            self.spawn_reader(
                cfg.name.clone(),
                svc.name.clone(),
                "stdout",
                out,
                ready_pattern.clone(),
                monitor_identity.clone(),
            );
        }
        if let Some(err) = child.stderr.take() {
            self.spawn_reader(
                cfg.name.clone(),
                svc.name.clone(),
                "stderr",
                err,
                ready_pattern,
                monitor_identity.clone(),
            );
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
            let identity = monitor_identity.clone();
            tokio::spawn(async move {
                let status = child.wait().await;
                let code = status.ok().and_then(|s| s.code());

                // Update status immediately so a Start currently awaiting
                // readiness can fail fast. The identity gate prevents an old
                // monitor from touching a replacement in the same slot.
                let cleanup = mark_exited_if_current(
                    &app, &runs, &app_name, &svc_name, &identity, code, true,
                )
                .await;
                let Some(cleanup) = cleanup else {
                    return;
                };

                // Persistence and reservation cleanup must serialize with a new
                // Start/Restart. Acquire only AFTER publishing Exited; acquiring
                // before it would deadlock a Start that holds the lifecycle lock
                // while awaiting this very status transition.
                let supervisor = me.upgrade();
                let lifecycle_guard = match supervisor.as_ref() {
                    Some(supervisor) => Some(supervisor.lock_lifecycle(&app_name).await),
                    None => None,
                };
                let still_current =
                    monitor_identity_is_current(&runs, &app_name, &svc_name, &identity).await;
                if still_current {
                    let _ = store.remove_run(&app_name, &svc_name);
                }
                if let Some(port) = cleanup.port {
                    release_port_if_unclaimed(&runs, &reserved, port).await;
                }
                drop(lifecycle_guard);

                if !cleanup.intended && still_current {
                    if let Some(supervisor) = supervisor {
                        // `on_unexpected_exit` returns a boxed `Send` future,
                        // crossing a type-erased boundary that breaks the Send
                        // auto-trait inference cycle from the async recursion
                        // (spawn_service → monitor → on_unexpected_exit → …).
                        supervisor
                            .on_unexpected_exit(&app_name, &svc_name, code)
                            .await;
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
                let identity = monitor_identity.clone();
                tokio::spawn(async move {
                    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
                    loop {
                        if tokio::time::Instant::now() >= deadline {
                            break;
                        }
                        // Stop polling if the service already left `starting`.
                        if !is_starting_current(&runs, &app_name, &svc_name, &identity).await {
                            break;
                        }
                        if health::check_once(&hc, port).await {
                            mark_ready_if_starting_current(
                                &app, &runs, &app_name, &svc_name, &identity,
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
        if !has_log_readiness && !has_probe {
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
        identity: MonitorIdentity,
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
                if !push_log_if_current(&runs, &app_name, &svc_name, &identity, log.clone()).await {
                    break;
                }
                let _ = app.emit(LOG_EVENT, &log);

                // readyLogPattern: flip starting → ready on a substring match.
                if let Some(pat) = &ready_pattern {
                    if !pat.is_empty() && line.contains(pat.as_str()) {
                        mark_ready_if_starting_current(
                            &app, &runs, &app_name, &svc_name, &identity,
                        )
                        .await;
                    }
                }
            }
        });
    }

    /// Stop every service in an app. This is the local, explicitly-confirmed UI
    /// path and may include a corroborated process started outside Harbor.
    pub async fn stop(&self, app_name: &str) -> Result<()> {
        self.stop_with_policy(app_name, StopPolicy::AllowExternal)
            .await
    }

    /// Stop only when every live service was spawned by Harbor. MCP and other
    /// remote callers use this policy so an observation/adoption cannot silently
    /// expand their authority to terminate a process started by another tool.
    pub async fn stop_managed_only(&self, app_name: &str) -> Result<()> {
        self.stop_with_policy(app_name, StopPolicy::ManagedOnly)
            .await
    }

    async fn stop_with_policy(&self, app_name: &str, policy: StopPolicy) -> Result<()> {
        let _lifecycle_guard = self.lock_lifecycle(app_name).await;

        self.stop_locked(app_name, policy).await
    }

    /// Stop implementation. The app lifecycle lock must be held by the caller
    /// across the policy check, signalling, verification, and state cleanup.
    async fn stop_locked(&self, app_name: &str, policy: StopPolicy) -> Result<()> {
        let identities: Vec<StopIdentity> = {
            let mut runs = self.runs.lock().await;
            let Some(run) = runs.get_mut(app_name) else {
                drop(runs);
                self.store.remove_app_runs(app_name)?;
                return Ok(());
            };

            if policy == StopPolicy::ManagedOnly
                && run
                    .services
                    .values()
                    .any(|service| service.status.is_live() && service.external)
            {
                bail!(
                    "external confirmation required: this app includes a process Harbor did not \
                     launch; stop it from the Harbor UI so the process-group impact is visible"
                );
            }

            let identities: Vec<StopIdentity> = run
                .services
                .values()
                .filter(|service| service.status.is_live())
                .filter_map(|service| {
                    service.pid.map(|pid| StopIdentity {
                        name: service.name.clone(),
                        pid,
                        started_at: service.started_at.clone(),
                        port: service.port,
                    })
                })
                .collect();

            // Latch intent before any signal. Monitors consume their own marker
            // when they reap; survivors have the marker removed before an error
            // is returned so a later, unrelated crash is not misclassified.
            for service in run
                .services
                .values()
                .filter(|service| service.status.is_live())
            {
                run.intentional_stop.insert(service.name.clone());
            }
            identities
        };

        let mut signal_errors = Vec::new();
        let targets = self.live_signal_targets(app_name).await;
        for (name, pid) in &targets {
            self.system_log(app_name, name, "SIGTERM → process group")
                .await;
            if let Err(error) = killpg(Pid::from_raw(*pid as i32), Signal::SIGTERM) {
                let message = format!("SIGTERM {name} (pid {pid}): {error}");
                self.system_log(app_name, name, &message).await;
                signal_errors.push(message);
            }
        }

        let mut survivors = self.wait_for_stop_identities(&identities, STOP_GRACE).await;
        if !survivors.is_empty() {
            // Re-resolve signal-safe targets immediately before escalation. A
            // stale/reused or no-longer-corroborated adopted pid is retained as a
            // survivor but never receives SIGKILL.
            let still_signalable = self.live_signal_targets(app_name).await;
            for (name, pid) in &still_signalable {
                self.system_log(app_name, name, "SIGKILL → process group")
                    .await;
                if let Err(error) = killpg(Pid::from_raw(*pid as i32), Signal::SIGKILL) {
                    let message = format!("SIGKILL {name} (pid {pid}): {error}");
                    self.system_log(app_name, name, &message).await;
                    signal_errors.push(message);
                }
            }
            survivors = self
                .wait_for_stop_identities(&identities, KILL_VERIFY_GRACE)
                .await;
        }

        if !survivors.is_empty() {
            {
                let mut runs = self.runs.lock().await;
                if let Some(run) = runs.get_mut(app_name) {
                    run.intentional_stop.clear();
                }
            }
            let detail = survivors
                .iter()
                .map(|identity| format!("{} (pid {})", identity.name, identity.pid))
                .collect::<Vec<_>>()
                .join(", ");
            let signal_detail = if signal_errors.is_empty() {
                String::new()
            } else {
                format!(" Signal errors: {}.", signal_errors.join("; "))
            };
            bail!(
                "Harbor could not stop every process for '{app_name}'; still running: {detail}.{signal_detail} State was retained."
            );
        }

        // Only now—after every original process-group identity is gone—drop the
        // run, persistence, and every allocated port. Monitor cleanup is
        // idempotent and may race this path after reaping a child.
        let mut ports_to_release: HashSet<u16> = identities
            .iter()
            .filter_map(|identity| identity.port)
            .collect();
        {
            let mut runs = self.runs.lock().await;
            if let Some(run) = runs.remove(app_name) {
                ports_to_release.extend(run.services.values().filter_map(|service| service.port));
            }
        }
        if !ports_to_release.is_empty() {
            let mut reserved = self.reserved.lock().await;
            for port in ports_to_release {
                reserved.remove(&port);
            }
        }
        self.store.remove_app_runs(app_name)?;
        Ok(())
    }

    /// Live services to signal, filtering out every entry whose leader identity
    /// is no longer provably the captured generation. Managed Child monitors can
    /// briefly lag behind `wait()`/reaping too, so PID-reuse safety is not only an
    /// adoption concern.
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
                            (
                                s.name.clone(),
                                p,
                                s.adopted,
                                s.started_at.clone(),
                                s.root.clone(),
                            )
                        })
                    })
                    .collect(),
                None => return vec![],
            }
        };
        candidates
            .into_iter()
            .filter(|(_, pid, adopted, started, root)| {
                if *adopted {
                    adopted_signal_ok(*pid, started.as_deref(), root.as_deref())
                } else {
                    safe_to_signal(*pid, started.as_deref())
                }
            })
            .map(|(name, pid, _, _, _)| (name, pid))
            .collect()
    }

    async fn surviving_stop_identities(&self, identities: &[StopIdentity]) -> Vec<StopIdentity> {
        if identities.is_empty() {
            return Vec::new();
        }
        let candidates = identities.to_vec();
        let fallback = candidates.clone();
        tokio::task::spawn_blocking(move || {
            candidates.into_iter().filter(stop_identity_alive).collect()
        })
        .await
        // A failed verification task is not evidence of process death.
        .unwrap_or(fallback)
    }

    async fn wait_for_stop_identities(
        &self,
        identities: &[StopIdentity],
        timeout: Duration,
    ) -> Vec<StopIdentity> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let survivors = self.surviving_stop_identities(identities).await;
            if survivors.is_empty() || tokio::time::Instant::now() >= deadline {
                return survivors;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
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
                            MonitorIdentity {
                                pid: s.pid,
                                started_at: s.started_at.clone(),
                                adopted: s.adopted,
                            },
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
            let Some((identity, external, status, port, cmd, env, count, ready_since, profile)) =
                snap
            else {
                return; // run gone (a Stop won the race) ⇒ nothing to do
            };
            // Belt-and-suspenders: adopted/external have no monitor and can't reach
            // here, but never restart something Harbor didn't launch.
            if identity.adopted || external || status != ServiceStatus::Exited {
                return;
            }

            // Read possibly-edited live config for the opt-in + service definition.
            let cfg = self.registry.read().await.get(app).cloned();
            let Some(cfg) = cfg else { return };
            if !cfg.trusted {
                self.system_log(
                    app,
                    svc,
                    "auto-restart cancelled — the current config requires approval",
                )
                .await;
                return;
            }
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

            // Serialize the post-backoff decision, reservation, and spawn with
            // Start/Stop/discovery. Without this lock a Stop could snapshot the
            // old Exited service, then remove all state while this task spawned a
            // replacement that was absent from Stop's static identity set.
            let _lifecycle_guard = self.lock_lifecycle(app).await;

            // Re-validate after both the backoff and lock wait: shutdown / a Stop /
            // a manual Start may have won while this task was suspended. Only
            // proceed if the service is still the same Exited generation.
            if self.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            let config_is_current = self
                .registry
                .read()
                .await
                .get(app)
                .is_some_and(|current| current == &cfg);
            if !config_is_current {
                self.system_log(
                    app,
                    svc,
                    "auto-restart cancelled — configuration changed during backoff",
                )
                .await;
                return;
            }
            {
                let runs = self.runs.lock().await;
                match runs.get(app).and_then(|r| r.services.get(svc)) {
                    Some(s)
                        if s.status == ServiceStatus::Exited
                            && identity.matches(s)
                            && !s.external => {}
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
            if m.get(&key)
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
                format!(
                    "Gave up after {RESTART_WINDOW_MAX} restart attempts. Click to open Harbor."
                ),
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
        let file = match self.store.load_runs() {
            Ok(file) => file,
            Err(error) => {
                eprintln!("[harbor] could not read saved run identities: {error}");
                return;
            }
        };
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

    /// Probe every configured service (across all profiles) on its pinned or
    /// preferred port and reflect any externally-started server that
    /// corroborates as this app. Already-tracked service names are skipped, but
    /// one running service no longer prevents discovery of its siblings.
    pub async fn reflect_external_if_idle(&self, cfg: &AppConfig) {
        if !cfg.trusted {
            return;
        }
        let _lifecycle_guard = self.lock_lifecycle(&cfg.name).await;
        let config_is_current = self
            .registry
            .read()
            .await
            .get(&cfg.name)
            .is_some_and(|current| current == cfg);
        if !config_is_current {
            return;
        }

        for svc in &cfg.services {
            let already_tracked = self
                .runs
                .lock()
                .await
                .get(&cfg.name)
                .and_then(|r| r.services.get(&svc.name))
                .map(|s| s.status.is_live())
                .unwrap_or(false);
            if already_tracked {
                continue;
            }
            let Some(port) = ports::discovery_port(svc) else {
                continue;
            };
            if ports::is_port_free(port) {
                continue; // nothing listening — no external process to reflect
            }
            let app = cfg.name.clone();
            let svc_name = svc.name.clone();
            let command = svc.command.clone();
            let root = cfg.root.clone();
            let rec = tokio::task::spawn_blocking(move || {
                detect_external(&app, &svc_name, &command, port, &root, None)
            })
            .await
            .ok()
            .flatten();
            if let Some(rec) = rec {
                if self.owns_pid(rec.pid).await {
                    continue;
                }
                self.adopt_external(cfg, None, rec).await;
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
        let me = self.me.clone();
        let identity = MonitorIdentity {
            pid: Some(e.pid),
            started_at: Some(e.started_at.clone()),
            adopted: true,
        };
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(4)).await;
                let live = {
                    let r = runs.lock().await;
                    r.get(&e.app)
                        .and_then(|a| a.services.get(&e.service))
                        .is_some_and(|service| {
                            service.status.is_live() && identity.matches(service)
                        })
                };
                if !live {
                    break; // stopped, removed, exited, or replaced generation
                }
                let entry = e.clone();
                let alive = tokio::task::spawn_blocking(move || still_ours(&entry))
                    .await
                    .unwrap_or(false);
                if !alive {
                    let cleanup = mark_exited_if_current(
                        &app, &runs, &e.app, &e.service, &identity, None, false,
                    )
                    .await;
                    let Some(cleanup) = cleanup else {
                        break;
                    };

                    // A Stop/Start may replace this slot between the liveness
                    // probe and cleanup. Serialize persistence deletion with
                    // lifecycle operations, then prove the old identity again.
                    let supervisor = me.upgrade();
                    let lifecycle_guard = match supervisor.as_ref() {
                        Some(supervisor) => Some(supervisor.lock_lifecycle(&e.app).await),
                        None => None,
                    };
                    if monitor_identity_is_current(&runs, &e.app, &e.service, &identity).await {
                        let _ = store.remove_run(&e.app, &e.service);
                    }
                    if let Some(port) = cleanup.port {
                        release_port_if_unclaimed(&runs, &reserved, port).await;
                    }
                    drop(lifecycle_guard);
                    break;
                }
            }
        });
    }

    // ---- small helpers ----------------------------------------------------

    /// Release ports reserved for this start attempt that no live service ended
    /// up claiming. Ports for already-ready, newly-ready, or unhealthy-but-live
    /// processes remain reserved; failed and not-yet-attempted services do not
    /// poison future allocations after Start returns an error.
    async fn release_unclaimed_reservations(
        &self,
        app_name: &str,
        allocated: &BTreeMap<String, u16>,
    ) {
        let claimed: HashSet<u16> = {
            let runs = self.runs.lock().await;
            runs.get(app_name)
                .map(|run| {
                    run.services
                        .values()
                        .filter(|service| service.status.is_live())
                        .filter_map(|service| service.port)
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut reserved = self.reserved.lock().await;
        for port in allocated.values() {
            if !claimed.contains(port) {
                reserved.remove(port);
            }
        }
    }

    async fn await_ready(&self, app_name: &str, svc_name: &str) -> Result<()> {
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        loop {
            let state = {
                let runs = self.runs.lock().await;
                runs.get(app_name)
                    .and_then(|r| r.services.get(svc_name))
                    .map(|s| (s.status, s.exit_code))
            };
            match state {
                Some((ServiceStatus::Ready, _)) => return Ok(()),
                Some((ServiceStatus::Starting, _)) => {}
                Some((ServiceStatus::Unhealthy, _)) => {
                    bail!("service '{svc_name}' is unhealthy; refusing to start its dependents");
                }
                Some((ServiceStatus::Exited, code)) => {
                    let detail = code
                        .map(|code| format!("exit {code}"))
                        .unwrap_or_else(|| "signal or unknown exit".to_string());
                    bail!(
                        "service '{svc_name}' exited before becoming ready ({detail}); refusing to start its dependents"
                    );
                }
                Some((ServiceStatus::Stopped, _)) => {
                    bail!(
                        "service '{svc_name}' stopped before becoming ready; refusing to start its dependents"
                    );
                }
                None => {
                    bail!(
                        "service '{svc_name}' disappeared before becoming ready; refusing to start its dependents"
                    );
                }
            }
            if tokio::time::Instant::now() >= deadline {
                if self.mark_unhealthy_if_starting(app_name, svc_name).await {
                    self.system_log(app_name, svc_name, "readiness timeout → marking unhealthy")
                        .await;
                    bail!(
                        "service '{svc_name}' did not become ready within {}s; refusing to start its dependents",
                        READY_TIMEOUT.as_secs()
                    );
                }
                // A health/log task won the race at the deadline. Re-read the
                // resulting state instead of overwriting Ready with Unhealthy.
                continue;
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

    /// Atomically marks a timed-out service unhealthy only while it is still
    /// starting. Readiness checks run independently and may publish Ready at
    /// the exact deadline; that successful transition must win the race.
    async fn mark_unhealthy_if_starting(&self, app_name: &str, svc_name: &str) -> bool {
        let event = {
            let mut runs = self.runs.lock().await;
            let Some(service) = runs
                .get_mut(app_name)
                .and_then(|run| run.services.get_mut(svc_name))
            else {
                return false;
            };
            if service.status != ServiceStatus::Starting {
                return false;
            }
            service.status = ServiceStatus::Unhealthy;
            (service.port, service.pid)
        };
        self.emit_status(
            app_name,
            svc_name.to_string(),
            ServiceStatus::Unhealthy,
            event.0,
            event.1,
            None,
        );
        true
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

async fn is_starting_current(
    runs: &Runs,
    app_name: &str,
    svc_name: &str,
    identity: &MonitorIdentity,
) -> bool {
    let runs = runs.lock().await;
    runs.get(app_name)
        .and_then(|r| r.services.get(svc_name))
        .is_some_and(|service| {
            service.status == ServiceStatus::Starting && identity.matches(service)
        })
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

async fn push_log_if_current(
    runs: &Runs,
    app_name: &str,
    svc_name: &str,
    identity: &MonitorIdentity,
    log: LogLine,
) -> bool {
    let mut runs = runs.lock().await;
    let Some(service) = runs
        .get_mut(app_name)
        .and_then(|run| run.services.get_mut(svc_name))
    else {
        return false;
    };
    if !identity.matches(service) {
        return false;
    }
    service.logs.push_back(log);
    while service.logs.len() > RING_CAP {
        service.logs.pop_front();
    }
    true
}

async fn mark_ready_if_starting_current(
    app: &AppHandle,
    runs: &Runs,
    app_name: &str,
    svc_name: &str,
    identity: &MonitorIdentity,
) -> bool {
    let (port, pid) = {
        let mut runs = runs.lock().await;
        let Some(service) = runs
            .get_mut(app_name)
            .and_then(|run| run.services.get_mut(svc_name))
        else {
            return false;
        };
        if service.status != ServiceStatus::Starting || !identity.matches(service) {
            return false;
        }
        service.status = ServiceStatus::Ready;
        if service.ready_since.is_none() {
            service.ready_since = Some(Instant::now());
        }
        (service.port, service.pid)
    };
    let _ = app.emit(
        STATUS_EVENT,
        StatusEvent {
            app: app_name.to_string(),
            service: svc_name.to_string(),
            status: ServiceStatus::Ready,
            port,
            pid,
            exit_code: None,
        },
    );
    true
}

struct IdentityExit {
    intended: bool,
    port: Option<u16>,
}

async fn monitor_identity_is_current(
    runs: &Runs,
    app_name: &str,
    svc_name: &str,
    identity: &MonitorIdentity,
) -> bool {
    runs.lock()
        .await
        .get(app_name)
        .and_then(|run| run.services.get(svc_name))
        .is_some_and(|service| identity.matches(service))
}

/// Reservations are global and ownerless, so an old monitor may release its
/// captured port only after proving no replacement/live service now claims it.
async fn release_port_if_unclaimed(runs: &Runs, reserved: &Reserved, port: u16) {
    let claimed = runs.lock().await.values().any(|run| {
        run.services
            .values()
            .any(|service| service.status.is_live() && service.port == Some(port))
    });
    if !claimed {
        reserved.lock().await.remove(&port);
    }
}

/// Transition only the process generation a monitor was created for. Start,
/// Restart, and external adoption may reuse the same `(app, service)` key while
/// an older monitor is still scheduled; touching the replacement would orphan
/// its live process and delete its persistence record.
async fn mark_exited_if_current(
    app: &AppHandle,
    runs: &Runs,
    app_name: &str,
    svc_name: &str,
    identity: &MonitorIdentity,
    exit_code: Option<i32>,
    consume_intent: bool,
) -> Option<IdentityExit> {
    let (intended, port, pid) = {
        let mut runs = runs.lock().await;
        let run = runs.get_mut(app_name)?;
        let is_current = run
            .services
            .get(svc_name)
            .is_some_and(|service| service.status.is_live() && identity.matches(service));
        if !is_current {
            return None;
        }
        let intended = consume_intent && run.intentional_stop.remove(svc_name);
        let service = run
            .services
            .get_mut(svc_name)
            .expect("identity checked above");
        service.status = ServiceStatus::Exited;
        service.exit_code = exit_code;
        (intended, service.port, service.pid)
    };
    let _ = app.emit(
        STATUS_EVENT,
        StatusEvent {
            app: app_name.to_string(),
            service: svc_name.to_string(),
            status: ServiceStatus::Exited,
            port,
            pid,
            exit_code,
        },
    );
    Some(IdentityExit { intended, port })
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
        assert_eq!(
            a.started_at, b.started_at,
            "lstart must be stable per process"
        );
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
        let stop_identity = StopIdentity {
            name: "s".into(),
            pid,
            started_at: Some(facts.started_at.clone()),
            port: None,
        };
        assert!(
            stop_identity_alive(&stop_identity),
            "stop verification should see the live process group"
        );

        // A start-time mismatch (PID-reuse simulation) must fail the gate.
        let mut reused = rec.clone();
        reused.started_at = "Mon Jan  1 00:00:00 2001".into();
        assert!(!still_ours(&reused), "different start-time → not ours");

        // Tear the group down; the gate must then report it gone.
        let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
        let mut waitable = child;
        let _ = waitable.wait();
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            !still_ours(&rec),
            "after kill the process is no longer ours"
        );
        assert!(
            !stop_identity_alive(&stop_identity),
            "stop verification should see the process group disappear"
        );
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
        assert!(
            total < 60,
            "a crash loop should give up in < 60s, got {total}s"
        );
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

        assert!(argv_mentions_path(
            "node /Users/x/opal/server.js --port 3000",
            "/Users/x/opal"
        ));
        assert!(argv_mentions_path(
            "node --cwd='/Users/x/opal' server.js",
            "/Users/x/opal"
        ));
        assert!(!argv_mentions_path(
            "node /Users/x/opal-sandbox/server.js",
            "/Users/x/opal"
        ));
        assert!(!argv_mentions_path(
            "node /prefix/Users/x/opal/server.js",
            "/Users/x/opal"
        ));
    }

    #[test]
    fn monitor_identity_rejects_replacement_generation() {
        let identity = MonitorIdentity {
            pid: Some(101),
            started_at: Some("Mon Jun 29 14:23:01 2026".into()),
            adopted: true,
        };
        let mut service = ServiceProc {
            name: "web".into(),
            status: ServiceStatus::Ready,
            pid: Some(101),
            port: Some(5173),
            resolved_command: Some("vite".into()),
            exit_code: None,
            logs: VecDeque::new(),
            started_at: identity.started_at.clone(),
            adopted: true,
            external: true,
            root: Some("/tmp/app".into()),
            resolved_env: None,
            restart_count: 0,
            ready_since: None,
            cpu: None,
            mem_bytes: None,
        };
        assert!(identity.matches(&service));
        service.pid = Some(202);
        assert!(!identity.matches(&service));
        service.pid = Some(101);
        service.started_at = Some("Mon Jun 29 14:24:01 2026".into());
        assert!(!identity.matches(&service));
        service.started_at = identity.started_at.clone();
        service.adopted = false;
        assert!(!identity.matches(&service));
    }

    #[test]
    fn adoption_command_matching_respects_script_boundaries() {
        assert!(command_matches_for_adoption(
            "npm run dev",
            "/opt/homebrew/bin/npm run dev -- --port 5173"
        ));
        assert!(!command_matches_for_adoption(
            "npm run dev",
            "/opt/homebrew/bin/npm run dev:docs -- --port 5174"
        ));
        assert!(command_matches_for_adoption(
            "vite --port ${PORT}",
            "node /project/node_modules/vite/bin/vite.js --port 5173"
        ));
        assert!(command_matches_for_adoption(
            "node server.js",
            "/opt/homebrew/bin/node /project/server.js"
        ));
        assert!(!command_matches_for_adoption(
            "next dev",
            "node /project/nextcloud/dev.js"
        ));
    }

    #[test]
    fn log_health_check_supplies_reader_pattern_with_explicit_override() {
        let mut svc = ServiceConfig {
            name: "web".into(),
            cwd: ".".into(),
            command: "npm run dev".into(),
            port: Some(5173),
            env: BTreeMap::new(),
            depends_on: vec![],
            health_check: Some(HealthCheck::Log {
                pattern: "listening".into(),
            }),
            ready_log_pattern: None,
        };
        assert_eq!(
            service_log_ready_pattern(&svc).as_deref(),
            Some("listening")
        );
        svc.ready_log_pattern = Some("explicit ready".into());
        assert_eq!(
            service_log_ready_pattern(&svc).as_deref(),
            Some("explicit ready")
        );
    }

    #[test]
    fn leader_is_shell_catches_terminals() {
        for s in [
            "zsh",
            "-zsh",
            "/bin/bash",
            "/usr/bin/fish -i",
            "login -pf x",
            "tmux",
            "sshd: u",
            "codex app-server",
            "/Applications/Cursor.app/Contents/MacOS/Cursor",
            "node /x/@anthropic-ai/claude-code/cli.js",
        ] {
            assert!(leader_is_shell(s), "{s:?} should read as a shell/login");
        }
        for s in [
            "node /x/yarn.js run dev",
            "next dev -p 3002",
            "/usr/bin/python3 -m http.server",
        ] {
            assert!(!leader_is_shell(s), "{s:?} should NOT read as a shell");
        }
    }

    #[test]
    fn group_belongs_to_app_refuses_shell_leader() {
        // Even with a real, live pid, a shell-looking leader command is rejected
        // outright (the kill-the-terminal guard) before any cwd/argv inspection.
        assert!(!group_belongs_to_app(
            std::process::id(),
            "/bin/zsh -i",
            "/"
        ));
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
        let js =
            format!("require('http').createServer((q,r)=>r.end('ok')).listen({port},()=>{{}})");
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
            detect_external("App", "web", "node -e", port, &dir_s, None)
        } else {
            None
        };
        let bad = if listening {
            detect_external("App", "web", "node -e", port, "/no/such/project/root", None)
        } else {
            None
        };
        let observed_good = listening && listener_belongs_to_app_observation(port, &dir_s);
        let observed_bad =
            listening && listener_belongs_to_app_observation(port, "/no/such/project/root");

        // Cleanup before asserting so a failure never leaks the process.
        let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
        let mut waitable = child;
        let _ = waitable.wait();

        assert!(listening, "node test server never bound the port");
        assert!(
            observed_good,
            "observation should corroborate the listener cwd"
        );
        assert!(
            !observed_bad,
            "observation must reject the wrong project root"
        );
        let good = good.expect("cwd under root must corroborate");
        assert_eq!(good.pid, pid, "identity must be the group leader");
        assert!(good.foreign, "must be flagged foreign");
        assert_eq!(good.root.as_deref(), Some(dir_s.as_str()));
        assert!(bad.is_none(), "a non-matching root must NOT corroborate");
    }
}
