//! Skills (procedural memory, K1): reusable named procedures that agents can
//! save, list, read, and delete.
//!
//! A skill is a markdown file under `<wuffagent_home>/skills/` with a small
//! frontmatter header:
//!
//! ```markdown
//! ---
//! name: git-rebase-workflow
//! description: How to rebase a feature branch in this repo
//! when_to_use: Before rebasing or force-pushing in a shared repo
//! ---
//! Step-by-step body...
//! ```
//!
//! The frontmatter (name + when_to_use) is injected into agent system prompts
//! (capped, see [`SkillStore::prompt_block`]) so the model can `read_skill`
//! the full body when relevant. The store is read-through on every call
//! (files are small and few); writes are atomic (temp file + rename).
//! Corrupt files are skipped rather than fatal.

use std::fs;
use std::path::{Path, PathBuf};

/// A full skill (frontmatter + body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub when_to_use: String,
    pub body: String,
    /// Last-modified timestamp of the backing file (`YYYY-MM-DD HH:MM:SS`,
    /// local time), or None when the metadata could not be read.
    pub modified_at: Option<String>,
}

/// Lightweight skill metadata for listings and prompt injection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub when_to_use: String,
}

/// Number of skills listed in the system-prompt block (keep it cheap).
const PROMPT_BLOCK_MAX_SKILLS: usize = 20;

/// 3c: maximum version-history snapshots kept per skill (oldest are pruned)
/// — mirrors `AgentManager::HISTORY_SNAPSHOTS_KEEP` (the skill/agent history
/// code paths are intentionally separate: different bricks, different file
/// layouts).
const HISTORY_KEEP: usize = 20;

/// Skill store rooted at a directory (default: `<wuffagent_home>/skills`).
#[derive(Debug, Clone)]
pub struct SkillStore {
    root: PathBuf,
}

impl Default for SkillStore {
    fn default() -> Self {
        Self::new(default_skills_dir())
    }
}

