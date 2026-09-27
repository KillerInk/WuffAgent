//! The self-improvement review panel's draw implementation (impl block for
//! `ImprovementsPanel`, split from `mod.rs`).

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui;
use wuffagent_core::agents::config::AgentManager;
use wuffagent_core::memory::MemoryManager;
use wuffagent_core::types::AppEvent;

use super::ImprovementsPanel;
use crate::ui::agent_history;
use crate::ui::theme::Theme;

/// 2d: the run-check selector's special "fleet" entry — a cross-agent review
/// (2b(b)) instead of one profile (the tool-side counterpart is
/// `run_self_improvement`'s `scope: "fleet"`).
const FLEET_SCOPE: &str = "fleet";

/// 2d: send a (manual) check's result over the AppEvent channel — the
/// suggestions (if any) plus the ALWAYS-sent done signal. Shared by the
/// per-agent and the fleet run-check buttons so the two code paths cannot
/// drift (a lost done signal would leave "Checking…" stuck).
fn send_check_result(
    events: &Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    agent_name: &str,
    result: &Result<Vec<wuffagent_core::types::ImprovementSuggestion>, String>,
) {
    let sender = match events {
        Some(s) => s,
        None => return,
    };
    let lock = match sender.lock() {
        Ok(l) => l,
        Err(_) => return,
    };
    match result {
        Ok(suggestions) => {
            let produced = !suggestions.is_empty();
            if produced {
                let _ = lock.send(AppEvent::ImprovementSuggested {
                    agent_name: agent_name.to_string(),
                    suggestions: suggestions.clone(),
                    session_id: String::new(),
                });
            }
            let _ = lock.send(AppEvent::ImprovementCheckFinished {
                agent_name: agent_name.to_string(),
                produced,
            });
        }
        // Err: the check already logged the failure; just clear the running
        // state (no suggestions to add).
        Err(_) => {
            let _ = lock.send(AppEvent::ImprovementCheckFinished {
                agent_name: agent_name.to_string(),
                produced: false,
            });
        }
    }
}

