use serde::{Deserialize, Serialize};

/// Remote connection configuration fields extracted from Config.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteConfig {
    pub remote_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_api_key: Option<String>,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            remote_url: String::new(),
            remote_api_key: None,
        }
    }
}
