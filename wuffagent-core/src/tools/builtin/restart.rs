use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agents::types::RestartRequest;
use crate::tools::types::{FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema};

/// How long a `build_cmd` may run before it is considered to have hung. A full
/// WuffAgent rebuild can be slow, so this is deliberately generous.
const BUILD_TIMEOUT: Duration = Duration::from_secs(600);

/// A tool that asks WuffAgent to restart itself (optionally after running a
/// build command) so a freshly built binary is loaded, then resume the session.
///
/// Per-execution tool: each `Agent` with `restart_enabled` gets its own
/// instance (its own mailbox), injected in `Agent::new` exactly like the
/// per-agent `shell` and `handoff` tools. It is NOT registered in
/// `register_builtins` because it needs execution-specific state.
///
/// Calling it ends the current agent's turn: the tool (optionally) runs the
/// build, writes a [`RestartRequest`] to the per-execution mailbox,
/// `Agent::run_llm_loop` picks it up before the next LLM round, and
/// `Agent::execute` emits [`crate::types::AppEvent::RestartRequested`]. The UI
/// then saves the session, relaunches the binary, and closes the window; on
/// startup the new process reads a marker file and continues the work.
pub struct RestartTool {
    /// Per-execution mailbox consumed by `Agent::run_llm_loop`.
    mailbox: Arc<Mutex<Option<RestartRequest>>>,
}

impl RestartTool {
    pub fn new(mailbox: Arc<Mutex<Option<RestartRequest>>>) -> Self {
        Self { mailbox }
    }
}

/// Run `cmd` in the OS shell, blocking, with a timeout. Output is captured to a
/// temp log (so a long build cannot deadlock on a full pipe) and its tail is
/// returned on failure for the model to act on.
fn run_build(cmd: &str) -> Result<(), String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let log_path = std::env::temp_dir().join(format!(
        "wuffagent_restart_build_{}_{}.log",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let stdout_file = std::fs::File::create(&log_path)
        .map_err(|e| format!("failed to create build log {:?}: {}", log_path, e))?;
    let stderr_file = std::fs::OpenOptions::new()
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("failed to open build log {:?}: {}", log_path, e))?;

    let (program, args): (String, Vec<&str>) = if cfg!(windows) {
        ("powershell".to_string(), vec!["-NoProfile", "-Command", cmd])
    } else {
        ("sh".to_string(), vec!["-c", cmd])
    };

    let mut child = match std::process::Command::new(&program)
        .args(&args)
        .stdout(std::process::Stdio::from(stdout_file))
        .stderr(std::process::Stdio::from(stderr_file))
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&log_path);
            return Err(format!("failed to run build command '{}': {}", cmd, e));
        }
    };

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let tail = read_tail(&log_path, 2000);
                let _ = std::fs::remove_file(&log_path);
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("build command '{}' failed ({}) — output tail:\n{}", cmd, status, tail))
                };
            }
            Ok(None) => {
                if start.elapsed() > BUILD_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    let tail = read_tail(&log_path, 2000);
                    let _ = std::fs::remove_file(&log_path);
                    return Err(format!(
                        "build command '{}' timed out after {}s — output tail:\n{}",
                        cmd,
                        BUILD_TIMEOUT.as_secs(),
                        tail
                    ));
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&log_path);
                return Err(format!("failed to wait on build command '{}': {}", cmd, e));
            }
        }
    }
}

/// Read up to the last `max_bytes` of a file as a lossy `String` (empty if the
/// file cannot be read).
fn read_tail(path: &std::path::Path, max_bytes: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let Ok(len) = f.metadata().map(|m| m.len()) else {
        return String::new();
    };
    let start = len.saturating_sub(max_bytes as u64);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).to_string()
}

impl Tool for RestartTool {
    fn name(&self) -> &str {
        "restart"
    }

    fn description(&self) -> &str {
        "Restart WuffAgent so a newly built binary is loaded, then resume this session automatically. \
         Call it after you have made changes that require a rebuild (most useful when you are editing \
         WuffAgent's own source: build it, then restart to load the new code and pick the work back up). \
         Parameters: reason (required) — what you changed and why you are restarting, shown to the user \
         and used to resume. build_cmd (optional) — a command to run FIRST; on failure the restart is \
         skipped so you can fix it. exe_path (optional) — the binary to launch; omit to relaunch the \
         current executable. On Windows you cannot relink the running exe, so for WuffAgent itself use \
         build_cmd=\"cargo build --target-dir target/relaunch\" and exe_path=\"target/relaunch/debug/wuffagent-egui.exe\". \
         Your turn ends when you call it; WuffAgent closes and reopens, then continues the same work."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "restart".to_string(),
            description: "Restart WuffAgent (optionally after a build) and resume the session".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "reason".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Why you are restarting (what you changed); shown to the user and used to build the auto-resume turn".to_string(),
                            nullable: false,
                        },
                    );
                    map.insert(
                        "build_cmd".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Optional command to run before restarting (e.g. 'cargo build --target-dir target/relaunch'); if it fails the restart is skipped".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "exe_path".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Optional path to the binary to launch (e.g. 'target/relaunch/debug/wuffagent-egui.exe'); omit to relaunch the current executable".to_string(),
                            nullable: true,
                        },
                    );
                    map
                }),
                required: vec!["reason".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let reason: String = params
            .get("reason")
            .ok_or_else(|| ToolError::InvalidParams("reason is required".to_string()))?;
        let reason = reason.trim().to_string();
        if reason.is_empty() {
            return Err(ToolError::InvalidParams("reason must not be empty".to_string()));
        }
        let build_cmd: Option<String> = params
            .get::<String>("build_cmd")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let exe_path: Option<String> = params
            .get::<String>("exe_path")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Optionally build first (blocking — we run on spawn_blocking). On
        // failure return an error output so the model can fix the build and
        // retry; do NOT queue a restart.
        if let Some(cmd) = &build_cmd {
            if let Err(e) = run_build(cmd) {
                return Ok(ToolOutput::Error(e));
            }
        }

        {
            let mut guard = self.mailbox.lock().unwrap();
            if guard.is_some() {
                return Err(ToolError::Execution("A restart is already pending".to_string()));
            }
            *guard = Some(RestartRequest {
                reason,
                build_cmd,
                exe_path,
            });
        }

        Ok(ToolOutput::Success(serde_json::json!({
            "status": "restart_queued",
            "note": "WuffAgent will restart and resume this session. Stop now."
        })))
    }
}

#[cfg(test)]
mod tests;
