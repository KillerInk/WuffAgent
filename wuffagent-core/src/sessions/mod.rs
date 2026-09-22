pub mod model;
pub mod runtime;
pub use model::{Session, SessionStatus};
pub use runtime::{ActiveTool, ChatAreaState, QueuedMessage, SessionRuntime};

use std::fs;
use std::path::{Path, PathBuf};

const SESSIONS_DIR: &str = "sessions";

pub fn sessions_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|p| p.join(SESSIONS_DIR))
        .unwrap_or_else(|| PathBuf::from(SESSIONS_DIR))
}

/// Magic bytes written at the start of an encrypted session file.
const ENCRYPTION_MARKER: &[u8] = b"WUFFENC";

/// Check whether a session file exists on disk for the given id.
pub fn session_exists(dir: &Path, id: &str) -> bool {
    dir.join(format!("{}.json", id)).exists()
}

pub fn load_session(dir: &Path, id: &str) -> Option<Session> {
    let path = dir.join(format!("{}.json", id));
    if path.exists() {
        if let Ok(bytes) = fs::read(&path) {
            if bytes.len() > ENCRYPTION_MARKER.len()
                && &bytes[..ENCRYPTION_MARKER.len()] == ENCRYPTION_MARKER
            {
                // Encrypted file — return None to signal that decrypt_and_load_session is needed
                return None;
            }
        }
    } else {
        tracing::warn!("Session file not found: {}", path.display());
    }
    let path = dir.join(format!("{}.json", id));
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Encrypt and save a session. Writes the file with an encryption marker prefix,
/// base64-encoded so it can be stored as a text file.
pub fn encrypt_session(dir: &Path, session: &Session, key: &[u8]) -> Result<(), anyhow::Error> {
    use chacha20poly1305::aead::Aead;
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};

    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| anyhow::anyhow!("failed to create cipher: {}", e))?;

    let content = serde_json::to_string_pretty(session)?;
    let plaintext = content.as_bytes();

    let mut nonce_bytes = [0u8; 12];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|e| anyhow::anyhow!("encryption failed: {}", e))?;

    // Layout: [ENCRYPTION_MARKER (7 bytes)][12-byte nonce][ciphertext + tag]
    let mut encoded =
        Vec::with_capacity(ENCRYPTION_MARKER.len() + nonce_bytes.len() + ciphertext.len());
    encoded.extend_from_slice(ENCRYPTION_MARKER);
    encoded.extend_from_slice(&nonce_bytes);
    encoded.extend_from_slice(&ciphertext);

    // Base64-encode the binary blob so it can be stored as a text file
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &encoded);
    let path = dir.join(format!("{}.json", session.id));
    fs::write(&path, b64)?;
    Ok(())
}

/// Decrypt and load a session from an encrypted file.
/// Returns None if the file is not encrypted or decryption fails.
pub fn decrypt_and_load_session(dir: &Path, session_id: &str, key: &[u8]) -> Option<Session> {
    use chacha20poly1305::aead::Aead;
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};

    let path = dir.join(format!("{}.json", session_id));
    let b64 = fs::read_to_string(&path).ok()?;
    let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64).ok()?;

    if raw.len() <= ENCRYPTION_MARKER.len() + 12 {
        return None;
    }
    if &raw[..ENCRYPTION_MARKER.len()] != ENCRYPTION_MARKER {
        return None;
    }

    let cipher = ChaCha20Poly1305::new_from_slice(key).ok()?;
    let nonce_start = ENCRYPTION_MARKER.len();
    let nonce = Nonce::from_slice(&raw[nonce_start..nonce_start + 12]);
    let ciphertext = &raw[nonce_start + 12..];

    let plaintext = cipher.decrypt(nonce, ciphertext).ok()?;
    serde_json::from_slice::<Session>(&plaintext).ok()
}

pub fn save_session(dir: &Path, session: &Session) -> Result<(), anyhow::Error> {
    save_session_atomic(dir, session)
}

pub fn save_session_atomic(dir: &Path, session: &Session) -> Result<(), anyhow::Error> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", session.id));
    let content = serde_json::to_string_pretty(session)?;
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, content)?;
    fs::rename(&temp_path, &path)?;
    Ok(())
}

