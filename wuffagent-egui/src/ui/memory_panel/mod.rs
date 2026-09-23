use eframe::egui;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use wuffagent_core::config::Config;
use wuffagent_core::memory::{MaintenanceProgress, MaintenanceReport, MemoryConfig, MemoryEntry, MemoryManager};
use super::theme::Theme;

mod editor;

/// Floor for the maintenance timeout so a misconfigured 0 can't cancel the
/// pass instantly.
pub(super) const MIN_MAINTENANCE_TIMEOUT_SECS: u64 = 10;

/// Full memory panel: list, search, edit, delete, and run-maintenance.
///
/// All reads and writes go through the shared [`MemoryManager`] (single-writer
/// discipline) — this widget never touches the JSON file directly.
pub struct MemoryPanel {
    pub show_panel: bool,
    /// Keyword filter; empty shows everything.
    pub(super) search: String,
    /// Id of the entry currently being edited, if any.
    pub(super) selected_id: Option<String>,
    /// Editable content for the selected entry.
    pub(super) edit_content: String,
    /// Editable tags (comma separated) for the selected entry.
    pub(super) edit_tags: String,
    /// One-shot: on the next frame, scroll the window so the editor for the
    /// selected entry is visible (set when the user clicks "Edit" on a row).
    pub(super) scroll_to_editor: bool,
    /// Confirmation dialog: pending delete id.
    pub(super) pending_delete: Option<String>,
    /// Result message shown at the top of the panel (success/error).
    pub(super) message: Option<String>,
    /// Latest maintenance summary.
    pub(super) maintenance_report: Option<String>,
    /// In-flight maintenance pass (result channel + start time); `None` when
    /// no pass is running.
    pub(super) maintenance: Option<MaintenanceJob>,
}

/// An in-flight maintenance pass. The batched pass runs on a helper thread
/// (the UI thread cannot `block_on` a runtime — see `start_maintenance`); the
/// result is delivered through `rx` and collected in `draw` each frame, while
/// `progress` (current/total batch) updates live for the "step i/M" hint.
pub(super) struct MaintenanceJob {
    pub(super) started_at: Instant,
    pub(super) rx: mpsc::Receiver<Result<MaintenanceReport, String>>,
    pub(super) progress: MaintenanceProgress,
}

impl MemoryPanel {
    pub fn new() -> Self {
        Self {
            show_panel: false,
            search: String::new(),
            selected_id: None,
            edit_content: String::new(),
            edit_tags: String::new(),
            scroll_to_editor: false,
            pending_delete: None,
            message: None,
            maintenance_report: None,
            maintenance: None,
        }
    }

