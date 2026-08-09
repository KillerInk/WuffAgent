pub mod model;
pub use model::Session;

use std::fs;
use std::path::{Path, PathBuf};

const SESSIONS_DIR: &str = "sessions";

pub fn sessions_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .expect("config path must have parent")
        .join(SESSIONS_DIR)
}

pub fn load_session(dir: &Path, id: &str) -> Option<Session> {
    let path = dir.join(format!("{}.json", id));
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn save_session(dir: &Path, session: &Session) -> Result<(), anyhow::Error> {
    fs::create_dir_all(dir)?;
    let content = serde_json::to_string_pretty(session)?;
    let path = dir.join(format!("{}.json", session.id));
    fs::write(&path, content)?;
    Ok(())
}

pub fn list_sessions(dir: &Path) -> Vec<Session> {
    if !dir.exists() {
        return Vec::new();
    }
    let mut sessions: Vec<Session> = Vec::new();
    for entry in fs::read_dir(dir).expect("cannot read sessions dir") {
        let entry = entry.expect("bad entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(s) = fs::read_to_string(&path) {
                if let Ok(session) = serde_json::from_str::<Session>(&s) {
                    sessions.push(session);
                }
            }
        }
    }
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    sessions
}

pub fn delete_session(dir: &Path, id: &str) -> bool {
    let path = dir.join(format!("{}.json", id));
    fs::remove_file(&path).is_ok()
}

pub fn create_session(dir: &Path, name: &str) -> Session {
    let session = Session::new(name);
    save_session(dir, &session).expect("failed to save new session");
    session
}