impl ImprovementsPanel {
    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        agent_manager: &AgentManager,
        agents_dirs: &[PathBuf],
        theme: &Theme,
        memory: &MemoryManager,
        memory_arc: Arc<MemoryManager>,
        events: Option<Arc<Mutex<mpsc::Sender<AppEvent>>>>,
    ) {
        // 4b: draw the panel even with an empty queue, so the "run check now"
        // button below stays reachable when there are no pending suggestions.
        if !self.show_panel {
            return;
        }
        // 3b: proposed skills apply through the shared SkillStore. The default
        // root is deterministic (`~/.wuffagent/skills`), so re-deriving it
        // here (mcp `config_path` pattern) keeps the panel independent of
        // whatever handle ChatApp happens to hold.
        let skill_store = wuffagent_core::memory::skills::SkillStore::default();

        // The window title bar is the panel's ONE identity: the pending
        // count is part of the title (visible even when the window is
        // collapsed), and there is no second heading row inside repeating
        // it. The window id is pinned explicitly, so the dynamic title
        // never changes the window's id (nor the child widgets' ids).
        let title = panel_title(self.pending.len());
        egui::Window::new(title)
            .id(egui::Id::new("improvements_panel"))
            .default_size([520.0, 360.0])
            .collapsible(true)
            .resizable(true)
            .show(ctx, |ui| {
                // Re-fetch the agent list ONLY if the agent dirs changed
                // since the last frame (see AgentListCache) — the previous
                // per-frame list_agents() re-scan flooded the log while
                // tokens streamed.
                self.refresh_agent_cache(agent_manager);
                // 4a: one-line loop-status header (data from 2a's state file,
                // same snapshot the list_improvement_status tool reads). The
                // panel is fleet-wide, so show the most recent check across
                // the global + per-agent states.
                let loop_status = memory.improvement_status();
                let status_text = loop_status_header_line(&loop_status);
                ui.label(egui::RichText::new(status_text).weak());
                ui.separator();

                // 4b: "run check now" — an on-demand improvement check that
                // bypasses the cooldown (the same core path the
                // `run_self_improvement` tool uses: `run_improvement_check`).
                // It runs on a background thread (LLM call, up to 180s); the
                // result comes back over the AppEvent channel (suggestions →
                // `ImprovementSuggested`, the always-sent done-signal →
                // `ImprovementCheckFinished`, which clears the running flag).
                {
                    let agents: Vec<String> = self
                        .agents()
                        .iter()
                        .map(|a| a.name.clone())
                        .collect();
                    // 2d: the selector lists every profile PLUS the special
                    // "fleet" entry (cross-agent review, 2b(b)).
                    let mut selector: Vec<String> = agents.clone();
                    selector.push(FLEET_SCOPE.to_string());
                    let auto_on = memory.config().auto_improve;
                    // Re-resolve the selector if it is empty or the entry
                    // disappeared (fresh agent list each frame). The default
                    // is the FIRST profile (not "fleet") — per-agent checks
                    // are the common case.
                    if self.run_check_agent.is_empty() || !selector.contains(&self.run_check_agent) {
                        self.run_check_agent = agents.first().cloned().unwrap_or_else(|| FLEET_SCOPE.to_string());
                    }
                    ui.horizontal(|ui| {
                        ui.label("Run check for:");
                        // egui 0.36: ComboBox is a widget struct (no Ui::combo_box).
                        // Write into a local, then copy back (matches input/mod.rs).
                        let mut next_agent = self.run_check_agent.clone();
                        egui::ComboBox::from_id_salt("improvements_run_check_agent")
                            .width(140.0)
                            .selected_text(self.run_check_agent.clone())
                            .show_ui(ui, |ui| {
                                for name in selector.iter() {
                                    ui.selectable_value(&mut next_agent, name.clone(), name.as_str());
                                }
                            });
                        self.run_check_agent = next_agent;
                        let label = if self.run_check_running {
                            "Checking…"
                        } else {
                            "Run check now"
                        };
                        let enabled = auto_on && !self.run_check_running;
                        let hover = if !auto_on {
                            "auto_improve is off (memory settings)"
                        } else if self.run_check_running {
                            "A check is already in progress"
                        } else if self.run_check_agent == FLEET_SCOPE {
                            "Run an on-demand FLEET review now: cross-agent patterns (shared failures → skills / new shared agents, skill maintenance). Bypasses the per-agent cooldowns"
                        } else {
                            "Run an on-demand self-improvement check for this agent now (bypasses the cooldown)"
                        };
                        if ui.add_enabled(enabled, egui::Button::new(label)).on_hover_text(hover).clicked()
                        {
                            let agent = self.run_check_agent.clone();
                            if agent == FLEET_SCOPE {
                                // 2d: FLEET review (2b(b)) — roster = name +
                                // description of every known profile (the
                                // MemoryManager does not know about profiles,
                                // so the panel builds it).
                                let roster: Vec<(String, String)> = self
                                    .agents()
                                    .iter()
                                    .map(|a| (a.name.clone(), a.description.clone()))
                                    .collect();
                                self.run_check_running = true;
                                self.run_check_status = "Checking the fleet…".to_string();
                                let memory = memory_arc.clone();
                                let events = events.clone();
                                std::thread::spawn(move || {
                                    let result = memory.run_fleet_improvement_check(&roster, None);
                                    send_check_result(&events, FLEET_SCOPE, &result);
                                });
                            } else {
                                match self
                                    .agents()
                                    .iter()
                                    .find(|a| a.name == agent)
                                    .cloned()
                                {
                                    Some(cfg) => {
                                        self.run_check_running = true;
                                        self.run_check_status = format!("Checking '{agent}'…");
                                        let memory = memory_arc.clone();
                                        let events = events.clone();
                                        std::thread::spawn(move || {
                                            let result = memory.run_improvement_check(&cfg, None);
                                            send_check_result(&events, &agent, &result);
                                        });
                                    }
                                    None => {
                                        self.run_check_status = format!("Agent '{agent}' not found");
                                    }
                                }
                            }
                        }
                    });
                    if !self.run_check_status.is_empty() {
                        ui.label(egui::RichText::new(self.run_check_status.clone()).weak());
                    }
                }
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

                // The CURRENT system prompt of each pending item's agent (for
                // the I3 old-vs-new comparison), from the cached agent list
                // (NOT a fresh disk scan per frame). Precomputed because the
                // loop below holds a &mut borrow of `self.pending`.
                let current_prompts: Vec<Option<String>> = self
                    .pending
                    .iter()
                    .map(|p| {
                        self.agents()
                            .iter()
                            .find(|a| a.name == p.agent_name)
                            .map(|c| c.system_prompt.clone())
                    })
                    .collect();

                // Collect actions to execute AFTER the loop (avoids mutating
                // `self.pending` while iterating).
                let mut to_remove: Vec<usize> = Vec::new();
                let mut to_approve: Vec<usize> = Vec::new();
                let mut to_revert: Vec<usize> = Vec::new();
                let mut to_revert_skills: Vec<(usize, String)> = Vec::new();
                let mut to_dismiss: Vec<usize> = Vec::new();

                for (i, imp) in self.pending.iter_mut().enumerate() {
                    // F2 can stack several items for the SAME agent (different
                    // rationales) — a label-derived widget id would then be
                    // used at two positions in one frame (egui id-clash
                    // warning: "First/Second use of widget ID"). Push an
                    // item-scoped id derived from the same (agent, rationale)
                    // identity the dedupe in `handle_improvement_suggested`
                    // uses, so every item's header id (and every child
                    // widget's, via the parent chain) is unique.
                    ui.push_id((imp.agent_name.as_str(), imp.rationale.as_str()), |ui| {
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
                            let current_prompt = &current_prompts[i];
                            // Equal columns: a plain ui.horizontal lets the
                            // read-only side (a long unwrapped label whose
                            // desired size is the full line length) eat the
                            // whole row and squeeze the editable side into
                            // an unreadable sliver. ui.columns pins each side
                            // to half the width.
                            if let Some(cur) = &current_prompt {
                                ui.columns(2, |cols| {
                                    {
                                        let ui = &mut cols[0];
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
                                    }
                                    {
                                        let ui = &mut cols[1];
                                        ui.label(
                                            egui::RichText::new("Proposed (editable)").strong(),
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
                                    }
                                });
                            } else {
                                // No current prompt to compare against: the
                                // proposed side gets the full width.
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new("Proposed system prompt (editable)")
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
                                    imp.edited_prompt = Some(buf);
                                });
                            }
                            ui.checkbox(&mut imp.apply_prompt, "Apply prompt change");
                        }

                        // 2c: proposed one-line description change (editable,
                        // same "user edit wins" pattern as the prompt).
                        if let Some(desc) = imp.description.clone() {
                            let mut buf = desc;
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new("Proposed description (editable)").strong(),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf)
                                        .desired_width(f32::INFINITY),
                                );
                                ui.checkbox(&mut imp.apply_description, "Apply description change");
                            });
                            imp.description = Some(buf);
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

                        // 3b: proposed skills (procedural memory) — read-only
                        // previews; Approve saves each via the SkillStore
                        // (overwriting an existing name is the versioning
                        // mechanism, so "update" and "new" both save).
                        if !imp.skill_updates.is_empty() {
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Proposed skills ({}):",
                                        imp.skill_updates.len()
                                    ))
                                    .strong(),
                                );
                                for sk in &imp.skill_updates {
                                    let action = sk.action.trim().to_ascii_lowercase();
                                    let is_delete = action == "delete";
                                    let verb = if is_delete {
                                        "retire"
                                    } else if action == "update" {
                                        "update"
                                    } else {
                                        "new"
                                    };
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("{} ({})", sk.name, verb))
                                                .strong(),
                                        );
                                        // 3c: per-skill Revert (F4 two-click
                                        // pattern) — restore this skill's
                                        // latest version snapshot, e.g. undo a
                                        // previously approved bad rewrite or a
                                        // retire (available for every verb).
                                        let canonical = sk.name.trim().to_ascii_lowercase();
                                        let sk_hist = skill_store.list_skill_history(&canonical);
                                        if !sk_hist.is_empty() {
                                            let armed =
                                                imp.skill_revert_armed.iter().any(|n| n == &canonical);
                                            let label = if armed {
                                                let ts = skill_snapshot_ts(&sk_hist[0], &canonical);
                                                format!("↩ Confirm revert to {}?", agent_history::format_ts(ts))
                                            } else {
                                                "↩ Revert".to_string()
                                            };
                                            if ui
                                                .add_enabled(
                                                    true,
                                                    egui::Button::new(egui::RichText::new(label).weak()).small(),
                                                )
                                                .on_hover_text("Restore this skill to its latest version snapshot (the state before the most recent overwrite/retire). Two clicks: this arms it, the next confirms.")
                                                .clicked()
                                            {
                                                if armed {
                                                    to_revert_skills.push((i, canonical.clone()));
                                                } else {
                                                    imp.skill_revert_armed.push(canonical.clone());
                                                }
                                            }
                                        }
                                    });
                                    if is_delete {
                                        // 3b: a retire carries no (or stale)
                                        // metadata — just state what it does.
                                        ui.label(
                                            egui::RichText::new(
                                                "Deletes the skill file if approved (usage: never read in the window).",
                                            )
                                            .weak(),
                                        );
                                        continue;
                                    }
                                    if !sk.description.is_empty() {
                                        ui.label(
                                            egui::RichText::new(sk.description.clone()).weak(),
                                        );
                                    }
                                    if !sk.when_to_use.is_empty() {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Use: {}",
                                                sk.when_to_use
                                            ))
                                            .weak(),
                                        );
                                    }
                                    let preview: String = sk.body.chars().take(400).collect();
                                    egui::ScrollArea::vertical()
                                        .max_height(120.0)
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(preview)
                                                    .monospace()
                                                    .size(11.0),
                                            );
                                        });
                                }
                                ui.checkbox(
                                    &mut imp.apply_skills,
                                    format!(
                                        "Apply {} proposed skill(s)",
                                        imp.skill_updates.len()
                                    ),
                                );
                            });
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
                            || imp.description.is_some()
                            || imp.allowed_tools.is_some()
                            || imp.reasoning_effort.is_some()
                            || imp.shell_config.is_some()
                            || imp.handoff_targets.is_some()
                            || imp.task_timeout_ms.is_some()
                            || !imp.skill_updates.is_empty();
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
                    });
                }

                // G.1: if anything was acted on this frame, the queue
                // changed — persist what remains after the removals below.
                let had_actions =
                    !to_approve.is_empty() || !to_dismiss.is_empty() || !to_revert.is_empty();

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

                // 3c: execute skill reverts (file I/O) — restore each skill
                // to its latest version snapshot. The pending item stays (a
                // skill revert does not invalidate the prompt/tool
                // proposals), but the user is told the skill suggestion may
                // now be stale.
                for (i, name) in to_revert_skills {
                    if i >= self.pending.len() {
                        continue; // defensive: indices came from this frame
                    }
                    let sk_hist = skill_store.list_skill_history(&name);
                    let Some(latest) = sk_hist.first().cloned() else {
                        self.pending[i].skill_revert_armed.retain(|n| n != &name);
                        self.message =
                            Some(format!("revert of skill '{name}': no history snapshot available"));
                        continue;
                    };
                    let ts = skill_snapshot_ts(&latest, &name);
                    match skill_store.revert_skill(&name, &latest) {
                        Ok(skill) => {
                            self.pending[i].skill_revert_armed.retain(|n| n != &name);
                            self.message = Some(format!(
                                "reverted skill '{}' to {} — the skill suggestion in this item may now be stale",
                                skill.name,
                                agent_history::format_ts(ts)
                            ));
                        }
                        Err(e) => {
                            self.pending[i].skill_revert_armed.retain(|n| n != &name);
                            self.message = Some(format!("Error: revert of skill '{name}' failed: {e}"));
                        }
                    }
                }

                // Execute collected actions (file I/O + list mutation) after
                // the loop so we never mutate while iterating.
                for i in to_approve.into_iter().rev() {
                    let (outcome, prompt_applied) = super::memory::apply_improvement_detailed(
                        agents_dirs,
                        agent_manager,
                        &skill_store,
                        &self.pending[i],
                    );
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

                // G.1: queue shrank (or changed) — persist the remainder.
                if had_actions {
                    self.persist();
                }
            });
    }
}

