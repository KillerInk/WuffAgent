use std::path::PathBuf;

use crate::sessions;
use super::sessions_panel::SessionsPanel;

#[derive(Debug)]
pub enum PanelAction {
    Rename { id: String, new_name: String },
    Create(String),
    Delete(String),
    Export { session_id: String },
    Import,
}

/// Result of applying a panel action.
pub struct PanelActionResult {
    /// The optionally newly-selected session id.
    pub selected_id: Option<String>,
    /// Whether the client session should also be cleared (happens on delete).
    pub clear_client_session: bool,
}

/// Build the action handlers and apply them to the panel.
pub fn apply_actions(
    panel: &mut SessionsPanel,
    action: PanelAction,
) -> PanelActionResult {
    match action {
        PanelAction::Rename { id, new_name } => {
            if let Some(mut s) = sessions::load_session(panel.sessions_dir(), &id) {
                s.name = new_name;
                let _ = sessions::save_session(panel.sessions_dir(), &s);
            }
            panel.refresh();
            PanelActionResult { selected_id: panel.selected_id().clone(), clear_client_session: false }
        }
        PanelAction::Create(name) => {
            let session = sessions::create_session(panel.sessions_dir(), &name);
            *panel.selected_id_mut() = Some(session.id.clone());
            // Update config so the new session is loaded on next app start
            {
                let mut cfg = panel.config().lock().unwrap();
                cfg.session_id = Some(session.id.clone());
                if let Err(e) = cfg.save() {
                    eprintln!("Failed to save config after creating session: {}", e);
                }
            }
            panel.refresh();
            PanelActionResult { selected_id: panel.selected_id().clone(), clear_client_session: false }
        }
        PanelAction::Delete(id) => {
            match sessions::delete_session(panel.sessions_dir(), &id) {
                Ok(()) => {
                    panel.show_notification(&format!("Session '{}' deleted", id), true);
                    // Clear the panel's session ID
                    panel.clear_session();
                    // Also clear the config's session_id so the next session is loaded on restart
                    panel.config().lock().unwrap().session_id = None;
                    PanelActionResult { selected_id: None, clear_client_session: true }
                }
                Err(e) => {
                    panel.show_notification(&format!("Failed to delete session: {}", e), false);
                    // Still deselect if it was selected
                    if panel.selected_id().as_deref() == Some(&id) {
                        *panel.selected_id_mut() = None;
                    }
                    panel.refresh();
                    PanelActionResult { selected_id: panel.selected_id().clone(), clear_client_session: false }
                }
            }
        }
        PanelAction::Export { session_id } => {
            let output_path = if panel.export_path().is_empty() {
                PathBuf::from(format!("{}.json", session_id))
            } else {
                PathBuf::from(panel.export_path())
            };
            match sessions::export_session(panel.sessions_dir(), &session_id, &output_path) {
                Ok(()) => {
                    panel.show_notification(&format!("Exported to {}", output_path.display()), true);
                }
                Err(e) => {
                    panel.show_notification(&format!("Export failed: {}", e), false);
                }
            }
            PanelActionResult { selected_id: panel.selected_id().clone(), clear_client_session: false }
        }
        PanelAction::Import => {
            let input_path = if panel.import_path().is_empty() {
                PathBuf::from("session.json")
            } else {
                PathBuf::from(panel.import_path())
            };
            match sessions::import_session(panel.sessions_dir(), &input_path) {
                Ok(new_id) => {
                    panel.show_notification(&format!("Imported session: {}", new_id), true);
                    panel.refresh();
                }
                Err(e) => {
                    panel.show_notification(&format!("Import failed: {}", e), false);
                }
            }
            PanelActionResult { selected_id: panel.selected_id().clone(), clear_client_session: false }
        }
    }
}
