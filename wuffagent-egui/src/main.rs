//! WuffAgent UI app entry point.
//!
//! Startup is staged in [`bootstrap`] (config → clients → tools → MCP →
//! memory → agents → engine); this file only wires the eframe window and the
//! restart auto-resume logic. Support code lives in sibling modules:
//! [`logging`] (tracing), [`fonts`] (emoji), [`image_loader`], [`ui`].

use std::sync::Arc;

mod bootstrap;
mod fonts;
mod image_loader;
mod logging;
mod ui;

#[tokio::main]
async fn main() -> eframe::Result {
    let mut ctx = bootstrap::bootstrap();

    // Auto-resume after a restart: if a restart marker exists (written by the UI
    // just before relaunching), resume that session automatically. Point the
    // active session at the marker's session (before the store is built below),
    // capture the reason for the continue turn, and delete the marker so it only
    // fires once.
    // (T4) consume_restart_marker reads + parses + deletes the marker (once),
    // keeping an unparseable file in place for inspection. `marker_session_id`
    // is tracked separately: auto-resume is only possible if that session can
    // actually be loaded below, otherwise the UI would start empty with no
    // explanation (see the resume-failed banner).
    let (auto_resume_reason, marker_session_id): (Option<String>, Option<String>) =
        match wuffagent_core::config::consume_restart_marker() {
            Some(m) => {
                ctx.config.session_id = Some(m.session_id.clone());
                tracing::info!(
                    "Restart marker found; auto-resuming session {}",
                    m.session_id
                );
                (Some(m.reason), Some(m.session_id))
            }
            None => (None, None),
        };

    // Dedicated runtime for UI-triggered async work (memory maintenance).
    // NOTE: the UI thread (main thread) IS inside the `#[tokio::main]` runtime
    // context — the async `main` body blocks in `eframe::run_native`, which
    // runs inside `rt.block_on`, and that context is also what lets the UI
    // loop `tokio::spawn` (remote n_ctx fetch). Because of it, no
    // `Runtime::block_on` may be called from the UI thread at all (it would
    // panic with "Cannot start a runtime from within a runtime"); the memory
    // panel therefore runs the maintenance pass on a helper thread that
    // `block_on`s this dedicated runtime, keeping it separate from the main
    // runtime.
    // Worker threads are capped well below `num_cpus` since only the
    // occasional maintenance pass runs here; the default would spin up one
    // idle thread per core.
    let memory_runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to create memory runtime"),
    );

    // Initialize session store with the configured session
    let (session_store, selected_session_id) = bootstrap::initial_session_store(
        &ctx.config,
        &ctx.connection,
        &ctx.agent_engine,
        &ctx.event_tx,
    );

    // (T4) A restart marker pointing at a session that did not load means the
    // auto-resume cannot happen — surface it to the UI as a one-shot banner
    // instead of failing silently into an empty window.
    let auto_resume_failed: Option<(String, String)> = marker_session_id
        .as_ref()
        .filter(|id| !session_store.contains_key(id.as_str()))
        .map(|id| (id.clone(), auto_resume_reason.clone().unwrap_or_default()));
    if auto_resume_failed.is_some() {
        tracing::warn!(
            "Restart marker's session '{}' could not be loaded; auto-resume will not run",
            marker_session_id.as_deref().unwrap_or("")
        );
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "WuffAgent (egui)",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_fonts(fonts::emoji_fonts());
            // egui ships no image decoders: register the app's loader so
            // `ImageSource::Bytes` (pasted/attached images, chat-bubble
            // images) renders instead of the red "no image loaders are
            // loaded" error texture.
            cc.egui_ctx
                .add_image_loader(Arc::new(image_loader::ImageBytesLoader));
            let bootstrap::AppContext {
                config,
                server,
                tool_manager,
                agent_engine,
                connection,
                memory_manager,
                mcp_manager,
                event_tx,
                event_rx,
            } = ctx;
            Ok(Box::new(ui::state::ChatApp::new(
                config,
                server,
                tool_manager,
                agent_engine,
                connection,
                session_store,
                selected_session_id,
                event_tx,
                event_rx,
                memory_manager,
                memory_runtime,
                mcp_manager,
                auto_resume_reason,
                auto_resume_failed,
            )))
        }),
    )
}