/// 3c: parse the unix timestamp out of a skill-history snapshot filename
/// (`<name>-<unixts>[-seq].md`) for the Revert button label. Mirrors the
/// agent-history label path (`agent_history::HistoryEntry.ts`).
fn skill_snapshot_ts(path: &std::path::Path, name: &str) -> u64 {
    let file_name = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default();
    let stem = file_name.strip_suffix(".md").unwrap_or(file_name);
    let tail = stem.strip_prefix(&format!("{name}-")).unwrap_or("");
    crate::ui::agent_history::parse_ts_seq(tail).0
}

/// The window title is the panel's single identifying label (no inner
/// heading repeats it). The pending count rides in the title so it stays
/// visible even when the window is collapsed.
fn panel_title(pending: usize) -> String {
    if pending == 0 {
        "Self-Improvement Suggestions".to_string()
    } else {
        format!("Self-Improvement Suggestions ({pending} pending)")
    }
}

/// 4a: the panel's one-line loop-status header, e.g.
/// "Loop: last check 12:41:05 (3m ago) · new evidence: yes · lessons: 5 · auto_improve: on
/// · min interval: off".
/// Read from the same 2a state snapshot the `list_improvement_status` tool uses.
fn loop_status_header_line(status: &wuffagent_core::memory::ImprovementStatus) -> String {
    let min_interval = if status.improvement_min_interval_hours == 0 {
        "off".to_string()
    } else {
        format!("{}h", status.improvement_min_interval_hours)
    };
    format!(
        "Loop: last check {} · new evidence: {} · lessons: {} · auto_improve: {} · min interval: {}",
        format_last_check(most_recent_check(status)),
        if status.has_new_evidence { "yes" } else { "no" },
        status.lesson_count,
        if status.auto_improve { "on" } else { "off" },
        min_interval,
    )
}

