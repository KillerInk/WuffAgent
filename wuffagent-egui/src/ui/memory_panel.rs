use eframe::egui;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::memory::{InjectionMode, MaintenanceProgress, MaintenanceReport, MemoryEntry, MemoryManager};
use super::theme::Theme;

/// Floor for the maintenance timeout so a misconfigured 0 can't cancel the
/// pass instantly.
const MIN_MAINTENANCE_TIMEOUT_SECS: u64 = 10;

/// Full memory panel: list, search, edit, delete, and run-maintenance.
///
/// All reads and writes go through the shared [`MemoryManager`] (single-writer
/// discipline) — this widget never touches the JSON file directly.
pub struct MemoryPanel {
    pub show_panel: bool,
    /// Keyword filter; empty shows everything.
    search: String,
    /// Id of the entry currently being edited, if any.
    selected_id: Option<String>,
    /// Editable content for the selected entry.
    edit_content: String,
    /// Editable tags (comma separated) for the selected entry.
    edit_tags: String,
    /// One-shot: on the next frame, scroll the window so the editor for the
    /// selected entry is visible (set when the user clicks "Edit" on a row).
    scroll_to_editor: bool,
    /// Confirmation dialog: pending delete id.
    pending_delete: Option<String>,
    /// Result message shown at the top of the panel (success/error).
    message: Option<String>,
    /// Latest maintenance summary.
    maintenance_report: Option<String>,
    /// In-flight maintenance pass (result channel + start time); `None` when
    /// no pass is running.
    maintenance: Option<MaintenanceJob>,
}

/// An in-flight maintenance pass. The batched pass runs on a helper thread
/// (the UI thread cannot `block_on` a runtime — see `start_maintenance`); the
/// result is delivered through `rx` and collected in `draw` each frame, while
/// `progress` (current/total batch) updates live for the "step i/M" hint.
struct MaintenanceJob {
    started_at: Instant,
    rx: mpsc::Receiver<Result<MaintenanceReport, String>>,
    progress: MaintenanceProgress,
}

impl Default for MemoryPanel {
    fn default() -> Self {
        Self::new()
    }
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

