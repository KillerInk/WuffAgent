use std::collections::HashMap;
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::tools::types::{Tool, ToolError, ToolOutput, ToolParams, ToolSchema};

/// Maximum command string length to prevent oversized injection.
const MAX_COMMAND_LEN: usize = 10_000;
/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_SIZE: usize = 1_048_576;

/// Dangerous patterns that are always blocked.
/// Covers common destructive operations across bash, cmd, and PowerShell.
const DANGEROUS_PATTERNS: &[&str] = &[
    // Bash destructive patterns
    "rm -rf /",
    "rm -rf /.",
    "rm -rf /*",
    "rm -rf ~/",
    "rm -rf ~/",
    "rm -rf /usr",
    "rm -rf /etc",
    "rm -rf /var",
    "rm -rf /tmp",
    "del C:\\",
    "del /f /s /q c:\\",
    "format",
    "format c:",
    ":() { :|:& };:",
    "mkfs",
    "mkfs.ext",
    "mkfs.ntfs",
    "dd if=/dev/zero",
    "dd if=/dev/urandom",
    // Disk wipe patterns
    "> /dev/",
    "> /dev/sda",
    "> /dev/sdb",
    "> /dev/nvme",
    // PowerShell destructive patterns
    "remove-item -recurse -force",
    "rm -recurse -force",
    "rd -recurse -force",
    "rmdir -recurse -force",
    "remove-item c:\\ -recurse -force",
    "clear-recyclebin -force",
    // Command substitution (potential for injection)
    "$(rm",
    "`)rm`",
    // Additional dangerous commands
    "shutdown -h now",
    "shutdown -r now",
    "halt -f",
    "reboot -f",
    "kill -9 1",
    "kill -9 -1",
];

/// Configuration for shell command execution.
#[derive(Clone, Debug)]
pub struct ShellConfig {
    /// Allowed command patterns (raw strings).
    pub allowed_commands: Vec<String>,
    /// Shell type: "powershell", "cmd", or "bash".
    pub shell_type: String,
    /// Default timeout in milliseconds.
    pub timeout_ms: u64,
    /// Whether shell commands are enabled.
    pub enabled: bool,
    /// Working directory restriction (optional).
    pub working_dir: Option<String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            allowed_commands: Vec::new(),
            shell_type: "powershell".to_string(),
            timeout_ms: 300_000,
            enabled: false,
            working_dir: None,
        }
    }
}

/// Convert a per-agent `ShellConfig` (from the agent JSON) into the shell
/// tool's runtime config. This is what lets each agent run its own shell
/// allowlist instead of sharing one global allow-all shell.
impl From<crate::agents::config::ShellConfig> for ShellConfig {
    fn from(c: crate::agents::config::ShellConfig) -> Self {
        Self {
            allowed_commands: c.allowed_commands,
            shell_type: c.shell_type,
            timeout_ms: c.shell_timeout_ms,
            enabled: c.shell_enabled,
            working_dir: c.working_dir,
        }
    }
}

/// A tool that executes shell commands with safety controls.
pub struct ShellTool {
    config: Arc<ShellConfig>,
    compiled_regexes: Vec<Regex>,
}

impl ShellTool {
    pub fn new(config: ShellConfig) -> Self {
        let compiled_regexes: Vec<Regex> = config
            .allowed_commands
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();
        Self {
            config: Arc::new(config),
            compiled_regexes,
        }
    }

    /// Check if a command is allowed based on the allowlist.
    fn is_command_allowed(&self, command: &str) -> Result<(), ToolError> {
        if !self.config.enabled {
            return Err(ToolError::Execution(
                "Shell commands are disabled for this agent".to_string(),
            ));
        }

        // Check dangerous patterns first. Both sides are lowercased so the
        // match is case-insensitive (e.g. `del C:\` still matches the `del c:\`
        // pattern); matching on the raw command would let upper-cased variants
        // slip through.
        let lower_cmd = command.to_lowercase();
        for dangerous in DANGEROUS_PATTERNS {
            if lower_cmd.contains(dangerous.to_lowercase().as_str()) {
                return Err(ToolError::Execution(format!(
                    "Dangerous command pattern detected: {}", dangerous
                )));
            }
        }

        // If allowlist is empty, allow all non-dangerous commands
        if self.config.allowed_commands.is_empty() {
            return Ok(());
        }

        // Check against pre-compiled allowlist patterns
        for re in &self.compiled_regexes {
            if re.is_match(command) {
                return Ok(());
            }
        }

        Err(ToolError::Execution(format!(
            "Command not allowed. Allowed patterns: {:?}", self.config.allowed_commands
        )))
    }

    /// Get the shell command and arguments for the given shell type.
    fn get_shell_command(&self, command: &str) -> (String, Vec<String>) {
        match self.config.shell_type.as_str() {
            "powershell" => (
                "powershell.exe".to_string(),
                vec!["-Command".to_string(), command.to_string()],
            ),
            "cmd" => {
                ("cmd.exe".to_string(), vec!["/C".to_string(), command.to_string()])
            }
            "bash" => ("bash".to_string(), vec!["-c".to_string(), command.to_string()]),
            _ => (
                "powershell.exe".to_string(),
                vec!["-Command".to_string(), command.to_string()],
            ),
        }
    }