/// The most recent improvement-check timestamp across the global state and
/// every per-agent state (the panel is fleet-wide, so "last check" means the
/// newest of them all).
fn most_recent_check(
    status: &wuffagent_core::memory::ImprovementStatus,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let mut best = status.last_check;
    for st in status.agents.values() {
        if let Some(ts) = st.last_check {
            if best.map_or(true, |b| ts > b) {
                best = Some(ts);
            }
        }
    }
    best
}

/// Render a timestamp as "HH:MM:SS (N ago)" or "never".
fn format_last_check(ts: Option<chrono::DateTime<chrono::Utc>>) -> String {
    match ts {
        Some(ts) => {
            let secs = chrono::Utc::now()
                .timestamp()
                .saturating_sub(ts.timestamp());
            let ago = if secs < 3_600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86_400 {
                format!("{}h ago", secs / 3_600)
            } else {
                format!("{}d ago", secs / 86_400)
            };
            format!("{} ({})", ts.format("%H:%M:%S"), ago)
        }
        None => "never".to_string(),
    }
}

#[cfg(test)]
mod tests_4a {
    use super::*;
    use chrono::Utc;
    use wuffagent_core::memory::{AgentImprovementState, ImprovementStatus};

    fn status() -> ImprovementStatus {
        ImprovementStatus {
            last_check: None,
            has_new_evidence: false,
            auto_improve: true,
            improvement_cooldown_tasks: 5,
            improvement_min_interval_hours: 0,
            lesson_count: 0,
            agents: std::collections::BTreeMap::new(),
        }
    }

