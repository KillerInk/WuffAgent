//! Memory panel: editor, settings and maintenance sections (C6 split).
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use wuffagent_core::memory::{
    InjectionMode, MaintenanceProgress, MaintenanceReport, MemoryConfig, MemoryManager,
};

use super::{
    MIN_MAINTENANCE_TIMEOUT_SECS, MaintenanceJob, MemoryPanel, short_id,
};
use super::super::theme::Theme;

impl MemoryPanel {
    /// Draw the entry editor (content + tags text fields) when an entry is
    /// selected.
    ///
    /// Note: the editor used to be wrapped in an auto-open `ui.collapsing`;
    /// `ui.collapsing` wrapper started out CLOSED, so clicking "Edit" on a
    /// row only revealed a collapsed "Edit memory …" header and the text
    /// fields were invisible — the panel looked uneditable.
    pub(super) fn draw_editor(&mut self, ui: &mut egui::Ui, theme: &Theme, memory: &MemoryManager) {
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
                        match memory.update(&id, Some(&self.edit_content), Some(tags)) {
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
    ///
    /// Returns the new config if the user changed anything — the caller
    /// (ChatApp) persists it via the centralized `save_config()` (I4).
    pub(super) fn draw_settings(&mut self, ui: &mut egui::Ui, memory: &MemoryManager) -> Option<MemoryConfig> {
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
            let auto_changed = ui.add(egui::Checkbox::new(&mut mconfig.auto_improve, "Auto-improve prompts (gated: cooldown + new evidence)")).changed();
            let cooldown_changed = ui
                .add(
                    egui::DragValue::new(&mut mconfig.improvement_cooldown_tasks)
                        .range(1..=1000)
                        .suffix(" tasks cooldown"),
                )
                .changed();

            if enabled || max_changed || inj_changed || maint_changed || thresh_changed || batch_changed || timeout_changed || auto_changed || cooldown_changed {
                memory.set_config(mconfig.clone());
                self.message = Some("✓ Memory settings updated".to_string());
                Some(mconfig)
            } else {
                None
            }
        })
        .body_returned
        .flatten()
    }

    /// Maintenance controls + report.
    pub(super) fn draw_maintenance(
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
    pub(super) fn start_maintenance(
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