/// Save a session, encrypting it if the key is provided.
pub fn save_session_encrypted(
    dir: &Path,
    session: &Session,
    key: &[u8],
) -> Result<(), anyhow::Error> {
    fs::create_dir_all(dir)?;
    encrypt_session(dir, session, key)?;
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
    sessions.sort_by_key(|a| std::cmp::Reverse(a.created_at));
    sessions
}

pub fn delete_session(dir: &Path, id: &str) -> Result<(), String> {
    let path = dir.join(format!("{}.json", id));
    if !path.exists() {
        return Err(format!("session file not found: {}", path.display()));
    }
    fs::remove_file(&path).map_err(|e| format!("failed to delete session file: {}", e))?;

    // Also delete backup files for this session (pattern: {id}.json.bak)
    let backup_path = dir.join(format!("{}.json.bak", id));
    if backup_path.exists() {
        let _ = fs::remove_file(&backup_path);
    }

    Ok(())
}

/// Clear all messages from a session while preserving the session itself.
/// Returns true if the session was found and cleared, false otherwise.
pub fn clear_session_messages(dir: &Path, id: &str) -> Result<(), anyhow::Error> {
    let session =
        load_session(dir, id).ok_or_else(|| anyhow::anyhow!("session not found: {}", id))?;
    let mut cleared = session;
    cleared.messages.clear();
    cleared.touch();
    save_session_atomic(dir, &cleared)
}

pub fn create_session(dir: &Path, name: &str) -> Session {
    let session = Session::new(name);
    save_session(dir, &session).expect("failed to save new session");
    session
}

/// Returns the number of session files and total size in bytes.
pub fn session_stats(dir: &Path) -> (usize, u64) {
    if !dir.exists() {
        return (0, 0);
    }
    let mut count = 0;
    let mut total_size = 0u64;
    for entry in fs::read_dir(dir).expect("cannot read sessions dir") {
        let entry = entry.expect("bad entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(meta) = fs::metadata(&path) {
                count += 1;
                total_size += meta.len();
            }
        }
    }
    (count, total_size)
}

/// Export a single session as a JSON file to the given output path.
pub fn export_session(
    dir: &Path,
    session_id: &str,
    output_path: &Path,
) -> Result<(), anyhow::Error> {
    let session = load_session(dir, session_id)
        .ok_or_else(|| anyhow::anyhow!("session not found: {}", session_id))?;
    fs::create_dir_all(
        output_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid output path"))?,
    )?;
    let content = serde_json::to_string_pretty(&session)?;
    fs::write(output_path, content)?;
    Ok(())
}

/// Import a session from a JSON file, generating a new ID to avoid conflicts.
pub fn import_session(dir: &Path, input_path: &Path) -> Result<String, anyhow::Error> {
    let content = fs::read_to_string(input_path)
        .map_err(|e| anyhow::anyhow!("failed to read import file: {}", e))?;
    let session: Session = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse session JSON: {}", e))?;

    // Generate a new ID by appending a timestamp suffix to avoid conflicts
    let new_id = format!(
        "{}_imported_{}",
        session.id,
        chrono::Utc::now().timestamp_millis()
    );
    let mut imported = session;
    imported.id = new_id.clone();
    imported.touch();

    save_session(dir, &imported)?;
    Ok(new_id)
}

/// Export all sessions as a tar archive.
///
/// Uses only `std::io` - no external crates needed.
/// The archive contains a `sessions/` directory with each session as a `.json` file.
pub fn export_all_sessions(dir: &Path, output_tar_path: &Path) -> Result<(), anyhow::Error> {
    let sessions = list_sessions(dir);
    if sessions.is_empty() {
        return Err(anyhow::anyhow!("no sessions to export"));
    }

    fs::create_dir_all(
        output_tar_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid output path"))?,
    )?;

    // Build a tar archive in memory
    let mut tar_bytes = Vec::new();
    {
        let mut archive = tar_builder::Builder::new(&mut tar_bytes);
        archive.set_prefix("sessions/");

        for session in &sessions {
            let json_path = format!("{}.json", session.id);
            let content = serde_json::to_string_pretty(session)?;
            let bytes = content.into_bytes();
            archive.append_file(&json_path, &bytes)?;
        }
        archive.finish()?;
    }

    // Write the tar archive to disk
    fs::write(output_tar_path, tar_bytes)?;
    Ok(())
}

mod tar_builder;

#[cfg(test)]
mod tests;
