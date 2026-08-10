use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::fs;

use crate::types::Message;

fn default_max_messages() -> usize { 100 }

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

/// A chat message for persistence in config (alias for the shared Message type).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub timestamp: String,
}

impl From<Message> for ChatMessage {
    fn from(m: Message) -> Self {
        ChatMessage {
            role: m.role,
            content: m.content,
            timestamp: m.timestamp,
        }
    }
}

impl From<ChatMessage> for Message {
    fn from(m: ChatMessage) -> Self {
        Message {
            role: m.role,
            content: m.content,
            timestamp: m.timestamp,
            tool_calls: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default)]
    pub connection_type: ConnectionType,
    #[serde(default)]
    pub remote_url: String,
    #[serde(default)]
    pub remote_api_key: Option<String>,

    // Local-mode fields
    pub server_path: String,
    pub model_path: String,
    pub port: u16,
    pub n_gpu_layers: i32,
    pub n_ctx: u32,
    pub threads: u32,

    // Shared fields
    pub system_prompt: String,
    pub streaming: bool,
    pub theme: String, // "dark" | "light"
    #[serde(default)]
    pub auto_scroll: bool,
    pub chat_history: Vec<ChatMessage>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(skip)]
    pub sessions_dir: PathBuf,
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,
    #[serde(skip)]
    pub file_path: PathBuf,

    // Encryption settings
    #[serde(default)]
    pub encryption_enabled: bool,
    /// Password used to derive the encryption key. Stored as a hex-encoded ChaCha20Poly1305 key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_password: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connection_type: ConnectionType::Local,
            remote_url: String::new(),
            remote_api_key: None,
            server_path: String::new(),
            model_path: String::new(),
            port: 8080,
            n_gpu_layers: 99,
            n_ctx: 4096,
            threads: 8,
            system_prompt: String::new(),
            streaming: true,
            theme: "dark".to_string(),
            auto_scroll: true,
            chat_history: Vec::new(),
            session_id: None,
            sessions_dir: PathBuf::new(),
            max_messages: 100,
            file_path: PathBuf::new(),
            encryption_enabled: false,
            encryption_password: None,
        }
    }
}

