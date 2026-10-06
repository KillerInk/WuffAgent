use std::collections::HashMap;
use std::io::Read;
use std::process::{Command as StdCommand, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::tools::types::{Tool, ToolError, ToolOutput, ToolParams, ToolProgress, ToolSchema};

/// Maximum command string length to prevent oversized injection.
const MAX_COMMAND_LEN: usize = 10_000;
/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_SIZE: usize = 1_048_576;

/// UTF-8 prologue prepended to every PowerShell command. PowerShell 5.1
/// decodes child-process output with the machine's ANSI codepage (cp1252 on
/// this box), which double-encodes UTF-8 text from git/builds/redirections
/// into mojibake (em dashes → "â€"", emojis → "ðŸ‘½"). Forcing the console
/// in/out encoding to UTF-8 — plus `chcp 65001` so children inherit a UTF-8
/// console codepage — makes shell-mediated file round-trips byte-safe.
const UTF8_PROLOGUE_PS: &str =
    "[Console]::OutputEncoding = [Text.Encoding]::UTF8; [Console]::InputEncoding = [Text.Encoding]::UTF8; $OutputEncoding = [Text.Encoding]::UTF8; chcp 65001 > $null; ";
/// Same idea for cmd: switch the console to UTF-8 before running the command
/// (`&` so the command runs even if chcp is unavailable).
const UTF8_PROLOGUE_CMD: &str = "chcp 65001 >nul & ";

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

/// Whether a byte can be part of an identifier/cmdlet/option token
/// (letters, digits, `_`, `-`, `.`). Used by `detect_dangerous` so dangerous
/// patterns match whole tokens, not substrings of longer names.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// Find the first dangerous pattern in `command`, or `None`.
///
/// Matching is case-insensitive and token-aware: a pattern only triggers
/// when it sits at token edges. `is_token_byte` treats letters, digits,
/// `_`, `-` and `.` as token characters (they form identifiers, cmdlets
/// and options), so a pattern whose first/last character is a token
/// character must not be embedded in a longer token:
/// - `format C:\` is blocked, but `ImageFormat`, `Format-Hex` and
///   `ffmpeg -format mp4` are not.
/// - Patterns starting/ending with a non-token character (`> /dev/sda`,
///   `del C:\`, `$(rm`) keep plain substring semantics.
fn detect_dangerous(command: &str, patterns: &[Regex]) -> Option<&'static str> {
    let lower = command.to_lowercase();
    let bytes = lower.as_bytes();
    for (idx, re) in patterns.iter().enumerate() {
        for m in re.find_iter(&lower) {
            let pat = DANGEROUS_PATTERNS[idx].as_bytes();
            let first_is_token = is_token_byte(pat[0]);
            let last_is_token = is_token_byte(*pat.last().unwrap_or(&0));
            let before_ok =
                !first_is_token || m.start() == 0 || !is_token_byte(bytes[m.start() - 1]);
            let after_ok =
                !last_is_token || m.end() == bytes.len() || !is_token_byte(bytes[m.end()]);
            if before_ok && after_ok {
                return Some(DANGEROUS_PATTERNS[idx]);
            }
        }
    }
    None
}

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
    /// Pre-compiled `DANGEROUS_PATTERNS` (escaped to literal strings) so the
    /// hot path does not recompile them on every command.
    dangerous_regexes: Vec<Regex>,
}

impl ShellTool {
    pub fn new(config: ShellConfig) -> Self {
        let compiled_regexes: Vec<Regex> = config
            .allowed_commands
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();
        let dangerous_regexes: Vec<Regex> = DANGEROUS_PATTERNS
            .iter()
            // Lowercase the pattern as well: the command is matched against
            // its lowercased form, so the regex must be too (e.g. `del C:\`).
            .map(|p| Regex::new(&regex::escape(&p.to_lowercase())).expect("escaped pattern is a valid regex"))
            .collect();
        Self {
            config: Arc::new(config),
            compiled_regexes,
            dangerous_regexes,
        }
    }

