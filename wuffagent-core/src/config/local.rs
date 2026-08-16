use serde::{Deserialize, Serialize};

/// Local connection configuration fields extracted from Config.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LocalConfig {
    pub server_path: String,
    pub model_path: String,
    pub port: u16,
    pub n_gpu_layers: i32,
    pub n_ctx: u32,
    pub threads: u32,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            server_path: String::new(),
            model_path: String::new(),
            port: 8080,
            n_gpu_layers: 99,
            n_ctx: 4096,
            threads: 8,
        }
    }
}
