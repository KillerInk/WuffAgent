//! Add/edit dialog state + field parsers (moved from `mcp_panel.rs`, C3).

use std::collections::HashMap;

use wuffagent_core::config::{McpServerConfig, McpTransport};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum TransportKind {
    Stdio,
    Http,
}

/// Editable add/edit dialog state.
pub(super) struct ServerEditState {
    /// When editing, the original name (the name field is fixed).
    pub(super) existing: Option<String>,
    pub(super) name: String,
    pub(super) transport_kind: TransportKind,
    pub(super) command: String,
    pub(super) args: String,
    pub(super) env: String,
    pub(super) working_dir: String,
    pub(super) url: String,
    pub(super) headers: String,
    pub(super) enabled: bool,
    pub(super) timeout_secs: u64,
    pub(super) allowed_tools: String,
}

impl ServerEditState {
    pub(super) fn new() -> Self {
        Self {
            existing: None,
            name: String::new(),
            transport_kind: TransportKind::Stdio,
            command: String::new(),
            args: String::new(),
            env: String::new(),
            working_dir: String::new(),
            url: String::new(),
            headers: String::new(),
            enabled: true,
            timeout_secs: 60,
            allowed_tools: String::new(),
        }
    }

    pub(super) fn from_config(cfg: McpServerConfig) -> Self {
        let mut s = Self::new();
        s.existing = Some(cfg.name.clone());
        s.name = cfg.name;
        s.enabled = cfg.enabled;
        s.timeout_secs = cfg.timeout_secs;
        s.allowed_tools = cfg.allowed_tools.join(", ");
        match cfg.transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                working_dir,
            } => {
                s.transport_kind = TransportKind::Stdio;
                s.command = command;
                s.args = args.join(" ");
                s.env = env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                s.working_dir = working_dir.unwrap_or_default();
            }
            McpTransport::Http { url, headers } => {
                s.transport_kind = TransportKind::Http;
                s.url = url;
                s.headers = headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
        s
    }

    /// Validate + build the config. None if invalid.
    pub(super) fn build(&self) -> Option<McpServerConfig> {
        let name = wuffagent_core::tools::mcp::sanitize_name_part(&self.name.trim());
        if name.is_empty() {
            return None;
        }
        let transport = match self.transport_kind {
            TransportKind::Stdio => {
                let command = self.command.trim();
                if command.is_empty() {
                    return None;
                }
                McpTransport::Stdio {
                    command: command.to_string(),
                    args: split_space_separated(&self.args),
                    env: parse_env_lines(&self.env),
                    working_dir: if self.working_dir.trim().is_empty() {
                        None
                    } else {
                        Some(self.working_dir.trim().to_string())
                    },
                }
            }
            TransportKind::Http => {
                let url = self.url.trim();
                if !url.starts_with("http") {
                    return None;
                }
                McpTransport::Http {
                    url: url.to_string(),
                    headers: parse_header_lines(&self.headers),
                }
            }
        };
        Some(McpServerConfig {
            name,
            transport,
            enabled: self.enabled,
            timeout_secs: self.timeout_secs.clamp(1, 600),
            allowed_tools: self
                .allowed_tools
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        })
    }
}

fn split_space_separated(s: &str) -> Vec<String> {
    s.split_whitespace().map(|t| t.to_string()).collect()
}

fn parse_env_lines(s: &str) -> HashMap<String, String> {
    s.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

fn parse_header_lines(s: &str) -> HashMap<String, String> {
    s.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (k, v) = line.split_once(':')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Truncate a string to at most `n` chars, appending an ellipsis if cut.
pub(super) fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
