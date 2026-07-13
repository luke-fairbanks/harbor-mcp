//! Port intelligence (DESIGN.md §4): topological ordering, preferred→free
//! allocation, and `${...}` placeholder resolution + dependent rewiring.
//!
//! All of this is pure logic over config + a set of already-taken ports, so it's
//! unit-testable without spawning anything.

use crate::model::{PortPlanEntry, ServiceConfig};
use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, HashSet};
use std::net::TcpListener;

/// How far upward to scan from a preferred port before giving up.
const SCAN_SPAN: u16 = 500;

/// Order `services` so every service comes after the ones it `dependsOn`
/// (Kahn's algorithm). Errors on cycles or references to unknown services.
pub fn topo_sort(services: &[ServiceConfig]) -> Result<Vec<ServiceConfig>> {
    let names: HashSet<&str> = services.iter().map(|s| s.name.as_str()).collect();
    let mut indegree: BTreeMap<&str, usize> =
        services.iter().map(|s| (s.name.as_str(), 0)).collect();
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();

    for s in services {
        for dep in &s.depends_on {
            if !names.contains(dep.as_str()) {
                bail!("service '{}' dependsOn unknown service '{}'", s.name, dep);
            }
            // dep -> s
            edges.entry(dep.as_str()).or_default().push(s.name.as_str());
            *indegree.get_mut(s.name.as_str()).unwrap() += 1;
        }
    }

    // Seed with zero-indegree nodes, preserving the original declaration order
    // for determinism.
    let mut queue: Vec<&str> = services
        .iter()
        .map(|s| s.name.as_str())
        .filter(|n| indegree[n] == 0)
        .collect();
    let mut ordered: Vec<&str> = Vec::with_capacity(services.len());

    while let Some(n) = queue_pop_front(&mut queue) {
        ordered.push(n);
        if let Some(children) = edges.get(n) {
            for &c in children {
                let d = indegree.get_mut(c).unwrap();
                *d -= 1;
                if *d == 0 {
                    queue.push(c);
                }
            }
        }
    }

    if ordered.len() != services.len() {
        bail!("dependency cycle detected among services");
    }

    // Map names back to (cloned) configs in topo order.
    let by_name: BTreeMap<&str, &ServiceConfig> =
        services.iter().map(|s| (s.name.as_str(), s)).collect();
    Ok(ordered.into_iter().map(|n| by_name[n].clone()).collect())
}

fn queue_pop_front<'a>(q: &mut Vec<&'a str>) -> Option<&'a str> {
    if q.is_empty() {
        None
    } else {
        Some(q.remove(0))
    }
}

/// True if `port` is free to bind. Dev servers usually bind the wildcard address
/// (Node `listen(port)` binds `[::]` dual-stack, which also covers IPv4), so we
/// probe **both** the IPv4 and IPv6 wildcards and call the port free only if both
/// binds succeed. Probing just `127.0.0.1` would miss an `[::]` holder and let us
/// hand out a port that the service then can't bind (EADDRINUSE).
pub fn is_port_free(port: u16) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    let v4 = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok();
    let v6 = TcpListener::bind((Ipv6Addr::UNSPECIFIED, port)).is_ok();
    v4 && v6
}

/// A literal port the command hard-codes — `-p 3002`, `--port 3002`, `-p=3002`,
/// `--port=3002`, `-p3002`, or `-P 3002`. When present it is **authoritative**:
/// the process will bind exactly this port regardless of what we'd allocate, so
/// the plan/health-check/Open URL must all use it (never a bumped value).
pub fn pinned_port(command: &str) -> Option<u16> {
    let toks: Vec<&str> = command.split_whitespace().collect();
    for (i, t) in toks.iter().enumerate() {
        // `--port=3002` / `-p=3002`
        if let Some(r) = t.strip_prefix("--port=").or_else(|| t.strip_prefix("-p=")) {
            if let Ok(p) = r.parse() {
                return Some(p);
            }
        }
        // `-p3002` (no space, digits only)
        if let Some(r) = t.strip_prefix("-p") {
            if !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(p) = r.parse() {
                    return Some(p);
                }
            }
        }
        // `-p 3002` / `--port 3002` / `-P 3002`
        if (*t == "-p" || *t == "--port" || *t == "-P") && i + 1 < toks.len() {
            if let Ok(p) = toks[i + 1].parse() {
                return Some(p);
            }
        }
    }
    None
}

