//! Eval tools (2a): save_eval / list_evals / delete_eval — the agent-visible
//! side of the eval harness. Evals are saved golden/regression tasks
//! (`<wuffagent_home>/evals/<agent>.jsonl`, see `crate::memory::evals`) that
//! `run_eval` (2b) executes headlessly and the verification judge scores.
//!
//! All three are per-profile `allowed_tools`-gated like every builtin; the
//! `agent` param names whose evals are being managed (wuffagent dogfoods its
//! own first).

use std::collections::HashMap;
use std::sync::Arc;

use crate::memory::evals::EvalStore;
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

/// Tool for saving (or upserting by id) a golden/regression eval.
pub struct SaveEvalTool {
    store: Arc<EvalStore>,
}

impl SaveEvalTool {
    pub fn new(store: Arc<EvalStore>) -> Self {
        Self { store }
    }
}

impl Tool for SaveEvalTool {
    fn name(&self) -> &str {
        "save_eval"
    }

    fn description(&self) -> &str {
        "Save (or upsert by id) a golden/regression eval for an agent. An eval is a \
         self-contained task with verification criteria that run_eval executes \
         headlessly and the verification judge scores — the basis for before/after \
         regression checks after prompt/profile changes."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "save_eval".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    (
                        "agent".to_string(),
                        required_str("Agent profile whose evals are managed (e.g. 'wuffagent')"),
                    ),
                    (
                        "id".to_string(),
                        required_str("Stable eval id (slug); upsert key"),
                    ),
                    (
                        "task".to_string(),
                        required_str("The task given to the agent headlessly"),
                    ),
                    (
                        "expect".to_string(),
                        required_str("Verification criteria the judge checks the result against"),
                    ),
                    (
                        "max_tool_calls".to_string(),
                        FieldSchema {
                            type_name: "integer".to_string(),
                            description: "Optional cap on tool calls (run aborts if exceeded)"
                                .to_string(),
                            nullable: true,
                        },
                    ),
                ])),
                required: vec![
                    "agent".to_string(),
                    "id".to_string(),
                    "task".to_string(),
                    "expect".to_string(),
                ],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let agent = params.get::<String>("agent").unwrap_or_default();
        if agent.trim().is_empty() {
            return Ok(ToolOutput::error(
                "save_eval requires a non-empty 'agent' param".to_string(),
            ));
        }
        let id = params.get::<String>("id").unwrap_or_default();
        let task = params.get::<String>("task").unwrap_or_default();
        let expect = params.get::<String>("expect").unwrap_or_default();
        let max_tool_calls = params.get::<usize>("max_tool_calls");
        let eval = crate::memory::evals::Eval {
            id,
            task,
            expect,
            max_tool_calls,
        };
        match self.store.save(&agent, eval) {
            Ok(()) => Ok(ToolOutput::success(format!(
                "Eval saved for agent '{agent}'."
            ))),
            Err(e) => Ok(ToolOutput::error(e)),
        }
    }
}

/// Tool for listing an agent's saved evals.
pub struct ListEvalsTool {
    store: Arc<EvalStore>,
}

impl ListEvalsTool {
    pub fn new(store: Arc<EvalStore>) -> Self {
        Self { store }
    }
}

impl Tool for ListEvalsTool {
    fn name(&self) -> &str {
        "list_evals"
    }

    fn description(&self) -> &str {
        "List an agent's saved evals (id, task, verification criteria). Use it to see what \
         golden/regression tasks exist before run_eval or save_eval."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_evals".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([(
                    "agent".to_string(),
                    required_str("Agent profile whose evals to list"),
                )])),
                required: vec!["agent".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let agent = params.get::<String>("agent").unwrap_or_default();
        if agent.trim().is_empty() {
            return Ok(ToolOutput::error(
                "list_evals requires a non-empty 'agent' param".to_string(),
            ));
        }
        let evals = self.store.list(&agent);
        if evals.is_empty() {
            return Ok(ToolOutput::success(format!(
                "No evals saved for agent '{agent}'. Save one with save_eval."
            )));
        }
        let mut out = format!("Evals for agent '{agent}':\n");
        for e in &evals {
            out.push_str(&format!("- {}: {}\n", e.id, e.task));
            out.push_str(&format!("  expect: {}\n", e.expect));
            if let Some(n) = e.max_tool_calls {
                out.push_str(&format!("  max_tool_calls: {n}\n"));
            }
        }
        Ok(ToolOutput::success(out.trim_end().to_string()))
    }
}

/// Tool for deleting an eval by id for an agent.
pub struct DeleteEvalTool {
    store: Arc<EvalStore>,
}

impl DeleteEvalTool {
    pub fn new(store: Arc<EvalStore>) -> Self {
        Self { store }
    }
}

impl Tool for DeleteEvalTool {
    fn name(&self) -> &str {
        "delete_eval"
    }

    fn description(&self) -> &str {
        "Delete a saved eval by id for an agent. Use it when an eval is stale or wrong \
         (save_eval upserts are the way to fix one in place)."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delete_eval".to_string(),
            description: self.description().to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(HashMap::from([
                    (
                        "agent".to_string(),
                        required_str("Agent profile whose evals are managed"),
                    ),
                    ("id".to_string(), required_str("Eval id to delete")),
                ])),
                required: vec!["agent".to_string(), "id".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let agent = params.get::<String>("agent").unwrap_or_default();
        if agent.trim().is_empty() {
            return Ok(ToolOutput::error(
                "delete_eval requires a non-empty 'agent' param".to_string(),
            ));
        }
        let id = params.get::<String>("id").unwrap_or_default();
        match self.store.delete(&agent, &id) {
            Ok(true) => Ok(ToolOutput::success(format!(
                "Eval '{id}' deleted for agent '{agent}'."
            ))),
            Ok(false) => Ok(ToolOutput::error(format!(
                "Eval '{id}' not found for agent '{agent}'. Use list_evals to see available ids."
            ))),
            Err(e) => Ok(ToolOutput::error(e)),
        }
    }
}

#[cfg(test)]
mod tests;
