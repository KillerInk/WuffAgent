use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::fs;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub server_path: String,
    pub model_path: String,
    pub port: u16,
    pub n_gpu_layers: i32,
    pub n_ctx: u32,
    pub threads: u32,
    pub system_prompt: String,
    pub streaming: bool,
    pub theme: String, // "dark" | "light"
    pub chat_history: Vec<ChatMessage>,
    #[serde(skip)]
    pub file_path: PathBuf,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_path: String::new(),
            model_path: String::new(),
            port: 8080,
            n_gpu_layers: 99,
            n_ctx: 4096,
            threads: 8,
            system_prompt: String::new(),
            streaming: true,
            theme: "dark".to_string(),
            chat_history: Vec::new(),
            file_path: PathBuf::new(),
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
        Ok(())
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
        assert!(cfg.server_path.is_empty());
        assert!(cfg.model_path.is_empty());
        assert!(cfg.chat_history.is_empty());
    }

    #[test]
    fn test_config_load_nonexistent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.port, 8080);
        assert!(cfg.server_path.is_empty());
    }

    #[test]
    fn test_config_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");

        let mut cfg = Config::default();
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
            ChatMessage { role: "user".to_string(), content: "Hello".to_string() },
            ChatMessage { role: "assistant".to_string(), content: "Hi there!".to_string() },
        ];
        cfg.file_path = path.clone();

        cfg.save().unwrap();

        let loaded = Config::load(&path).unwrap();
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
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_validate_nonexistent_paths() {
        let mut cfg = Config::default();
        cfg.server_path = "/nonexistent/server".to_string();
        cfg.model_path = "/nonexistent/model".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_validate_invalid_port() {
        let mut cfg = Config::default();
        cfg.server_path = "/tmp/server".to_string();
        cfg.model_path = "/tmp/model".to_string();
        cfg.port = 80; // below 1024
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_validate_invalid_threads() {
        let mut cfg = Config::default();
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
}