/// Whether Harbor's allocated port actually reaches the process — i.e. the
/// command or some env value references `${PORT}` / `${services.*.port}`. If it
/// doesn't, the service binds a port of its own choosing (often hard-coded in a
/// `package.json` dev script), so Harbor can't relocate it and must treat the
/// configured port as fixed rather than bumping to a phantom value.
pub fn injects_port(svc: &ServiceConfig) -> bool {
    let refs = |s: &str| s.contains("${PORT}") || s.contains("${services.");
    refs(&svc.command) || svc.env.values().any(|v| refs(v))
}

/// The port a service will actually bind, by the same precedence [`allocate`]
/// uses for a HARD PIN: a command-literal port, else a fixed `svc.port` the
/// command can't relocate (no `${PORT}`). Returns `None` for relocatable
/// services (Harbor chooses those only at allocation, so there's nothing stable
/// to detect an external process against) and for portless services. This is the
/// port external-process detection probes, so it matches what the app binds.
#[allow(dead_code)] // retained as the authoritative hard-pin helper for callers/tests
pub fn effective_port(svc: &ServiceConfig) -> Option<u16> {
    pinned_port(&svc.command).or(match svc.port {
        Some(p) if !injects_port(svc) => Some(p),
        _ => None,
    })
}

/// Port worth probing when looking for an already-running copy of a configured
/// service. Unlike [`effective_port`], this intentionally includes a relocatable
/// service's preferred port: an externally-started Vite/Next server normally
/// still uses that default, and discovering it *before* allocation prevents
/// Harbor from quietly starting a duplicate on the next port.
pub fn discovery_port(svc: &ServiceConfig) -> Option<u16> {
    pinned_port(&svc.command).or(svc.port)
}

/// Pick a concrete port for a preferred value, skipping `taken` and anything the
/// OS reports as busy. Returns `(resolved, note)`.
fn allocate_one(preferred: u16, taken: &HashSet<u16>) -> Result<(u16, Option<String>)> {
    if !taken.contains(&preferred) && is_port_free(preferred) {
        return Ok((preferred, None));
    }
    let start = preferred.saturating_add(1);
    for p in start..=start.saturating_add(SCAN_SPAN) {
        if p == 0 {
            continue;
        }
        if !taken.contains(&p) && is_port_free(p) {
            return Ok((p, Some(format!("{preferred} was busy → {p}"))));
        }
    }
    Err(anyhow!(
        "no free port found near {preferred} (scanned {SCAN_SPAN})"
    ))
}

/// The resolved-port map for a run plus the human-readable plan.
pub struct Allocation {
    /// service name → resolved port (only services that declared a port).
    pub ports: BTreeMap<String, u16>,
    pub plan: Vec<PortPlanEntry>,
}

/// Allocate ports for `ordered` services (already topo-sorted). `reserved` holds
/// ports already claimed by other live Harbor runs, which we never reuse.
#[allow(dead_code)] // convenience wrapper used by focused allocator tests
pub fn allocate(ordered: &[ServiceConfig], reserved: &HashSet<u16>) -> Result<Allocation> {
    allocate_with_claims(ordered, reserved, &BTreeMap::new())
}

