//! `AppState` — the single `Arc`-shared core reached by both Tauri commands and
//! the MCP server (DESIGN.md §3.2: tools must act on *live* state).

use crate::model::{AppConfig, HealthCheck};
use crate::ports;
use crate::store::{McpSettings, Registry, Store};
use crate::supervisor::Supervisor;
use anyhow::Result;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AppState {
    /// Shared with the supervisor, which persists/reads `runs.json` for adoption.
    pub store: Arc<Store>,
    /// In-memory mirror of the registry; persisted on every mutation. Shared
    /// (`Arc`) with the supervisor so auto-restart reads live config at crash time.
    pub registry: Arc<RwLock<BTreeMap<String, AppConfig>>>,
    pub supervisor: Arc<Supervisor>,
    pub mcp: McpSettings,
}

impl AppState {
    pub fn new(
        store: Arc<Store>,
        registry: Arc<RwLock<BTreeMap<String, AppConfig>>>,
        supervisor: Arc<Supervisor>,
        mcp: McpSettings,
    ) -> Self {
        AppState {
            store,
            registry,
            supervisor,
            mcp,
        }
    }

    pub async fn list_configs(&self) -> Vec<AppConfig> {
        self.registry.read().await.values().cloned().collect()
    }

    pub async fn get_config(&self, name: &str) -> Option<AppConfig> {
        self.registry.read().await.get(name).cloned()
    }

    /// Insert or replace an app config, then persist the whole registry.
    pub async fn upsert(&self, cfg: AppConfig) -> Result<()> {
        validate_config(&cfg)?;
        let mut reg = self.registry.write().await;
        let mut next = reg.clone();
        next.insert(cfg.name.clone(), cfg);
        self.persist(&next)?;
        *reg = next;
        drop(reg);
        self.supervisor.notify_registry_changed();
        Ok(())
    }

    /// Atomically replace one registry key, including a rename. Validation and
    /// disk persistence happen before live state changes, so a bad edit cannot
    /// delete the prior config.
    pub async fn replace(&self, old_name: &str, cfg: AppConfig) -> Result<()> {
        validate_config(&cfg)?;
        let mut reg = self.registry.write().await;
        if !reg.contains_key(old_name) {
            anyhow::bail!("no such app: {old_name}");
        }
        if old_name != cfg.name && reg.contains_key(&cfg.name) {
            anyhow::bail!("an app named '{}' already exists", cfg.name);
        }
        let mut next = reg.clone();
        next.remove(old_name);
        next.insert(cfg.name.clone(), cfg);
        self.persist(&next)?;
        *reg = next;
        drop(reg);
        self.supervisor.notify_registry_changed();
        Ok(())
    }

    /// Approve only the exact config the person reviewed. An MCP update racing
    /// the click fails the equality precondition instead of inheriting trust.
    pub async fn approve_if_unchanged(&self, name: &str, expected: &AppConfig) -> Result<()> {
        let mut reg = self.registry.write().await;
        let current = reg
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("no such app: {name}"))?;
        if current != expected {
            anyhow::bail!("config changed while you were reviewing it; review the latest commands");
        }
        let mut next = reg.clone();
        next.get_mut(name).expect("checked above").trusted = true;
        self.persist(&next)?;
        *reg = next;
        drop(reg);
        self.supervisor.notify_registry_changed();
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> Result<bool> {
        let mut reg = self.registry.write().await;
        let mut next = reg.clone();
        let existed = next.remove(name).is_some();
        if existed {
            self.persist(&next)?;
            *reg = next;
            self.supervisor.notify_registry_changed();
        }
        Ok(existed)
    }

    /// Apply a closure to a stored config and persist. Returns false if absent.
    pub async fn mutate<F: FnOnce(&mut AppConfig)>(&self, name: &str, f: F) -> Result<bool> {
        let mut reg = self.registry.write().await;
        let mut next = reg.clone();
        let Some(cfg) = next.get_mut(name) else {
            return Ok(false);
        };
        f(cfg);
        validate_config(cfg)?;
        self.persist(&next)?;
        *reg = next;
        drop(reg);
        self.supervisor.notify_registry_changed();
        Ok(true)
    }

    fn persist(&self, reg: &BTreeMap<String, AppConfig>) -> Result<()> {
        let registry = Registry { apps: reg.clone() };
        self.store.save_registry(&registry)
    }
}

