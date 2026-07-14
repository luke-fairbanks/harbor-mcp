//! Flat-JSON config store (DESIGN.md §12: "lean JSON for MVP").
//!
//! The central registry lives at `<app_data_dir>/registry.json` and maps app
//! name → [`AppConfig`]. A per-project `harbor.json` can be imported/exported so
//! configs are shareable and committable; the central registry is source of
//! truth.

use crate::model::{AppConfig, PersistedRun};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::io::{Read, Write};
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

/// Persisted MCP endpoint descriptor, kept beside the registry. The native
/// stdio bridge re-reads this file whenever a client sends a message, so a
/// single bridge process can follow Harbor across token and port rotations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpSettings {
    #[serde(default = "current_mcp_schema_version")]
    pub schema_version: u8,
    #[serde(default)]
    pub instance_id: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_started_at: Option<String>,
    pub token: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_executable: Option<String>,
}

fn current_mcp_schema_version() -> u8 {
    1
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

    fn bridge_version_path(&self) -> PathBuf {
        self.dir.join("harbor-mcp-bridge.version")
    }

    /// Install the signed native MCP sidecar at Harbor's stable app-support
    /// path. Existing Claude/Codex configurations already point here. A rename
    /// over the old inode lets an already-running bridge finish normally while
    /// every newly spawned client receives the updated executable.
    ///
    /// Returns `true` when the installed executable changed. The bridge has an
    /// independent version: timestamped Developer ID signatures change bytes on
    /// every app release, but an unchanged bridge must not force users to
    /// restart Claude or Codex merely to load equivalent code.
    pub fn install_bridge_from(&self, source: &Path, version: &str) -> Result<bool> {
        self.ensure_dir()?;

        if version.is_empty()
            || version.len() > 64
            || !version.bytes().all(|byte| byte.is_ascii_graphic())
        {
            anyhow::bail!("native bridge version is invalid");
        }

        let source_meta = std::fs::symlink_metadata(source)
            .with_context(|| format!("reading native bridge metadata at {}", source.display()))?;
        if !source_meta.file_type().is_file() || source_meta.file_type().is_symlink() {
            anyhow::bail!("native bridge source is not a regular file");
        }
        if !has_native_executable_header(source) {
            anyhow::bail!("native bridge source is not an executable image");
        }

        let destination = self.bridge_path();
        let destination_is_regular = std::fs::symlink_metadata(&destination)
            .map(|meta| meta.file_type().is_file() && !meta.file_type().is_symlink())
            .unwrap_or(false);
        let installed_version = std::fs::read_to_string(self.bridge_version_path()).ok();
        let bundled_requirement = code_designated_requirement(source);
        if destination_is_regular
            && has_native_executable_header(&destination)
            && installed_version.as_deref().map(str::trim) == Some(version)
            // A production build must repair a same-version unsigned developer
            // bridge. Timestamped Developer ID signatures otherwise differ on
            // every app release, so two valid signed copies are version-equal.
            && bundled_requirement.as_ref().is_some_and(|expected| {
                code_designated_requirement(&destination).as_ref() == Some(expected)
            })
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o700))?;
            }
            return Ok(false);
        }

        if destination_is_regular && files_equal(source, &destination)? {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o700))?;
            }
            self.write_private(&self.bridge_version_path(), version)?;
            return Ok(false);
        }

        let tmp = self.dir.join(format!(
            ".harbor-mcp-bridge.{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let install_result = (|| -> Result<()> {
            std::fs::copy(source, &tmp).with_context(|| "copying native MCP bridge")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700))?;
            }
            std::fs::File::open(&tmp)?.sync_all()?;
            std::fs::rename(&tmp, &destination)
                .with_context(|| "installing native MCP bridge atomically")?;
            // Persist the directory entry before setup continues. Some file
            // systems do not support directory fsync, so the executable itself
            // remains the hard requirement and this final durability nudge is
            // best-effort.
            let _ = std::fs::File::open(&self.dir).and_then(|dir| dir.sync_all());
            Ok(())
        })();
        if install_result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        install_result?;
        self.write_private(&self.bridge_version_path(), version)?;
        Ok(true)
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