/// Allocate while honoring services already corroborated as running outside
/// Harbor. `claims` maps service name → its observed port. Those ports remain in
/// the shared plan so `${services.X.port}` rewiring points at the existing
/// process instead of a duplicate Harbor launch.
pub fn allocate_with_claims(
    ordered: &[ServiceConfig],
    reserved: &HashSet<u16>,
    claims: &BTreeMap<String, u16>,
) -> Result<Allocation> {
    let mut taken = reserved.clone();
    let mut ports: BTreeMap<String, u16> = BTreeMap::new();
    let mut plan: Vec<PortPlanEntry> = Vec::new();

    for svc in ordered {
        if let Some(&p) = claims.get(&svc.name) {
            taken.insert(p);
            ports.insert(svc.name.clone(), p);
            plan.push(PortPlanEntry {
                service: svc.name.clone(),
                preferred: svc.port,
                resolved: p,
                note: Some("already running outside Harbor — reused".to_string()),
            });
            continue;
        }
        // A port is a HARD PIN when Harbor can't relocate it: either the command
        // names a literal port flag, or the service doesn't consume `${PORT}` (so
        // it binds its own fixed port). Pinning binds it as-is and never bumps —
        // bumping would desync the plan / health probe / Open URL from the real
        // bound port, which is exactly what crashed opal with EADDRINUSE.
        let pin = pinned_port(&svc.command).or(match svc.port {
            Some(p) if !injects_port(svc) => Some(p),
            _ => None,
        });
        if let Some(p) = pin {
            let why = if pinned_port(&svc.command).is_some() {
                format!("command pins port {p}")
            } else {
                format!("fixed port {p} (command doesn't read ${{PORT}})")
            };
            let note = if taken.contains(&p) || !is_port_free(p) {
                format!("{why} — in use, will adopt or report")
            } else {
                why
            };
            taken.insert(p);
            ports.insert(svc.name.clone(), p);
            plan.push(PortPlanEntry {
                service: svc.name.clone(),
                preferred: svc.port,
                resolved: p,
                note: Some(note),
            });
            continue;
        }

        let Some(pref) = svc.port else { continue };
        let (resolved, note) = allocate_one(pref, &taken)?;
        taken.insert(resolved);
        ports.insert(svc.name.clone(), resolved);
        plan.push(PortPlanEntry {
            service: svc.name.clone(),
            preferred: Some(pref),
            resolved,
            note,
        });
    }

    Ok(Allocation { ports, plan })
}

/// Resolve `${PORT}` and `${services.<name>.port}` inside a string against the
/// resolved-port map. `own` is this service's port (for bare `${PORT}`).
/// Unknown placeholders are left untouched (e.g. `${HOME}` for the shell).
pub fn resolve_placeholders(
    input: &str,
    own: Option<u16>,
    ports: &BTreeMap<String, u16>,
) -> String {
    // Tiny hand-rolled scanner — avoids a regex dependency for one pattern.
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = input[i + 2..].find('}') {
                let key = &input[i + 2..i + 2 + end];
                if let Some(val) = lookup(key, own, ports) {
                    out.push_str(&val);
                    i = i + 2 + end + 1;
                    continue;
                }
            }
        }
        // Not a recognized placeholder — emit the byte as-is.
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn lookup(key: &str, own: Option<u16>, ports: &BTreeMap<String, u16>) -> Option<String> {
    if key == "PORT" {
        return own.map(|p| p.to_string());
    }
    // services.<name>.port
    if let Some(rest) = key.strip_prefix("services.") {
        if let Some(name) = rest.strip_suffix(".port") {
            return ports.get(name).map(|p| p.to_string());
        }
    }
    None
}

