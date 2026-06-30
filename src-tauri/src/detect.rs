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

        let (pm_dev, pm_start) = detect_pm(path);
        notes.push(format!(
            "package manager: {}",
            pm_dev.split(' ').next().unwrap_or("npm")
        ));

        let has = |k: &str| deps.contains(&k.to_string());
        // Order matters: bundler-on-Vite frameworks before bare `vite`.
        let framework = if has("next") {
            Some(("next", 3000u16))
        } else if has("nuxt") {
            Some(("nuxt", 3000))
        } else if has("@sveltejs/kit") {
            Some(("sveltekit", 5173))
        } else if has("astro") {
            Some(("astro", 4321))
        } else if has("@remix-run/dev") {
            Some(("remix", 3000))
        } else if has("@angular/core") {
            Some(("angular", 4200))
        } else if has("react-scripts") {
            Some(("cra", 3000))
        } else if has("gatsby") {
            Some(("gatsby", 8000))
        } else if has("vite") {
            Some(("vite", 5173))
        } else if has("express") || has("fastify") || has("@nestjs/core") {
            Some(("node-server", 3000))
        } else {
            None
        };
        if let Some((fw, _)) = framework {
            notes.push(format!("framework signature: {fw}"));
        }

        let has_script = |k: &str| scripts.map(|s| s.contains_key(k)).unwrap_or(false);
        let is_frontend = matches!(
            framework,
            Some(("next", _))
                | Some(("vite", _))
                | Some(("remix", _))
                | Some(("nuxt", _))
                | Some(("sveltekit", _))
                | Some(("astro", _))
                | Some(("angular", _))
                | Some(("cra", _))
                | Some(("gatsby", _))
        );

        // A `dev` script is the runnable local entry point for a frontend
        // framework — and the right default. (`npm start` / `next start` would
        // need a production `npm run build` first, which trips people up.)
        if has_script("dev") {
            let (port, ready) = match framework {
                Some(("vite", p)) | Some(("sveltekit", p)) | Some(("astro", p)) => {
                    (p, Some("ready in"))
                }
                Some(("next", p)) | Some(("remix", p)) | Some(("nuxt", p)) => (p, Some("Local:")),
                Some(("angular", p)) | Some(("cra", p)) => (p, Some("Compiled successfully")),
                Some(("gatsby", p)) => (p, Some("You can now view")),
                Some((_, p)) => (p, None),
                None => (3000, None),
            };
            services.push(ServiceConfig {
                name: "web".to_string(),
                cwd: ".".to_string(),
                command: pm_dev.to_string(),
                port: Some(port),
                env: port_env(),
                depends_on: vec![],
                health_check: Some(HealthCheck::Tcp),
                ready_log_pattern: ready.map(|s| s.to_string()),
            });
            notes.push(format!("`{pm_dev}` → web service (port {port})"));
        }

        // Only treat `npm start` as a runnable service when it's a plain backend
        // (no dev server) — for a frontend framework it needs a build first, so
        // we keep the default to `npm run dev`.
        let start_runnable = has_script("start") && !(is_frontend && has_script("dev"));
        if start_runnable {
            let server_port = match framework {
                Some(("node-server", p)) => p,
                _ => 3000,
            };
            services.push(ServiceConfig {
                name: "server".to_string(),
                cwd: ".".to_string(),
                command: pm_start.to_string(),
                port: Some(server_port),
                env: port_env(),
                depends_on: vec![],
                health_check: Some(HealthCheck::Http {
                    path: "/".to_string(),
                    expect: Some("2xx-3xx".to_string()),
                }),
                ready_log_pattern: None,
            });
            notes.push(format!("`{pm_start}` → server service"));
        } else if has_script("start") && is_frontend {
            notes.push(format!(
                "`{pm_start}` needs a build first — defaulting to `{pm_dev}`"
            ));
        } else if !has_script("start") && !has_script("dev") {
            if let Some(entry) = ["server.js", "index.js", "app.js", "main.js"]
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
    }

    // ---- monorepo (note only; high false-positive to auto-expand) --------
    if path.join("pnpm-workspace.yaml").exists()
        || read_json(&pkg_path)
            .map(|p| p.get("workspaces").is_some())
            .unwrap_or(false)
    {
        notes.push(
            "monorepo workspaces detected — register sub-packages individually".to_string(),
        );
    }

    // ---- non-JS frameworks (only when package.json produced nothing) -----
    if services.is_empty() {
        detect_non_js(path, &mut services, &mut notes);
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

    // ---- static site fallback (root index.html only) --------------------
    if services.is_empty() && path.join("index.html").exists() {
        services.push(ServiceConfig {
            name: "web".to_string(),
            cwd: ".".to_string(),
            command: "python3 -m http.server ${PORT}".to_string(),
            port: Some(8080),
            env: port_env(),
            depends_on: vec![],
            health_check: Some(HealthCheck::Http {
                path: "/".to_string(),
                expect: Some("2xx-3xx".to_string()),
            }),
            ready_log_pattern: None,
        });
        notes.push("static index.html → python3 -m http.server".to_string());
    }

    if services.is_empty() {
        notes.push("no recognizable services — propose a manual config".to_string());
    }

    // ---- profiles --------------------------------------------------------
    let mut profiles: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let names: Vec<String> = services.iter().map(|s| s.name.clone()).collect();
    // Prefer the dev/web server as the default — it runs without a build step.
    if names.iter().any(|n| n == "web") {
        profiles.insert("default".to_string(), vec!["web".to_string()]);
    } else if names.iter().any(|n| n == "server") {
        profiles.insert("default".to_string(), vec!["server".to_string()]);
    } else if let Some(first) = names.first() {
        profiles.insert("default".to_string(), vec![first.clone()]);
    }
    if names.len() > 1 {
        profiles.insert("dev".to_string(), names.clone());
    }

    Detection {
        proposed: AppConfig {
            name,
            root: path.to_string_lossy().into_owned(),
            services,
            profiles,
            auto_restart: false,
        },
        notes,
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// JS package manager → its (dev, start) command, inferred from the lockfile.
fn detect_pm(path: &Path) -> (&'static str, &'static str) {
    if path.join("pnpm-lock.yaml").exists() {
        ("pnpm dev", "pnpm start")
    } else if path.join("yarn.lock").exists() {
        ("yarn dev", "yarn start")
    } else if path.join("bun.lockb").exists() || path.join("bun.lock").exists() {
        ("bun run dev", "bun run start")
    } else {
        ("npm run dev", "npm start")
    }
}

/// Detect a non-JS backend (Python/Go/Rails). Called only when package.json
/// yielded no services. Each match is corroborated by a marker file + deps so an
/// incidental file doesn't trigger a false positive; ports are distinct to avoid
/// collisions. First match wins (returns early).
fn detect_non_js(path: &Path, services: &mut Vec<ServiceConfig>, notes: &mut Vec<String>) {
    // Django — the `manage.py` marker is unambiguous.
    if path.join("manage.py").exists() {
        services.push(ServiceConfig {
            name: "web".into(),
            cwd: ".".into(),
            command: "python manage.py runserver 0.0.0.0:${PORT}".into(),
            port: Some(8000),
            env: port_env(),
            depends_on: vec![],
            health_check: Some(HealthCheck::Http {
                path: "/".into(),
                expect: Some("2xx-3xx".into()),
            }),
            ready_log_pattern: Some("Starting development server".into()),
        });
        notes.push("Django (manage.py) → runserver".into());
        return;
    }

    // Python deps → FastAPI / Flask.
    let mut py = String::new();
    for f in ["requirements.txt", "pyproject.toml"] {
        if let Some(s) = read_text(&path.join(f)) {
            py.push_str(&s);
            py.push('\n');
        }
    }
    let py = py.to_lowercase();
    if py.contains("uvicorn") || py.contains("fastapi") {
        let module = ["main.py", "app.py", "app/main.py"]
            .iter()
            .find(|f| path.join(f).exists())
            .map(|f| f.trim_end_matches(".py").replace('/', "."))
            .unwrap_or_else(|| "main".into());
        services.push(ServiceConfig {
            name: "web".into(),
            cwd: ".".into(),
            command: format!("uvicorn {module}:app --reload --port ${{PORT}}"),
            port: Some(8001),
            env: port_env(),
            depends_on: vec![],
            health_check: Some(HealthCheck::Tcp),
            ready_log_pattern: Some("Application startup complete".into()),
        });
        notes.push("FastAPI/uvicorn → web service".into());
        return;
    }
    let flask_app = read_text(&path.join("app.py"))
        .map(|s| s.contains("Flask("))
        .unwrap_or(false);
    if py.contains("flask") || flask_app {
        services.push(ServiceConfig {
            name: "web".into(),
            cwd: ".".into(),
            command: "flask run --port ${PORT}".into(),
            port: Some(5000),
            env: port_env(),
            depends_on: vec![],
            health_check: Some(HealthCheck::Http {
                path: "/".into(),
                expect: Some("2xx-3xx".into()),
            }),
            ready_log_pattern: Some("Running on".into()),
        });
        notes.push("Flask → flask run".into());
        return;
    }

    // Go — port is unknown, leave it None so the allocator doesn't fight it.
    if path.join("go.mod").exists() {
        services.push(ServiceConfig {
            name: "server".into(),
            cwd: ".".into(),
            command: "go run .".into(),
            port: None,
            env: BTreeMap::new(),
            depends_on: vec![],
            health_check: Some(HealthCheck::Process),
            ready_log_pattern: None,
        });
        notes.push("Go module (go.mod) → go run .".into());
        return;
    }

    // Rails — Gemfile plus a Rails-specific marker.
    if path.join("Gemfile").exists()
        && (path.join("bin/rails").exists() || path.join("config/application.rb").exists())
    {
        services.push(ServiceConfig {
            name: "web".into(),
            cwd: ".".into(),
            command: "bin/rails server -p ${PORT}".into(),
            port: Some(3000),
            env: port_env(),
            depends_on: vec![],
            health_check: Some(HealthCheck::Http {
                path: "/".into(),
                expect: Some("2xx-3xx".into()),
            }),
            ready_log_pattern: Some("Listening on".into()),
        });
        notes.push("Rails (Gemfile) → bin/rails server".into());
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp project dir we can drop marker files into. Auto-cleaned on drop.
    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!("harbor-detect-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }
        fn write(&self, name: &str, body: &str) -> &Self {
            let p = self.0.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, body).unwrap();
            self
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn svc<'a>(d: &'a Detection, name: &str) -> &'a ServiceConfig {
        d.proposed.services.iter().find(|s| s.name == name).expect("service present")
    }

    #[test]
    fn detects_sveltekit_with_pnpm() {
        let t = Tmp::new("svelte");
        t.write(
            "package.json",
            r#"{"scripts":{"dev":"vite dev"},"devDependencies":{"@sveltejs/kit":"2"}}"#,
        )
        .write("pnpm-lock.yaml", "");
        let d = detect(&t.0);
        let web = svc(&d, "web");
        assert_eq!(web.command, "pnpm dev"); // package-manager aware
        assert_eq!(web.port, Some(5173)); // sveltekit default
    }

    #[test]
    fn detects_django_only_when_js_empty() {
        let t = Tmp::new("django");
        t.write("manage.py", "# django").write("requirements.txt", "Django==5.0");
        let d = detect(&t.0);
        let web = svc(&d, "web");
        assert!(web.command.contains("manage.py runserver"));
        assert_eq!(web.port, Some(8000));
    }

    #[test]
    fn detects_go_module() {
        let t = Tmp::new("go");
        t.write("go.mod", "module example.com/x\n");
        let d = detect(&t.0);
        let s = svc(&d, "server");
        assert_eq!(s.command, "go run .");
        assert_eq!(s.port, None); // unknown port → leave to the app
    }

    #[test]
    fn static_index_html_fallback_only_at_root() {
        let t = Tmp::new("static");
        t.write("index.html", "<h1>hi</h1>");
        let d = detect(&t.0);
        assert!(svc(&d, "web").command.contains("http.server"));
    }

    #[test]
    fn js_wins_over_incidental_python() {
        // A Node app that also ships a helper requirements.txt must NOT be Django/Flask.
        let t = Tmp::new("mixed");
        t.write(
            "package.json",
            r#"{"scripts":{"dev":"next dev"},"dependencies":{"next":"14"}}"#,
        )
        .write("requirements.txt", "flask\n");
        let d = detect(&t.0);
        assert_eq!(svc(&d, "web").command, "npm run dev");
        // No python service got added.
        assert!(d.proposed.services.iter().all(|s| !s.command.contains("flask")));
    }
}
