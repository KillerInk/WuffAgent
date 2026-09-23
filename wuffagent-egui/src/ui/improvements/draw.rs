//! The self-improvement review panel's draw implementation (impl block for
//! `ImprovementsPanel`, split from `mod.rs`).

use std::path::PathBuf;

use eframe::egui;
use wuffagent_core::agents::config::AgentManager;
use wuffagent_core::memory::MemoryManager;

use super::ImprovementsPanel;
use crate::ui::agent_history;
use crate::ui::theme::Theme;

impl ImprovementsPanel {
    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        agent_manager: &AgentManager,
        agents_dirs: &[PathBuf],
        theme: &Theme,
        memory: &MemoryManager,
    ) {
        if !self.show_panel || self.pending.is_empty() {
            return;
        }

        egui::Window::new("Self-Improvement Suggestions")
            .id(egui::Id::new("improvements_panel"))
            .default_size([520.0, 360.0])
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                let badge = egui::RichText::new(format!("({} pending)", self.pending.len()))
                    .strong()
                    .color(theme.accent);
                ui.horizontal(|ui| {
                    ui.heading("Self-Improvement Suggestions");
                    ui.label(badge);
                });
                ui.separator();

                if let Some(msg) = &self.message {
                    let color = if msg.contains("Error") {
                        theme.error
                    } else {
                        theme.success
                    };
                    ui.colored_label(color, msg);
                    ui.separator();
                }

                ui.label(
                    egui::RichText::new("Review the LLM's proposed changes. Approve to apply, dismiss to reject.")
                        .weak(),
                );
                ui.separator();

                ui.label(egui::RichText::new("Pending suggestions:").strong());
                ui.add_space(4.0);

                // F4: per-item prompt history (newest first) for the Revert
                // button — only items proposing a prompt change for an
                // existing agent can be reverted; new-agent items have none.
                let item_histories: Vec<Vec<agent_history::HistoryEntry>> = self
                    .pending
                    .iter()
                    .map(|p| {
                        if p.prompt_change.is_some() {
                            agent_history::list_history(agents_dirs, &p.agent_name)
                        } else {
                            Vec::new()
                        }
                    })
                    .collect();

                // Collect actions to execute AFTER the loop (avoids mutating
                // `self.pending` while iterating).
                let mut to_remove: Vec<usize> = Vec::new();
                let mut to_approve: Vec<usize> = Vec::new();
                let mut to_revert: Vec<usize> = Vec::new();
                let mut to_dismiss: Vec<usize> = Vec::new();

                for (i, imp) in self.pending.iter_mut().enumerate() {
                    ui.collapsing(format!("Agent: {}", imp.agent_name), |ui| {
                        ui.label(egui::RichText::new(format!("Rationale: {}", imp.rationale)).weak());
                        ui.add_space(4.0);

                        // I3: show the evidence that triggered the suggestion.
                        if !imp.evidence.is_empty() {
                            ui.collapsing("Evidence (what the improver saw)", |ui| {
                                for e in &imp.evidence {
                                    ui.label(egui::RichText::new(e).weak().size(11.0));
                                }
                            });
                        }

                        if let Some(new_prompt) = &imp.prompt_change {
                            // I3: old-vs-new side by side — current prompt
                            // (read-only) next to the proposed one (editable).
                            let current_prompt = agent_manager
                                .get_agent(&imp.agent_name)
                                .map(|c| c.system_prompt.clone());
                            ui.horizontal(|ui| {
                                if let Some(cur) = &current_prompt {
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new("Current (read-only)")
                                                .strong()
                                                .weak(),
                                        );
                                        egui::ScrollArea::vertical()
                                            .max_height(150.0)
                                            .show(ui, |ui| {
                                                ui.label(
                                                    egui::RichText::new(cur)
                                                        .monospace()
                                                        .size(11.0),
                                                );
                                            });
                                    });
                                    ui.separator();
                                }
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(if current_prompt.is_some() {
                                            "Proposed (editable)"
                                        } else {
                                            "Proposed system prompt (editable)"
                                        })
                                        .strong(),
                                    );
                                    let mut buf = imp
                                        .edited_prompt
                                        .as_deref()
                                        .unwrap_or(new_prompt)
                                        .to_string();
                                    ui.add(
                                        egui::TextEdit::multiline(&mut buf)
                                            .desired_width(f32::INFINITY)
                                            .desired_rows(6),
                                    );
                                    // F1: persist the edited value in place so
                                    // the user's changes survive across frames
                                    // and are what gets applied on Approve.
                                    imp.edited_prompt = Some(buf);
                                });
                            });
                            ui.checkbox(&mut imp.apply_prompt, "Apply prompt change");
                        }

                        for na in imp.new_agents.iter_mut() {
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "New agent: '{}' — {}",
                                    na.proposal.name, na.proposal.description
                                ))
                                .strong(),
                            );
                            let mut sp = na.edited_system_prompt.clone();
                            ui.add(
                                egui::TextEdit::multiline(&mut sp)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(4),
                            );
                            // F1: persist the edited value (see above).
                            na.edited_system_prompt = sp;
                        }

                        // I2/I3: per-field changes with approve toggles — the
                        // user can accept the prompt but reject a tool change
                        // (or vice versa). Only fields the LLM proposed show.
                        if let Some(tools) = &imp.allowed_tools {
                            ui.checkbox(
                                &mut imp.apply_allowed_tools,
                                format!(
                                    "Apply tool allowlist change ({} tools: {})",
                                    tools.len(),
                                    tools.join(", ")
                                ),
                            );
                        }
                        if let Some(re) = &imp.reasoning_effort {
                            ui.checkbox(
                                &mut imp.apply_reasoning_effort,
                                format!("Apply reasoning effort change ({:?})", re),
                            );
                        }
                        if let Some(sc) = &imp.shell_config {
                            let desc = if sc.shell_enabled {
                                format!(
                                    "shell enabled, {} command pattern(s)",
                                    sc.allowed_commands.len()
                                )
                            } else {
                                "shell disabled".to_string()
                            };
                            ui.checkbox(
                                &mut imp.apply_shell_config,
                                format!("Apply shell config change ({})", desc),
                            );
                        }
                        if let Some(ht) = &imp.handoff_targets {
                            ui.checkbox(
                                &mut imp.apply_handoff_targets,
                                format!("Apply handoff target change ({})", ht.join(", ")),
                            );
                        }
                        if let Some(ms) = imp.task_timeout_ms {
                            ui.checkbox(
                                &mut imp.apply_task_timeout,
                                format!("Apply task timeout change ({} ms)", ms),
                            );
                        }

                        let has_config_change = imp.prompt_change.is_some()
                            || imp.allowed_tools.is_some()
                            || imp.reasoning_effort.is_some()
                            || imp.shell_config.is_some()
                            || imp.handoff_targets.is_some()
                            || imp.task_timeout_ms.is_some();
                        if has_config_change {
                            // Existing-agent prompt change → Approve + Dismiss + Revert.
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(true, egui::Button::new("✓ Approve").fill(theme.primary))
                                    .clicked()
                                {
                                    if !to_approve.contains(&i) {
                                        to_approve.push(i);
                                    }
                                }
                                if ui.add(egui::Button::new("✗ Dismiss")).clicked() {
                                    if !to_remove.contains(&i) {
                                        to_remove.push(i);
                                    }
                                    if !to_dismiss.contains(&i) {
                                        to_dismiss.push(i);
                                    }
                                }
                                // F4: revert this agent to its latest prompt
                                // snapshot. "Click again to confirm" — no
                                // modal, keeps the per-frame draw simple.
                                let hist = &item_histories[i];
                                let has_hist = !hist.is_empty();
                                let label = if imp.revert_armed && has_hist {
                                    format!("↩ Confirm revert to {}?", agent_history::format_ts(hist[0].ts))
                                } else {
                                    "↩ Revert".to_string()
                                };
                                if ui
                                    .add_enabled(has_hist, egui::Button::new(label))
                                    .on_hover_text("Restore the previous prompt from the latest history snapshot. Pending suggestions for this agent are dropped (they were reviewed against the now-reverted prompt).")
                                    .clicked()
                                {
                                    if imp.revert_armed {
                                        to_revert.push(i);
                                    } else {
                                        imp.revert_armed = true;
                                    }
                                }
                                if !has_hist {
                                    ui.label(egui::RichText::new("(no prompt history)").weak());
                                }
                            });
                        } else {
                            // New-agent proposal → only dismiss makes sense
                            // (approve applies every bundled proposal).
                            ui.horizontal(|ui| {
                                if ui.add(egui::Button::new("✗ Dismiss")).clicked() {
                                    if !to_remove.contains(&i) {
                                        to_remove.push(i);
                                    }
                                    if !to_dismiss.contains(&i) {
                                        to_dismiss.push(i);
                                    }
                                }
                            });
                        }
                        ui.separator();
                    });
                }

                // F4: execute reverts (file I/O) before approves so both see
                // the pre-removal list; pending-list removals happen only in
                // the single pass below, so approve indices stay valid.
                for i in &to_revert {
                    let name = self.pending[*i].agent_name.clone();
                    let entry = item_histories[*i].first().cloned();
                    match entry {
                        Some(e) => match agent_history::revert(&e.dir, &name, &e) {
                            Ok(_) => {
                                self.message = Some(format!(
                                    "reverted '{}' to {} — pending suggestions for this agent were dropped",
                                    name,
                                    agent_history::format_ts(e.ts)
                                ));
                                // Drop ALL pending suggestions for this agent:
                                // they were reviewed against the now-reverted prompt.
                                for k in 0..self.pending.len() {
                                    if self.pending[k].agent_name == name && !to_remove.contains(&k) {
                                        to_remove.push(k);
                                    }
                                }
                            }
                            Err(err) => {
                                self.pending[*i].revert_armed = false;
                                self.message = Some(format!(
                                    "Error: revert of '{}' failed: {}",
                                    name, err
                                ));
                            }
                        },
                        None => {
                            self.pending[*i].revert_armed = false;
                            self.message = Some(format!(
                                "revert of '{}': no history snapshot available",
                                name
                            ));
                        }
                    }
                }

                // Execute collected actions (file I/O + list mutation) after
                // the loop so we never mutate while iterating.
                for i in to_approve.into_iter().rev() {
                    let (outcome, prompt_applied) =
                        super::memory::apply_improvement_detailed(agents_dirs, agent_manager, &self.pending[i]);
                    self.message = Some(outcome);
                    // I5: an approved prompt change gets a marker so the next
                    // improvement check can weigh the outcomes since it and
                    // propose a revert. A store failure is surfaced (F5-style)
                    // but does not undo the approval itself.
                    if prompt_applied {
                        if let Err(err) = super::memory::remember_applied_prompt(memory, &self.pending[i]) {
                            self.message = Some(format!(
                                "approved '{}', but remembering the prompt change failed: {}",
                                self.pending[i].agent_name, err
                            ));
                        }
                    }
                    if !to_remove.contains(&i) {
                        to_remove.push(i);
                    }
                }

                // F5: remember explicit dismissals as negative evidence for
                // the improver. The dedup gate in `MemoryManager::add` makes
                // re-dismissing the same suggestion a no-op. Approves and
                // reverts do NOT record a lesson (the item was acted on, not
                // rejected).
                for i in &to_dismiss {
                    if let Err(err) = super::memory::remember_dismissal(memory, &self.pending[*i]) {
                        self.message = Some(format!(
                            "dismissed '{}', but remembering the rejection failed: {}",
                            self.pending[*i].agent_name, err
                        ));
                    }
                }

                for i in to_remove.into_iter().rev() {
                    if i < self.pending.len() {
                        self.pending.remove(i);
                    }
                }
            });
    }
}
