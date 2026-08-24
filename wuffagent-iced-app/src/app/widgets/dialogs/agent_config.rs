use iced::widget::{button, column, container, row, text, text_input};
use iced::{Alignment, Element};
use std::sync::Arc;

use crate::app::backend::Backend;
use crate::app::messages::Message;
use crate::agents::config::WorkerConfig;
use iced::theme::Palette;

/// State for the agent config dialog, mutable across update calls.
pub struct AgentConfigDialog {
    pub agents: Vec<WorkerConfig>,
    pub selected_index: isize,
    pub is_new: bool,
    pub name: String,
    pub description: String,
    pub system_prompt: String,
    pub priority: u32,
    pub max_concurrent: usize,
    pub enabled: bool,
    /// Reasoning effort for this agent (Off = inherit the global toggle).
    pub reasoning_effort: wuffagent_core::types::ReasoningEffort,
    pub tool_checkboxes: Vec<bool>,
    pub available_tools: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub message: Option<String>,
}

impl AgentConfigDialog {
    pub fn new(backend: &Arc<Backend>) -> Self {
        let agent_manager = backend.agent_manager.lock().unwrap();
        let agents = agent_manager.list_agents().unwrap_or_default();
        drop(agent_manager);
        let available_tools: Vec<String> = backend
            .tool_manager
            .get_tool_definitions()
            .into_iter()
            .map(|t| t.function.name)
            .collect();
        let tool_checkboxes = vec![false; available_tools.len()];

        Self {
            agents,
            selected_index: -1,
            is_new: false,
            name: String::new(),
            description: String::new(),
            system_prompt: String::new(),
            priority: 0,
            max_concurrent: 1,
            enabled: true,
            reasoning_effort: wuffagent_core::types::ReasoningEffort::default(),
            tool_checkboxes,
            available_tools,
            allowed_tools: Vec::new(),
            message: None,
        }
    }

