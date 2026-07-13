//! User-wide local TCP listener discovery.
//!
//! This module observes first and owns nothing. It takes one `lsof` snapshot,
//! enriches listeners with stock `ps`/cwd data, matches them to registered app
//! roots, and performs a bounded localhost HTTP probe for useful human labels.
//! Unknown processes are never persisted or adopted. Cleanup is available only
//! for an isolated process group whose PID + start-time identity is rechecked.

use crate::model::{AppConfig, LocalServer, LocalServerInventory};
use crate::ports;
use anyhow::{anyhow, bail, Result};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone)]
pub struct TrackedServer {
    pub app: String,
    pub service: String,
    pub leader_pid: u32,
    pub port: Option<u16>,
    pub external: bool,
}

#[derive(Debug, Clone)]
pub struct RootListener {
    pub port: u16,
    pub command: String,
}

#[derive(Debug, Clone)]
struct ProcFacts {
    pgid: u32,
    started_at: String,
    command: String,
}

#[derive(Debug, Clone)]
struct RawListener {
    pid: u32,
    port: u16,
    process: String,
    addresses: Vec<String>,
}

#[derive(Debug, Clone)]
struct HttpProbe {
    status: Option<u16>,
    title: Option<String>,
    server: Option<String>,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn ps_facts(pid: u32) -> Option<ProcFacts> {
    let out = Command::new("ps")
        .args(["-o", "pid=,pgid=,lstart=,command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let toks: Vec<&str> = raw.lines().next()?.split_whitespace().collect();
    if toks.len() < 8 {
        return None;
    }
    Some(ProcFacts {
        pgid: toks[1].parse().ok()?,
        started_at: toks[2..7].join(" "),
        command: toks[7..].join(" "),
    })
}

fn pid_cwd(pid: u32) -> Option<String> {
    for prog in ["lsof", "/usr/sbin/lsof"] {
        let Ok(out) = Command::new(prog)
            .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
            .output()
        else {
            continue;
        };
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(path) = line.strip_prefix('n') {
                if !path.trim().is_empty() {
                    return Some(path.trim().to_string());
                }
            }
        }
        return None;
    }
    None
}

fn group_argv_lines(pgid: u32) -> Vec<String> {
    let Ok(out) = Command::new("ps")
        .args(["-g", &pgid.to_string(), "-o", "command="])
        .output()
    else {
        return vec![];
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn group_has_live_members(pgid: u32) -> bool {
    let Ok(out) = Command::new("ps")
        .args(["-g", &pgid.to_string(), "-o", "stat="])
        .output()
    else {
        return true;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .any(|status| !status.is_empty() && !status.starts_with('Z'))
}

fn command_basename(command: &str) -> &str {
    command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_start_matches('-')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .trim_end_matches(':')
}

fn unsafe_group_host(command: &str) -> bool {
    let base = command_basename(command).to_ascii_lowercase();
    if matches!(
        base.as_str(),
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
    ) {
        return true;
    }
    let lower = command.to_ascii_lowercase();
    lower.contains("claude-code")
        || lower.contains("visual studio code")
        || lower.contains("cursor.app")
        || lower.contains("codex.app")
}

fn path_under(path: &str, root: &str) -> bool {
    let path = path.trim_end_matches('/');
    let root = root.trim_end_matches('/');
    !root.is_empty() && (path == root || path.starts_with(&format!("{root}/")))
}

fn canonical_string(path: &str) -> String {
    std::fs::canonicalize(path)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.trim_end_matches('/').to_string())
}

fn project_marker(path: &Path) -> bool {
    [
        "harbor.json",
        "package.json",
        "pyproject.toml",
        "requirements.txt",
        "Cargo.toml",
        "go.mod",
        "Gemfile",
        "composer.json",
        "docker-compose.yml",
        "compose.yml",
        ".git",
    ]
    .iter()
    .any(|name| path.join(name).exists())
}

fn infer_project_root(cwd: &str) -> Option<String> {
    let mut current = PathBuf::from(cwd);
    loop {
        if project_marker(&current) {
            return Some(current.to_string_lossy().into_owned());
        }
        if !current.pop() {
            return None;
        }
    }
}

fn package_name(root: &str) -> Option<String> {
    let text = std::fs::read_to_string(Path::new(root).join("package.json")).ok()?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

fn command_boundary(byte: Option<u8>) -> bool {
    byte.is_none_or(|byte| {
        byte.is_ascii_whitespace()
            || matches!(
                byte,
                b'/' | b'\\' | b'\'' | b'"' | b'=' | b';' | b',' | b'(' | b')'
            )
    })
}

fn contains_command_term(haystack: &str, needle: &str) -> bool {
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
                .and_then(|index| haystack.as_bytes().get(index).copied()),
        ) && command_boundary(haystack.as_bytes().get(end).copied())
        {
            return true;
        }
        from = end;
    }
    false
}

fn configured_command_matches(command: &str, observed: &str) -> bool {
    let command = command.to_ascii_lowercase();
    let observed = observed.to_ascii_lowercase();
    let command_core = command
        .split_whitespace()
        .take_while(|token| *token != "--")
        .filter(|token| !token.contains("${"))
        .collect::<Vec<_>>()
        .join(" ");
    if command_core.len() >= 5 && contains_command_term(&observed, &command_core) {
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
        "cargo run",
        "go run",
    ];
    if distinctive.iter().any(|needle| {
        contains_command_term(&command, needle) && contains_command_term(&observed, needle)
    }) {
        return true;
    }

    // Script/entry filenames are usually more reliable than generic `npm run`.
    command.split_whitespace().any(|token| {
        let token = token
            .trim_matches(|c: char| c == '\'' || c == '"' || c == ';')
            .rsplit('/')
            .next()
            .unwrap_or("");
        token.len() > 3
            && (token.ends_with(".js")
                || token.ends_with(".ts")
                || token.ends_with(".py")
                || token.ends_with(".rb"))
            && contains_command_term(&observed, token)
    })
}

pub fn command_matches(configured: &str, observed: &str) -> bool {
    configured_command_matches(configured, observed)
}

fn match_config(
    configs: &[AppConfig],
    port: u16,
    cwd: Option<&str>,
    leader_cwd: Option<&str>,
    observed: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let mut candidates: Vec<(&AppConfig, usize)> = Vec::new();
    for cfg in configs {
        let root = canonical_string(&cfg.root);
        let belongs = cwd.is_some_and(|p| path_under(&canonical_string(p), &root))
            || leader_cwd.is_some_and(|p| path_under(&canonical_string(p), &root))
            || contains_command_term(observed, &cfg.root)
            || (root != cfg.root && contains_command_term(observed, &root));
        if belongs {
            candidates.push((cfg, root.len()));
        }
    }
    candidates.sort_by_key(|(_, len)| std::cmp::Reverse(*len));
    let Some((cfg, _)) = candidates.first().copied() else {
        return (None, None, None);
    };

    if let Some(svc) = cfg
        .services
        .iter()
        .find(|svc| ports::discovery_port(svc) == Some(port))
    {
        return (
            Some(cfg.name.clone()),
            Some(svc.name.clone()),
            Some("project folder + configured port".to_string()),
        );
    }

    let command_matches: Vec<_> = cfg
        .services
        .iter()
        .filter(|svc| configured_command_matches(&svc.command, observed))
        .collect();
    if command_matches.len() == 1 {
        return (
            Some(cfg.name.clone()),
            Some(command_matches[0].name.clone()),
            Some("project folder + command".to_string()),
        );
    }
    if cfg.services.len() == 1 {
        return (
            Some(cfg.name.clone()),
            Some(cfg.services[0].name.clone()),
            Some("project folder (different port)".to_string()),
        );
    }
    (
        Some(cfg.name.clone()),
        None,
        Some("project folder".to_string()),
    )
}

fn classify(command: &str, process: &str, server: Option<&str>, title: Option<&str>) -> String {
    let hay = format!(
        "{} {} {} {}",
        command,
        process,
        server.unwrap_or(""),
        title.unwrap_or("")
    )
    .to_ascii_lowercase();
    let checks = [
        ("harbor mcp", "Harbor MCP"),
        ("vite", "Vite"),
        ("next", "Next.js"),
        ("nuxt", "Nuxt"),
        ("astro", "Astro"),
        ("svelte", "SvelteKit"),
        ("remix", "Remix"),
        ("webpack", "Webpack"),
        ("angular", "Angular"),
        ("gatsby", "Gatsby"),
        ("uvicorn", "FastAPI / Uvicorn"),
        ("fastapi", "FastAPI"),
        ("flask", "Flask"),
        ("django", "Django"),
        ("rails", "Rails"),
        ("http.server", "Python static server"),
        ("deno", "Deno"),
        ("bun", "Bun"),
        ("node", "Node.js"),
        ("python", "Python"),
        ("ruby", "Ruby"),
        ("cargo", "Rust"),
        ("spring", "Spring"),
        ("java", "Java"),
        ("php", "PHP"),
        ("postgres", "PostgreSQL"),
        ("mongod", "MongoDB"),
        ("mysqld", "MySQL"),
        ("redis", "Redis"),
        ("ollama", "Ollama"),
        ("qdrant", "Qdrant"),
        ("caddy", "Caddy"),
        ("nginx", "nginx"),
        ("docker", "Docker"),
    ];
    checks
        .iter()
        .find_map(|(needle, label)| hay.contains(needle).then(|| (*label).to_string()))
        .unwrap_or_else(|| "Local service".to_string())
}

fn parse_listener_port(name: &str) -> Option<u16> {
    let tail = name.rsplit(':').next()?.trim();
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn parse_lsof(raw: &str) -> Vec<RawListener> {
    let mut current_pid: Option<u32> = None;
    let mut current_process = String::new();
    let mut map: BTreeMap<(u32, u16), RawListener> = BTreeMap::new();
    for line in raw.lines() {
        let Some(kind) = line.chars().next() else {
            continue;
        };
        let value = line.get(1..).unwrap_or("").trim();
        match kind {
            'p' => {
                current_pid = value.parse().ok();
                current_process.clear();
            }
            'c' => current_process = value.to_string(),
            'n' => {
                let (Some(pid), Some(port)) = (current_pid, parse_listener_port(value)) else {
                    continue;
                };
                let entry = map.entry((pid, port)).or_insert_with(|| RawListener {
                    pid,
                    port,
                    process: current_process.clone(),
                    addresses: Vec::new(),
                });
                if !value.is_empty() && !entry.addresses.iter().any(|a| a == value) {
                    entry.addresses.push(value.to_string());
                }
            }
            _ => {}
        }
    }
    map.into_values().collect()
}

fn network_exposed(addresses: &[String]) -> bool {
    addresses.iter().any(|address| {
        let address = address.trim().to_ascii_lowercase();
        if address.starts_with("127.")
            || address.starts_with("localhost:")
            || address.starts_with("[::1]:")
            || address.starts_with("::1:")
        {
            return false;
        }
        !address.is_empty()
    })
}

fn lsof_snapshot() -> Result<Vec<RawListener>> {
    let user = std::env::var("USER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            Command::new("/usr/bin/id")
                .arg("-un")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .ok_or_else(|| anyhow!("could not determine the current user for listener discovery"))?;
    for prog in ["lsof", "/usr/sbin/lsof"] {
        let mut args = vec!["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcn"];
        args.extend(["-a", "-u", user.as_str()]);
        let Ok(out) = Command::new(prog).args(&args).output() else {
            continue;
        };
        if !out.status.success() && out.stdout.is_empty() {
            return Ok(vec![]);
        }
        return Ok(parse_lsof(&String::from_utf8_lossy(&out.stdout)));
    }
    Err(anyhow!(
        "could not inspect listeners because lsof is unavailable"
    ))
}

/// Observation-only listeners whose socket owner, group leader, or argv points
/// at a project root. Used by Start to reuse a uniquely matching service even
/// when another agent selected a non-default port.
pub fn listeners_for_root(root: &str) -> Vec<RootListener> {
    let Ok(raw) = lsof_snapshot() else {
        return vec![];
    };
    let canonical = canonical_string(root);
    let mut out = Vec::new();
    for listener in raw {
        let Some(facts) = ps_facts(listener.pid) else {
            continue;
        };
        let leader_cwd = pid_cwd(facts.pgid);
        let listener_cwd = pid_cwd(listener.pid);
        let commands = group_argv_lines(facts.pgid).join("\n");
        let belongs = listener_cwd
            .as_deref()
            .is_some_and(|cwd| path_under(&canonical_string(cwd), &canonical))
            || leader_cwd
                .as_deref()
                .is_some_and(|cwd| path_under(&canonical_string(cwd), &canonical))
            || contains_command_term(&commands, root)
            || (canonical != root && contains_command_term(&commands, &canonical));
        if belongs {
            out.push(RootListener {
                port: listener.port,
                command: if commands.is_empty() {
                    facts.command
                } else {
                    commands
                },
            });
        }
    }
    out
}

fn likely_dev(kind: &str, project_root: Option<&str>, matched: bool, tracked: bool) -> bool {
    if matched || tracked || project_root.is_some() {
        return true;
    }
    kind != "Local service"
}

fn cleanup_kind_allowed(kind: &str) -> bool {
    matches!(
        kind,
        "Vite"
            | "Next.js"
            | "Nuxt"
            | "Astro"
            | "SvelteKit"
            | "Remix"
            | "Webpack"
            | "Angular"
            | "Gatsby"
            | "FastAPI / Uvicorn"
            | "FastAPI"
            | "Flask"
            | "Django"
            | "Rails"
            | "Python static server"
            | "Deno"
            | "Bun"
            | "Node.js"
            | "Python"
            | "Ruby"
            | "Rust"
            | "Spring"
    )
}

fn runtime_fingerprint(server: &LocalServer) -> String {
    let mut command = server.command.to_ascii_lowercase();
    // Make otherwise-identical commands on `--port 5173` / `--port 5174`
    // comparable without pretending the exact command is an identity token.
    command = command
        .split_whitespace()
        .map(|token| {
            if token.chars().all(|c| c.is_ascii_digit()) {
                "#"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{}|{}", server.kind, command)
}

fn duplicate_identity(server: &LocalServer) -> Option<String> {
    if server.harbor_internal {
        return None;
    }
    let project = server
        .matched_app
        .as_deref()
        .or(server.project_root.as_deref())?;
    Some(format!("{project}|{}", runtime_fingerprint(server)))
}

fn enrich(
    raw: Vec<RawListener>,
    configs: &[AppConfig],
    tracked: &[TrackedServer],
    mcp_port: u16,
) -> Vec<LocalServer> {
    let mut servers = Vec::new();
    for listener in raw {
        let Some(listener_facts) = ps_facts(listener.pid) else {
            continue;
        };
        let leader_pid = listener_facts.pgid;
        let leader = ps_facts(leader_pid).unwrap_or_else(|| listener_facts.clone());
        let cwd = pid_cwd(listener.pid).or_else(|| pid_cwd(leader_pid));
        let leader_cwd = pid_cwd(leader_pid);
        let group_commands = group_argv_lines(leader_pid);
        let observed = if group_commands.is_empty() {
            listener_facts.command.clone()
        } else {
            group_commands.join("\n")
        };

        let harbor_internal = listener.pid == std::process::id() && listener.port == mcp_port;
        let tracked_match = if harbor_internal {
            None
        } else {
            tracked
                .iter()
                .find(|item| {
                    item.leader_pid == leader_pid
                        && (item.port == Some(listener.port) || item.port.is_none())
                })
                .or_else(|| tracked.iter().find(|item| item.leader_pid == leader_pid))
        };
        let (matched_app, matched_service, match_reason, external) = if harbor_internal {
            (None, None, None, false)
        } else if let Some(item) = tracked_match {
            (
                Some(item.app.clone()),
                Some(item.service.clone()),
                Some("tracked Harbor process".to_string()),
                item.external,
            )
        } else {
            let (app, service, reason) = match_config(
                configs,
                listener.port,
                cwd.as_deref(),
                leader_cwd.as_deref(),
                &observed,
            );
            (app, service, reason, false)
        };

        let project_root = if harbor_internal {
            None
        } else {
            matched_app
                .as_ref()
                .and_then(|name| configs.iter().find(|cfg| &cfg.name == name))
                .map(|cfg| cfg.root.clone())
                .or_else(|| cwd.as_deref().and_then(infer_project_root))
        };
        let kind = if harbor_internal {
            "Harbor MCP".to_string()
        } else {
            classify(&listener_facts.command, &listener.process, None, None)
        };
        let tracked_flag = tracked_match.is_some();
        let matched_flag = matched_app.is_some();
        let safe_to_stop = !tracked_flag
            && !harbor_internal
            && leader_pid > 1
            && leader_pid != std::process::id()
            && leader.pgid == leader_pid
            && !unsafe_group_host(&leader.command)
            && cleanup_kind_allowed(&kind);
        let display_name = matched_app
            .clone()
            .or_else(|| project_root.as_deref().and_then(package_name))
            .or_else(|| {
                project_root.as_deref().and_then(|root| {
                    Path::new(root)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
            })
            .unwrap_or_else(|| kind.clone());
        let is_likely_dev = !harbor_internal
            && likely_dev(&kind, project_root.as_deref(), matched_flag, tracked_flag);

        servers.push(LocalServer {
            pid: listener.pid,
            leader_pid,
            port: listener.port,
            network_exposed: network_exposed(&listener.addresses),
            addresses: listener.addresses,
            process: listener.process,
            command: listener_facts.command,
            cwd,
            project_root,
            display_name,
            kind,
            started_at: leader.started_at,
            url: format!("http://localhost:{}", listener.port),
            http_status: None,
            page_title: None,
            server_header: None,
            matched_app,
            matched_service,
            match_reason,
            tracked: tracked_flag,
            external,
            safe_to_stop,
            likely_dev: is_likely_dev,
            duplicate_count: 1,
            harbor_internal,
        });
    }
    servers
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn clean_title(raw: &str) -> Option<String> {
    let title = raw
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        None
    } else {
        Some(title.chars().take(120).collect())
    }
}

fn parse_http_response(text: &str) -> HttpProbe {
    let (headers, body) = text
        .split_once("\r\n\r\n")
        .or_else(|| text.split_once("\n\n"))
        .unwrap_or((text, ""));
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok());
    let server = headers.lines().find_map(|line| {
        line.split_once(':').and_then(|(name, value)| {
            name.trim()
                .eq_ignore_ascii_case("server")
                .then(|| value.trim().chars().take(100).collect::<String>())
        })
    });
    let title = find_ascii_case_insensitive(body, "<title").and_then(|start| {
        let after = &body[start..];
        let open_end = after.find('>')? + 1;
        let content = &after[open_end..];
        let close = find_ascii_case_insensitive(content, "</title>")?;
        clean_title(&content[..close])
    });
    HttpProbe {
        status,
        title,
        server,
    }
}

async fn http_probe(port: u16) -> Option<HttpProbe> {
    tokio::time::timeout(Duration::from_millis(550), async move {
        let mut stream = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Ok(stream) => stream,
            Err(_) => tokio::net::TcpStream::connect(("::1", port)).await.ok()?,
        };
        let request = b"GET / HTTP/1.0\r\nHost: localhost\r\nUser-Agent: Harbor/0.2\r\nAccept: text/html,*/*\r\nConnection: close\r\n\r\n";
        stream.write_all(request).await.ok()?;
        let mut data = Vec::with_capacity(8192);
        let mut buf = [0u8; 4096];
        while data.len() < 32768 {
            let n = stream.read(&mut buf).await.ok()?;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
        }
        let text = String::from_utf8_lossy(&data);
        text.starts_with("HTTP/").then(|| parse_http_response(&text))
    })
    .await
    .ok()
    .flatten()
}

/// Take a fresh current-user listener snapshot. HTTP probes run concurrently and are
/// attempted only for likely development listeners, keeping the scan bounded.
pub async fn scan(
    configs: &[AppConfig],
    tracked: &[TrackedServer],
    mcp_port: u16,
) -> Result<LocalServerInventory> {
    let configs_owned = configs.to_vec();
    let tracked_owned = tracked.to_vec();
    let mut servers = tokio::task::spawn_blocking(move || {
        let raw = lsof_snapshot()?;
        Ok::<_, anyhow::Error>(enrich(raw, &configs_owned, &tracked_owned, mcp_port))
    })
    .await
    .map_err(|e| anyhow!("local-server scan task failed: {e}"))??;

    let probe_limit = Arc::new(tokio::sync::Semaphore::new(16));
    let probes: Vec<_> = servers
        .iter()
        .map(|server| {
            (server.likely_dev && !server.harbor_internal).then(|| {
                let limit = probe_limit.clone();
                let port = server.port;
                tokio::spawn(async move {
                    let _permit = limit.acquire_owned().await.ok()?;
                    http_probe(port).await
                })
            })
        })
        .collect();
    for (server, probe) in servers.iter_mut().zip(probes) {
        if let Some(handle) = probe {
            if let Ok(Some(result)) = handle.await {
                server.http_status = result.status;
                server.page_title = result.title;
                server.server_header = result.server;
                if !server.harbor_internal {
                    server.kind = classify(
                        &server.command,
                        &server.process,
                        server.server_header.as_deref(),
                        server.page_title.as_deref(),
                    );
                }
            }
        }
    }

    let mut duplicate_groups: HashMap<String, HashSet<u32>> = HashMap::new();
    for server in &servers {
        if let Some(key) = duplicate_identity(server) {
            duplicate_groups
                .entry(key)
                .or_default()
                .insert(server.leader_pid);
        }
    }
    for server in &mut servers {
        if let Some(key) = duplicate_identity(server) {
            server.duplicate_count = duplicate_groups.get(&key).map_or(1, HashSet::len);
        }
    }
    servers.sort_by_key(|server| {
        (
            !server.tracked,
            server.matched_app.is_none(),
            !server.likely_dev,
            server.display_name.to_ascii_lowercase(),
            server.port,
        )
    });

    let dev_count = servers.iter().filter(|s| s.likely_dev).count();
    let other_count = servers
        .iter()
        .filter(|s| !s.likely_dev && !s.harbor_internal)
        .count();
    let mapped_count = servers
        .iter()
        .filter(|s| !s.harbor_internal && s.matched_app.is_some())
        .count();
    let duplicate_count = duplicate_groups
        .values()
        .map(|leaders| leaders.len().saturating_sub(1))
        .sum();
    Ok(LocalServerInventory {
        scanned_at: now_millis(),
        servers,
        dev_count,
        other_count,
        mapped_count,
        duplicate_count,
    })
}

fn cleanup_identity_ok(leader_pid: u32, started_at: &str, port: u16) -> bool {
    let Some(leader) = ps_facts(leader_pid) else {
        return false;
    };
    if leader.pgid != leader_pid
        || leader.started_at != started_at
        || unsafe_group_host(&leader.command)
    {
        return false;
    }
    let Ok(listeners) = lsof_snapshot() else {
        return false;
    };
    listeners.into_iter().any(|listener| {
        if listener.port != port {
            return false;
        }
        let Some(facts) = ps_facts(listener.pid) else {
            return false;
        };
        facts.pgid == leader_pid
            && cleanup_kind_allowed(&classify(&facts.command, &listener.process, None, None))
    })
}

/// Stop an untracked, isolated server group. The caller must separately verify
/// that Supervisor does not own this PID. A stale PID/start token is refused.
pub async fn stop_untracked(leader_pid: u32, started_at: &str, port: u16) -> Result<()> {
    if leader_pid <= 1 || leader_pid == std::process::id() {
        bail!("Harbor refuses to stop this process group");
    }
    let started = started_at.to_string();
    let valid =
        tokio::task::spawn_blocking(move || cleanup_identity_ok(leader_pid, &started, port))
            .await
            .unwrap_or(false);
    if !valid {
        bail!("process identity changed or its group is not safe to stop; refresh and try again");
    }

    killpg(Pid::from_raw(leader_pid as i32), Signal::SIGTERM)
        .map_err(|e| anyhow!("sending SIGTERM: {e}"))?;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !group_has_live_members(leader_pid) {
            return Ok(());
        }
    }

    let started = started_at.to_string();
    let still_valid =
        tokio::task::spawn_blocking(move || cleanup_identity_ok(leader_pid, &started, port))
            .await
            .unwrap_or(false);
    if !still_valid {
        bail!("process identity changed while stopping; refusing SIGKILL");
    }
    killpg(Pid::from_raw(leader_pid as i32), Signal::SIGKILL)
        .map_err(|e| anyhow!("sending SIGKILL: {e}"))?;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !group_has_live_members(leader_pid) {
            return Ok(());
        }
    }
    bail!("process group {leader_pid} is still running after SIGKILL")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matching_config(port: u16) -> AppConfig {
        AppConfig {
            name: "Harbor workspace".to_string(),
            root: std::env::current_dir()
                .expect("test process has a current directory")
                .to_string_lossy()
                .into_owned(),
            services: vec![crate::model::ServiceConfig {
                name: "web".to_string(),
                cwd: ".".to_string(),
                command: "cargo test".to_string(),
                port: Some(port),
                env: BTreeMap::new(),
                depends_on: vec![],
                health_check: None,
                ready_log_pattern: None,
            }],
            profiles: BTreeMap::new(),
            auto_restart: false,
            trusted: true,
        }
    }

    fn internal_listener(port: u16) -> RawListener {
        RawListener {
            pid: std::process::id(),
            port,
            process: "harbor".to_string(),
            addresses: vec![format!("127.0.0.1:{port}")],
        }
    }

    #[test]
    fn parses_lsof_and_coalesces_dual_stack_addresses() {
        let raw = "p123\ncnode\nn*:5173\nn[::1]:5173\np456\ncpython3\nn127.0.0.1:8000\n";
        let rows = parse_lsof(raw);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].pid, 123);
        assert_eq!(rows[0].port, 5173);
        assert_eq!(rows[0].addresses.len(), 2);
        assert_eq!(rows[1].process, "python3");
        assert!(network_exposed(&rows[0].addresses));
        assert!(!network_exposed(&rows[1].addresses));
    }

    #[test]
    fn parses_http_metadata_without_a_dependency() {
        let p = parse_http_response(
            "HTTP/1.1 200 OK\r\nServer: Vite\r\nContent-Type: text/html\r\n\r\n<html><title>  My &amp; App </title></html>",
        );
        assert_eq!(p.status, Some(200));
        assert_eq!(p.server.as_deref(), Some("Vite"));
        assert_eq!(p.title.as_deref(), Some("My & App"));
    }

    #[test]
    fn protects_shells_and_coding_agents_from_group_cleanup() {
        for command in [
            "/bin/zsh -l",
            "codex app-server",
            "/Applications/Cursor.app/Contents/MacOS/Cursor",
            "node /x/@anthropic-ai/claude-code/cli.js",
        ] {
            assert!(unsafe_group_host(command), "{command}");
        }
        assert!(!unsafe_group_host(
            "node /project/node_modules/vite/bin/vite.js"
        ));
    }

    #[test]
    fn command_matching_prefers_framework_or_entry_file() {
        assert!(configured_command_matches(
            "vite --port ${PORT}",
            "node /x/node_modules/vite/bin/vite.js --port 5173"
        ));
        assert!(configured_command_matches(
            "node server.js",
            "/opt/homebrew/bin/node /tmp/project/server.js"
        ));
        assert!(configured_command_matches(
            "npm run dev -- --port ${PORT}",
            "npm run dev\nnode /tmp/project/node_modules/vite/bin/vite.js --port 5188"
        ));
        assert!(!configured_command_matches(
            "npm start",
            "python -m http.server"
        ));
        assert!(!configured_command_matches(
            "npm run dev",
            "npm run dev:docs -- --port 5174"
        ));
        assert!(!configured_command_matches(
            "next dev",
            "node /tmp/nextcloud/dev.js"
        ));
        assert!(contains_command_term("node /x/app/server.js", "/x/app"));
        assert!(!contains_command_term(
            "node /x/app-old/server.js",
            "/x/app"
        ));
    }

    #[test]
    fn harbor_internal_listener_is_not_matched_to_a_config() {
        let port = 42_424;
        let config = matching_config(port);
        let cwd = std::env::current_dir()
            .expect("test process has a current directory")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            match_config(
                std::slice::from_ref(&config),
                port,
                Some(&cwd),
                None,
                "cargo test"
            )
            .0,
            Some(config.name.clone()),
            "fixture must match without the Harbor-internal guard"
        );

        let servers = enrich(vec![internal_listener(port)], &[config], &[], port);
        assert_eq!(servers.len(), 1);
        let server = &servers[0];
        assert!(server.harbor_internal);
        assert_eq!(server.kind, "Harbor MCP");
        assert_eq!(server.display_name, "Harbor MCP");
        assert!(server.project_root.is_none());
        assert!(server.matched_app.is_none());
        assert!(server.matched_service.is_none());
        assert!(server.match_reason.is_none());
        assert!(!server.tracked);
        assert!(!server.external);
        assert!(!server.likely_dev);
        assert!(!server.safe_to_stop);
        assert!(duplicate_identity(server).is_none());
    }

    #[test]
    fn harbor_internal_listener_ignores_a_tracked_group_match() {
        let port = 42_425;
        let leader_pid = ps_facts(std::process::id())
            .expect("test process is visible to ps")
            .pgid;
        let tracked = TrackedServer {
            app: "Wrong app".to_string(),
            service: "web".to_string(),
            leader_pid,
            port: Some(port),
            external: true,
        };

        let servers = enrich(vec![internal_listener(port)], &[], &[tracked], port);
        assert_eq!(servers.len(), 1);
        let server = &servers[0];
        assert!(server.harbor_internal);
        assert!(server.matched_app.is_none());
        assert!(server.matched_service.is_none());
        assert!(server.match_reason.is_none());
        assert!(!server.tracked);
        assert!(!server.external);
        assert!(duplicate_identity(server).is_none());
    }
}
