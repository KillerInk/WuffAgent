use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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

/// Returns the path to the presets JSON file.
/// Same resolution as config: executable dir first, then platform config dir.
pub fn get_presets_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("presets.json");
        }
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wuffagent")
        .join("presets.json")
}
