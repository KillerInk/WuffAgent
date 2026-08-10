use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::fs;

pub use self::chat::ChatMessage;
pub use self::local::LocalConfig;
pub use self::remote::RemoteConfig;
pub use self::encryption::EncryptionSettings;
pub use paths::get_config_path;

mod chat;
mod local;
mod remote;
mod encryption;
mod paths;
#[cfg(test)]
mod tests;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ConnectionType {
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "remote")]
    Remote,
}

impl Default for ConnectionType {
    fn default() -> Self {
        Self::Local
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default)]
    pub connection_type: ConnectionType,

    // Local-mode fields
    pub server_path: String,
    pub model_path: String,
    pub port: u16,
    pub n_gpu_layers: i32,
    pub n_ctx: u32,
    pub threads: u32,

    // Remote-mode fields
    pub remote_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_api_key: Option<String>,

    // Chat settings
    pub system_prompt: String,
    pub streaming: bool,
    pub theme: String,
    #[serde(default)]
    pub auto_scroll: bool,
    pub chat_history: Vec<ChatMessage>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,

    // Internal paths (not serialized)
    #[serde(skip)]
    pub sessions_dir: PathBuf,
    #[serde(skip)]
    pub file_path: PathBuf,

    // Encryption settings
    #[serde(default)]
    pub encryption_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_password: Option<String>,
}

fn default_max_messages() -> usize {
    100
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connection_type: ConnectionType::Local,
            server_path: String::new(),
            model_path: String::new(),
            port: 8080,
            n_gpu_layers: 99,
            n_ctx: 4096,
            threads: 8,
            remote_url: String::new(),
            remote_api_key: None,
            system_prompt: String::new(),
            streaming: true,
            theme: "dark".to_string(),
            auto_scroll: true,
            chat_history: Vec::new(),
            session_id: None,
            max_messages: 100,
            sessions_dir: PathBuf::new(),
            file_path: PathBuf::new(),
            encryption_enabled: false,
            encryption_password: None,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Error> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub fn save(&self) -> Result<(), Error> {
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(&self.file_path, content)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), Error> {
        match self.connection_type {
            ConnectionType::Local => {
                if self.server_path.is_empty() {
                    return Err(Error::EmptyServerPath);
                }
                if self.model_path.is_empty() {
                    return Err(Error::EmptyModelPath);
                }
                if !Path::new(&self.server_path).exists() {
                    return Err(Error::ServerPathNotFound(self.server_path.clone()));
                }
                if !Path::new(&self.model_path).exists() {
                    return Err(Error::ModelPathNotFound(self.model_path.clone()));
                }
                if self.port < 1024 || self.port > 65535 {
                    return Err(Error::InvalidPort(self.port));
                }
                if self.threads == 0 || self.threads > 64 {
                    return Err(Error::InvalidThreads(self.threads));
                }
                if self.n_gpu_layers < 0 || self.n_gpu_layers > 99 {
                    return Err(Error::InvalidGPULayers(self.n_gpu_layers));
                }
            }
            ConnectionType::Remote => {
                if self.remote_url.is_empty() {
                    return Err(Error::EmptyRemoteUrl);
                }
                if !self.remote_url.starts_with("http://") && !self.remote_url.starts_with("https://") {
                    return Err(Error::InvalidRemoteUrl(self.remote_url.clone()));
                }
            }
        }
        Ok(())
    }

    /// Returns the base URL to use for the ChatClient.
    pub fn base_url(&self) -> String {
        match self.connection_type {
            ConnectionType::Local => format!("http://127.0.0.1:{}", self.port),
            ConnectionType::Remote => self.remote_url.clone(),
        }
    }

    /// Returns the sessions directory path.
    pub fn sessions_dir(&self) -> &PathBuf {
        &self.sessions_dir
    }

    /// Returns true if this config is in remote mode.
    pub fn is_remote(&self) -> bool {
        matches!(self.connection_type, ConnectionType::Remote)
    }

    /// Derive a 32-byte encryption key from a password using PBKDF2 (via the `chacha20poly1305` crate's key derivation).
    /// Returns None if no password is set.
    pub fn encryption_key(&self) -> Option<[u8; 32]> {
        EncryptionSettings {
            encryption_enabled: self.encryption_enabled,
            encryption_password: self.encryption_password.clone(),
        }
        .encryption_key()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Server path is empty")]
    EmptyServerPath,
    #[error("Model path is empty")]
    EmptyModelPath,
    #[error("Server path not found: {0}")]
    ServerPathNotFound(String),
    #[error("Model path not found: {0}")]
    ModelPathNotFound(String),
    #[error("Remote URL is empty")]
    EmptyRemoteUrl,
    #[error("Invalid remote URL: {0} (must start with http:// or https://)")]
    InvalidRemoteUrl(String),
    #[error("Invalid port: {0}")]
    InvalidPort(u16),
    #[error("Invalid threads: {0}")]
    InvalidThreads(u32),
    #[error("Invalid GPU layers: {0}")]
    InvalidGPULayers(i32),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