    /// Draw the memory panel window. No-op when not shown.
    ///
    /// Takes disjoint references into app state (manager, runtime, config) so
    /// the panel — which is itself a field of `ChatApp` — can be borrowed
    /// mutably while the app's other fields are read.
    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        memory: &Arc<MemoryManager>,
        runtime: &Arc<tokio::runtime::Runtime>,
        config: &Config,
    ) {
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
            return;
        }
        let theme = Theme::from_name(&config.theme);

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
                self.draw_settings(ui, memory);
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

    /// Inline editor for the selected entry.
    ///
    /// Rendered as a permanent (non-collapsible) section: the previous
    /// `ui.collapsing` wrapper started out CLOSED, so clicking "Edit" on a
    /// row only revealed a collapsed "Edit memory …" header and the text
    /// fields were invisible — the panel looked uneditable.
    fn draw_editor(&mut self, ui: &mut egui::Ui, theme: &Theme, memory: &MemoryManager) {
        let Some(id) = self.selected_id.clone() else {
            return;
        };

        let resp = egui::Frame::new()
            .fill(theme.surface_light)
            .corner_radius(4)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(format!("Edit memory {}", short_id(&id)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            self.selected_id = None;
                        }
                    });
                });
                ui.label("Content:");
                ui.add(
                    egui::TextEdit::multiline(&mut self.edit_content)
                        .desired_rows(6)
                        .desired_width(f32::INFINITY),
                );
                ui.label("Tags (comma separated):");
                ui.add(egui::TextEdit::singleline(&mut self.edit_tags).desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new("Save").fill(theme.primary)).clicked() {
                        let tags: Vec<String> = self
                            .edit_tags
                            .split(',')
                            .map(|t| t.trim().to_string())
                            .filter(|t| !t.is_empty())
                            .collect();
                        match memory.update(&id, &self.edit_content, Some(tags)) {
                            Ok(updated) => {
                                self.message = Some(format!("✓ Updated {}", short_id(&updated.id)));
                                // Close the editor now that the edit is committed
                                // (Cancel/✕ do this too; Save previously left it open).
                                self.selected_id = None;
                            }
                            Err(e) => {
                                self.message = Some(format!("✗ Update failed: {}", e));
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.selected_id = None;
                    }
                });
            });

        // If the user just clicked "Edit", the window may be scrolled such
        // that the editor (drawn below the entry list) is off-screen. Force
        // the window's scroll area to include the editor rect, once.
        if self.scroll_to_editor {
            ui.scroll_to_rect(resp.response.rect, Some(egui::Align::TOP));
            self.scroll_to_editor = false;
        }
    }

    /// Memory settings section (edits the live config in place).
    fn draw_settings(&mut self, ui: &mut egui::Ui, memory: &MemoryManager) {
        let mut mconfig = memory.config();

        ui.collapsing("Settings", |ui| {
            let enabled = ui.add(egui::Checkbox::new(&mut mconfig.enabled, "Memory enabled")).changed();

            ui.horizontal(|ui| {
                ui.label("Injection mode:");
                if ui
                    .selectable_label(mconfig.injection_mode == InjectionMode::Always, "Always")
                    .clicked()
                {
                    mconfig.injection_mode = InjectionMode::Always;
                }
                if ui
                    .selectable_label(mconfig.injection_mode == InjectionMode::Smart, "Smart")
                    .clicked()
                {
                    mconfig.injection_mode = InjectionMode::Smart;
                }
                if ui
                    .selectable_label(mconfig.injection_mode == InjectionMode::Off, "Off")
                    .clicked()
                {
                    mconfig.injection_mode = InjectionMode::Off;
                }
            });

            let max_changed = ui
                .add(egui::DragValue::new(&mut mconfig.max_entries).range(1..=10_000).suffix(" max entries"))
                .changed();
            let inj_changed = ui
                .add(
                    egui::DragValue::new(&mut mconfig.injection_max_entries)
                        .range(0..=100)
                        .suffix(" inject"),
                )
                .changed();

            let maint_changed = ui.add(egui::Checkbox::new(&mut mconfig.memory_maintenance, "LLM maintenance (opt-in)")).changed();
            let thresh_changed = ui
                .add(
                    egui::DragValue::new(&mut mconfig.memory_maintenance_threshold)
                        .range(1..=1000)
                        .prefix("at ≥ "),
                )
                .changed();
            let batch_changed = ui
                .add(
                    egui::DragValue::new(&mut mconfig.memory_maintenance_batch_size)
                        .range(5..=100)
                        .suffix(" entries / step"),
                )
                .changed();
            let timeout_changed = ui
                .add(
                    egui::DragValue::new(&mut mconfig.memory_maintenance_timeout_secs)
                        .range(MIN_MAINTENANCE_TIMEOUT_SECS..=3600)
                        .suffix(" s step timeout"),
                )
                .changed();
            let auto_changed = ui.add(egui::Checkbox::new(&mut mconfig.auto_improve, "Auto-improve prompts (off by default)")).changed();

            if enabled || max_changed || inj_changed || maint_changed || thresh_changed || batch_changed || timeout_changed || auto_changed {
                memory.set_config(mconfig.clone());
                self.message = Some("✓ Memory settings updated".to_string());
            }
        });
    }

    /// Maintenance controls + report.
    fn draw_maintenance(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        memory: &Arc<MemoryManager>,
        runtime: &Arc<tokio::runtime::Runtime>,
    ) {
        let mconfig = memory.config();
        let count = memory.count();
        let running = self.maintenance.is_some();
        // Per-STEP (batch) timeout, user-configurable (Settings → "… s step
        // timeout"). Each batch self-bounds to this inside
        // `run_maintenance_full`, so the outer pass timeout below scales with
        // the estimated number of batches.
        let step_timeout = Duration::from_secs(
            mconfig
                .memory_maintenance_timeout_secs
                .max(MIN_MAINTENANCE_TIMEOUT_SECS),
        );
        let batch_size = mconfig.memory_maintenance_batch_size.clamp(5, 100);
        let est_batches = ((count + batch_size - 1) / batch_size).max(1);
        let total_timeout = Duration::from_secs(step_timeout.as_secs() * est_batches as u64);

        ui.horizontal(|ui| {
            ui.label("Maintenance:");
            let can_run = mconfig.memory_maintenance
                && count >= mconfig.memory_maintenance_threshold
                && !running;
            let hint = if running {
                let job = self.maintenance.as_ref().unwrap();
                let secs = job.started_at.elapsed().as_secs();
                let current = job.progress.current.load(Ordering::SeqCst);
                let total = job.progress.total.load(Ordering::SeqCst);
                let step = if total == 0 {
                    "starting".to_string()
                } else {
                    format!("step {current}/{total}")
                };
                format!(
                    "running in background: {step} ({}s elapsed, {}s limit per step)",
                    secs,
                    step_timeout.as_secs()
                )
            } else if !mconfig.memory_maintenance {
                "disabled in settings".to_string()
            } else if count < mconfig.memory_maintenance_threshold {
                format!("below threshold ({})", mconfig.memory_maintenance_threshold)
            } else {
                format!(
                    "ready (runs in batches of {})",
                    batch_size
                )
            };

            if ui
                .add_enabled(can_run, egui::Button::new("Run maintenance now").fill(theme.primary))
                .clicked()
            {
                self.start_maintenance(memory, runtime, total_timeout);
            }
            ui.label(egui::RichText::new(hint).color(theme.text_dim).small());
        });

        if let Some(report) = &self.maintenance_report {
            ui.add(egui::Label::new(egui::RichText::new(report).color(theme.text_secondary)).wrap());
        }
    }

    /// Start a maintenance pass on a helper thread and track it in
    /// `self.maintenance` (result collected per-frame in `draw`).
    ///
    /// The UI thread is inside the `#[tokio::main]` runtime context, so
    /// `block_on`-ing ANY runtime from here would panic with "Cannot start a
    /// runtime from within a runtime". A fresh OS thread has no runtime
    /// context, so `block_on`-ing the dedicated memory runtime on it is safe.
    fn start_maintenance(
        &mut self,
        memory: &Arc<MemoryManager>,
        runtime: &Arc<tokio::runtime::Runtime>,
        total_timeout: Duration,
    ) {
        // A pass is already in flight (the button is disabled, just in case).
        if self.maintenance.is_some() {
            return;
        }
        let memory = memory.clone();
        let runtime = runtime.clone();
        let (tx, rx) = mpsc::channel();
        // Live batch progress (current/total), shared with the helper thread.
        let progress = MaintenanceProgress {
            current: Arc::new(AtomicUsize::new(0)),
            total: Arc::new(AtomicUsize::new(0)),
        };
        let progress_for_ui = progress.clone();
        let spawned = std::thread::Builder::new()
            .name("memory-maintenance".to_string())
            .spawn(move || {
                // The outer timeout is a safety net over the WHOLE batched
                // pass (steps × per-step limit; each batch already self-bounds
                // inside the manager). Dropping it cancels the pass.
                let result = runtime.block_on(async {
                    tokio::time::timeout(total_timeout, memory.run_maintenance_full(Some(progress))).await
                });
                let outcome: Result<MaintenanceReport, String> = match result {
                    Ok(inner) => inner,
                    Err(_) => Err(format!(
                        "Maintenance timed out after {}s total (raise the step limit in Settings)",
                        total_timeout.as_secs()
                    )),
                };
                let _ = tx.send(outcome);
            });
        match spawned {
            Ok(_) => {
                self.maintenance = Some(MaintenanceJob {
                    started_at: Instant::now(),
                    rx,
                    progress: progress_for_ui,
                });
                self.message = Some("✓ Maintenance started (running in background)".to_string());
            }
            Err(e) => {
                self.message = Some(format!("✗ Failed to start maintenance: {}", e));
            }
        }
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
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Human age label in days.
fn format_age(entry: &MemoryEntry) -> usize {
    entry
        .timestamp
        .map(|ts| chrono::Utc::now().signed_duration_since(ts).num_days().max(0) as usize)
        .unwrap_or(0)
}
