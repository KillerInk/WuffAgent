use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::get_wuffagent_home;
use super::Config;

/// A local connection preset.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LocalPreset {
    pub name: String,
    pub server_path: String,
    pub model_path: String,
    pub port: u16,
    pub n_gpu_layers: i32,
    pub n_ctx: u32,
    pub threads: u32,
}

/// A remote connection preset.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemotePreset {
    pub name: String,
    pub remote_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_api_key: Option<String>,
}

/// A preset is either local or remote.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Preset {
    Local(LocalPreset),
    Remote(RemotePreset),
}

impl Preset {
    pub fn name(&self) -> &str {
        match self {
            Preset::Local(p) => &p.name,
            Preset::Remote(p) => &p.name,
        }
    }

    pub fn preset_type(&self) -> &str {
        match self {
            Preset::Local(_) => "local",
            Preset::Remote(_) => "remote",
        }
    }

    /// True when the preset's connection settings exactly match `cfg` — i.e.
    /// this is the preset the app is currently running with. The settings
    /// dialog uses this to mark the active entry, so the user can tell which
    /// stored preset is in effect without reopening anything.
    pub fn matches(&self, cfg: &Config) -> bool {
        match self {
            Preset::Local(p) => {
                cfg.connection_type == super::ConnectionType::Local
                    && cfg.server_path == p.server_path
                    && cfg.model_path == p.model_path
                    && cfg.port == p.port
                    && cfg.n_gpu_layers == p.n_gpu_layers
                    && cfg.n_ctx == p.n_ctx
                    && cfg.threads == p.threads
            }
            Preset::Remote(p) => {
                cfg.connection_type == super::ConnectionType::Remote
                    && cfg.remote_url == p.remote_url
                    && cfg.remote_api_key == p.remote_api_key
            }
        }
    }
}

/// Stores all saved presets.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PresetStore {
    pub presets: Vec<Preset>,
}

impl PresetStore {
    /// Load presets from a JSON file.
    pub fn load(path: &Path) -> Result<Self, PresetError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        let store: PresetStore = serde_json::from_str(&content)?;
        Ok(store)
    }

    /// Save presets to a JSON file.
    pub fn save(&self, path: &Path) -> Result<(), PresetError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Add a new preset. Returns error if name already exists.
    pub fn add(&mut self, preset: Preset) -> Result<(), PresetError> {
        if self.presets.iter().any(|p| p.name() == preset.name()) {
            return Err(PresetError::DuplicateName(preset.name().to_string()));
        }
        self.presets.push(preset);
        Ok(())
    }

    /// Remove a preset by name.
    pub fn remove(&mut self, name: &str) -> Result<(), PresetError> {
        let pos = self
            .presets
            .iter()
            .position(|p| p.name() == name)
            .ok_or_else(|| PresetError::NotFound(name.to_string()))?;
        self.presets.remove(pos);
        Ok(())
    }

    /// Get a preset by name.
    pub fn get(&self, name: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.name() == name)
    }

    /// Apply a local preset to a Config, switching to local mode.
    pub fn apply_local(&self, name: &str, config: &mut Config) -> Result<(), PresetError> {
        let preset = self
            .get(name)
            .ok_or_else(|| PresetError::NotFound(name.to_string()))?;
        match preset {
            Preset::Local(p) => {
                config.connection_type = super::ConnectionType::Local;
                config.server_path.clone_from(&p.server_path);
                config.model_path.clone_from(&p.model_path);
                config.port = p.port;
                config.n_gpu_layers = p.n_gpu_layers;
                config.n_ctx = p.n_ctx;
                config.threads = p.threads;
                Ok(())
            }
            Preset::Remote(_) => Err(PresetError::WrongType(name.to_string())),
        }
    }

    /// Apply a remote preset to a Config, switching to remote mode.
    pub fn apply_remote(&self, name: &str, config: &mut Config) -> Result<(), PresetError> {
        let preset = self
            .get(name)
            .ok_or_else(|| PresetError::NotFound(name.to_string()))?;
        match preset {
            Preset::Remote(p) => {
                config.connection_type = super::ConnectionType::Remote;
                config.remote_url.clone_from(&p.remote_url);
                config.remote_api_key = p.remote_api_key.clone();
                Ok(())
            }
            Preset::Local(_) => Err(PresetError::WrongType(name.to_string())),
        }
    }

    /// Apply a preset by name, auto-detecting local vs remote.
    pub fn apply(&self, name: &str, config: &mut Config) -> Result<(), PresetError> {
        let preset = self
            .get(name)
            .ok_or_else(|| PresetError::NotFound(name.to_string()))?;
        match preset {
            Preset::Local(_) => self.apply_local(name, config),
            Preset::Remote(_) => self.apply_remote(name, config),
        }
    }
}

/// Error type for preset operations.
#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    #[error("Preset not found: {0}")]
    NotFound(String),
    #[error("Preset with name '{0}' already exists")]
    DuplicateName(String),
    #[error("Preset '{0}' is not a local preset")]
    WrongType(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Returns the path to the presets JSON file: `~/.wuffagent/presets.json`,
/// next to `config.json` (the app home).
///
/// Older versions stored the file next to the executable, which made presets
/// vanish whenever the exe moved — the self-restart build alone alternates
/// between `target/debug` and `target/relaunch/debug` on every restart. A
/// non-empty legacy file is migrated into the app home on first use (see
/// [`resolve_presets_path`]).
pub fn get_presets_path() -> PathBuf {
    let primary = get_wuffagent_home().join("presets.json");
    let legacy = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("presets.json")));
    resolve_presets_path(&primary, legacy.as_deref())
}

/// Pure path selection for [`get_presets_path`], extracted so every branch is
/// unit-testable without touching the real app home:
///
/// 1. The primary (app-home) file wins when it already exists.
/// 2. Otherwise a non-empty, valid-JSON legacy (exe-dir) file is COPIED to
///    the primary location (one-time migration; the legacy file is kept).
///    An empty store (`{"presets": []}`) carries no data, so the primary
///    wins without a copy.
/// 3. Otherwise the primary path is returned (created on first save).
pub(crate) fn resolve_presets_path(primary: &Path, legacy: Option<&Path>) -> PathBuf {
    if primary.exists() {
        return primary.to_path_buf();
    }
    if let Some(legacy) = legacy {
        if let Ok(content) = std::fs::read_to_string(legacy) {
            // Only a store that actually contains presets is worth migrating:
            // an empty store (`{"presets": []}`) carries no data, so the
            // (fresh) primary wins instead of copying the empty file over.
            let has_presets = serde_json::from_str::<PresetStore>(&content)
                .map(|s| !s.presets.is_empty())
                .unwrap_or(false);
            if has_presets {
                if let Some(parent) = primary.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::write(primary, &content).is_ok() {
                    tracing::info!(
                        from = %legacy.display(),
                        to = %primary.display(),
                        "Migrated legacy exe-dir presets.json to app home"
                    );
                }
            }
        }
    }
    primary.to_path_buf()
}
