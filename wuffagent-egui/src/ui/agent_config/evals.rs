//! F6: the golden/regression eval panel (2d) for the selected existing
//! agent — saved-eval count, all-time pass rate, the last run's verdict,
//! plus the "Run evals" button that drives the registered `run_eval` tool
//! headlessly on a background thread.

use std::collections::HashMap;

use eframe::egui;
use wuffagent_core::agents::metrics::MetricsLine;
use wuffagent_core::tools::{ToolOutput, ToolParams};
use wuffagent_core::types::AppEvent;

use super::AgentConfigDialog;

impl AgentConfigDialog {
    /// 2d: the golden/regression eval panel for the selected EXISTING agent —
    /// saved-eval count, all-time pass rate, the last run's verdict, plus a
    /// "Run evals" button that drives the registered `run_eval` tool headlessly
    /// (each eval = a fresh isolated agent + its own LLM call, up to 180 s).
    /// The result returns via `EvalsRunFinished` and is shown as a status line.
    pub(super) fn draw_evals(&mut self, ui: &mut egui::Ui, agent_name: &str) {
        let saved = wuffagent_core::memory::evals::EvalStore::default()
            .list(agent_name)
            .len();
        // One read of the agent's metrics; count evals + capture the newest.
        let mut total = 0usize;
        let mut passed = 0usize;
        let mut last_desc: Option<String> = None;
        let mut last_passed: Option<bool> = None;
        for l in &self.cached_metrics_lines(agent_name) {
            if let MetricsLine::Eval { ts, id, passed: p, .. } = l {
                total += 1;
                if *p {
                    passed += 1;
                }
                let id_label = if id.is_empty() { "(ad-hoc)" } else { id.as_str() };
                last_desc = Some(format!("{} — {}", ts.format("%Y-%m-%d %H:%M"), id_label));
                last_passed = Some(*p);
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
        let runtime = self.runtime.clone();
        std::thread::spawn(move || {
            let mut values = HashMap::new();
            values.insert("agent".to_string(), serde_json::json!(&agent));
            // Shared helper runtime (the memory-maintenance worker uses the
            // same one): the UI thread is inside the main runtime, so the
            // tool runs on this fresh OS thread where `block_on` is safe.
            let summary = match runtime.block_on(tm.execute("run_eval", ToolParams { values })) {
                Ok(ToolOutput::Success(v)) => v.as_str().unwrap_or("").to_string(),
                Ok(ToolOutput::Error(e)) => format!("Error: {e}"),
                Err(e) => format!("Error: {e}"),
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
}