    /// Check if a command is allowed based on the allowlist.
    fn is_command_allowed(&self, command: &str) -> Result<(), ToolError> {
        if !self.config.enabled {
            return Err(ToolError::Execution(
                "Shell commands are disabled for this agent".to_string(),
            ));
        }

        // Check dangerous patterns first (case-insensitive + token-aware, see
        // `detect_dangerous`).
        if let Some(dangerous) = detect_dangerous(command, &self.dangerous_regexes) {
            return Err(ToolError::Execution(format!(
                "Dangerous command pattern detected: {}",
                dangerous
            )));
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
            "Command not allowed. Allowed patterns: {:?}",
            self.config.allowed_commands
        )))
    }

    /// Get the shell command and arguments for the given shell type.
    ///
    /// The UTF-8 prologue is prepended (see `UTF8_PROLOGUE_*`) AFTER the
    /// allowlist/dangerous checks have run on the user's command, so the
    /// allowlist keeps matching the command as the model wrote it.
    fn get_shell_command(&self, command: &str) -> (String, Vec<String>) {
        match self.config.shell_type.as_str() {
            "powershell" => (
                "powershell.exe".to_string(),
                vec!["-Command".to_string(), format!("{UTF8_PROLOGUE_PS}{command}")],
            ),
            "cmd" => (
                "cmd.exe".to_string(),
                vec!["/C".to_string(), format!("{UTF8_PROLOGUE_CMD}{command}")],
            ),
            "bash" => (
                "bash".to_string(),
                vec!["-c".to_string(), command.to_string()],
            ),
            _ => (
                "powershell.exe".to_string(),
                vec!["-Command".to_string(), format!("{UTF8_PROLOGUE_PS}{command}")],
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

    /// Execute a command synchronously with a timeout, streaming output lines
    /// to `progress` (throttled to ~10/s) while it runs.
    ///
    /// The child is spawned with piped stdout/stderr; two reader threads push
    /// chunks over a channel while this loop accumulates the full (capped)
    /// output and keeps a rolling window of recent lines for the progress
    /// display. Semantics of the returned `Output` match `cmd.output()`.
    ///
    /// `cancel` is the per-call token (the live tool card's Stop button):
    /// when it fires, the process is killed and `wait()` reaps it (no
    /// zombie) — the error tells the model the user stopped the command.
    fn run_command(
        shell_cmd: &str,
        shell_args: &[String],
        working_dir: Option<&str>,
        timeout_ms: u64,
        progress: Option<&(dyn Fn(&str) + Send + Sync)>,
        cancel: &CancellationToken,
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
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Execution(format!("Command execution failed: {}", e)))?;

        let stdout_pipe = child.stdout.take().expect("stdout is piped");
        let stderr_pipe = child.stderr.take().expect("stderr is piped");

        // (stream id: 0 = stdout, 1 = stderr, chunk bytes)
        let (tx, rx) = mpsc::channel::<(u8, Vec<u8>)>();
        fn spawn_reader<R: Read + Send + 'static>(
            pipe: R,
            id: u8,
            tx: mpsc::Sender<(u8, Vec<u8>)>,
        ) -> thread::JoinHandle<()> {
            thread::spawn(move || {
                let mut reader = std::io::BufReader::new(pipe);
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            if tx.send((id, buf[..n].to_vec())).is_err() {
                                break; // main loop gone (timeout) — stop reading
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
        }
        let out_handle = spawn_reader(stdout_pipe, 0, tx.clone());
        let err_handle = spawn_reader(stderr_pipe, 1, tx);

        let mut state = PipeState::default();
        let mut last_report = Instant::now();
        const REPORT_INTERVAL: Duration = Duration::from_millis(100);

        loop {
            if start.elapsed() >= timeout_duration {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ToolError::Execution(format!(
                    "Command timed out after {}ms",
                    timeout_ms
                )));
            }
            // Per-call cancellation (the live tool card's Stop button):
            // kill the process and report an error so the model can adapt
            // (e.g. break the command into smaller pieces) instead of the
            // whole run being aborted.
            if cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ToolError::Execution(
                    "Command was cancelled by the user (process stopped)".to_string(),
                ));
            }

            // Drain everything available right now.
            loop {
                match rx.try_recv() {
                    Ok((id, chunk)) => state.feed(id, &chunk),
                    Err(_) => break, // Empty or Disconnected
                }
            }

            // Report the latest tail (throttled) so the UI tool card can
            // show live output while the command runs.
            if let Some(report) = progress {
                if last_report.elapsed() >= REPORT_INTERVAL {
                    if let Some(tail) = state.take_tail(6) {
                        report(&tail);
                    }
                    last_report = Instant::now();
                }
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    // Child exited: drain the remaining output (reader threads
                    // reach EOF because the pipe write ends are closed).
                    for (id, chunk) in rx {
                        state.feed(id, &chunk);
                    }
                    let _ = out_handle.join();
                    let _ = err_handle.join();
                    return Ok(std::process::Output {
                        status,
                        stdout: state.stdout,
                        stderr: state.stderr,
                    });
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(e) => {
                    let _ = child.kill();
                    return Err(ToolError::Execution(format!(
                        "Failed to wait for command: {}",
                        e
                    )));
                }
            }
        }
    }
}

/// Accumulated command output plus a rolling window of recent output lines
/// for live progress display (latest-tail semantics).
#[derive(Default)]
struct PipeState {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Partial last line per stream (line splitting across chunk boundaries).
    stdout_line: Vec<u8>,
    stderr_line: Vec<u8>,
    /// Most recent non-empty output lines (shared by both streams).
    recent: std::collections::VecDeque<String>,
}

