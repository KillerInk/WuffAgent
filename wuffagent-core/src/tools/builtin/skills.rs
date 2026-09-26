//! Skill tools (K1): save_skill / list_skills / read_skill / delete_skill.
//!
//! Skills are procedural memory — reusable named procedures stored under
//! `<wuffagent_home>/skills/` (see `crate::memory::skills`). Their
//! frontmatter is injected into agent system prompts; the full body is
//! fetched on demand with `read_skill`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::memory::skills::SkillStore;
use crate::tools::types::{
    FieldSchema, JsonSchema, Tool, ToolOutput, ToolParams, ToolResult, ToolSchema,
};

fn required_str(description: &str) -> FieldSchema {
    FieldSchema {
        type_name: "string".to_string(),
        description: description.to_string(),
        nullable: false,
    }
}

/// Tool for saving (or overwriting) a reusable procedure.
pub struct SaveSkillTool {
    store: Arc<SkillStore>,
}

impl SaveSkillTool {
    pub fn new(store: Arc<SkillStore>) -> Self {
        Self { store }
    }
}

impl Tool for SaveSkillTool {
    fn name(&self) -> &str {
        "save_skill"
    }

    fn description(&self) -> &str {
        "Save a reusable procedure (skill) as a named markdown document. Use when you complete \
         a multi-step process, workflow, or workaround that will be needed again in future \
         sessions. Overwrites an existing skill with the same name (that is the versioning \
         mechanism). Skill names are slugs: a-z, 0-9 and '-', starting with a letter."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "save_skill".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    (
                        "name".to_string(),
                        required_str("Skill name (slug: a-z, 0-9, '-', starts with a letter)"),
                    ),
                    (
                        "description".to_string(),
                        required_str("One-line description of what the skill does"),
                    ),
                    (
                        "when_to_use".to_string(),
                        required_str(
                            "Short hint of when the skill applies (shown in the system prompt \
                             so the model knows to read_skill)",
                        ),
                    ),
                    (
                        "body".to_string(),
                        required_str("The full step-by-step procedure (markdown)"),
                    ),
                ])),
                required: vec![
                    "name".to_string(),
                    "description".to_string(),
                    "when_to_use".to_string(),
                    "body".to_string(),
                ],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let name = params.get::<String>("name").unwrap_or_default();
        let description = params.get::<String>("description").unwrap_or_default();
        let when_to_use = params.get::<String>("when_to_use").unwrap_or_default();
        let body = params.get::<String>("body").unwrap_or_default();
        match self.store.save(&name, &description, &when_to_use, &body) {
            Ok(meta) => Ok(ToolOutput::success(format!(
                "Skill '{}' saved ({} chars). It now appears in the SKILLS block of agent \
                 system prompts; overwrote the previous version if one existed.",
                meta.name,
                body.trim().len()
            ))),
            Err(e) => Ok(ToolOutput::error(e)),
        }
    }
}

/// Tool for listing saved skills (metadata only — bodies stay out of context).
pub struct ListSkillsTool {
    store: Arc<SkillStore>,
}

impl ListSkillsTool {
    pub fn new(store: Arc<SkillStore>) -> Self {
        Self { store }
    }
}

impl Tool for ListSkillsTool {
    fn name(&self) -> &str {
        "list_skills"
    }

    fn description(&self) -> &str {
        "List saved skills (name, description, when_to_use). No parameters. Use it to see what \
         procedures exist before read_skill or save_skill."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_skills".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: None,
                required: vec![],
            }),
        }
    }

    fn execute(&self, _params: ToolParams) -> ToolResult<ToolOutput> {
        let skills = self.store.list();
        if skills.is_empty() {
            return Ok(ToolOutput::success(
                "No skills saved yet. Save one with save_skill (name, description, when_to_use, body).",
            ));
        }
        let mut out = String::from("Saved skills:\n");
        for s in skills {
            out.push_str(&format!("- {} — use: {}\n  {}", s.name, s.when_to_use, s.description));
        }
        Ok(ToolOutput::success(out))
    }
}

/// Tool for reading a full skill (frontmatter + body) by name.
pub struct ReadSkillTool {
    store: Arc<SkillStore>,
}

impl ReadSkillTool {
    pub fn new(store: Arc<SkillStore>) -> Self {
        Self { store }
    }
}

impl Tool for ReadSkillTool {
    fn name(&self) -> &str {
        "read_skill"
    }

    fn description(&self) -> &str {
        "Read a saved skill's full content (frontmatter + step-by-step body) by name. Call it \
         when a skill from the SKILLS block (or list_skills output) looks relevant."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_skill".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([(
                    "name".to_string(),
                    required_str("Name of the skill to read"),
                )])),
                required: vec!["name".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let name = params.get::<String>("name").unwrap_or_default();
        let shown = name.trim().to_ascii_lowercase();
        match self.store.read(&name) {
            Some(skill) => {
                // 3a: usage signal for the self-improvement loop (best-effort;
                // the improver's evidence shows whether skills get read).
                crate::agents::metrics::record_skill_use(&skill.name);
                let modified = skill
                    .modified_at
                    .map(|t| format!("\n(last modified: {t})"))
                    .unwrap_or_default();
                let mut out = String::from("── ");
                out.push_str(&skill.name);
                out.push_str(" ──\n");
                if !skill.description.is_empty() {
                    out.push_str(&format!("description: {}\n", skill.description));
                }
                if !skill.when_to_use.is_empty() {
                    out.push_str(&format!("when_to_use: {}\n", skill.when_to_use));
                }
                out.push('\n');
                out.push_str(&skill.body);
                out.push_str(&modified);
                Ok(ToolOutput::success(out))
            }
            None => Ok(ToolOutput::error(format!(
                "Skill '{shown}' not found. Use list_skills to see available skill names."
            ))),
        }
    }
}

/// Tool for deleting a saved skill by name.
pub struct DeleteSkillTool {
    store: Arc<SkillStore>,
}

impl DeleteSkillTool {
    pub fn new(store: Arc<SkillStore>) -> Self {
        Self { store }
    }
}

impl Tool for DeleteSkillTool {
    fn name(&self) -> &str {
        "delete_skill"
    }

    fn description(&self) -> &str {
        "Delete a saved skill by name. Use it when a skill is stale or wrong (save_skill \
         overwrites is the way to fix one in place)."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delete_skill".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([(
                    "name".to_string(),
                    required_str("Name of the skill to delete"),
                )])),
                required: vec!["name".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let name = params.get::<String>("name").unwrap_or_default();
        let shown = name.trim().to_ascii_lowercase();
        match self.store.delete(&name) {
            Ok(true) => Ok(ToolOutput::success(format!("Skill '{shown}' deleted."))),
            Ok(false) => Ok(ToolOutput::error(format!(
                "Skill '{shown}' not found. Use list_skills to see available skill names."
            ))),
            Err(e) => Ok(ToolOutput::error(e)),
        }
    }
}

#[cfg(test)]
mod tests;