    fn state(ts: Option<chrono::DateTime<chrono::Utc>>) -> AgentImprovementState {
        AgentImprovementState {
            last_check: ts,
            runs_since_check: 0,
            no_op_streak: 0,
            last_effect_verdict: None,
        }
    }

    #[test]
    fn panel_title_is_single_identity_and_carries_count() {
        // No duplicate identity: the plain title is the panel's name, and the
        // pending count is appended (not a second heading row inside).
        assert_eq!(panel_title(0), "Self-Improvement Suggestions");
        assert_eq!(panel_title(1), "Self-Improvement Suggestions (1 pending)");
        assert_eq!(panel_title(5), "Self-Improvement Suggestions (5 pending)");
    }

    #[test]
    fn header_shows_never_when_no_check() {
        let line = loop_status_header_line(&status());
        assert!(line.contains("last check never"), "got: {line}");
        assert!(line.contains("new evidence: no"), "got: {line}");
        assert!(line.contains("auto_improve: on"), "got: {line}");
        assert!(line.contains("min interval: off"), "got: {line}");
    }

    #[test]
    fn header_shows_configured_min_interval() {
        let mut s = status();
        s.improvement_min_interval_hours = 24;
        let line = loop_status_header_line(&s);
        assert!(line.contains("min interval: 24h"), "got: {line}");
    }

    #[test]
    fn most_recent_prefers_newest_across_agents() {
        let mut s = status();
        let older = Utc::now() - chrono::Duration::hours(5);
        let newer = Utc::now() - chrono::Duration::minutes(2);
        s.last_check = Some(older);
        s.agents.insert("coder".into(), state(Some(newer)));
        s.agents.insert("architect".into(), state(Some(older)));
        assert_eq!(most_recent_check(&s), Some(newer));
    }

    #[test]
    fn format_last_check_never_and_relative() {
        assert_eq!(format_last_check(None), "never");
        let recent = Utc::now() - chrono::Duration::minutes(3);
        let out = format_last_check(Some(recent));
        assert!(out.contains("3m ago"), "got: {out}");
    }
}