    pub fn view(&self, pal: Palette) -> Element<'_, Message> {
        let msg = self.message.clone();
        let agent_list: Element<'_, Message> = if self.agents.is_empty() {
            text("No agents configured.").size(12).color(pal.text).into()
        } else {
            let mut col = column!().spacing(2);
            for (i, agent) in self.agents.iter().enumerate() {
                let selected = i as isize == self.selected_index;
                let label = format!("{} {}", if agent.enabled { "v" } else { "o" }, agent.name);
                col = col.push(
                    button(text(label).size(11).color(if selected { iced::Color::WHITE } else { pal.text }))
                        .on_press(Message::AgentConfigSelect(i))
                        .padding([4, 8])
                );
            }
            Element::from(col)
        };

        let editor: Element<'_, Message> = if self.is_new || self.selected_index >= 0 {
            let mut col = column!()
                .push(text(if self.is_new { "New Agent" } else { "Edit Agent" }).size(14).color(pal.text));

            col = col.push(text("Name").size(11).color(pal.text));
            col = col.push(text_input("", &self.name).on_input(|v| Message::AgentConfigName(v)).padding([4, 8]));

            col = col.push(text("Description").size(11).color(pal.text));
            col = col.push(text_input("", &self.description).on_input(|v| Message::AgentConfigDescription(v)).padding([4, 8]));

            col = col.push(text("Priority (lower = preferred)").size(11).color(pal.text));
            col = col.push(text_input("", &self.priority.to_string()).on_input(|v| Message::AgentConfigPriority(v)).padding([4, 8]));

            col = col.push(text("Max Concurrent").size(11).color(pal.text));
            col = col.push(text_input("", &self.max_concurrent.to_string()).on_input(|v| Message::AgentConfigMaxConcurrent(v)).padding([4, 8]));

            col = col.push(
                row!()
                    .push(button(if self.enabled { "v Enabled" } else { "o Enabled" })
                        .on_press(Message::AgentConfigEnabled(!self.enabled)).padding([4, 8]))
                    .align_y(Alignment::Center)
            );

            col = col.push(text("Reasoning Effort (Off = inherit global)").size(11).color(pal.text));
            {
                let mut eff_row = row!().spacing(4);
                for (i, variant) in wuffagent_core::types::ReasoningEffort::VARIANTS.iter().enumerate() {
                    let selected = *variant == self.reasoning_effort;
                    eff_row = eff_row.push(
                        button(
                            text(format!("[{}] {}", if selected { "v" } else { " " }, variant.name()))
                                .size(11)
                                .color(if selected { iced::Color::WHITE } else { pal.text })
                        )
                        .on_press(Message::AgentConfigReasoningEffort(i as u8))
                        .padding([2, 8])
                    );
                }
                col = col.push(eff_row);
            }

            col = col.push(text("System Prompt").size(11).color(pal.text));
            col = col.push(text_input("", &self.system_prompt).on_input(|v| Message::AgentConfigSystemPrompt(v)).padding([4, 8]));

            col = col.push(text("Allowed Tools").size(11).color(pal.text));
            col = col.push(
                row!()
                    .push(button(text("All")).on_press(Message::AgentConfigSelectAllTools).padding([2, 8]))
                    .push(button(text("None")).on_press(Message::AgentConfigClearTools).padding([2, 8]))
            );

            if self.available_tools.is_empty() {
                col = col.push(text("No tools registered.").size(11).color(pal.text));
            } else {
                for (i, tool) in self.available_tools.iter().enumerate() {
                    col = col.push(
                        row!()
                            .push(button(if self.tool_checkboxes[i] { "v" } else { "o" })
                                .on_press(Message::AgentConfigToolToggle(i, !self.tool_checkboxes[i])).padding([2, 4]))
                            .push(text(tool).size(11).color(pal.text))
                            .align_y(Alignment::Center).spacing(4)
                    );
                }
            }

            if let Some(ref m) = msg {
                let color = if m.to_lowercase().contains("error") || m.to_lowercase().contains("failed") {
                    pal.danger
                } else {
                    pal.success
                };
                let msg_str = m.clone();
                col = col.push(text(msg_str).size(11).color(color));
            }

            col = col.push(
                row!()
                    .push(button(text("Save")).on_press(Message::AgentConfigSave).padding([4, 16]))
                    .push(button(text("Cancel")).on_press(Message::AgentConfigCancel).padding([4, 16]))
                    .push(if !self.is_new && self.selected_index >= 0 {
                        row!()
                            .push(button(text("Delete")).on_press(Message::AgentConfigDelete).padding([4, 12]))
                            .into()
                    } else {
                        Element::from(row!())
                    })
                    .push(iced::widget::text(""))
                    .align_y(Alignment::Center).spacing(8)
            );

            Element::from(col)
        } else {
            Element::from(column!().push(text("Select an agent or click \"+ Add Agent\"").size(12).color(pal.text)))
        };

        let content = column!()
            .push(text("Agent Configuration").size(16).color(pal.text))
            .push(
                row!()
                    .push(column!()
                        .push(text("Agents").size(13).color(pal.text))
                        .push(button(text("+ Add Agent")).on_press(Message::AgentConfigNew).padding([4, 8]))
                        .push(agent_list)
                        .width(180))
                    .push(iced::widget::text(""))
                    .push(editor)
                    .align_y(Alignment::Center).spacing(0)
            )
            .padding([16, 16]).width(620);

        container(content).width(620).into()
    }

    pub fn reload(&mut self, backend: &Arc<Backend>) {
        let agent_manager = backend.agent_manager.lock().unwrap();
        match agent_manager.reload() {
            Ok(agents) => {
                self.agents = agents;
                self.clear_form();
            }
            Err(e) => {
                self.message = Some(format!("Reload error: {}", e));
            }
        }
    }

    pub fn select_agent(&mut self, idx: usize) {
        self.selected_index = idx as isize;
        self.is_new = false;
        let agent = self.agents[idx].clone();
        self.name = agent.name.clone();
        self.description = agent.description.clone();
        self.system_prompt = agent.system_prompt.clone();
        self.priority = agent.priority;
        self.max_concurrent = agent.max_concurrent;
        self.enabled = agent.enabled;
        self.reasoning_effort = agent.reasoning_effort;
        self.sync_tools_from_agent(&agent.allowed_tools);
        self.message = None;
    }

    pub fn edit_agent(&mut self, idx: usize) {
        self.select_agent(idx);
    }

    pub fn new_agent(&mut self) {
        self.is_new = true;
        self.selected_index = -1;
        self.clear_form();
    }

    pub fn clear_form(&mut self) {
        self.is_new = false;
        self.selected_index = -1;
        self.name.clear();
        self.description.clear();
        self.system_prompt.clear();
        self.priority = 0;
        self.max_concurrent = 1;
        self.enabled = true;
        self.reasoning_effort = wuffagent_core::types::ReasoningEffort::default();
        self.allowed_tools.clear();
        for cb in &mut self.tool_checkboxes {
            *cb = false;
        }
        self.message = None;
    }

    pub fn save(&mut self, backend: &Arc<Backend>) -> bool {
        if self.name.trim().is_empty() {
            self.message = Some("Name is required.".to_string());
            return false;
        }
        self.sync_tools_from_checkboxes();
        let config = WorkerConfig {
            name: self.name.trim().to_string(),
            description: self.description.trim().to_string(),
            system_prompt: self.system_prompt.clone(),
            allowed_tools: self.allowed_tools.clone(),
            priority: self.priority,
            max_concurrent: self.max_concurrent,
            enabled: self.enabled,
            can_invoke: vec![],
            handoff_enabled: false,
            shell_config: wuffagent_core::agents::config::ShellConfig::default(),
            reasoning_effort: self.reasoning_effort,
        };
        let agent_manager = backend.agent_manager.lock().unwrap();
        let result = if self.is_new {
            agent_manager.add_agent(&config)
        } else if let Some(idx) = self.selected_index() {
            let old_name = self.agents[idx].name.clone();
            agent_manager.edit_agent(&old_name, &config)
        } else {
            return false;
        };
        drop(agent_manager);
        match result {
            Ok(()) => {
                self.message = Some("Agent saved successfully.".to_string());
                self.reload(backend);
                true
            }
            Err(e) => {
                self.message = Some(format!("Error: {}", e));
                false
            }
        }
    }

    pub fn delete_agent(&mut self, backend: &Arc<Backend>) {
        if let Some(idx) = self.selected_index() {
            let name = self.agents[idx].name.clone();
            let agent_manager = backend.agent_manager.lock().unwrap();
            match agent_manager.remove_agent(&name) {
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

    pub fn sync_tools_from_agent(&mut self, allowed: &[String]) {
        self.allowed_tools = allowed.to_vec();
        self.tool_checkboxes = self.available_tools.iter().map(|t| allowed.contains(t)).collect();
    }

    pub fn sync_tools_from_checkboxes(&mut self) {
        self.allowed_tools = self.available_tools.iter()
            .zip(&self.tool_checkboxes)
            .filter_map(|(name, checked)| if *checked { Some(name.clone()) } else { None })
            .collect();
    }

    fn selected_index(&self) -> Option<usize> {
        if self.selected_index >= 0 {
            Some(self.selected_index as usize)
        } else {
            None
        }
    }
}
