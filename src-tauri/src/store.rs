//! Flat-JSON config store (DESIGN.md §12: "lean JSON for MVP").
//!
//! The central registry lives at `<app_data_dir>/registry.json` and maps app
//! name → [`AppConfig`]. A per-project `harbor.json` can be imported/exported so
//! configs are shareable and committable; the central registry is source of
//! truth.

use crate::model::{AppConfig, PersistedRun};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// On-disk shape of `registry.json`.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub apps: BTreeMap<String, AppConfig>,
}

/// On-disk shape of `runs.json` — the live processes Harbor spawned, so a
/// restarted (or crashed-and-relaunched) Harbor can re-adopt the ones still
/// running instead of spawning a duplicate that fails with `EADDRINUSE`.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RunsFile {
    #[serde(default)]
    pub runs: Vec<PersistedRun>,
}

/// Persisted MCP settings (token + port), kept beside the registry.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpSettings {
    pub token: String,
    pub port: u16,
}

pub struct Store {
    dir: PathBuf,
    runs_lock: std::sync::Mutex<()>,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Store {
            dir: dir.into(),
            runs_lock: std::sync::Mutex::new(()),
        }
    }

    fn registry_path(&self) -> PathBuf {
        self.dir.join("registry.json")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.dir.join("mcp.json")
    }

    pub fn bridge_path(&self) -> PathBuf {
        self.dir.join("harbor-mcp-bridge")
    }

    fn runs_path(&self) -> PathBuf {
        self.dir.join("runs.json")
    }

    fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating app data dir {}", self.dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("securing app data dir {}", self.dir.display()))?;
        }
        Ok(())
    }

    /// App-data files may contain bearer tokens, environment values, commands,
    /// and project paths. Write them atomically with owner-only permissions.
    fn write_private(&self, path: &Path, text: &str) -> Result<()> {
        self.ensure_dir()?;
        let tmp = path.with_extension(format!(
            "{}.tmp",
            path.extension().and_then(|e| e.to_str()).unwrap_or("json")
        ));
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        drop(file);
        std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
        Ok(())
    }

    pub fn load_registry(&self) -> Result<Registry> {
        let path = self.registry_path();
        if !path.exists() {
            return Ok(Registry::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let reg: Registry =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(reg)
    }

    pub fn save_registry(&self, reg: &Registry) -> Result<()> {
        let text = serde_json::to_string_pretty(reg)?;
        let path = self.registry_path();
        self.write_private(&path, &text)
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
        let text = serde_json::to_string_pretty(s)?;
        self.write_private(&self.settings_path(), &text)
    }

    // ---- runs.json: live-process adoption records ------------------------

    pub fn load_runs(&self) -> Result<RunsFile> {
        let path = self.runs_path();
        if !path.exists() {
            return Ok(RunsFile::default());
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&text)?)
    }

    fn save_runs_unlocked(&self, f: &RunsFile) -> Result<()> {
        let text = serde_json::to_string_pretty(f)?;
        let path = self.runs_path();
        self.write_private(&path, &text)
    }

    pub fn save_runs(&self, f: &RunsFile) -> Result<()> {
        let _guard = self
            .runs_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.save_runs_unlocked(f)
    }

    /// Record (or replace) one spawned service. Keyed by `(app, service)`.
    pub fn upsert_run(&self, r: PersistedRun) -> Result<()> {
        let _guard = self
            .runs_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut f = self.load_runs()?;
        f.runs
            .retain(|e| !(e.app == r.app && e.service == r.service));
        f.runs.push(r);
        self.save_runs_unlocked(&f)
    }

    /// Drop one service's record (clean exit / adopted process found dead).
    pub fn remove_run(&self, app: &str, service: &str) -> Result<()> {
        let _guard = self
            .runs_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut f = self.load_runs()?;
        let before = f.runs.len();
        f.runs.retain(|e| !(e.app == app && e.service == service));
        if f.runs.len() == before {
            return Ok(());
        }
        self.save_runs_unlocked(&f)
    }

    /// Drop every record for an app (a deliberate Stop tears the whole app down).
    pub fn remove_app_runs(&self, app: &str) -> Result<()> {
        let _guard = self
            .runs_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut f = self.load_runs()?;
        let before = f.runs.len();
        f.runs.retain(|e| e.app != app);
        if f.runs.len() == before {
            return Ok(());
        }
        self.save_runs_unlocked(&f)
    }
}

/// Read a per-project `harbor.json` into an [`AppConfig`]. The file may omit
/// `root`; the caller supplies the directory it was read from.
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
pub fn export_harbor_json(cfg: &AppConfig, path: &Path) -> Result<()> {
    let mut value = serde_json::to_value(cfg)?;
    if let Some(obj) = value.as_object_mut() {
        // Trust is machine-local approval state and must never travel with a
        // shareable config.
        obj.remove("trusted");
    }
    let text = serde_json::to_string_pretty(&value)?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PersistedRun;

    fn rec(app: &str, svc: &str, pid: u32) -> PersistedRun {
        PersistedRun {
            app: app.into(),
            service: svc.into(),
            pid,
            port: Some(8000 + pid as u16),
            command: "node server.js".into(),
            cwd: "/tmp".into(),
            profile: Some("default".into()),
            started_at: "Mon Jun 29 14:23:01 2026".into(),
            foreign: false,
            root: None,
        }
    }

    #[test]
    fn runs_round_trip_upsert_remove() {
        let dir = std::env::temp_dir().join(format!("harbor-runs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);

        // Empty when absent.
        assert!(store.load_runs().unwrap().runs.is_empty());

        // Upsert two services of one app; upsert replaces by (app, service).
        store.upsert_run(rec("App", "web", 100)).unwrap();
        store.upsert_run(rec("App", "api", 101)).unwrap();
        store.upsert_run(rec("App", "web", 200)).unwrap(); // replaces web
        let runs = store.load_runs().unwrap().runs;
        assert_eq!(runs.len(), 2);
        let web = runs.iter().find(|r| r.service == "web").unwrap();
        assert_eq!(web.pid, 200, "upsert should replace the prior web record");

        // Remove one service.
        store.remove_run("App", "api").unwrap();
        assert_eq!(store.load_runs().unwrap().runs.len(), 1);

        // Remove whole app.
        store.upsert_run(rec("Other", "x", 300)).unwrap();
        store.remove_app_runs("App").unwrap();
        let left = store.load_runs().unwrap().runs;
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].app, "Other");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn app_data_and_mcp_token_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("harbor-private-store-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(&dir);
        store
            .save_settings(&McpSettings {
                token: "secret".into(),
                port: 7777,
            })
            .unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(store.settings_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
