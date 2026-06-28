//! `AppState` — the single `Arc`-shared core reached by both Tauri commands and
//! the MCP server (DESIGN.md §3.2: tools must act on *live* state).

use crate::model::AppConfig;
use crate::store::{McpSettings, Registry, Store};
use crate::supervisor::Supervisor;
use anyhow::Result;
use std::collections::BTreeMap;
use tokio::sync::RwLock;

pub struct AppState {
    pub store: Store,
    /// In-memory mirror of the registry; persisted on every mutation.
    pub registry: RwLock<BTreeMap<String, AppConfig>>,
    pub supervisor: Supervisor,
    pub mcp: McpSettings,
}

impl AppState {
    pub fn new(store: Store, registry: BTreeMap<String, AppConfig>, supervisor: Supervisor, mcp: McpSettings) -> Self {
        AppState {
            store,
            registry: RwLock::new(registry),
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
        let mut reg = self.registry.write().await;
        reg.insert(cfg.name.clone(), cfg);
        self.persist(&reg)
    }

    pub async fn remove(&self, name: &str) -> Result<bool> {
        let mut reg = self.registry.write().await;
        let existed = reg.remove(name).is_some();
        if existed {
            self.persist(&reg)?;
        }
        Ok(existed)
    }

    /// Apply a closure to a stored config and persist. Returns false if absent.
    pub async fn mutate<F: FnOnce(&mut AppConfig)>(&self, name: &str, f: F) -> Result<bool> {
        let mut reg = self.registry.write().await;
        let Some(cfg) = reg.get_mut(name) else {
            return Ok(false);
        };
        f(cfg);
        self.persist(&reg)?;
        Ok(true)
    }

    fn persist(&self, reg: &BTreeMap<String, AppConfig>) -> Result<()> {
        let registry = Registry { apps: reg.clone() };
        self.store.save_registry(&registry)
    }
}
