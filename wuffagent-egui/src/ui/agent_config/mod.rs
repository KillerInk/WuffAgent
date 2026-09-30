//! Agent configuration dialog — left: agent list with Add/Delete, right:
//! the editor form (basic fields, shell group, handoff group, grouped
//! allowed-tools tree, metrics view, evals, prompt-history revert).
//!
//! F6: split into sub-files (all in the same module tree, so `pub(super)`
//! keeps everything module-private):
//! - [`tools`] — the grouped allowed-tools tree (types + builder + rendering);
//! - [`list`] — the agent list (Add/Delete);
//! - [`metrics_view`] — the read-only metrics view (window toggle, summary,
//!   percentiles, per-tool table, chart, recent lines, 4a run detail);
//! - [`evals`] — the golden/regression eval panel (2d);
//! - [`editor`] — the editor form + F4 prompt-history revert.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui;

use super::theme::Theme;
use wuffagent_core::agents::config::{AgentConfig, AgentManager};
use wuffagent_core::tools::ToolManager;
use wuffagent_core::types::AppEvent;

mod editor;
mod evals;
mod list;
mod metrics_view;
mod tools;

use tools::{build_tools_tree, ToolNode};
#[cfg(test)]
use tools::mcp_server_tool;

/// UI dialog for adding/editing/removing agent configurations.
pub struct AgentConfigDialog {
    /// Loaded agents from disk.
    agents: Vec<AgentConfig>,
    /// Currently selected agent index (-1 = none).
    selected_index: isize,
    /// Whether we're creating a new agent.
    is_new: bool,
    /// Form fields for the current agent being edited.
    name: String,
    description: String,
    system_prompt: String,
    enabled: bool,
    /// Reasoning effort for this agent (Off = inherit the global toggle).
    reasoning_effort: wuffagent_core::types::ReasoningEffort,
    /// Shell settings (persisted as the agent's `shell_config`).
    shell_enabled: bool,
    shell_type: String,
    shell_timeout_ms: u32,
    /// Comma-separated list of allowed command patterns (empty = allow all
    /// non-dangerous commands).
    shell_allowed_commands: String,
    /// Whether this agent may hand off the session to another agent.
    handoff_enabled: bool,
    /// Comma-separated handoff target names (empty = any enabled agent).
    handoff_targets: String,
    /// Checked status per tool index.
    tool_checkboxes: Vec<bool>,
    /// Available tool names from the tool registry.
    available_tools: Vec<String>,
    /// Grouped tree over `available_tools` (Builtin tools / MCP tools →
    /// category or server subgroups) for the "Allowed Tools" section.
    tools_tree: Vec<ToolNode>,
    /// Derived from tool_checkboxes at save time.
    allowed_tools: Vec<String>,
    /// Error/success messages.
    message: Option<String>,
    /// Whether the window is open (drives the title-bar close button).
    open: bool,
    /// 2d: the shared tool manager — the "Run evals" button drives the
    /// registered `run_eval` tool through it (headless, on a background
    /// thread), reusing the deps (llm/session/tool) wired at registration.
    tool_manager: Arc<ToolManager>,
    /// 2d: the core→UI event channel (to post `EvalsRunFinished` when a manual
    /// eval run ends and refresh the button's status line).
    events: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    /// 2d: the shared helper runtime (bootstrap's `memory_runtime`) — the
    /// "Run evals" worker `block_on`s the `run_eval` tool on it, mirroring
    /// the memory-maintenance worker (no throwaway runtime per run).
    runtime: Arc<tokio::runtime::Runtime>,
    /// 2d: a manual "Run evals" is in flight (disables the button).
    run_eval_running: bool,
    /// 2d: the last manual eval run's summary (or error), shown as a status line.
    run_eval_status: Option<String>,
    /// 2a: per-agent metrics cache, keyed by agent name:
    /// (file len, file mtime) → full line list. While the editor is open, the
    /// per-frame metrics/evals blocks re-read the JSONL only when the file
    /// changed (len or mtime), instead of scanning it twice per frame.
    metrics_cache: std::collections::HashMap<String, (u64, std::time::SystemTime, Vec<wuffagent_core::agents::metrics::MetricsLine>)>,
    /// 3b: the metrics window toggle for the read-only metrics block
    /// (7d / 30d / all-time; default all-time = the pre-3b view).
    metrics_window: metrics_view::MetricsWindow,
    /// 4a: the recent run whose cross-store detail view is expanded (its 1e
    /// run_id); `None` = nothing expanded.
    expanded_run_id: Option<String>,
    /// 4a: the lazily-loaded join view of the expanded run, keyed by
    /// (run_id, usage-file len) — reloaded when a different run is expanded
    /// or usage.jsonl grew (new rounds appended).
    run_detail: Option<(String, u64, wuffagent_core::agents::metrics::RunDetail)>,
}