/// Validate structural and filesystem assumptions before a config reaches the
/// registry or an executable path. This is intentionally deterministic and is
/// shared by UI and MCP mutations through AppState.
pub fn validate_config(cfg: &AppConfig) -> Result<()> {
    let name = cfg.name.trim();
    if name.is_empty() {
        anyhow::bail!("app name cannot be empty");
    }
    if name != cfg.name {
        anyhow::bail!("app name cannot start or end with whitespace");
    }
    let root = Path::new(&cfg.root);
    if !root.is_absolute() {
        anyhow::bail!("app root must be an absolute path");
    }
    if !root.is_dir() {
        anyhow::bail!("app root is not a directory: {}", cfg.root);
    }
    if cfg.services.is_empty() {
        anyhow::bail!("app must define at least one service");
    }

    let mut names = HashSet::new();
    for svc in &cfg.services {
        if svc.name.trim().is_empty() {
            anyhow::bail!("service name cannot be empty");
        }
        if svc.name.trim() != svc.name {
            anyhow::bail!(
                "service name '{}' cannot start or end with whitespace",
                svc.name
            );
        }
        if !names.insert(svc.name.clone()) {
            anyhow::bail!("duplicate service name: {}", svc.name);
        }
        if svc.command.trim().is_empty() {
            anyhow::bail!("service '{}' has an empty command", svc.name);
        }
        if svc.command.contains('\0') {
            anyhow::bail!("service '{}' command contains a null byte", svc.name);
        }
        if svc.port == Some(0) || ports::pinned_port(&svc.command) == Some(0) {
            anyhow::bail!("service '{}' cannot use port 0", svc.name);
        }
        let unique_dependencies: HashSet<&str> =
            svc.depends_on.iter().map(String::as_str).collect();
        if unique_dependencies.len() != svc.depends_on.len() {
            anyhow::bail!("service '{}' contains a duplicate dependency", svc.name);
        }
        for (key, value) in &svc.env {
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                anyhow::bail!("service '{}' has an invalid environment key", svc.name);
            }
            if value.contains('\0') {
                anyhow::bail!(
                    "service '{}' environment value for '{}' contains a null byte",
                    svc.name,
                    key
                );
            }
        }
        if matches!(
            &svc.health_check,
            Some(HealthCheck::Http { .. } | HealthCheck::Tcp)
        ) && svc.port.is_none()
        {
            anyhow::bail!(
                "service '{}' needs a port for its HTTP/TCP health check",
                svc.name
            );
        }
        if matches!(
            &svc.health_check,
            Some(HealthCheck::Log { pattern }) if pattern.trim().is_empty()
        ) {
            anyhow::bail!("service '{}' has an empty log health pattern", svc.name);
        }
        if svc
            .ready_log_pattern
            .as_ref()
            .is_some_and(|pattern| pattern.trim().is_empty())
        {
            anyhow::bail!("service '{}' has an empty ready log pattern", svc.name);
        }
        let cwd = Path::new(&svc.cwd);
        let resolved = if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            root.join(cwd)
        };
        if !resolved.is_dir() {
            anyhow::bail!(
                "service '{}' working directory does not exist: {}",
                svc.name,
                resolved.display()
            );
        }
    }

    // Covers unknown dependencies and cycles.
    ports::topo_sort(&cfg.services)?;
    for (profile, services) in &cfg.profiles {
        if profile.trim().is_empty() {
            anyhow::bail!("profile name cannot be empty");
        }
        if profile.trim() != profile {
            anyhow::bail!("profile '{profile}' cannot start or end with whitespace");
        }
        if services.is_empty() {
            anyhow::bail!("profile '{profile}' cannot be empty");
        }
        let selected: HashSet<&str> = services.iter().map(String::as_str).collect();
        if selected.len() != services.len() {
            anyhow::bail!("profile '{profile}' contains a duplicate service");
        }
        for service in services {
            if !names.contains(service) {
                anyhow::bail!("profile '{profile}' references unknown service '{service}'");
            }
            let svc = cfg
                .services
                .iter()
                .find(|candidate| candidate.name == *service)
                .expect("profile reference checked above");
            for dependency in &svc.depends_on {
                if !selected.contains(dependency.as_str()) {
                    anyhow::bail!(
                        "profile '{profile}' selects '{}' but omits its dependency '{}'",
                        svc.name,
                        dependency
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ServiceConfig;

    fn config(root: String) -> AppConfig {
        AppConfig {
            name: "Test".into(),
            root,
            services: vec![ServiceConfig {
                name: "web".into(),
                cwd: ".".into(),
                command: "npm run dev".into(),
                port: Some(5173),
                env: BTreeMap::new(),
                depends_on: vec![],
                health_check: None,
                ready_log_pattern: None,
            }],
            profiles: BTreeMap::from([("default".into(), vec!["web".into()])]),
            auto_restart: false,
            trusted: true,
        }
    }

    #[test]
    fn validates_profile_refs_and_duplicate_services() {
        let root = std::env::temp_dir().to_string_lossy().into_owned();
        let mut cfg = config(root);
        assert!(validate_config(&cfg).is_ok());
        cfg.profiles.insert("broken".into(), vec!["missing".into()]);
        assert!(validate_config(&cfg).is_err());
        cfg.profiles.remove("broken");
        cfg.services.push(cfg.services[0].clone());
        assert!(validate_config(&cfg).is_err());
    }

    #[test]
    fn rejects_incomplete_profiles_and_invalid_probe_ports() {
        let root = std::env::temp_dir().to_string_lossy().into_owned();
        let mut cfg = config(root);
        cfg.services.push(ServiceConfig {
            name: "worker".into(),
            cwd: ".".into(),
            command: "npm run worker".into(),
            port: None,
            env: BTreeMap::new(),
            depends_on: vec!["web".into()],
            health_check: None,
            ready_log_pattern: None,
        });
        cfg.profiles
            .insert("worker-only".into(), vec!["worker".into()]);
        assert!(validate_config(&cfg).is_err());

        cfg.profiles
            .insert("worker-only".into(), vec!["web".into(), "worker".into()]);
        cfg.services[0].port = Some(0);
        assert!(validate_config(&cfg).is_err());

        cfg.services[0].port = None;
        cfg.services[0].health_check = Some(HealthCheck::Tcp);
        assert!(validate_config(&cfg).is_err());
    }
}
