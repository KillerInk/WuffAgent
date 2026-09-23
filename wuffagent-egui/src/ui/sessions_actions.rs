use std::path::PathBuf;

use super::state::ChatApp;

#[derive(Debug)]
pub enum PanelAction {
    Rename { id: String, new_name: String },
    Create(String),
    Delete(String),
    Export { session_id: String },
    Import,
}

/// Apply a sessions-panel action (session CRUD dispatch).
///
/// Moved out of `ui/state.rs` as a free fn (U2) so state.rs stops growing;
/// all `ChatApp` fields are public, so this module can drive the panel.
pub fn apply_sessions_action(app: &mut ChatApp, action: PanelAction) {
    let Some(panel) = app.sessions_panel.as_mut() else {
        return;
    };
    let sessions_dir = panel.sessions_dir().clone();

    match action {
        PanelAction::Rename { id, new_name } => {
            if let Some(mut s) = wuffagent_core::sessions::load_session(&sessions_dir, &id) {
                s.name = new_name.clone();
                // Heal legacy/corrupted history (incl. truncated tool call
                // arguments) while we have the session in hand.
                s.sanitize();
                let _ = wuffagent_core::sessions::save_session(&sessions_dir, &s);
            }
            if let Some(runtime) = app.session_store.get_mut(&id) {
                runtime.name = new_name;
            }
            panel.refresh();
        }
        PanelAction::Create(name) => {
            let session = wuffagent_core::sessions::create_session(&sessions_dir, &name);

            let mut runtime = wuffagent_core::sessions::SessionRuntime::create_from_config(
                &app.config,
                &app.connection,
                &app.agent_engine,
                session.id.clone(),
                session.name.clone(),
                app.pending_tx.as_ref().unwrap().lock().unwrap().clone(),
            );

            // Default the new session agent to "general" (per-session
            // selection shown in the input selector).
            runtime.selected_agent = Some("general".to_string());

            app.session_store.insert(session.id.clone(), runtime);
            *panel.selected_id_mut() = Some(session.id.clone());

            {
                let mut cfg = panel.config().clone();
                cfg.session_id = Some(session.id.clone());
                if let Err(e) = cfg.save() {
                    eprintln!("Failed to save config after creating session: {}", e);
                }
            }
            panel.refresh();
        }
        PanelAction::Delete(id) => {
            match wuffagent_core::sessions::delete_session(&sessions_dir, &id) {
                Ok(()) => {
                    app.session_store.remove(&id);
                    if app.selected_session_id.as_deref() == Some(&*id) {
                        app.selected_session_id = None;
                    }
                    panel.show_notification(&format!("Session '{}' deleted", id), true);
                    panel.clear_session();
                    panel.refresh();
                    let mut cfg = panel.config().clone();
                    cfg.session_id = None;
                    if let Err(e) = cfg.save() {
                        eprintln!("Failed to save config after deleting session: {}", e);
                    }
                    panel.show_notification("Session deleted", true);
                }
                Err(e) => {
                    panel.show_notification(&format!("Failed to delete session: {}", e), false);
                    if panel.selected_id().as_deref() == Some(&*id) {
                        *panel.selected_id_mut() = None;
                    }
                    panel.refresh();
                }
            }
        }
        PanelAction::Export { session_id } => {
            let output_path = if panel.export_path().is_empty() {
                PathBuf::from(format!("{}.json", session_id))
            } else {
                PathBuf::from(panel.export_path())
            };
            match wuffagent_core::sessions::export_session(&sessions_dir, &session_id, &output_path) {
                Ok(()) => panel.show_notification(&format!("Exported to {}", output_path.display()), true),
                Err(e) => panel.show_notification(&format!("Export failed: {}", e), false),
            }
        }
        PanelAction::Import => {
            let input_path = if panel.import_path().is_empty() {
                PathBuf::from("session.json")
            } else {
                PathBuf::from(panel.import_path())
            };
            match wuffagent_core::sessions::import_session(&sessions_dir, &input_path) {
                Ok(new_id) => {
                    panel.show_notification(&format!("Imported session: {}", new_id), true);
                    panel.refresh();
                }
                Err(e) => {
                    panel.show_notification(&format!("Import failed: {}", e), false);
                }
            }
        }
    }
}
