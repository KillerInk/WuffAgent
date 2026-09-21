use eframe::egui;

use super::state::ChatApp;
use crate::types::AppStatus;
use super::theme::Theme;

impl ChatApp {
    pub(super) fn draw_status_bar(&self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.theme);
        let chat = match self.selected_chat_state() {
            Some(c) => c,
            None => return,
        };

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
             
            // Status indicator with colored dot
            let status_color = match &chat.status {
                AppStatus::Stopped => theme.text_secondary,
                AppStatus::Connecting => theme.warning,
                AppStatus::Ready => theme.success,
                AppStatus::Generating => theme.primary,
                AppStatus::Error(_) => theme.error,
            };
            let status_text = match &chat.status {
                AppStatus::Stopped => "● Stopped",
                AppStatus::Connecting => "● Connecting...",
                AppStatus::Ready => "● Ready",
                AppStatus::Generating => "● Generating...",
                AppStatus::Error(_) => return,
            };
            ui.label(egui::RichText::new(status_text).color(status_color).size(11.0));

            if chat.is_generating {
                ui.label(egui::RichText::new("Streaming").color(theme.accent).size(11.0));
            }

            // Tools executing right now (live cards are in the chat area; this
            // keeps them visible even when scrolled out of view).
            if !chat.active_tools.is_empty() {
                let names: Vec<&str> = chat
                    .active_tools
                    .iter()
                    .map(|t| t.tool_name.as_str())
                    .collect();
                ui.label(
                    egui::RichText::new(format!(
                        "🔧 {} tool{} running",
                        names.len(),
                        if names.len() == 1 { "" } else { "s" }
                    ))
                    .color(theme.accent)
                    .size(11.0),
                )
                .on_hover_text(names.join("\n"));
            }
             
            ui.separator();
            ui.label(egui::RichText::new(format!("Messages: {}", chat.messages.len())).color(theme.text_secondary).size(11.0));