    /// Draw the memory panel window. No-op (returns `None`) when not shown.
    ///
    /// Takes disjoint references into app state (manager, runtime, config) so
    /// the panel — which is itself a field of `ChatApp` — can be borrowed
    /// mutably while the app's other fields are read.
    ///
    /// Returns the new memory config if the user changed a setting in the
    /// Settings section, so the caller can persist it (I4 — before, memory
    /// settings were runtime-only and reverted on restart).
    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        memory: &Arc<MemoryManager>,
        runtime: &Arc<tokio::runtime::Runtime>,
        config: &Config,
    ) -> Option<MemoryConfig> {
        // Collect a finished maintenance pass (even while the window is
        // closed) so the result is ready the next time the panel is shown.
        if let Some(job) = &mut self.maintenance {
            let outcome = job.rx.try_recv();
            match outcome {
                Ok(Ok(report)) => {
                    self.maintenance_report = Some(report.summary.clone());
                    self.message = Some(format!("✓ Maintenance: {}", report.summary));
                    self.maintenance = None;
                }
                Ok(Err(e)) => {
                    self.message = Some(format!("✗ Maintenance failed: {}", e));
                    self.maintenance = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                // Helper thread exited without sending (shouldn't happen).
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.maintenance = None;
                }
            }
        }

        if !self.show_panel {
            return None;
        }
        let theme = Theme::from_name(&config.theme);
        let mut updated_mconfig: Option<MemoryConfig> = None;

        egui::Window::new("Memory")
            .collapsible(true)
            .resizable(true)
            .default_size([640.0, 480.0])
            .show(ctx, |ui| {
                ui.visuals_mut().panel_fill = theme.surface;

                let mconfig = memory.config();

                ui.heading("Memory");
                ui.label(egui::RichText::new(format!(
                    "{} active memories · project '{}' · max {}",
                    memory.count(),
                    mconfig.project,
                    mconfig.max_entries
                ))
                .color(theme.text_secondary));
                ui.separator();

                // Settings section (edits the live memory config).
                updated_mconfig = self.draw_settings(ui, memory);
                ui.separator();

                // Status / result message
                if let Some(msg) = &self.message {
                    let color = if msg.starts_with("✓") { theme.success } else { theme.warning };
                    ui.add(egui::Label::new(egui::RichText::new(msg).color(color)).wrap());
                    ui.separator();
                }

                // Search box
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("filter by keyword")
                            .desired_width(280.0),
                    );
                    if !self.search.trim().is_empty()
                        && ui.small_button("✕ Clear").clicked()
                    {
                        self.search.clear();
                    }
                });
                ui.separator();

                // Entry list
                let entries: Vec<MemoryEntry> = if self.search.trim().is_empty() {
                    memory.get_all_memories()
                } else {
                    memory.search(&self.search)
                };

                if entries.is_empty() {
                    ui.label(if self.search.trim().is_empty() {
                        "No memories yet. The agent writes them via the save_memory tool."
                    } else {
                        "No memories match the search."
                    });
                }

                // Cap the visible height: with auto_shrink (the default) the
                // area would otherwise request the full content height (dozens
                // of entries => thousands of px) and stretch the window to the
                // screen height.
                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .show_rows(ui, 46.0, entries.len(), |ui, range| {
                        for entry in &entries[range.clone()] {
                            self.draw_entry_row(ui, &theme, entry, memory);
                        }
                    });

                ui.separator();

                // Editor for the selected entry
                if self.selected_id.is_some() {
                    self.draw_editor(ui, &theme, memory);
                }

                ui.separator();

                // Maintenance
                self.draw_maintenance(ui, &theme, memory, runtime);

                ui.separator();
                if ui.button("Close").clicked() {
                    self.show_panel = false;
                }
            });
        updated_mconfig
    }

    /// A single list row: type badge, tags, age, id, preview + actions.
    fn draw_entry_row(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        entry: &MemoryEntry,
        memory: &MemoryManager,
    ) {
        let preview = truncate(&entry.content, 90);
        let age = format_age(entry);
        let is_selected = self.selected_id.as_deref() == Some(&entry.id);

        let frame_fill = if is_selected {
            theme.selected_bg
        } else {
            theme.surface_light
        };

        egui::Frame::new()
            .fill(frame_fill)
            .corner_radius(4)
            .inner_margin(egui::Margin::same(6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Type badge
                    let badge = format!("[{}]", entry.r#type);
                    ui.label(egui::RichText::new(badge).color(theme.accent).strong());

                    // Tags
                    if !entry.tags.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("#{}", entry.tags.join(" #")))
                                .color(theme.text_dim)
                                .small(),
                        );
                    }

                    // Age (right-aligned)
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("{}d old", age))
                                .color(theme.text_dim)
                                .small(),
                        );
                    });
                });

                // Truncated id
                ui.label(egui::RichText::new(short_id(&entry.id)).color(theme.text_dim).monospace());

                // Content preview
                ui.add(egui::Label::new(egui::RichText::new(preview).color(theme.text_primary)).wrap());

                ui.horizontal(|ui| {
                    let (content, tags) = (entry.content.clone(), entry.tags.join(", "));
                    let selected = self.selected_id.clone();
                    if ui
                        .add_enabled(
                            selected.as_deref() != Some(&entry.id),
                            egui::Button::new("Edit").fill(theme.surface),
                        )
                        .clicked()
                    {
                        self.selected_id = Some(entry.id.clone());
                        self.edit_content = content;
                        self.edit_tags = tags;
                        // The editor is drawn below the entry list and is
                        // often outside the visible window area — bring it
                        // into view on the next frame (see `draw_editor`).
                        self.scroll_to_editor = true;
                    }
                    if ui.add(egui::Button::new("Delete").fill(theme.surface_light)).clicked() {
                        self.pending_delete = Some(entry.id.clone());
                    }
                });
            });

        // Delete confirmation dialog (driven by pending_delete == this id).
        if self.pending_delete.as_deref() == Some(&entry.id) {
            let entry_id = entry.id.clone();
            egui::Window::new("Confirm delete")
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label("Delete this memory? This cannot be undone.");
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(truncate(&entry.content, 160))
                                .color(theme.text_secondary),
                        )
                        .wrap(),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.pending_delete = None;
                        }
                        if ui.add(egui::Button::new("Delete").fill(theme.error)).clicked() {
                            match memory.delete(&entry_id) {
                                Ok(Some(_)) => {
                                    self.message = Some(format!("✓ Deleted {}", short_id(&entry_id)));
                                    if self.selected_id.as_deref() == Some(&entry_id) {
                                        self.selected_id = None;
                                    }
                                }
                                Ok(None) => {
                                    self.message = Some(format!("Entry {} not found", short_id(&entry_id)));
                                }
                                Err(e) => {
                                    self.message = Some(format!("✗ Delete failed: {}", e));
                                }
                            }
                            self.pending_delete = None;
                        }
                    });
                });
        }
        ui.add_space(4.0);
    }
}

/// Truncate a string to at most `n` chars, appending an ellipsis if cut.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{}…", cut)
    }
}

/// Short display id (first 8 chars).
pub(super) fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Human age label in days.
fn format_age(entry: &MemoryEntry) -> usize {
    entry
        .timestamp
        .map(|ts| chrono::Utc::now().signed_duration_since(ts).num_days().max(0) as usize)
        .unwrap_or(0)
}
