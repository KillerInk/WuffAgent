# Phase 2: Config Management (Rust)

## Status: Pending

---

### Step 2.1: Config Struct and Defaults

**Objective**: Define configuration data structure with serde.

**Tasks**:
- Create `src/config/mod.rs`
- Define `Config` struct with `#[derive(Serialize, Deserialize, Clone)]`
- Implement `default_config()` function
- Implement `load()` with JSON deserialization
- Implement `save()` with JSON serialization
- Implement `validate()` with field validation

```rust
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
    #[serde(skip)]
    pub file_path: PathBuf,
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
    // Use the directory where the binary/executable is located
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("config.json");
        }
    }
    // Fallback to config directory
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wuffagent")
        .join("config.json")
}
```

**Success Criteria**:
- Config loads from file
- `save()` writes valid JSON
- `validate()` catches errors (empty paths, invalid port, etc.)

**Dependencies**: Step 1.1 (module exists)

---

### Step 2.2: Config File Location

**Objective**: Config file location strategy.

**Tasks**:
- Implement `get_config_path()` using multiple strategies:
  1. Directory of executable (preferred for portable apps)
  2. Platform config directory as fallback
- Handle platform differences

```rust
pub fn get_config_path() -> PathBuf {
    // Try executable directory first (portable app style)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join("config.json");
            if path.exists() {
                return path;
            }
            // If it doesn't exist yet, we'll create it there
            return path;
        }
    }
    
    // Fallback: platform config directory
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wuffagent")
        .join("config.json")
}
```

**Success Criteria**:
- Config file path is correct on each platform

**Dependencies**: Step 2.1

---

### Step 2.3: Config Persistence

**Objective**: Persistent settings between sessions.

**Tasks**:
- Implement file read/write logic
- Handle file not found gracefully
- Return default config when file missing
- Create config directory if needed

```rust
impl Config {
    pub fn load() -> Result<Self, Error> {
        let path = get_config_path();
        if !path.exists() {
            // Create default config and save it
            let config = Self::default();
            config.save()?;
            return Ok(config);
        }
        Self::load(&path)
    }
}
```

**Success Criteria**:
- Config file is created on first run
- Existing config is loaded on subsequent runs

**Dependencies**: Step 2.1

---

## Files Created:
- `src/config/mod.rs`

## Dependencies on other phases:
- Phase 1 (Cargo.toml, module exists)
- Phase 3 (Server Manager needs config)
- Phase 4 (Chat Client needs base URL from config)

## Review Notes:
- Config file uses serde with JSON serialization
- Config location prefers executable directory for portable deployment
- Validation checks file existence for server_path, model_path
- Port validation: 1024-65535
- Threads validation: 1-64
- GPU layers validation: 0-99 (0 = CPU only, 99 = all layers to GPU)
- Theme validation: "dark", "light"
- Uses `thiserror` for error types (add to Cargo.toml if needed)