            // Memory count indicator with a tooltip listing project + threshold.
            let mem_count = self.memory_manager.count();
            let mconfig = self.memory_manager.config();
            ui.label(egui::RichText::new(format!("🧠 {}", mem_count)).color(theme.text_secondary).size(11.0))
                .on_hover_text(format!(
                    "{} active memories (project '{}', max {})\nMaintenance: {}",
                    mem_count,
                    mconfig.project,
                    mconfig.max_entries,
                    if mconfig.memory_maintenance {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ));

            // MCP indicator: connected / total servers.
            let mcp_servers = self.mcp_manager.snapshot();
            if !mcp_servers.is_empty() {
                let (connected, total) = self.mcp_manager.connected_counts();
                let detail: Vec<String> = mcp_servers
                    .iter()
                    .map(|s| {
                        let tool_count = s.tools.iter().filter(|t| t.enabled).count();
                        format!("{}: {:?} ({} enabled tools)", s.name, s.status, tool_count)
                    })
                    .collect();
                ui.label(
                    egui::RichText::new(format!("MCP {connected}/{total}"))
                        .color(if connected == total {
                            theme.success
                        } else if connected == 0 {
                            theme.text_dim
                        } else {
                            theme.warning
                        })
                        .size(11.0),
                )
                .on_hover_text(detail.join("\n"));
            }
        });
    }

    /// Returns the effective n_ctx for trim/gauge budgeting.
    ///
    /// Prefers the value the connected server reports via `/props` (fetched in
    /// BOTH local and remote mode — the server is the source of truth). While
    /// that value has not been fetched yet (0), we return 0 — NOT a fallback to
    /// the local config. Substituting the config value here was the bug that
    /// made the server's real limit invisible: the trimmer ran on the wrong
    /// (4096) budget even though the server allowed more. A 0 return safely
    /// disables trimming (`n_ctx() > 0` guards in the agent/client loop) until
    /// the real value arrives.
    pub(super) fn get_effective_n_ctx(&self) -> u32 {
        self.remote_n_ctx
    }

    /// Returns true when the connected server's /props n_ctx has not been
    /// fetched yet. Used to show "…" in the bottom bar instead of a misleading
    /// value.
    pub(super) fn remote_props_unknown(&self) -> bool {
        self.remote_n_ctx == 0
    }

    pub(super) fn draw_bottom_bar(&self, ui: &mut egui::Ui) {
        let theme = Theme::from_name(&self.config.theme);
        let n_ctx = self.get_effective_n_ctx();
        let n_gpu_layers = self.server.get_n_gpu_layers();
        let threads = self.server.get_threads();
        let chat = match self.selected_chat_state() {
            Some(c) => c,
            None => return,
        };

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
             
            // Token count pill (counts up live while generating; snaps to the
            // server-reported total when a round completes)
            ui.add(egui::Label::new(
                egui::RichText::new(format!("Tokens: {}", chat.token_count))
                    .color(theme.text_secondary)
                    .size(11.0)
            ).wrap())
            .on_hover_text(format!(
                "Total context tokens for this chat (prompt + all messages)\n\
                 Counts up live while generating (estimated ~3.5 chars/token);\n\
                 snaps to the server-reported total when a round completes.\n\
                 Use \u{21e9} to reset the context\n\n\
                 Context window: {} tokens\nUsage: {:.1}%",
                n_ctx,
                chat.context_used
            ));
             
            // Context usage pill with color coding
            let context_color = if chat.context_used > 80.0 {
                theme.error
            } else if chat.context_used > 60.0 {
                theme.warning
            } else {
                theme.success
            };
            ui.add(egui::Label::new(
                egui::RichText::new(format!("Ctx: {:.1}%", chat.context_used))
                    .color(context_color)
                    .size(11.0)
            ).wrap());

            // Live prompt-processing progress (llama.cpp `prompt_progress`):
            // shown while the server works through the prompt, before the
            // first token. Percentage + count are over NON-cached tokens
            // (the "timed" progress per the llama.cpp docs) — cached tokens
            // are near-free and would otherwise make it hit 100% instantly.
            if let Some(pp) = &chat.prompt_progress {
                let total_new = pp.total.saturating_sub(pp.cache);
                let done_new = pp.processed.saturating_sub(pp.cache);
                let pct = if total_new > 0 {
                    done_new as f32 / total_new as f32 * 100.0
                } else {
                    100.0
                };
                ui.add(egui::Label::new(
                    egui::RichText::new(format!("PP {:.0}% ({}/{})", pct, done_new, total_new))
                        .color(egui::Color32::from_rgb(255, 176, 64))
                        .size(11.0)
                ).wrap())
                .on_hover_text(format!(
                    "Prompt processing progress (llama.cpp)\n{}/{} uncached tokens processed ({} cached, {} total)\nSpeed in the PP t/s readout beside it",
                    done_new, total_new, pp.cache, pp.total
                ));
            }

            // llama.cpp speeds: shown only while a run is in progress (the
            // user doesn't want stale numbers lingering when idle). Only
            // backends that report them get readouts (llama.cpp does; OpenAI
            // and most others don't).
            if chat.is_generating {
                let mut speeds: Vec<String> = Vec::new();
                if let Some(pp) = chat.prompt_tps {
                    speeds.push(format!("PP {:.1} t/s", pp));
                }
                if let Some(tg) = chat.gen_tps {
                    speeds.push(format!("TG {:.1} t/s", tg));
                }
                if !speeds.is_empty() {
                    ui.add(egui::Label::new(
                        egui::RichText::new(speeds.join(" · ")).color(theme.accent).size(11.0)
                    ).wrap())
                    .on_hover_text(
                        "PP = prompt processing (tokens/s) — live from llama.cpp `prompt_progress`\nwhile the prompt is processed (non-cached tokens), final server value on complete\nTG = token generation (tokens/s) — live estimate while generating,\nserver-reported value once a round completes (llama.cpp backends)"
                    );
                }
            }

            ui.separator();
            
            // Server specs. In remote mode before /props has been fetched,
            // show "…" rather than the local config's n_ctx (which is not the
            // remote server's limit and would be misleading).
            let ctx_label = if self.remote_props_unknown() {
                "…".to_string()
            } else {
                n_ctx.to_string()
            };
            ui.add(egui::Label::new(
                egui::RichText::new(format!("Ctx: {} | GPU: {} | Threads: {}", ctx_label, n_gpu_layers, threads))
                    .color(theme.text_dim)
                    .size(11.0)
            ).wrap());
        });
    }
}
