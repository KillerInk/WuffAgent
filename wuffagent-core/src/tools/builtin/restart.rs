use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::agents::types::RestartRequest;
use crate::tools::types::{FieldSchema, JsonSchema, Tool, ToolError, ToolOutput, ToolParams, ToolSchema};

/// How long a `build_cmd` may run before it is considered to have hung. A full
/// WuffAgent rebuild can be slow, so this is deliberately generous.
const BUILD_TIMEOUT: Duration = Duration::from_secs(600);

/// The secondary self-build target directory. A WuffAgent self-restart only
/// ever uses TWO builds — the default `cargo build` output (`target/debug`)
/// and this second copy — and alternates between them on each restart, because
/// on Windows the running exe cannot be relinked in place.
const RELAUNCH_TARGET_DIR: &str = "target/relaunch";

/// File stem of the WuffAgent UI binary (without the platform extension).
const WUFFAGENT_EXE_STEM: &str = "wuffagent-egui";

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
///
/// Self-restart with no explicit `build_cmd`/`exe_path` alternates between
/// exactly two standard builds — the default `cargo build` output
/// (`target/debug`) and a second copy (`target/relaunch`) — always building +
/// launching the OTHER of the two, since on Windows the running exe cannot be
/// relinked in place.
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
/// returned on failure for the model to act on. `cwd` (if given) is the
/// working directory; otherwise the current process directory is inherited.
fn run_build(cmd: &str, cwd: Option<&std::path::Path>) -> Result<(), String> {
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

    let mut command = std::process::Command::new(&program);
    command
        .args(&args)
        .stdout(std::process::Stdio::from(stdout_file))
        .stderr(std::process::Stdio::from(stderr_file));
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let mut child = match command.spawn() {
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

/// Walk up from `start` looking for a directory containing `Cargo.toml`
/// (the Cargo workspace root). Returns `None` if none is found.
fn find_repo_root(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Whether `path` points into the secondary self-build, i.e. contains a
/// `target/relaunch` component pair.
fn is_relaunch_build(path: &std::path::Path) -> bool {
    let comps: Vec<_> = path.components().collect();
    comps
        .windows(2)
        .any(|w| w[0].as_os_str() == "target" && w[1].as_os_str() == "relaunch")
}

/// A resolved self-restart plan: build the OTHER of the two standard builds
/// (default `cargo build` output vs. `--target-dir target/relaunch`) and
/// launch that copy, so the currently running exe is never relinked.
struct SelfRestartPlan {
    /// The cargo command that produces the target copy (run from `repo_root`).
    build_cmd: String,
    /// Absolute path of the binary to launch after the build.
    exe_path: std::path::PathBuf,
    /// Working directory for `build_cmd` (the Cargo workspace root).
    repo_root: std::path::PathBuf,
}

/// Plan a self-restart for the given current executable: pick the OTHER of the
/// two standard builds to build + launch. Returns `None` if `current_exe` is
/// not a WuffAgent UI binary we can locate inside a Cargo workspace (the
/// caller should then just relaunch the current executable, unmodified).
fn plan_self_restart(current_exe: &std::path::Path) -> Option<SelfRestartPlan> {
    if current_exe.file_stem()?.to_str()? != WUFFAGENT_EXE_STEM {
        return None;
    }
    let repo_root = find_repo_root(current_exe.parent()?)?;
    let exe_name = if cfg!(windows) {
        format!("{}.exe", WUFFAGENT_EXE_STEM)
    } else {
        WUFFAGENT_EXE_STEM.to_string()
    };
    if is_relaunch_build(current_exe) {
        // Currently running the secondary build → build + launch the default.
        Some(SelfRestartPlan {
            build_cmd: "cargo build".to_string(),
            exe_path: repo_root.join("target").join("debug").join(exe_name),
            repo_root,
        })
    } else {
        // Running the default build (or elsewhere) → build + launch the
        // secondary copy.
        Some(SelfRestartPlan {
            build_cmd: format!("cargo build --target-dir {}", RELAUNCH_TARGET_DIR),
            exe_path: repo_root
                .join("target")
                .join("relaunch")
                .join("debug")
                .join(exe_name),
            repo_root,
        })
    }
}

impl Tool for RestartTool {
    fn name(&self) -> &str {
        "restart"
    }

    fn description(&self) -> &str {
        "Restart WuffAgent so a newly built binary is loaded, then resume this session automatically. \
         Call it after you have made changes that require a rebuild (most useful when you are editing \
         WuffAgent's own source: edit, then restart to build, load the new code, and pick the work back up). \
         Parameters: reason (required) — what you changed and why you are restarting, shown to the user \
         and used to resume. build_cmd (optional) — a command to run FIRST; on failure the restart is \
         skipped so you can fix it. exe_path (optional) — the binary to launch. \
         For WuffAgent itself, omit BOTH build_cmd and exe_path: the tool then builds and launches the \
         OTHER of WuffAgent's two standard builds — the default `cargo build` output (target/debug) and a \
         second copy (target/relaunch) — alternating between them on every restart, because on Windows the \
         running exe cannot be relinked in place. Your turn ends when you call it; WuffAgent closes and \
         reopens, then continues the same work."
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
                            description: "Optional command to run before restarting; if it fails the restart is skipped. For a WuffAgent self-restart omit this (and exe_path) to auto-build the OTHER of the two standard builds".to_string(),
                            nullable: true,
                        },
                    );
                    map.insert(
                        "exe_path".to_string(),
                        FieldSchema {
                            type_name: "string".to_string(),
                            description: "Optional path to the binary to launch. For a WuffAgent self-restart omit this (and build_cmd) to auto-launch the OTHER of the two standard builds (target/debug / target/relaunch)".to_string(),
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
        let build_cmd_param: Option<String> = params
            .get::<String>("build_cmd")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let exe_path_param: Option<String> = params
            .get::<String>("exe_path")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Resolve which build to run and which binary to launch. Explicit
        // parameters win. When BOTH are omitted and we are running the
        // WuffAgent UI binary from a Cargo workspace, alternate between the
        // two standard builds (default `cargo build` output vs.
        // `target/relaunch`): on Windows the running exe cannot be relinked,
        // so we always build + launch the OTHER copy.
        let (build_cmd, exe_path, build_cwd) = match (build_cmd_param, exe_path_param) {
            (Some(cmd), exe) => (Some(cmd), exe, None),
            (None, Some(exe)) => (None, Some(exe), None),
            (None, None) => match std::env::current_exe()
                .ok()
                .as_deref()
                .and_then(plan_self_restart)
            {
                Some(plan) => (
                    Some(plan.build_cmd),
                    Some(plan.exe_path.to_string_lossy().into_owned()),
                    Some(plan.repo_root),
                ),
                None => (None, None, None),
            },
        };

        // Optionally build first (blocking — we run on spawn_blocking). On
        // failure return an error output so the model can fix the build and
        // retry; do NOT queue a restart.
        if let Some(cmd) = &build_cmd {
            if let Err(e) = run_build(cmd, build_cwd.as_deref()) {
                return Ok(ToolOutput::Error(e));
            }
        }

        // Build the success response BEFORE moving build_cmd/exe_path into the
        // request below.
        let response = serde_json::json!({
            "status": "restart_queued",
            "build_cmd": build_cmd.as_ref(),
            "exe_path": exe_path.as_ref(),
            "note": "WuffAgent will now restart and resume this session. Stop now."
        });

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

        Ok(ToolOutput::Success(response))
    }
}

#[cfg(test)]
mod tests;
