//! Eval definitions (2a): saved regression / golden tasks for the
//! self-improvement loop's eval harness.
//!
//! Evals live under `<wuffagent_home>/evals/<agent>.jsonl` — one JSON object
//! per line: `{id, task, expect, max_tool_calls?}`. `task` is the task the
//! agent is given headlessly; `expect` is the verification criteria the
//! verification judge checks the result against; `max_tool_calls` is an
//! optional cap on tool calls (the run is aborted if exceeded).
//!
//! Parsing is tolerant: corrupt lines are skipped (a single bad line never
//! fails the whole load). Writes are atomic (temp file + rename).

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A saved eval (golden task) for an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Eval {
    /// Stable id (slug) — the upsert / delete key.
    pub id: String,
    /// The task given to the agent headlessly.
    pub task: String,
    /// Verification criteria the judge checks the result against.
    pub expect: String,
    /// Optional cap on tool calls (the run is aborted if exceeded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<usize>,
}

impl Eval {
    /// Validate the required fields are non-empty.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("eval id must not be empty".to_string());
        }
        if self.task.trim().is_empty() {
            return Err("eval task must not be empty".to_string());
        }
        if self.expect.trim().is_empty() {
            return Err("eval `expect` (verification criteria) must not be empty".to_string());
        }
        Ok(())
    }
}

/// The eval store: a directory of per-agent `<agent>.jsonl` files.
#[derive(Debug, Clone)]
pub struct EvalStore {
    root: PathBuf,
}

impl Default for EvalStore {
    fn default() -> Self {
        Self::new(default_evals_dir())
    }
}

impl EvalStore {
    /// A store rooted at `root` (used by tests and the tools).
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The path of one agent's evals file.
    pub fn path_for(&self, agent: &str) -> PathBuf {
        self.root.join(format!("{}.jsonl", file_stem(agent)))
    }

    /// Read all evals for `agent` (tolerant: corrupt lines are skipped).
    /// A missing file simply means "no evals yet".
    pub fn list(&self, agent: &str) -> Vec<Eval> {
        let path = self.path_for(agent);
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Eval>(l).ok())
            .collect()
    }

    /// Save (upsert by `id`) an eval for `agent`.
    pub fn save(&self, agent: &str, eval: Eval) -> Result<(), String> {
        eval.validate()?;
        let mut evals = self.list(agent);
        match evals.iter_mut().find(|e| e.id == eval.id) {
            Some(slot) => *slot = eval.clone(),
            None => evals.push(eval),
        }
        self.write_agent(agent, &evals)
    }

    /// Delete an eval by `id` for `agent`. Returns `true` if it existed.
    pub fn delete(&self, agent: &str, id: &str) -> Result<bool, String> {
        let mut evals = self.list(agent);
        let before = evals.len();
        evals.retain(|e| e.id != id);
        if evals.len() == before {
            return Ok(false);
        }
        self.write_agent(agent, &evals)?;
        Ok(true)
    }

    fn write_agent(&self, agent: &str, evals: &[Eval]) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|e| format!("create evals dir: {e}"))?;
        let mut out = String::new();
        for e in evals {
            out.push_str(&serde_json::to_string(e).map_err(|e| e.to_string())?);
            out.push('\n');
        }
        let path = self.path_for(agent);
        let tmp = self.root.join(format!("{}.jsonl.tmp", file_stem(agent)));
        fs::write(&tmp, out)
            .and_then(|_| fs::rename(&tmp, &path))
            .map_err(|e| {
                let _ = fs::remove_file(&tmp);
                format!("write evals {:?}: {e}", path)
            })?;
        Ok(())
    }
}

/// Sanitize an agent name into a safe file stem (no path separators).
fn file_stem(agent: &str) -> String {
    let s: String = agent
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() {
        "agent".to_string()
    } else {
        s
    }
}

// ── Default location + test override ────────────────────────────────────────

static TEST_EVALS_DIR: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
    std::sync::OnceLock::new();

fn test_evals_dir() -> &'static std::sync::Mutex<Option<PathBuf>> {
    TEST_EVALS_DIR.get_or_init(|| std::sync::Mutex::new(None))
}

/// Set (or clear with `None`) the test override for the default evals dir.
/// Process-global: tests using it must serialize on a lock.
pub fn set_evals_dir_for_testing(path: Option<PathBuf>) {
    *test_evals_dir().lock().unwrap() = path;
}

fn default_evals_dir() -> PathBuf {
    if let Some(p) = test_evals_dir().lock().unwrap().clone() {
        return p;
    }
    crate::config::get_wuffagent_home().join("evals")
}

#[cfg(test)]
mod tests;
