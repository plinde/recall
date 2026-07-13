use crate::parser::SessionLocator;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Tracks independently indexable sessions by stable locator identity.
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexState {
    pub indexed_sessions: HashMap<String, SessionState>,
    pub version: u32,
    #[serde(skip)]
    rebuild_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub fingerprint: String,
}

impl Default for IndexState {
    fn default() -> Self {
        Self {
            indexed_sessions: HashMap::new(),
            version: Self::CURRENT_VERSION,
            rebuild_required: false,
        }
    }
}

impl IndexState {
    const CURRENT_VERSION: u32 = 2;

    pub fn load(state_path: &Path) -> Result<Self> {
        if !state_path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(state_path).context("Failed to read state file")?;
        let version = serde_json::from_str::<serde_json::Value>(&content)
            .context("Failed to parse state file")?
            .get("version")
            .and_then(|value| value.as_u64());
        if version != Some(Self::CURRENT_VERSION as u64) {
            return Ok(Self {
                rebuild_required: true,
                ..Self::default()
            });
        }
        serde_json::from_str(&content).context("Failed to parse state file")
    }

    pub fn save(&self, state_path: &Path) -> Result<()> {
        if let Some(parent) = state_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self).context("Failed to serialize state")?;
        std::fs::write(state_path, content).context("Failed to write state file")?;
        Ok(())
    }

    pub fn needs_reindex(&self, locator: &SessionLocator) -> bool {
        self.indexed_sessions
            .get(&locator.identity)
            .is_none_or(|indexed| indexed.fingerprint != locator.fingerprint)
    }

    pub fn mark_indexed(&mut self, locator: &SessionLocator) {
        self.indexed_sessions.insert(
            locator.identity.clone(),
            SessionState {
                fingerprint: locator.fingerprint.clone(),
            },
        );
    }

    pub fn remove(&mut self, identity: &str) {
        self.indexed_sessions.remove(identity);
    }

    pub fn identities(&self) -> impl Iterator<Item = &String> {
        self.indexed_sessions.keys()
    }

    pub fn take_rebuild_required(&mut self) -> bool {
        std::mem::take(&mut self.rebuild_required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_state_requests_rebuild() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("state.json");
        std::fs::write(&path, r#"{"indexed_files":{},"version":1}"#).unwrap();

        let mut state = IndexState::load(&path).unwrap();
        assert!(state.take_rebuild_required());
        assert!(!state.take_rebuild_required());
        assert_eq!(state.version, 2);
    }
}