impl PipeState {
    const RECENT_CAP: usize = 12;
    const LINE_CAP: usize = 400;

    fn feed(&mut self, stream: u8, chunk: &[u8]) {
        if stream == 0 {
            Self::append(
                &mut self.stdout,
                &mut self.stdout_line,
                chunk,
                &mut self.recent,
            );
        } else {
            Self::append(
                &mut self.stderr,
                &mut self.stderr_line,
                chunk,
                &mut self.recent,
            );
        }
    }

    fn append(
        buf: &mut Vec<u8>,
        line: &mut Vec<u8>,
        chunk: &[u8],
        recent: &mut std::collections::VecDeque<String>,
    ) {
        // Stop accumulating past the cap, but still drain the reader so the
        // child never blocks on a full pipe buffer.
        let room = MAX_OUTPUT_SIZE.saturating_sub(buf.len());
        let take = chunk.len().min(room);
        for &b in &chunk[..take] {
            if b == b'\n' {
                let s = String::from_utf8_lossy(line)
                    .trim_end_matches('\r')
                    .to_string();
                let s = s.trim_end().to_string();
                if !s.trim().is_empty() {
                    let s: String = s.chars().take(Self::LINE_CAP).collect();
                    recent.push_back(s);
                    while recent.len() > Self::RECENT_CAP {
                        recent.pop_front();
                    }
                }
                line.clear();
            } else {
                line.push(b);
            }
        }
        buf.extend_from_slice(&chunk[..take]);
    }

    /// The last `max_lines` recent lines (or fewer), oldest first.
    fn take_tail(&mut self, max_lines: usize) -> Option<String> {
        let n = max_lines.min(self.recent.len());
        if n == 0 {
            return None;
        }
        let lines: Vec<&str> = self
            .recent
            .iter()
            .rev()
            .take(n)
            .map(String::as_str)
            .collect();
        Some(lines.into_iter().rev().collect::<Vec<_>>().join("\n"))
    }
}

impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command (PowerShell, cmd, or bash). Use for building, running tests, git, and system operations. Do NOT use it for reading/writing/editing/searching file content — use read_file / write_file / append_file / apply_diff / replace_lines / search_content instead: shell redirection, Get-Content and command-line arguments can mangle non-ASCII text (UTF-8 output is normalized to cp65001, but Get-Content/Out-File default encodings and files without BOM are not covered)."
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
                            description: "Timeout in milliseconds (optional, uses agent default)"
                                .to_string(),
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
        self.execute_with_progress(params, &ToolProgress::none())
    }

    /// Run the command, streaming output lines to the progress sink so the
    /// UI tool card shows live command output while it runs.
    ///
    /// No cancel token (no live UI): delegates with a fresh token that
    /// never fires.
    fn execute_with_progress(
        &self,
        params: ToolParams,
        progress: &ToolProgress,
    ) -> crate::tools::types::ToolResult<ToolOutput> {
        self.execute_with_cancel(params, progress, &CancellationToken::new())
    }

    /// Same as [`Tool::execute_with_progress`], plus the per-call cancel
    /// token (the live tool card's Stop button): when it fires, the running
    /// process is killed and the call returns the "cancelled by the user"
    /// error, which the agent loop feeds back to the model.
    fn execute_with_cancel(
        &self,
        params: ToolParams,
        progress: &ToolProgress,
        cancel: &CancellationToken,
    ) -> crate::tools::types::ToolResult<ToolOutput> {
        let command: String = params
            .get("command")
            .ok_or_else(|| ToolError::InvalidParams("command is required".to_string()))?;

        // Reject overly long commands to prevent resource exhaustion.
        if command.len() > MAX_COMMAND_LEN {
            return Err(ToolError::InvalidParams(format!(
                "Command exceeds maximum length of {} characters",
                MAX_COMMAND_LEN
            )));
        }

        let working_dir: Option<String> = params.get("working_dir");
        let timeout_ms: u64 = params.get("timeout_ms").unwrap_or(self.config.timeout_ms);

        // Validate command
        self.is_command_allowed(&command)?;

        let (shell_cmd, shell_args) = self.get_shell_command(&command);
        let start = Instant::now();

        // Execute, forwarding output lines to the progress sink (if any).
        let fp: Option<&(dyn Fn(&str) + Send + Sync)> = progress.on_progress.as_deref();
        let result = Self::run_command(
            &shell_cmd,
            &shell_args,
            working_dir
                .as_deref()
                .or(self.config.working_dir.as_deref()),
            timeout_ms,
            fp,
            cancel,
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
mod tests;