/// Resolve all placeholders in a service's command + env, returning the concrete
/// command line and a fully-resolved env map.
pub fn resolve_service(
    svc: &ServiceConfig,
    ports: &BTreeMap<String, u16>,
) -> (String, BTreeMap<String, String>) {
    let own = ports.get(&svc.name).copied();
    let command = resolve_placeholders(&svc.command, own, ports);
    let env = svc
        .env
        .iter()
        .map(|(k, v)| (k.clone(), resolve_placeholders(v, own, ports)))
        .collect();
    (command, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ServiceConfig;
    use std::collections::BTreeMap as Map;

    fn svc(name: &str, port: Option<u16>, deps: &[&str]) -> ServiceConfig {
        ServiceConfig {
            name: name.into(),
            cwd: ".".into(),
            command: "true".into(),
            port,
            env: Map::new(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            health_check: None,
            ready_log_pattern: None,
        }
    }

    #[test]
    fn topo_orders_deps_first() {
        let s = vec![
            svc("web", Some(5173), &["api"]),
            svc("api", Some(4321), &[]),
        ];
        let ordered = topo_sort(&s).unwrap();
        assert_eq!(ordered[0].name, "api");
        assert_eq!(ordered[1].name, "web");
    }

    #[test]
    fn topo_detects_cycle() {
        let s = vec![svc("a", None, &["b"]), svc("b", None, &["a"])];
        assert!(topo_sort(&s).is_err());
    }

    #[test]
    fn placeholders_resolve() {
        let mut ports = Map::new();
        ports.insert("api".to_string(), 4322u16);
        let out = resolve_placeholders(
            "vite --proxy http://127.0.0.1:${services.api.port} --port ${PORT}",
            Some(5173),
            &ports,
        );
        assert_eq!(out, "vite --proxy http://127.0.0.1:4322 --port 5173");
    }

    #[test]
    fn unknown_placeholder_untouched() {
        let ports = Map::new();
        let out = resolve_placeholders("echo ${HOME}", None, &ports);
        assert_eq!(out, "echo ${HOME}");
    }

    #[test]
    fn pinned_port_all_forms() {
        assert_eq!(pinned_port("next dev -p 3002"), Some(3002));
        assert_eq!(pinned_port("next dev --port 3002"), Some(3002));
        assert_eq!(pinned_port("next dev -p=3002"), Some(3002));
        assert_eq!(pinned_port("next dev --port=3002"), Some(3002));
        assert_eq!(pinned_port("next dev -p3002"), Some(3002));
        assert_eq!(pinned_port("some-server -P 8080"), Some(8080));
        assert_eq!(pinned_port("npm run dev"), None);
        // `${PORT}` is not a literal — Harbor still controls the port.
        assert_eq!(pinned_port("vite --port ${PORT}"), None);
    }

    #[test]
    fn allocate_pins_fixed_port_when_command_ignores_port() {
        // `npm run dev` with no ${PORT} anywhere → Harbor can't relocate it, so
        // the configured port is fixed and must NOT bump even when reserved.
        let s = vec![svc_cmd("web", "npm run dev", Some(3002))];
        let mut reserved = HashSet::new();
        reserved.insert(3002u16);
        let alloc = allocate(&s, &reserved).unwrap();
        assert_eq!(alloc.ports.get("web"), Some(&3002u16));
        assert!(alloc.plan[0]
            .note
            .as_deref()
            .unwrap()
            .contains("fixed port"));
    }

    #[test]
    fn allocate_bumps_relocatable_port() {
        // Command consumes ${PORT} → relocatable → a taken preferred port bumps.
        let s = vec![svc_cmd("web", "vite --port ${PORT}", Some(5000))];
        let mut reserved = HashSet::new();
        reserved.insert(5000u16);
        let alloc = allocate(&s, &reserved).unwrap();
        assert_ne!(alloc.ports.get("web"), Some(&5000u16));
    }

    #[test]
    fn external_claim_reuses_relocatable_preferred_port() {
        let s = vec![svc_cmd("web", "vite --port ${PORT}", Some(5173))];
        assert_eq!(discovery_port(&s[0]), Some(5173));
        let claims = Map::from([("web".to_string(), 5173u16)]);
        let alloc = allocate_with_claims(&s, &HashSet::new(), &claims).unwrap();
        assert_eq!(alloc.ports.get("web"), Some(&5173));
        assert!(alloc.plan[0].note.as_deref().unwrap().contains("reused"));
    }

    #[test]
    fn allocate_respects_pinned_port_over_bump() {
        // Pinned 3002 must be reported as resolved=3002 even though `taken` holds
        // it; it is never bumped.
        let s = vec![svc_cmd("web", "next dev -p 3002", Some(9999))];
        let mut reserved = HashSet::new();
        reserved.insert(3002u16);
        let alloc = allocate(&s, &reserved).unwrap();
        assert_eq!(alloc.ports.get("web"), Some(&3002u16));
        assert_eq!(alloc.plan[0].resolved, 3002);
    }

    fn svc_cmd(name: &str, command: &str, port: Option<u16>) -> ServiceConfig {
        ServiceConfig {
            name: name.into(),
            cwd: ".".into(),
            command: command.into(),
            port,
            env: Map::new(),
            depends_on: vec![],
            health_check: None,
            ready_log_pattern: None,
        }
    }
}
