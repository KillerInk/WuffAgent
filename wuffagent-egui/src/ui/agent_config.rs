use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui;

use super::agent_history;
use super::theme::Theme;
use wuffagent_core::agents::config::{AgentConfig, AgentManager};
use wuffagent_core::agents::metrics::MetricsLine;
use wuffagent_core::tools::{ToolManager, ToolOutput, ToolParams};
use wuffagent_core::types::AppEvent;

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
    /// 2d: a manual "Run evals" is in flight (disables the button).
    run_eval_running: bool,
    /// 2d: the last manual eval run's summary (or error), shown as a status line.
    run_eval_status: Option<String>,
}

impl AgentConfigDialog {
    pub fn new(
        agent_manager: Arc<Mutex<AgentManager>>,
        tool_manager: &Arc<ToolManager>,
        events: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
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
            allowed_tools: Vec::new(),
            message: None,
            open: true,
            tool_manager: tool_manager.clone(),
            events,
            run_eval_running: false,
            run_eval_status: None,
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

    /// Left panel: the agent list with Add/Delete buttons.
    fn draw_agent_list(&mut self, ui: &mut egui::Ui, agent_manager: &Arc<Mutex<AgentManager>>) {
        ui.heading("Agents");
        ui.separator();

        if ui.button("+ Add Agent").clicked() {
            self.start_new();
        }

        ui.separator();

        if self.agents.is_empty() {
            ui.label("No agents configured.");
        } else {
            // Pre-collect agent data to avoid borrowing self mutably inside the loop.
            // (Display data + index only: selecting an agent copies fields via
            // `select_agent`, which reads `self.agents`.)
            struct AgentButtonData {
                label: String,
                bg: egui::Color32,
                idx: usize,
            }
            let button_data: Vec<AgentButtonData> = self
                .agents
                .iter()
                .enumerate()
                .map(|(i, agent)| {
                    let selected = i as isize == self.selected_index;
                    AgentButtonData {
                        label: format!(
                            "[{}] {}",
                            if agent.enabled { "x" } else { " " },
                            agent.name
                        ),
                        bg: if selected {
                            egui::Color32::from_rgb(0x33, 0x66, 0xCC)
                        } else {
                            egui::Color32::from_rgb(0x33, 0x33, 0x33)
                        },
                        idx: i,
                    }
                })
                .collect();
            for bd in &button_data {
                if ui.add(egui::Button::new(&bd.label).fill(bd.bg)).clicked() {
                    self.select_agent(bd.idx);
                }
            }
        }

        if !self.agents.is_empty() && self.selected_index >= 0 {
            ui.separator();
            if ui
                .add_enabled(
                    true,
                    egui::Button::new("Delete").fill(egui::Color32::from_rgb(0xCC, 0x33, 0x33)),
                )
                .clicked()
            {
                self.delete_agent(agent_manager);
            }
        }
    }

    /// Right panel: the form for the selected (or new) agent, plus the
    /// post-draw prompt-history revert execution (F4).
    fn draw_editor(
        &mut self,
        ui: &mut egui::Ui,
        agent_manager: &Arc<Mutex<AgentManager>>,
        theme: &Theme,
    ) {
        ui.heading("Agent Editor");
        ui.separator();

        if self.is_new || self.selected_index >= 0 {
            // F4: prompt history of the selected EXISTING agent - snapshots
            // from every directory the manager can see (primary + search
            // dirs; a profile's snapshots live next to its file, see F3).
            let history_agent: Option<String> = if self.is_new {
                None
            } else {
                self.selected_index()
                    .filter(|&idx| idx < self.agents.len())
                    .map(|idx| self.agents[idx].name.clone())
            };
            let history_entries: Option<Vec<agent_history::HistoryEntry>> =
                history_agent.as_ref().and_then(|name| {
                    agent_manager.lock().ok().map(|m| {
                        let mut dirs: Vec<PathBuf> = vec![m.agents_dir().clone()];
                        for d in m.search_dirs() {
                            if !dirs.contains(d) {
                                dirs.push(d.clone());
                            }
                        }
                        agent_history::list_history(&dirs, name)
                    })
                });

            let mut revert_target: Option<agent_history::HistoryEntry> = None;
            ui.vertical(|ui| {
                self.draw_basic_fields(ui);

                ui.separator();
                self.draw_shell_group(ui);

                ui.separator();
                self.draw_handoff_group(ui);

                ui.separator();
                self.draw_tools_and_metrics(ui, history_agent.as_deref());

                // 2d: the golden/regression eval panel (existing agents only).
                if let Some(name) = history_agent.as_deref() {
                    ui.separator();
                    self.draw_evals(ui, name);
                }

                revert_target =
                    self.draw_prompt_history(ui, &history_entries, history_agent.as_deref());

                ui.separator();
                self.draw_save_row(ui, agent_manager, theme);
            });

            // F4: execute a requested revert (file I/O + list refresh),
            // outside the drawing closure.
            if let (Some(name), Some(entry)) = (&history_agent, revert_target) {
                self.execute_revert(agent_manager, name, &entry);
            }
        } else {
            // No agent selected - show info
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(
                        "Select an agent from the list, or click \"+ Add Agent\" to create one.",
                    )
                    .strong(),
                );
            });
        }
    }

    /// Name / description / system prompt / enabled / reasoning effort.
    fn draw_basic_fields(&mut self, ui: &mut egui::Ui) {
        ui.label("Name:");
        ui.text_edit_singleline(&mut self.name);

        ui.label("Description:");
        ui.text_edit_singleline(&mut self.description);

        ui.label("System Prompt:");
        ui.text_edit_multiline(&mut self.system_prompt);

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.enabled, "Enabled");
            ui.separator();
            ui.label("Reasoning effort:");
            for variant in wuffagent_core::types::ReasoningEffort::VARIANTS {
                ui.selectable_value(&mut self.reasoning_effort, variant, variant.name());
            }
        });
    }

    /// The shell-tool settings group.
    fn draw_shell_group(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.checkbox(&mut self.shell_enabled, "Enable shell tool");
            ui.horizontal(|ui| {
                ui.label("Shell type:");
                for t in ["powershell", "cmd", "bash"] {
                    ui.selectable_value(&mut self.shell_type, t.to_string(), t);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Timeout (ms):");
                ui.add(egui::DragValue::new(&mut self.shell_timeout_ms).range(1000..=600_000));
            });
            ui.label(
                "Allowed commands (comma-separated patterns; empty = allow all non-dangerous):",
            );
            ui.text_edit_singleline(&mut self.shell_allowed_commands);
        });
    }

    /// The handoff settings group.
    fn draw_handoff_group(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.checkbox(
                &mut self.handoff_enabled,
                "Allow agent handoff (this agent may switch the session to another agent)",
            );
            ui.label("Handoff targets (comma-separated agent names; empty = any enabled agent):");
            ui.text_edit_singleline(&mut self.handoff_targets);
        });
    }

    /// Allowed-tools checkboxes + the read-only recent-metrics block (M1).
    fn draw_tools_and_metrics(&mut self, ui: &mut egui::Ui, history_agent: Option<&str>) {
        ui.label("Allowed Tools:");

        // Tool checkboxes
        for (i, tool) in self.available_tools.iter().enumerate() {
            ui.checkbox(&mut self.tool_checkboxes[i], tool);
        }

        // M1: read-only recent metrics for the selected EXISTING agent
        // (run/outcome/feedback counts + the last few metric lines) from the
        // per-agent log.
        if let Some(name) = history_agent {
            let metrics = wuffagent_core::agents::metrics::MetricsLog::default();
            let summary = metrics.summary_since(name, None);
            if summary.runs + summary.feedback_up + summary.feedback_down > 0 {
                ui.separator();
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Recent metrics (all time):").strong());
                    ui.label(format!(
                        "{} run(s): {} verified, {} after retry, {} gave up, {} before verification; {} tool call(s) ({} errors); feedback {} up / {} down",
                        summary.runs,
                        summary.verified,
                        summary.verified_after_retry,
                        summary.gave_up,
                        summary.not_verified,
                        summary.tool_calls,
                        summary.tool_errors,
                        summary.feedback_up,
                        summary.feedback_down
                    ));
                    for line in metrics.recent(name, 5).iter().rev() {
                        ui.label(
                            egui::RichText::new(format!("  {}", line.describe()))
                                .weak()
                                .small(),
                        );
                    }
                });
            }
        }
    }

    /// 2d: the golden/regression eval panel for the selected EXISTING agent —
    /// saved-eval count, all-time pass rate, the last run's verdict, plus a
    /// "Run evals" button that drives the registered `run_eval` tool headlessly
    /// (each eval = a fresh isolated agent + its own LLM call, up to 180 s).
    /// The result returns via `EvalsRunFinished` and is shown as a status line.
    fn draw_evals(&mut self, ui: &mut egui::Ui, agent_name: &str) {
        let saved = wuffagent_core::memory::evals::EvalStore::default()
            .list(agent_name)
            .len();
        // One read of the agent's metrics; count evals + capture the newest.
        let mut total = 0usize;
        let mut passed = 0usize;
        let mut last_desc: Option<String> = None;
        let mut last_passed: Option<bool> = None;
        for l in wuffagent_core::agents::metrics::MetricsLog::default().read_all(agent_name) {
            if let MetricsLine::Eval { ts, id, passed: p, .. } = l {
                total += 1;
                if p {
                    passed += 1;
                }
                let id_label = if id.is_empty() { "(ad-hoc)" } else { id.as_str() };
                last_desc = Some(format!("{} — {}", ts.format("%Y-%m-%d %H:%M"), id_label));
                last_passed = Some(p);
            }
        }
        let failed = total - passed;

        ui.group(|ui| {
            ui.label(egui::RichText::new("Evals (golden / regression):").strong());
            if saved == 0 {
                ui.label(
                    egui::RichText::new(
                        "None saved — the agent can add one with the save_eval tool.",
                    )
                    .weak(),
                );
            }
            ui.label(format!("{saved} saved"));
            if total > 0 {
                let pct = (passed as f64 * 100.0 / total as f64).round() as u32;
                ui.label(format!(
                    "Pass rate: {passed}/{total} ({pct}%) — {failed} failed (all time)"
                ));
                if let (Some(desc), Some(p)) = (&last_desc, last_passed) {
                    ui.label(format!(
                        "Last run: {} {}",
                        desc,
                        if p { "PASS" } else { "FAIL" },
                    ));
                }
            } else {
                ui.label(egui::RichText::new("Not run yet.").weak());
            }
            ui.horizontal(|ui| {
                if self.run_eval_running {
                    ui.label(egui::RichText::new("Running evals…").weak());
                } else if saved > 0 && ui.button("▶ Run evals").clicked() {
                    self.start_eval_run(agent_name);
                }
            });
            if let Some(status) = &self.run_eval_status {
                ui.label(egui::RichText::new(status).weak().small());
            }
        });
    }

    /// 2d: kick off a manual eval run on a background thread. The registered
    /// `run_eval` tool runs each saved eval headlessly (isolated fresh agent,
    /// its own LLM call, up to 180 s each) and reports a pass/fail table; the
    /// result is posted back over the event channel as `EvalsRunFinished`.
    fn start_eval_run(&mut self, agent_name: &str) {
        let Some(events) = self.events.clone() else {
            self.run_eval_status = Some(
                "No event channel — the run still executes, but its result cannot be shown here."
                    .to_string(),
            );
            return;
        };
        let tm = self.tool_manager.clone();
        let agent = agent_name.to_string();
        self.run_eval_running = true;
        self.run_eval_status =
            Some(format!("Running evals for '{agent}'… (each runs headlessly, up to 180 s)"));
        std::thread::spawn(move || {
            let mut values = HashMap::new();
            values.insert("agent".to_string(), serde_json::json!(&agent));
            let summary =
                match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => match rt.block_on(tm.execute("run_eval", ToolParams { values })) {
                        Ok(ToolOutput::Success(v)) => v.as_str().unwrap_or("").to_string(),
                        Ok(ToolOutput::Error(e)) => format!("Error: {e}"),
                        Err(e) => format!("Error: {e}"),
                    },
                    Err(e) => format!("Failed to build eval runtime: {e}"),
                };
            if let Ok(s) = events.lock() {
                let _ = s.send(AppEvent::EvalsRunFinished {
                    agent_name: agent,
                    summary,
                });
            }
        });
    }

    /// 2d: a manual eval run finished — record the summary and clear the
    /// running flag (called from the `EvalsRunFinished` event handler).
    pub fn mark_evals_finished(&mut self, agent_name: &str, summary: &str) {
        self.run_eval_running = false;
        self.run_eval_status = Some(summary.to_string());
        tracing::info!(%agent_name, "manual eval run finished");
    }

    /// F4: prompt history + per-version Revert. Returns the entry the user
    /// asked to revert (executed by the caller, outside the drawing closure).
    fn draw_prompt_history(
        &mut self,
        ui: &mut egui::Ui,
        history_entries: &Option<Vec<agent_history::HistoryEntry>>,
        history_agent: Option<&str>,
    ) -> Option<agent_history::HistoryEntry> {
        if history_agent.is_none() {
            return None;
        }
        let mut revert_target: Option<agent_history::HistoryEntry> = None;
        ui.separator();
        ui.group(|ui| {
            ui.label(egui::RichText::new("Prompt history (newest first):").strong());
            ui.label(
                egui::RichText::new(
                    "A snapshot is saved before every edit. Revert restores the selected version; the current state is snapshotted first, so a revert is itself reversible.",
                )
                .weak(),
            );
            match history_entries {
                None => {
                    ui.label(
                        egui::RichText::new("(could not read prompt history)").weak(),
                    );
                }
                Some(entries) if entries.is_empty() => {
                    ui.label(egui::RichText::new("No prompt history yet.").weak());
                }
                Some(entries) => {
                    for e in entries.iter() {
                        ui.horizontal(|ui| {
                            let mut ts_label = agent_history::format_ts(e.ts);
                            if e.seq > 0 {
                                ts_label.push_str(&format!("#{}", e.seq));
                            }
                            ui.label(ts_label);
                            ui.label(
                                egui::RichText::new(agent_history::prompt_preview(&e.path))
                                    .weak()
                                    .monospace(),
                            );
                            if ui.small_button("Revert").clicked() {
                                revert_target = Some(e.clone());
                            }
                        });
                    }
                }
            }
        });
        revert_target
    }

    /// Save button + the one-shot status message.
    fn draw_save_row(
        &mut self,
        ui: &mut egui::Ui,
        agent_manager: &Arc<Mutex<AgentManager>>,
        theme: &Theme,
    ) {
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new("Save").fill(theme.success))
                .clicked()
            {
                if self.save(agent_manager) {
                    self.clear_form();
                }
            }
        });

        if let Some(msg) = &self.message {
            if msg.contains("Error") {
                ui.label(egui::RichText::new(msg).color(egui::Color32::from_rgb(0xCC, 0x33, 0x33)));
            } else {
                ui.label(egui::RichText::new(msg).color(theme.success));
            }
            self.message = None;
        }
    }

    /// F4: run the requested prompt-history revert (file I/O + list refresh).
    fn execute_revert(
        &mut self,
        agent_manager: &Arc<Mutex<AgentManager>>,
        name: &str,
        entry: &agent_history::HistoryEntry,
    ) {
        match agent_history::revert(&entry.dir, name, entry) {
            Ok(config) => {
                if let Ok(m) = agent_manager.lock() {
                    self.agents = m.reload().unwrap_or_default();
                }
                if let Some(pos) = self.agents.iter().position(|a| a.name == config.name) {
                    self.selected_index = pos as isize;
                }
                self.load_into_form(&config);
                self.message = Some(format!(
                    "Reverted '{}' to {}.",
                    config.name,
                    agent_history::format_ts(entry.ts)
                ));
            }
            Err(e) => {
                self.message = Some(format!("Error: revert of '{}' failed: {}", name, e));
            }
        }
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

    fn delete_agent(&mut self, agent_manager: &Arc<Mutex<AgentManager>>) {
        if let Some(idx) = self.selected_index() {
            let name = self.agents[idx].name.clone();
            if let Ok(m) = agent_manager.lock() {
                match m.remove_agent(&name) {
                    Ok(()) => {
                        self.agents.remove(idx);
                        self.clear_form();
                    }
                    Err(e) => {
                        self.message = Some(format!("Error: {}", e));
                    }
                }
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
