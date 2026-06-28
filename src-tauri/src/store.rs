//! Flat-JSON config store (DESIGN.md §12: "lean JSON for MVP").
//!
//! The central registry lives at `<app_data_dir>/registry.json` and maps app
//! name → [`AppConfig`]. A per-project `harbor.json` can be imported/exported so
//! configs are shareable and committable; the central registry is source of
//! truth.

use crate::model::AppConfig;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// On-disk shape of `registry.json`.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub apps: BTreeMap<String, AppConfig>,
}

/// Persisted MCP settings (token + port), kept beside the registry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpSettings {
    pub token: String,
    pub port: u16,
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Store { dir: dir.into() }
    }

    fn registry_path(&self) -> PathBuf {
        self.dir.join("registry.json")
    }

    fn settings_path(&self) -> PathBuf {
        self.dir.join("mcp.json")
    }

    fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating app data dir {}", self.dir.display()))
    }

    pub fn load_registry(&self) -> Result<Registry> {
        let path = self.registry_path();
        if !path.exists() {
            return Ok(Registry::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let reg: Registry = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(reg)
    }

    pub fn save_registry(&self, reg: &Registry) -> Result<()> {
        self.ensure_dir()?;
        let text = serde_json::to_string_pretty(reg)?;
        let path = self.registry_path();
        // Write atomically-ish: temp file then rename.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))?;
        Ok(())
    }

    pub fn load_settings(&self) -> Result<Option<McpSettings>> {
        let path = self.settings_path();
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(&text)?))
    }

    pub fn save_settings(&self, s: &McpSettings) -> Result<()> {
        self.ensure_dir()?;
        let text = serde_json::to_string_pretty(s)?;
        std::fs::write(self.settings_path(), text)?;
        Ok(())
    }
}

/// Read a per-project `harbor.json` into an [`AppConfig`]. The file may omit
/// `root`; the caller supplies the directory it was read from.
#[allow(dead_code)] // wired to import/export commands in M4
pub fn import_harbor_json(path: &Path, default_root: &Path) -> Result<AppConfig> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut cfg: AppConfig =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if cfg.root.is_empty() {
        cfg.root = default_root.to_string_lossy().into_owned();
    }
    Ok(cfg)
}

/// Write an [`AppConfig`] back out as a shareable `harbor.json`.
#[allow(dead_code)] // wired to import/export commands in M4
pub fn export_harbor_json(cfg: &AppConfig, path: &Path) -> Result<()> {
    let text = serde_json::to_string_pretty(cfg)?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// The QuizletLocal seed config (DESIGN.md §10/§13). Used on first run so the
/// MVP loop is demonstrable immediately. `root` is filled in by the caller from
/// the user's home dir.
pub fn quizletlocal_seed(root: String) -> AppConfig {
    use crate::model::{HealthCheck, ServiceConfig};

    let server = ServiceConfig {
        name: "server".to_string(),
        cwd: ".".to_string(),
        command: "node server.js".to_string(),
        port: Some(4321),
        env: BTreeMap::from([("PORT".to_string(), "${PORT}".to_string())]),
        depends_on: vec![],
        health_check: Some(HealthCheck::Http {
            path: "/".to_string(),
            expect: Some("2xx-3xx".to_string()),
        }),
        ready_log_pattern: Some("running".to_string()),
    };

    // Dev profile adds a Vite `web` service that proxies the server's port.
    let web = ServiceConfig {
        name: "web".to_string(),
        cwd: ".".to_string(),
        command: "npx vite --port ${PORT} --strictPort".to_string(),
        port: Some(5173),
        env: BTreeMap::from([(
            "VITE_API_TARGET".to_string(),
            "http://127.0.0.1:${services.server.port}".to_string(),
        )]),
        depends_on: vec!["server".to_string()],
        health_check: Some(HealthCheck::Tcp),
        ready_log_pattern: Some("ready in".to_string()),
    };

    AppConfig {
        name: "QuizletLocal".to_string(),
        root,
        services: vec![server, web],
        profiles: BTreeMap::from([
            ("default".to_string(), vec!["server".to_string()]),
            (
                "dev".to_string(),
                vec!["server".to_string(), "web".to_string()],
            ),
        ]),
    }
}