impl SkillStore {
    /// Create a store rooted at `root` (created on first save).
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.md"))
    }

    /// Save (or overwrite) a skill. The name is slug-validated (and
    /// lowercased); metadata values are flattened to single lines; the write
    /// is atomic. Overwriting an existing skill first snapshots the current
    /// file into the history dir (3c), so a bad overwrite (e.g. a wrong
    /// auto-suggested rewrite) can be rolled back via
    /// [`Self::revert_skill`].
    pub fn save(
        &self,
        name: &str,
        description: &str,
        when_to_use: &str,
        body: &str,
    ) -> Result<SkillMeta, String> {
        let name = name.trim().to_ascii_lowercase();
        validate_skill_name(&name)?;
        if body.trim().is_empty() {
            return Err("skill body must not be empty".to_string());
        }
        // 3c: snapshot the current version before it is overwritten
        // (no-op on first save).
        self.snapshot_skill(&name);
        let meta = SkillMeta {
            name: name.clone(),
            description: flatten(description),
            when_to_use: flatten(when_to_use),
        };
        let path = self.path(&name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create skills dir {:?}: {e}", parent))?;
        }
        let tmp = path.with_extension("md.tmp");
        fs::write(&tmp, render_skill(&meta, body))
            .map_err(|e| format!("failed to write skill {:?}: {e}", tmp))?;
        fs::rename(&tmp, &path)
            .map_err(|e| format!("failed to finalize skill {:?}: {e}", path))?;
        Ok(meta)
    }

    /// List skill metadata, sorted by name. Files that cannot be parsed are
    /// skipped (a corrupt skill must not break the store).
    pub fn list(&self) -> Vec<SkillMeta> {
        let mut out = Vec::new();
        let Some(names) = self.list_names() else {
            return out;
        };
        for name in names {
            if let Some(skill) = self.read(&name) {
                out.push(SkillMeta {
                    name: skill.name,
                    description: skill.description,
                    when_to_use: skill.when_to_use,
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn list_names(&self) -> Option<Vec<String>> {
        let mut names = Vec::new();
        let entries = fs::read_dir(&self.root).ok()?;
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            if let Some(stem) = file_name.strip_suffix(".md") {
                if !stem.is_empty() {
                    names.push(stem.to_string());
                }
            }
        }
        Some(names)
    }

    /// Read a full skill by name, or None when it does not exist / cannot be
    /// parsed.
    pub fn read(&self, name: &str) -> Option<Skill> {
        let name = name.trim().to_ascii_lowercase();
        if validate_skill_name(&name).is_err() {
            return None;
        }
        let path = self.path(&name);
        let content = fs::read_to_string(&path).ok()?;
        let modified_at = fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(format_timestamp);
        parse_skill(&name, &content, modified_at)
    }

    /// Delete a skill. The current file is snapshotted into the history dir
    /// first (3c), so a retired skill (e.g. by the improver's "retire"
    /// suggestion) can be restored via [`Self::revert_skill`]. Returns true
    /// when a file was removed, false when the skill did not exist.
    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let name = name.trim().to_ascii_lowercase();
        validate_skill_name(&name)?;
        let path = self.path(&name);
        // 3c: snapshot BEFORE the file disappears (no-op when missing).
        self.snapshot_skill(&name);
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("failed to delete skill {:?}: {e}", path)),
        }
    }

    /// The `═══ SKILLS ═══` block for system-prompt injection: one line per
    /// skill (name + when_to_use + description), capped at
    /// [`PROMPT_BLOCK_MAX_SKILLS`]. Empty string when the store has no
    /// skills.
    pub fn prompt_block(&self) -> String {
        let skills = self.list();
        if skills.is_empty() {
            return String::new();
        }
        let mut block = String::from(
            "\n═══ SKILLS ═══\n(Reusable procedures saved by agents. Call read_skill with a \
             name to get the full steps.)\n\n",
        );
        for skill in skills.iter().take(PROMPT_BLOCK_MAX_SKILLS) {
            let when = if skill.when_to_use.is_empty() {
                String::new()
            } else {
                format!(" — use: {}", skill.when_to_use)
            };
            block.push_str(&format!("- {}{}: {}\n", skill.name, when, skill.description));
        }
        block.push_str("══════════════════════\n");
        block
    }

    // ── 3c: version history (mirrors the AgentManager snapshot pattern) ────

    /// 3c: directory holding skill version-history snapshots:
    /// `<root>/history/`.
    fn history_dir(&self) -> PathBuf {
        self.root.join("history")
    }

    /// 3c: copy the CURRENT `<root>/<name>.md` into the history dir, keeping
    /// at most [`HISTORY_KEEP`] snapshots per skill.
    ///
    /// No-op when the skill file does not exist (nothing to roll back to).
    /// Failures are logged but never abort the caller: history is a safety
    /// net, not a correctness dependency of the save/delete itself.
    fn snapshot_skill(&self, name: &str) {
        let src = self.path(name);
        if !src.is_file() {
            return;
        }
        let hist = self.history_dir();
        if fs::create_dir_all(&hist).is_err() {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let dst = self.next_history_path(name, now);
        if let Err(e) = fs::copy(&src, &dst) {
            tracing::warn!("failed to snapshot skill '{name}' history: {e}");
            return;
        }
        tracing::info!("snapshotted skill '{name}': {dst:?}");
        self.prune_skill_history(name);
    }

    /// 3c: choose the destination path for the next snapshot of `name` at
    /// `now`. Same scheme as the agent history: the first snapshot at a
    /// timestamp uses the base name (`<name>-<now>.md`, seq 0); every later
    /// one appends `-1`, `-2`, ... where the seq is (max existing seq for
    /// this timestamp) + 1 — NOT the first free slot. A reused low seq would
    /// sort as the OLDEST snapshot and get pruned immediately.
    fn next_history_path(&self, name: &str, now: u64) -> PathBuf {
        let hist = self.history_dir();
        let base = hist.join(format!("{name}-{now}.md"));
        let prefix = format!("{name}-{now}-");
        let mut max_seq: u32 = 0;
        let mut any = false;
        if base.exists() {
            any = true;
        }
        if let Ok(entries) = fs::read_dir(&hist) {
            for entry in entries.flatten() {
                let file_name_os = entry.file_name();
                let Some(file_name) = file_name_os.to_str() else {
                    continue;
                };
                let Some(stem) = file_name.strip_suffix(".md") else {
                    continue;
                };
                let Some(tail) = stem.strip_prefix(&prefix) else {
                    continue;
                };
                if let Ok(seq) = tail.parse::<u32>() {
                    any = true;
                    if seq > max_seq {
                        max_seq = seq;
                    }
                }
            }
        }
        if !any {
            return base;
        }
        let next_seq = max_seq + 1;
        hist.join(format!("{name}-{now}-{next_seq}.md"))
    }

    /// 3c: delete the oldest snapshots of `name` beyond [`HISTORY_KEEP`].
    fn prune_skill_history(&self, name: &str) {
        let hist = self.history_dir();
        let prefix = format!("{name}-");
        let entries = match fs::read_dir(&hist) {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut snaps: Vec<(PathBuf, u64, u32)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().map(|e| e == "md").unwrap_or(false) != true {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            let Some(stem) = file_name.strip_suffix(".md") else {
                continue;
            };
            let Some(tail) = stem.strip_prefix(&prefix) else {
                continue;
            };
            if tail.is_empty() {
                continue;
            }
            let (ts, seq) = Self::history_file_order(tail);
            if ts > 0 {
                snaps.push((path, ts, seq));
            }
        }
        if snaps.len() <= HISTORY_KEEP {
            return;
        }
        snaps.sort_by_key(|(path, ts, seq)| {
            (
                *ts,
                *seq,
                path.file_name()
                    .map(|f| f.to_os_string())
                    .unwrap_or_default(),
            )
        });
        let excess = snaps.len() - HISTORY_KEEP;
        for (path, _, _) in snaps.into_iter().take(excess) {
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!("failed to prune skill history {path:?}: {e}");
            }
        }
    }

    /// Parse `(unix_ts, same_second_seq)` out of a history filename tail
    /// (the part after the `<name>-` prefix, i.e. `<unixts>` or
    /// `<unixts>-<seq>`). Mirrors `AgentManager::history_file_order`;
    /// non-numeric tails sort as (0, 0), before all real snapshots.
    fn history_file_order(ts_seq: &str) -> (u64, u32) {
        let mut parts = ts_seq.splitn(2, '-');
        let ts = parts
            .next()
            .and_then(|t| t.parse::<u64>().ok())
            .unwrap_or(0);
        let seq = parts
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        (ts, seq)
    }

    /// 3c: list the version-history snapshots of a skill, NEWEST first.
    ///
    /// Returns an empty vec when the skill has no history (missing history
    /// dir or no snapshots) — not an error.
    pub fn list_skill_history(&self, name: &str) -> Vec<PathBuf> {
        let hist = self.history_dir();
        if !hist.is_dir() {
            return Vec::new();
        }
        let name = name.trim().to_ascii_lowercase();
        let prefix = format!("{name}-");
        let mut snaps: Vec<(PathBuf, u64, u32)> = Vec::new();
        let Ok(entries) = fs::read_dir(&hist) else {
            return Vec::new();
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().map(|e| e == "md").unwrap_or(false) != true {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            let Some(stem) = file_name.strip_suffix(".md") else {
                continue;
            };
            let Some(tail) = stem.strip_prefix(&prefix) else {
                continue;
            };
            if tail.is_empty() {
                continue;
            }
            let (ts, seq) = Self::history_file_order(tail);
            if ts > 0 {
                snaps.push((path, ts, seq));
            }
        }
        snaps.sort_by_key(|(path, ts, seq)| {
            (
                *ts,
                *seq,
                path.file_name()
                    .map(|f| f.to_os_string())
                    .unwrap_or_default(),
            )
        });
        snaps.reverse(); // newest first
        snaps.into_iter().map(|(path, _, _)| path).collect()
    }

    /// 3c: restore a skill from one of its history snapshots.
    ///
    /// `snapshot` must live inside [`Self::history_dir`] and name the skill
    /// (`<name>-<unixts>[-seq].md`). The CURRENT skill file (when it exists)
    /// is itself snapshotted first, so a revert is reversible (the same
    /// rule as `AgentManager::revert_agent`). Returns the restored
    /// [`Skill`].
    pub fn revert_skill(&self, name: &str, snapshot: &Path) -> Result<Skill, String> {
        let name = name.trim().to_ascii_lowercase();
        validate_skill_name(&name)?;
        let hist = self.history_dir();
        // The snapshot must be a direct child of the history dir.
        if snapshot.parent() != Some(hist.as_path()) {
            return Err(format!(
                "history snapshot {snapshot:?} is not under the history directory {hist:?}"
            ));
        }
        let file_name = snapshot
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or_default();
        let Some(stem) = file_name.strip_suffix(".md") else {
            return Err(format!("history snapshot {snapshot:?} must be a .md file"));
        };
        let Some(rest) = stem.strip_prefix(&name).and_then(|s| s.strip_prefix('-')) else {
            return Err(format!(
                "history snapshot {snapshot:?} does not belong to skill '{name}'"
            ));
        };
        // Validate the `<unixts>[-seq]` suffix.
        let ts_ok = !rest.is_empty()
            && rest
                .split('-')
                .all(|part| !part.is_empty() && part.parse::<u64>().is_ok());
        if !ts_ok {
            return Err(format!(
                "history snapshot {snapshot:?} is not a valid history file for skill '{name}'"
            ));
        }
        if !snapshot.is_file() {
            return Err(format!("history snapshot {snapshot:?} does not exist"));
        }
        // Reverting is itself a change: snapshot the current file (when it
        // exists) so the user can go forward again.
        self.snapshot_skill(&name);
        let dst = self.path(&name);
        fs::copy(snapshot, &dst).map_err(|e| {
            format!(
                "failed to revert skill '{name}' from {snapshot:?}: {e}"
            )
        })?;
        self.read(&name)
            .ok_or_else(|| format!("reverted skill '{name}' but the file no longer parses"))
    }
}

/// A skill name is a slug: 1-64 chars of a-z, 0-9 and '-', starting with a
/// letter. (Stores lowercase before validating, so this is the canonical
/// form.)
pub fn validate_skill_name(name: &str) -> Result<(), String> {
    let chars: Vec<char> = name.chars().collect();
    if chars.is_empty() || chars.len() > 64 {
        return Err(format!(
            "invalid skill name '{name}': must be 1-64 characters"
        ));
    }
    if !chars[0].is_ascii_lowercase() {
        return Err(format!(
            "invalid skill name '{name}': must start with a letter (a-z)"
        ));
    }
    if !chars
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
    {
        return Err(format!(
            "invalid skill name '{name}': only a-z, 0-9 and '-' are allowed"
        ));
    }
    Ok(())
}

/// Flatten whitespace/newlines into single spaces (metadata is one line).
fn flatten(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Render a skill file (frontmatter + body).
fn render_skill(meta: &SkillMeta, body: &str) -> String {
    let mut s = String::new();
    s.push_str("---\n");
    s.push_str(&format!("name: {}\n", meta.name));
    s.push_str(&format!("description: {}\n", meta.description));
    s.push_str(&format!("when_to_use: {}\n", meta.when_to_use));
    s.push_str("---\n");
    s.push_str(body.trim_end());
    if !body.trim_end().is_empty() && !body.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Parse a skill file: optional `---` frontmatter (name/description/
/// when_to_use) + markdown body. Tolerant: a file without frontmatter is
/// treated as body-only (the name comes from the file stem). Returns None
/// for files whose frontmatter is not terminated (corrupt).
fn parse_skill(stem: &str, content: &str, modified_at: Option<String>) -> Option<Skill> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = content.lines();
    let first = lines.next()?.trim();
    let (description, when_to_use, body) = if first == "---" {
        let mut desc = String::new();
        let mut when = String::new();
        let mut found_close = false;
        let mut rest: Vec<&str> = Vec::new();
        for line in lines {
            if !found_close {
                if line.trim() == "---" {
                    found_close = true;
                    continue;
                }
                if let Some((k, v)) = line.split_once(':') {
                    match k.trim() {
                        "description" => desc = v.trim().to_string(),
                        "when_to_use" => when = v.trim().to_string(),
                        _ => {}
                    }
                }
            } else {
                rest.push(line);
            }
        }
        if !found_close {
            return None; // unterminated frontmatter — corrupt
        }
        (
            desc,
            when,
            rest.join("\n").trim_start_matches('\n').to_string(),
        )
    } else {
        (String::new(), String::new(), content.to_string())
    };
    Some(Skill {
        name: stem.to_string(),
        description,
        when_to_use,
        body,
        modified_at,
    })
}

fn format_timestamp(t: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(t)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

// ── Default location + test override ────────────────────────────────────────

static TEST_SKILLS_DIR: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
    std::sync::OnceLock::new();

fn test_skills_dir() -> &'static std::sync::Mutex<Option<PathBuf>> {
    TEST_SKILLS_DIR.get_or_init(|| std::sync::Mutex::new(None))
}

/// Set (or clear with `None`) the test override for the default skills dir.
/// Process-global: tests using it must serialize on a lock.
pub fn set_skills_dir_for_testing(path: Option<PathBuf>) {
    *test_skills_dir().lock().unwrap() = path;
}

fn default_skills_dir() -> PathBuf {
    if let Some(p) = test_skills_dir().lock().unwrap().clone() {
        return p;
    }
    crate::config::get_wuffagent_home().join("skills")
}

/// The `═══ SKILLS ═══` prompt block for the DEFAULT store (what
/// `build_system_prompt` injects).
pub fn build_skills_prompt_block() -> String {
    SkillStore::default().prompt_block()
}

#[cfg(test)]
mod tests;