    /// Truncate output to max size.
    fn truncate_output(output: &str) -> String {
        let bytes = output.as_bytes();
        if bytes.len() <= MAX_OUTPUT_SIZE {
            output.to_string()
        } else {
            format!(
                "{}...[output truncated, exceeded {} bytes]",
                String::from_utf8_lossy(&bytes[..MAX_OUTPUT_SIZE]),
                MAX_OUTPUT_SIZE
            )
        }
    }

    /// Execute a command synchronously with timeout using a separate thread.
    fn run_command(
        shell_cmd: &str,
        shell_args: &[String],
        working_dir: Option<&str>,
        timeout_ms: u64,
    ) -> Result<std::process::Output, ToolError> {
        let mut cmd = StdCommand::new(shell_cmd);
        cmd.args(shell_args);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        let timeout_duration = Duration::from_millis(timeout_ms);
        let start = Instant::now();

        // Spawn a thread to run the command
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(cmd.output());
        });

        // Wait for the result or timeout, using recv_timeout to avoid busy-wait.
        loop {
            if start.elapsed() >= timeout_duration {
                return Err(ToolError::Execution(format!(
                    "Command timed out after {}ms", timeout_ms
                )));
            }

            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(output)) => return Ok(output),
                Ok(Err(e)) => {
                    return Err(ToolError::Execution(format!(
                        "Command execution failed: {}", e
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Still running — check timeout again
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ToolError::Execution(
                        "Command thread panicked".to_string(),
                    ));
                }
            }
        }
    }
}

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command (PowerShell, cmd, or bash). Use for building, running tests, and system operations."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "shell".to_string(),
            description: "Execute a shell command".to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "command".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "The shell command to execute".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "working_dir".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Working directory for the command (optional)".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "timeout_ms".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "integer".to_string(),
                            description: "Timeout in milliseconds (optional, uses agent default)".to_string(),
                            nullable: true,
                        },
                    );
                    map
                }),
                required: vec!["command".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let command: String = params
            .get("command")
            .ok_or_else(|| ToolError::InvalidParams("command is required".to_string()))?;

        // Reject overly long commands to prevent resource exhaustion.
        if command.len() > MAX_COMMAND_LEN {
            return Err(ToolError::InvalidParams(format!(
                "Command exceeds maximum length of {} characters", MAX_COMMAND_LEN
            )));
        }

        let working_dir: Option<String> = params.get("working_dir");
        let timeout_ms: u64 = params.get("timeout_ms").unwrap_or(self.config.timeout_ms);

        // Validate command
        self.is_command_allowed(&command)?;

        let (shell_cmd, shell_args) = self.get_shell_command(&command);
        let start = Instant::now();

        // Execute
        let result = Self::run_command(
            &shell_cmd,
            &shell_args,
            working_dir.as_deref().or(self.config.working_dir.as_deref()),
            timeout_ms,
        );

        match result {
            Ok(output) => {
                let duration_ms = start.elapsed().as_millis() as u64;
                let stdout = Self::truncate_output(&String::from_utf8_lossy(&output.stdout));
                let stderr = Self::truncate_output(&String::from_utf8_lossy(&output.stderr));

                Ok(ToolOutput::Success(serde_json::json!({
                    "command": command,
                    "working_dir": working_dir.or(self.config.working_dir.clone()).unwrap_or_else(|| ".".to_string()),
                    "exit_code": output.status.code().unwrap_or(-1),
                    "stdout": stdout,
                    "stderr": stderr,
                    "duration_ms": duration_ms,
                })))
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dangerous_command_blocked() {
        let config = ShellConfig {
            enabled: true,
            allowed_commands: vec![".*".to_string()], // allow all
            ..Default::default()
        };
        let tool = ShellTool::new(config);

        assert!(tool.is_command_allowed("rm -rf /").is_err());
        assert!(tool.is_command_allowed("del C:\\").is_err());
    }

    #[test]
    fn test_disabled_shell_blocks() {
        let config = ShellConfig {
            enabled: false,
            ..Default::default()
        };
        let tool = ShellTool::new(config);

        assert!(tool.is_command_allowed("echo hello").is_err());
    }

    #[test]
    fn test_allowlist_matching() {
        let config = ShellConfig {
            enabled: true,
            allowed_commands: vec!["cargo build.*".to_string(), "git.*".to_string()],
            ..Default::default()
        };
        let tool = ShellTool::new(config);

        assert!(tool.is_command_allowed("cargo build").is_ok());
        assert!(tool.is_command_allowed("git status").is_ok());
        assert!(tool.is_command_allowed("rm -rf /tmp").is_err()); // not in allowlist
    }

    #[test]
    fn test_new_dangerous_patterns() {
        let config = ShellConfig {
            enabled: true,
            allowed_commands: Vec::new(),
            ..Default::default()
        };
        let tool = ShellTool::new(config);

        // Patterns added in the security expansion
        assert!(tool.is_command_allowed("rm -rf ~/").is_err());
        assert!(tool.is_command_allowed("rm -rf /*").is_err());
        assert!(tool.is_command_allowed("> /dev/sda").is_err());
        assert!(tool.is_command_allowed("remove-item -recurse -force c:\\").is_err());
        assert!(tool.is_command_allowed("$(rm -rf /)").is_err());
        assert!(tool.is_command_allowed("shutdown -h now").is_err());
        assert!(tool.is_command_allowed("kill -9 1").is_err());
    }
}