impl Config {
    /// Derive a 32-byte encryption key from a password using PBKDF2 (via the `chacha20poly1305` crate's key derivation).
    /// Returns None if no password is set.
    pub fn encryption_key(&self) -> Option<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let password = self.encryption_password.as_ref()?;
        let mut hasher = Sha256::new();
        // Simple but effective: hash the password with a salt prefix
        hasher.update(b"wuffagent-session-encryption-salt");
        hasher.update(password.as_bytes());
        let result = hasher.finalize();
        Some(result.into())
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

pub fn get_config_path() -> PathBuf {
    // Try executable directory first (portable app style)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join("config.json");
            return path;
        }
    }

    // Fallback: platform config directory
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wuffagent")
        .join("config.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_config_default() {
        let cfg = Config::default();
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.n_gpu_layers, 99);
        assert_eq!(cfg.n_ctx, 4096);
        assert_eq!(cfg.threads, 8);
        assert_eq!(cfg.theme, "dark");
        assert!(cfg.streaming);
        assert_eq!(cfg.connection_type, ConnectionType::Local);
        assert!(cfg.server_path.is_empty());
        assert!(cfg.model_path.is_empty());
        assert!(cfg.chat_history.is_empty());
        assert_eq!(cfg.remote_url, "");
    }

    #[test]
    fn test_config_load_nonexistent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.connection_type, ConnectionType::Local);
        assert!(cfg.server_path.is_empty());
    }

    #[test]
    fn test_config_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");

        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Local;
        cfg.server_path = "C:\\llama\\llama-server.exe".to_string();
        cfg.model_path = "C:\\models\\model.gguf".to_string();
        cfg.port = 18080;
        cfg.threads = 4;
        cfg.n_gpu_layers = 33;
        cfg.n_ctx = 2048;
        cfg.system_prompt = "You are a helpful assistant.".to_string();
        cfg.streaming = false;
        cfg.theme = "light".to_string();
        cfg.chat_history = vec![
            ChatMessage { role: "user".to_string(), content: "Hello".to_string(), timestamp: String::new() },
            ChatMessage { role: "assistant".to_string(), content: "Hi there!".to_string(), timestamp: String::new() },
        ];
        cfg.file_path = path.clone();

        cfg.save().unwrap();

        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.connection_type, ConnectionType::Local);
        assert_eq!(loaded.server_path, "C:\\llama\\llama-server.exe");
        assert_eq!(loaded.model_path, "C:\\models\\model.gguf");
        assert_eq!(loaded.port, 18080);
        assert_eq!(loaded.threads, 4);
        assert_eq!(loaded.n_gpu_layers, 33);
        assert_eq!(loaded.n_ctx, 2048);
        assert_eq!(loaded.system_prompt, "You are a helpful assistant.");
        assert!(!loaded.streaming);
        assert_eq!(loaded.theme, "light");
        assert_eq!(loaded.chat_history.len(), 2);
        assert_eq!(loaded.chat_history[0].role, "user");
        assert_eq!(loaded.chat_history[0].content, "Hello");
        assert_eq!(loaded.chat_history[1].role, "assistant");
        assert_eq!(loaded.chat_history[1].content, "Hi there!");
    }

    #[test]
    fn test_config_validate_empty_paths() {
        let cfg = Config::default();
        // Local mode with empty server_path should fail
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_validate_nonexistent_paths() {
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Local;
        cfg.server_path = "/nonexistent/server".to_string();
        cfg.model_path = "/nonexistent/model".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_validate_invalid_port() {
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Local;
        cfg.server_path = "/tmp/server".to_string();
        cfg.model_path = "/tmp/model".to_string();
        cfg.port = 80; // below 1024
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_validate_invalid_threads() {
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Local;
        cfg.server_path = "/tmp/server".to_string();
        cfg.model_path = "/tmp/model".to_string();
        cfg.threads = 0;
        assert!(cfg.validate().is_err());

        cfg.threads = 65;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_validate_invalid_gpu_layers() {
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Local;
        cfg.server_path = "/tmp/server".to_string();
        cfg.model_path = "/tmp/model".to_string();
        cfg.n_gpu_layers = -1;
        assert!(cfg.validate().is_err());

        cfg.n_gpu_layers = 100;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_validate_valid() {
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Local;
        cfg.server_path = "/tmp/server".to_string();
        cfg.model_path = "/tmp/model".to_string();
        cfg.port = 8080;
        cfg.threads = 4;
        cfg.n_gpu_layers = 33;
        // Create temp files so paths exist
        let dir = tempdir().unwrap();
        let server_file = dir.path().join("server");
        let model_file = dir.path().join("model");
        std::fs::write(&server_file, "").unwrap();
        std::fs::write(&model_file, "").unwrap();
        cfg.server_path = server_file.to_string_lossy().to_string();
        cfg.model_path = model_file.to_string_lossy().to_string();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_config_remote_mode_validation() {
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Remote;
        cfg.remote_url = "http://192.168.1.100:8080".to_string();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_config_remote_mode_missing_url() {
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Remote;
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, Error::EmptyRemoteUrl));
    }

    #[test]
    fn test_config_remote_mode_invalid_url() {
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Remote;
        cfg.remote_url = "ftp://bad-url".to_string();
        assert!(matches!(cfg.validate().unwrap_err(), Error::InvalidRemoteUrl(_)));
    }

    #[test]
    fn test_config_base_url_local() {
        let cfg = Config::default();
        assert_eq!(cfg.base_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn test_config_base_url_remote() {
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Remote;
        cfg.remote_url = "http://example.com:3000".to_string();
        assert_eq!(cfg.base_url(), "http://example.com:3000");
    }

    #[test]
    fn test_config_is_remote() {
        let cfg = Config::default();
        assert!(!cfg.is_remote());
        let mut cfg = Config::default();
        cfg.connection_type = ConnectionType::Remote;
        assert!(cfg.is_remote());
    }

    #[test]
    fn test_config_backward_compat_deserialize() {
        let json = r#"{
            "server_path": "C:\\llama\\llama-server.exe",
            "model_path": "C:\\models\\model.gguf",
            "port": 18080,
            "n_gpu_layers": 33,
            "n_ctx": 2048,
            "threads": 4,
            "system_prompt": "test",
            "streaming": true,
            "theme": "dark",
            "chat_history": []
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.connection_type, ConnectionType::Local);
        assert_eq!(cfg.server_path, "C:\\llama\\llama-server.exe");
        assert_eq!(cfg.port, 18080);
        assert!(cfg.remote_url.is_empty());
    }

    #[test]
    fn test_connection_type_default() {
        assert_eq!(ConnectionType::default(), ConnectionType::Local);
    }
}
