//! MCP (Model Context Protocol) server configuration.
//!
//! `config.json` may contain an `mcp_servers` array describing MCP servers
//! the app can connect to (stdio child process or Streamable HTTP). Old
//! config files without the field load with an empty list
//! (`#[serde(default)]`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

fn default_enabled() -> bool {
    true
}

fn default_timeout_secs() -> u64 {
    60
}

/// One MCP server entry in the app config.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct McpServerConfig {
    /// Unique server name; restricted to `[a-zA-Z0-9_-]` because it is part
    /// of the generated tool names.
    pub name: String,
    pub transport: McpTransport,
    /// Whether this server is auto-connected at startup and active.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Per tool-call timeout in seconds (handshake/list requests use it too).
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Allowlist of tool names exposed from this server (empty = all).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// Runtime defaults match the serde defaults (enabled, 60s timeout) so that
/// `McpServerConfig::default()` and a minimal JSON entry behave the same.
impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            transport: McpTransport::default(),
            enabled: default_enabled(),
            timeout_secs: default_timeout_secs(),
            allowed_tools: Vec::new(),
        }
    }
}

/// How the app talks to the MCP server.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum McpTransport {
    /// Spawn a local process and speak line-delimited JSON-RPC over its stdio.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default)]
        working_dir: Option<String>,
    },
    /// MCP Streamable HTTP endpoint.
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

impl Default for McpTransport {
    fn default() -> Self {
        McpTransport::Stdio {
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            working_dir: None,
        }
    }
}

impl McpServerConfig {
    /// Validate an entry for add/edit. `other_names` are the names of all
    /// other configured servers.
    pub fn validate(&self, other_names: &[String]) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Name is required".to_string());
        }
        if self
            .name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        {
            return Err("Name may only contain letters, digits, '_' and '-'".to_string());
        }
        if other_names.iter().any(|n| n == &self.name) {
            return Err(format!("A server named '{}' already exists", self.name));
        }
        match &self.transport {
            McpTransport::Stdio { command, .. } if command.trim().is_empty() => {
                return Err("Command is required for the stdio transport".to_string());
            }
            McpTransport::Http { url, .. } if url.trim().is_empty() => {
                return Err("URL is required for the HTTP transport".to_string());
            }
            _ => {}
        }
        if self.timeout_secs == 0 {
            return Err("Timeout must be at least 1 second".to_string());
        }
        Ok(())
    }

    /// Short human-readable description of the transport for UI rows.
    pub fn transport_summary(&self) -> String {
        match &self.transport {
            McpTransport::Stdio { command, args, .. } => {
                if args.is_empty() {
                    command.clone()
                } else {
                    format!("{command} {}", args.join(" "))
                }
            }
            McpTransport::Http { url, .. } => url.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_defaults() {
        let cfg: McpServerConfig =
            serde_json::from_str(r#"{"name":"fs","transport":{"Stdio":{"command":"npx"}}}"#)
                .unwrap();
        assert_eq!(cfg.name, "fs");
        assert!(cfg.enabled, "enabled defaults to true");
        assert_eq!(cfg.timeout_secs, 60, "timeout defaults to 60s");
        assert!(cfg.allowed_tools.is_empty());
        match cfg.transport {
            McpTransport::Stdio {
                args,
                env,
                working_dir,
                ..
            } => {
                assert!(args.is_empty());
                assert!(env.is_empty());
                assert_eq!(working_dir, None);
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn validate_rejects_bad_names_and_duplicates() {
        let cfg = McpServerConfig {
            name: "bad name!".to_string(),
            ..Default::default()
        };
        assert!(cfg.validate(&[]).is_err());

        let mut cfg = McpServerConfig {
            name: "ok-name".to_string(),
            ..Default::default()
        };
        match &mut cfg.transport {
            McpTransport::Stdio { command, .. } => *command = "npx".to_string(),
            _ => unreachable!(),
        }
        assert!(cfg.validate(&[]).is_ok());
        assert!(cfg.validate(&["ok-name".to_string()]).is_err());
    }

    #[test]
    fn roundtrip() {
        let mut cfg = McpServerConfig::default();
        cfg.name = "srv".to_string();
        cfg.transport = McpTransport::Http {
            url: "http://127.0.0.1:9000/mcp".to_string(),
            headers: HashMap::from([("Authorization".to_string(), "Bearer x".to_string())]),
        };
        cfg.timeout_secs = 30;
        cfg.allowed_tools = vec!["a".to_string()];
        let v = serde_json::to_value(&cfg).unwrap();
        let back: McpServerConfig = serde_json::from_value(v).unwrap();
        assert_eq!(back, cfg);
    }
}