fn files_equal(left: &Path, right: &Path) -> Result<bool> {
    let left_meta = std::fs::metadata(left)?;
    let right_meta = std::fs::metadata(right)?;
    if left_meta.len() != right_meta.len() {
        return Ok(false);
    }

    let mut left = std::fs::File::open(left)?;
    let mut right = std::fs::File::open(right)?;
    let mut left_buf = [0_u8; 64 * 1024];
    let mut right_buf = [0_u8; 64 * 1024];
    loop {
        let left_read = left.read(&mut left_buf)?;
        let right_read = right.read(&mut right_buf)?;
        if left_read != right_read || left_buf[..left_read] != right_buf[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn has_native_executable_header(path: &Path) -> bool {
    let mut header = [0_u8; 4];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok()
        && matches!(
            header,
            [0xca, 0xfe, 0xba, 0xbe]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xbf, 0xba, 0xfe, 0xca]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0x7f, b'E', b'L', b'F']
                | [b'M', b'Z', _, _]
        )
}

#[cfg(target_os = "macos")]
fn code_designated_requirement(path: &Path) -> Option<String> {
    let verified = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(path)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !verified {
        return None;
    }

    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(path)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    String::from_utf8(output.stderr)
        .ok()?
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("designated => ")
                .map(str::to_owned)
        })
}

#[cfg(not(target_os = "macos"))]
fn code_designated_requirement(_path: &Path) -> Option<String> {
    None
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
                schema_version: 1,
                instance_id: "test-instance".into(),
                pid: std::process::id(),
                process_started_at: None,
                token: "secret".into(),
                port: 7777,
                app_executable: Some("/Applications/Harbor.app/Contents/MacOS/harbor".into()),
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

    #[test]
    fn legacy_endpoint_descriptor_remains_readable_during_native_migration() {
        let settings: McpSettings =
            serde_json::from_str(r#"{"token":"legacy-token","port":7777}"#).unwrap();
        assert_eq!(settings.schema_version, 1);
        assert_eq!(settings.token, "legacy-token");
        assert_eq!(settings.port, 7777);
        assert!(settings.instance_id.is_empty());
        assert!(settings.app_executable.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn native_bridge_install_is_private_atomic_and_version_stable() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "harbor-native-bridge-install-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let source = root.join("bundled-bridge");
        let data = root.join("data");
        std::fs::create_dir_all(&root).unwrap();
        let signed_v1 = [b"\xcf\xfa\xed\xfe".as_slice(), b"native-v1-signature-a"].concat();
        let resigned_v1 = [b"\xcf\xfa\xed\xfe".as_slice(), b"native-v1-signature-b"].concat();
        std::fs::write(&source, &signed_v1).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();

        let store = Store::new(&data);
        assert!(store.install_bridge_from(&source, "1.0.0").unwrap());
        assert_eq!(std::fs::read(store.bridge_path()).unwrap(), signed_v1);
        assert_eq!(
            std::fs::metadata(store.bridge_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(!store.install_bridge_from(&source, "1.0.0").unwrap());

        // Unsigned development builds have no stable signing identity. Changed
        // bytes therefore fail closed and replace the installed executable.
        std::fs::write(&source, &resigned_v1).unwrap();
        assert!(store.install_bridge_from(&source, "1.0.0").unwrap());
        assert_eq!(std::fs::read(store.bridge_path()).unwrap(), resigned_v1);
        assert!(!store.install_bridge_from(&source, "1.0.0").unwrap());

        let v2 = [b"\xcf\xfa\xed\xfe".as_slice(), b"native-v2"].concat();
        std::fs::write(&source, &v2).unwrap();
        assert!(store.install_bridge_from(&source, "1.1.0").unwrap());
        assert_eq!(std::fs::read(store.bridge_path()).unwrap(), v2);
        assert_eq!(
            std::fs::read_to_string(store.bridge_version_path()).unwrap(),
            "1.1.0"
        );
        assert!(std::fs::read_dir(&data).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));

        let _ = std::fs::remove_dir_all(&root);
    }
}
