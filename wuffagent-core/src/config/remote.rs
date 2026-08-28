use serde::{Deserialize, Serialize};

/// Remote connection configuration fields extracted from Config.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct RemoteConfig {
    pub remote_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_api_key: Option<String>,
}
