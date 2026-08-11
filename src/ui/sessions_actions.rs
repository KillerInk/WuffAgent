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

/// Build the action handlers and apply them to the panel.
/// Returns the optionally newly-selected session id.
pub fn apply_actions(
    panel: &mut SessionsPanel,
    action: PanelAction,
) -> Option<String> {
    match action {
        PanelAction::Rename { id, new_name } => {
            if let Some(mut s) = sessions::load_session(panel.sessions_dir(), &id) {
                s.name = new_name;
                let _ = sessions::save_session(panel.sessions_dir(), &s);
            }
            panel.refresh();
            panel.selected_id().clone()
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
            panel.selected_id().clone()
        }
        PanelAction::Delete(id) => {
            match sessions::delete_session(panel.sessions_dir(), &id) {
                Ok(()) => {
                    panel.show_notification(&format!("Session '{}' deleted", id), true);
                }
                Err(e) => {
                    panel.show_notification(&format!("Failed to delete session: {}", e), false);
                    // Still deselect if it was selected
                    if panel.selected_id().as_deref() == Some(&id) {
                        *panel.selected_id_mut() = None;
                    }
                    panel.refresh();
                    return panel.selected_id().clone();
                }
            }
            if panel.selected_id().as_deref() == Some(&id) {
                *panel.selected_id_mut() = None;
            }
            panel.refresh();
            panel.selected_id().clone()
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
            panel.selected_id().clone()
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
            panel.selected_id().clone()
        }
    }
}
