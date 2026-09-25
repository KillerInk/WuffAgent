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
    /// is atomic. Overwriting is the versioning mechanism (file mtime).
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

    /// Delete a skill. Returns true when a file was removed, false when the
    /// skill did not exist.
    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let name = name.trim().to_ascii_lowercase();
        validate_skill_name(&name)?;
        let path = self.path(&name);
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