impl AgentConfigDialog {
    pub fn new(
        agent_manager: Arc<Mutex<AgentManager>>,
        tool_manager: &Arc<ToolManager>,
        events: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
        runtime: &Arc<tokio::runtime::Runtime>,
    ) -> Self {
        let agents = agent_manager
            .lock()
            .map(|m| m.list_agents().unwrap_or_default())
            .unwrap_or_default();
        // `shell` is intentionally not in the tools list: its availability is
        // controlled solely by the "Enable shell tool" checkbox (a disabled
        // shell is removed from the agent's schema entirely).
        let available_tools: Vec<String> = tool_manager
            .get_tool_definitions()
            .into_iter()
            .map(|t| t.function.name)
            .filter(|name| name != "shell")
            .collect();
        let tool_checkboxes = vec![false; available_tools.len()];
        let tools_tree = build_tools_tree(&available_tools);

        Self {
            agents,
            selected_index: -1,
            is_new: false,
            name: String::new(),
            description: String::new(),
            system_prompt: String::new(),
            enabled: true,
            reasoning_effort: wuffagent_core::types::ReasoningEffort::default(),
            shell_enabled: false,
            shell_type: "powershell".to_string(),
            shell_timeout_ms: 300_000,
            shell_allowed_commands: String::new(),
            handoff_enabled: false,
            handoff_targets: String::new(),
            tool_checkboxes,
            available_tools,
            tools_tree,
            allowed_tools: Vec::new(),
            message: None,
            open: true,
            tool_manager: tool_manager.clone(),
            events,
            run_eval_running: false,
            run_eval_status: None,
            runtime: runtime.clone(),
            metrics_cache: std::collections::HashMap::new(),
            metrics_window: metrics_view::MetricsWindow::AllTime,
            expanded_run_id: None,
            run_detail: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, agent_manager: &Arc<Mutex<AgentManager>>) -> bool {
        let theme = Theme::from_name("dark");
        // Copy the flag out so the title-bar close button can toggle it
        // without clashing with the closure's `&mut self` borrow below.
        let mut open = self.open;

        egui::Window::new("Agent Configuration")
            .collapsible(false)
            .resizable(true)
            .default_size([780.0, 540.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.style_mut().spacing.item_spacing.y = 6.0;

                self.draw_header(ui, agent_manager);
                ui.separator();

                // Split into left (agent list) and right (editor) panels
                egui::containers::ScrollArea::both()
                    .id_salt("agent_config_scroll")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Left panel: agent list
                            ui.allocate_ui_with_layout(
                                egui::Vec2::new(220.0, ui.available_height()),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| self.draw_agent_list(ui, agent_manager),
                            );

                            ui.separator();

                            // Right panel: editor
                            ui.allocate_ui_with_layout(
                                egui::Vec2::new(ui.available_width(), ui.available_height()),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| self.draw_editor(ui, agent_manager, &theme),
                            );
                        });
                    });
            });
        self.open = open;
        !open
    }

    /// Header row with the Reload button.
    fn draw_header(&mut self, ui: &mut egui::Ui, agent_manager: &Arc<Mutex<AgentManager>>) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Agent Configuration").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Reload").clicked() {
                    if let Ok(m) = agent_manager.lock() {
                        self.agents = m.reload().unwrap_or_default();
                    }
                    self.clear_form();
                }
            });
        });
    }

    fn save(&mut self, agent_manager: &Arc<Mutex<AgentManager>>) -> bool {
        if self.name.trim().is_empty() {
            self.message = Some("Name is required.".to_string());
            return false;
        }

        self.sync_tools_from_checkboxes();
        let config = AgentConfig {
            name: self.name.trim().to_string(),
            description: self.description.trim().to_string(),
            system_prompt: self.system_prompt.clone(),
            allowed_tools: self.allowed_tools.clone(),
            enabled: self.enabled,
            task_timeout_ms: 60_000,
            shell_config: wuffagent_core::agents::config::ShellConfig {
                allowed_commands: self
                    .shell_allowed_commands
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                shell_type: self.shell_type.clone(),
                shell_timeout_ms: self.shell_timeout_ms as u64,
                shell_enabled: self.shell_enabled,
                working_dir: None,
            },
            agents_dir: std::path::PathBuf::from(""),
            agents_search_dirs: Vec::new(),
            custom_prompts: std::collections::HashMap::new(),
            reasoning_effort: self.reasoning_effort,
            trim_config: wuffagent_core::trimming::config::TrimConfig::default(),
            handoff_enabled: self.handoff_enabled,
            handoff_targets: self
                .handoff_targets
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            // The agent editor has no restart toggle yet; keep the default
            // (enabled) so created/edited profiles can restart WuffAgent.
            restart_enabled: true,
            // The agent editor has no hand_back toggle yet; keep the default
            // (enabled) so sub-session agents can return to their parent.
            hand_back_enabled: true,
            // The agent editor has no session_note toggle yet; keep the
            // default (enabled) — S4a pinned notes are on for every agent.
            session_note_enabled: true,
            // The agent editor has no metrics toggle; keep the default (enabled)
            // so profile runs keep emitting run-metrics lines (the eval harness
            // is the only place that disables it).
            metrics_enabled: true,
        };

        // Ensure the directory exists before saving
        if let Ok(m) = agent_manager.lock() {
            if let Err(e) = std::fs::create_dir_all(m.agents_dir()) {
                self.message = Some(format!("Failed to create directory: {}", e));
                return false;
            }
        }

        let result = if self.is_new {
            agent_manager.lock().map(|m| m.add_agent(&config))
        } else if let Some(idx) = self.selected_index() {
            let old_name = self.agents[idx].name.clone();
            agent_manager
                .lock()
                .map(|m| m.edit_agent(&old_name, &config))
        } else {
            return false;
        };

        match result {
            Ok(Ok(())) => {
                self.message = Some("Agent saved successfully.".to_string());
                if let Ok(m) = agent_manager.lock() {
                    self.agents = m.reload().unwrap_or_default();
                }
                self.clear_form();
                true
            }
            Ok(Err(e)) => {
                self.message = Some(format!("Error: {}", e));
                false
            }
            Err(e) => {
                self.message = Some(format!("Lock error: {}", e));
                false
            }
        }
    }

    fn select_agent(&mut self, idx: usize) {
        // 2d: a manual eval result belongs to the previously selected agent;
        // don't show it under the new selection.
        self.run_eval_status = None;
        self.selected_index = idx as isize;
        if idx < self.agents.len() {
            // Clone out first: `load_into_form` takes `&mut self`, so we
            // cannot pass `&self.agents[idx]` directly.
            let agent = self.agents[idx].clone();
            self.load_into_form(&agent);
        }
    }

    /// Load an externally-sourced config into the form fields (e.g. the one
    /// `revert_agent` returns after a F4 revert restored a historical
    /// version).
    fn load_into_form(&mut self, agent: &AgentConfig) {
        self.is_new = false;
        self.name = agent.name.clone();
        self.description = agent.description.clone();
        self.system_prompt = agent.system_prompt.clone();
        self.enabled = agent.enabled;
        self.reasoning_effort = agent.reasoning_effort;
        self.shell_enabled = agent.shell_config.shell_enabled;
        self.shell_type = agent.shell_config.shell_type.clone();
        self.shell_timeout_ms = agent.shell_config.shell_timeout_ms as u32;
        self.shell_allowed_commands = agent.shell_config.allowed_commands.join(", ");
        self.handoff_enabled = agent.handoff_enabled;
        self.handoff_targets = agent.handoff_targets.join(", ");
        self.sync_tools_from_agent(&agent.allowed_tools);
        self.message = None;
    }

    fn start_new(&mut self) {
        // Clear first: clear_form() resets is_new to false, so it must be
        // set afterwards or the editor panel never appears.
        self.clear_form();
        self.is_new = true;
    }

    fn clear_form(&mut self) {
        self.is_new = false;
        self.selected_index = -1;
        self.name.clear();
        self.description.clear();
        self.system_prompt.clear();
        self.enabled = true;
        self.reasoning_effort = wuffagent_core::types::ReasoningEffort::default();
        self.shell_enabled = false;
        self.shell_type = "powershell".to_string();
        self.shell_timeout_ms = 300_000;
        self.shell_allowed_commands.clear();
        self.handoff_enabled = false;
        self.handoff_targets.clear();
        self.allowed_tools.clear();
        for cb in &mut self.tool_checkboxes {
            *cb = false;
        }
        self.message = None;
    }

    fn selected_index(&self) -> Option<usize> {
        if self.selected_index >= 0 {
            Some(self.selected_index as usize)
        } else {
            None
        }
    }

    fn sync_tools_from_agent(&mut self, allowed: &[String]) {
        self.allowed_tools = allowed.to_vec();
        // Reset checkboxes to match
        self.tool_checkboxes = self
            .available_tools
            .iter()
            .map(|t| allowed.contains(t))
            .collect();
    }

    fn sync_tools_from_checkboxes(&mut self) {
        self.allowed_tools = self
            .available_tools
            .iter()
            .zip(&self.tool_checkboxes)
            .filter_map(|(name, checked)| if *checked { Some(name.clone()) } else { None })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf_names(node: &ToolNode, tools: &[String]) -> Vec<String> {
        match node {
            ToolNode::Leaf(i) => vec![tools[*i].clone()],
            ToolNode::Group { children, .. } => children.iter().flat_map(|c| leaf_names(c, tools)).collect(),
        }
    }

    fn group_titles(node: &ToolNode) -> Vec<String> {
        match node {
            ToolNode::Leaf(_) => vec![],
            ToolNode::Group { title, children } => {
                let mut v = vec![title.clone()];
                for c in children {
                    v.extend(group_titles(c));
                }
                v
            }
        }
    }

    #[test]
    fn tree_groups_builtin_and_mcp() {
        let tools = vec![
            "read_file".into(),
            "mcp__hello__hello".into(),
            "web_search".into(),
            "hello".into(),
            "save_memory".into(),
        ];
        let tree = build_tools_tree(&tools);
        // Top level: Builtin tools + MCP tools.
        assert_eq!(
            group_titles(&tree[0])
                .into_iter()
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()[0],
            "Builtin tools"
        );
        assert_eq!(group_titles(&tree[1]).first().unwrap(), "MCP tools");

        // Every input tool appears exactly once, in some leaf.
        let mut all: Vec<String> = tree.iter().flat_map(|n| leaf_names(n, &tools)).collect();
        all.sort();
        let mut expected: Vec<String> = tools.clone();
        expected.sort();
        assert_eq!(all, expected);

        // MCP server subgroup holds its tool.
        let mcp_titles = group_titles(&tree[1]);
        assert!(mcp_titles.contains(&"hello".to_string()));
    }

    #[test]
    fn mcp_server_tool_parses_server_with_underscores() {
        assert_eq!(
            mcp_server_tool("mcp__hello__hello"),
            Some(("hello".to_string(), "hello".to_string()))
        );
        // Ambiguous when BOTH sides contain `__`; convention: the last
        // segment is the tool name (MCP tool names are [a-zA-Z0-9_-]).
        assert_eq!(
            mcp_server_tool("mcp__my__server__tool"),
            Some(("my__server".to_string(), "tool".to_string()))
        );
        assert_eq!(mcp_server_tool("mcp__x"), None);
        assert_eq!(mcp_server_tool("read_file"), None);
    }
}
