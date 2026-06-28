//! `detect_app` heuristics (DESIGN.md §8). Scans a folder and **proposes** a
//! config — it never saves. Returns the proposal plus human-readable confidence
//! notes so Claude (or the user) can reason before registering.

use crate::model::{AppConfig, HealthCheck, ServiceConfig};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Detection {
    pub proposed: AppConfig,
    pub notes: Vec<String>,
}

fn port_env() -> BTreeMap<String, String> {
    BTreeMap::from([("PORT".to_string(), "${PORT}".to_string())])
}

pub fn detect(path: &Path) -> Detection {
    let mut notes = Vec::new();
    let mut services: Vec<ServiceConfig> = Vec::new();

    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "app".to_string());

    // ---- package.json ----------------------------------------------------
    let pkg_path = path.join("package.json");
    if let Some(pkg) = read_json(&pkg_path) {
        notes.push("found package.json".to_string());
        let scripts = pkg.get("scripts").and_then(|v| v.as_object());
        let deps = collect_deps(&pkg);

        let has = |k: &str| deps.contains(&k.to_string());
        let framework = if has("next") {
            Some(("next", 3000u16))
        } else if has("vite") {
            Some(("vite", 5173))
        } else if has("@remix-run/dev") {
            Some(("remix", 3000))
        } else if has("express") || has("fastify") || has("@nestjs/core") {
            Some(("node-server", 3000))
        } else {
            None
        };
        if let Some((fw, _)) = framework {
            notes.push(format!("framework signature: {fw}"));
        }

        let has_script = |k: &str| scripts.map(|s| s.contains_key(k)).unwrap_or(false);

        // A `dev` script usually means the frontend/dev server.
        if has_script("dev") {
            let (port, ready) = match framework {
                Some(("vite", p)) => (p, Some("ready in")),
                Some(("next", p)) => (p, Some("started server")),
                Some((_, p)) => (p, None),
                None => (3000, None),
            };
            services.push(ServiceConfig {
                name: "web".to_string(),
                cwd: ".".to_string(),
                command: "npm run dev".to_string(),
                port: Some(port),
                env: port_env(),
                depends_on: vec![],
                health_check: Some(HealthCheck::Tcp),
                ready_log_pattern: ready.map(|s| s.to_string()),
            });
            notes.push(format!("`npm run dev` → web service (guess port {port})"));
        }

        // A `start` script (or a bare entry file) means a long-running server.
        if has_script("start") {
            // A backend server gets its own port — not the frontend framework's
            // dev port (vite 5173 etc.), which belongs to the `web` service.
            let server_port = match framework {
                Some(("node-server", p)) | Some(("next", p)) | Some(("remix", p)) => p,
                _ => 3000,
            };
            services.push(ServiceConfig {
                name: "server".to_string(),
                cwd: ".".to_string(),
                command: "npm start".to_string(),
                port: Some(server_port),
                env: port_env(),
                depends_on: vec![],
                health_check: Some(HealthCheck::Http {
                    path: "/".to_string(),
                    expect: Some("2xx-3xx".to_string()),
                }),
                ready_log_pattern: None,
            });
            notes.push("`npm start` → server service".to_string());
        } else if let Some(entry) = ["server.js", "index.js", "app.js", "main.js"]
            .iter()
            .find(|f| path.join(f).exists())
        {
            services.push(ServiceConfig {
                name: "server".to_string(),
                cwd: ".".to_string(),
                command: format!("node {entry}"),
                port: Some(3000),
                env: port_env(),
                depends_on: vec![],
                health_check: Some(HealthCheck::Http {
                    path: "/".to_string(),
                    expect: Some("2xx-3xx".to_string()),
                }),
                ready_log_pattern: None,
            });
            notes.push(format!("entry file `{entry}` → server service"));
        }
    }

    // ---- Procfile --------------------------------------------------------
    let procfile = path.join("Procfile");
    if let Ok(text) = std::fs::read_to_string(&procfile) {
        notes.push("found Procfile".to_string());
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((pname, cmd)) = line.split_once(':') {
                let pname = pname.trim();
                if services.iter().any(|s| s.name == pname) {
                    continue;
                }
                services.push(ServiceConfig {
                    name: pname.to_string(),
                    cwd: ".".to_string(),
                    command: cmd.trim().to_string(),
                    port: None,
                    env: BTreeMap::new(),
                    depends_on: vec![],
                    health_check: None,
                    ready_log_pattern: None,
                });
            }
        }
    }

    // ---- docker-compose / Makefile (noted, not auto-imported) -----------
    for f in ["docker-compose.yml", "docker-compose.yaml", "compose.yaml"] {
        if path.join(f).exists() {
            notes.push(format!("{f} present — review services manually (not auto-imported)"));
        }
    }
    if path.join("Makefile").exists() {
        notes.push("Makefile present — `make` targets may be runnable".to_string());
    }

    if services.is_empty() {
        notes.push("no recognizable services — propose a manual config".to_string());
    }

    // ---- profiles --------------------------------------------------------
    let mut profiles: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let names: Vec<String> = services.iter().map(|s| s.name.clone()).collect();
    if names.iter().any(|n| n == "server") {
        profiles.insert("default".to_string(), vec!["server".to_string()]);
    } else if let Some(first) = names.first() {
        profiles.insert("default".to_string(), vec![first.clone()]);
    }
    if names.len() > 1 {
        profiles.insert("dev".to_string(), names.clone());
        // If both web and server exist, wire web → server ordering in the proposal.
        if names.iter().any(|n| n == "web") && names.iter().any(|n| n == "server") {
            if let Some(web) = services.iter_mut().find(|s| s.name == "web") {
                web.depends_on = vec!["server".to_string()];
            }
        }
    }

    Detection {
        proposed: AppConfig {
            name,
            root: path.to_string_lossy().into_owned(),
            services,
            profiles,
        },
        notes,
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn collect_deps(pkg: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for key in ["dependencies", "devDependencies"] {
        if let Some(obj) = pkg.get(key).and_then(|v| v.as_object()) {
            out.extend(obj.keys().cloned());
        }
    }
    out
}
