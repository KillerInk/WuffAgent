pub mod model;
pub use model::Session;

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
            if bytes.len() > ENCRYPTION_MARKER.len() && &bytes[..ENCRYPTION_MARKER.len()] == ENCRYPTION_MARKER {
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
    let mut encoded = Vec::with_capacity(
        ENCRYPTION_MARKER.len() + nonce_bytes.len() + ciphertext.len(),
    );
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
pub fn save_session_encrypted(dir: &Path, session: &Session, key: &[u8]) -> Result<(), anyhow::Error> {
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
    let session = load_session(dir, id)
        .ok_or_else(|| anyhow::anyhow!("session not found: {}", id))?;
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
pub fn export_session(dir: &Path, session_id: &str, output_path: &Path) -> Result<(), anyhow::Error> {
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
    let session: Session =
        serde_json::from_str(&content).map_err(|e| anyhow::anyhow!("failed to parse session JSON: {}", e))?;

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

/// A minimal tar archive builder using only std::io.
mod tar_builder {
    use std::io::{Result, Write};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Header {
        name: [u8; 100],
        mode: [u8; 8],
        uid: [u8; 8],
        gid: [u8; 8],
        size: [u8; 12],
        mtime: [u8; 12],
        checksum: [u8; 8],
        type_flag: u8,
        linkname: [u8; 100],
        magic: [u8; 6],
        version: [u8; 2],
        uname: [u8; 32],
        gname: [u8; 32],
        devmajor: [u8; 8],
        devminor: [u8; 8],
        prefix: [u8; 155],
        padding: [u8; 12],
    }

    impl Header {
        fn new(size: u64, name: &str, prefix: &str) -> Self {
            let full_name = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{}/{}", prefix, name)
            };
            let name_bytes = full_name.as_bytes();
            let mut header = Self::default();
            header.name[..name_bytes.len()].copy_from_slice(name_bytes);
            header.size = octal_bytes(size);
            header.type_flag = b'0';
            header.magic = *b"ustar\0";
            header.version = *b"00";
            header
        }

        fn checksum(&self) -> u32 {
            let mut raw = *self;
            raw.checksum = [0u8; 8];
            let bytes = unsafe {
                std::slice::from_raw_parts(&raw as *const Self as *const u8, std::mem::size_of::<Self>())
            };
            bytes.iter().map(|&b| b as u32).sum()
        }

        fn as_bytes(&self) -> &[u8] {
            unsafe { std::slice::from_raw_parts(self as *const Self as *const u8, std::mem::size_of::<Self>()) }
        }
    }

    impl Default for Header {
        fn default() -> Self {
            Self {
                name: [0u8; 100],
                mode: [0u8; 8],
                uid: [0u8; 8],
                gid: [0u8; 8],
                size: [0u8; 12],
                mtime: [0u8; 12],
                checksum: [0u8; 8],
                type_flag: 0,
                linkname: [0u8; 100],
                magic: [0u8; 6],
                version: [0u8; 2],
                uname: [0u8; 32],
                gname: [0u8; 32],
                devmajor: [0u8; 8],
                devminor: [0u8; 8],
                prefix: [0u8; 155],
                padding: [0u8; 12],
            }
        }
    }

    fn octal_bytes(mut value: u64) -> [u8; 12] {
        let mut bytes = [b'0'; 12];
        for i in (0..12).rev() {
            bytes[i] = b'0' + (value % 10) as u8;
            value /= 10;
        }
        bytes
    }

    pub struct Builder<W: Write> {
        inner: W,
        prefix: String,
    }

    impl<W: Write> Builder<W> {
        pub fn new(inner: W) -> Self {
            Self {
                inner,
                prefix: String::new(),
            }
        }

        pub fn set_prefix(&mut self, prefix: &str) {
            self.prefix = prefix.to_string();
        }

        pub fn append_file(&mut self, name: &str, data: &[u8]) -> Result<()> {
            let size = data.len() as u64;
            let mut header = Header::new(size, name, &self.prefix);
            let checksum = header.checksum();
            let checksum_bytes = octal_bytes(checksum as u64);
            header.checksum[..checksum_bytes.len()].copy_from_slice(&checksum_bytes);

            self.inner.write_all(header.as_bytes())?;
            self.inner.write_all(data)?;
            // Pad to 512-byte blocks
            let padded = (512 - (data.len() % 512)) % 512;
            self.inner.write_all(&vec![0u8; padded])?;
            Ok(())
        }

        pub fn finish(mut self) -> Result<()> {
            // Write two empty 512-byte blocks to mark end of archive
            self.inner.write_all(&[0u8; 1024])?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;
    use rand::RngCore;
    fn gen_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        key
    }

    #[test]
    fn test_create_and_load_session() {
        let dir = std::env::temp_dir().join("wuffagent_test_sessions");
        let _ = fs::create_dir_all(&dir);
        let session = create_session(&dir, "Test Session");
        assert_eq!(session.name, "Test Session");
        assert!(!session.id.is_empty());
        assert!(session.messages.is_empty());

        let loaded = load_session(&dir, &session.id).expect("session should exist");
        assert_eq!(loaded.name, "Test Session");
        assert_eq!(loaded.id, session.id);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_and_reload_with_messages() {
        let dir = std::env::temp_dir().join("wuffagent_test_sessions2");
        let _ = fs::create_dir_all(&dir);
        let mut session = create_session(&dir, "Chat");
        session.add_message(Message {
            role: "user".to_string(),
            content: "Hello".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        session.add_message(Message {
            role: "assistant".to_string(),
            content: "Hi there!".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        save_session(&dir, &session).unwrap();

        let reloaded = load_session(&dir, &session.id).unwrap();
        assert_eq!(reloaded.messages.len(), 2);
        assert_eq!(reloaded.messages[0].content, "Hello");
        assert_eq!(reloaded.messages[1].content, "Hi there!");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_list_sessions() {
        let dir = std::env::temp_dir().join("wuffagent_test_sessions3");
        let _ = fs::create_dir_all(&dir);
        let _s1 = create_session(&dir, "First");
        let _s2 = create_session(&dir, "Second");
        let _s3 = create_session(&dir, "Third");

        let sessions = list_sessions(&dir);
        assert_eq!(sessions.len(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_delete_session() {
        let dir = std::env::temp_dir().join("wuffagent_test_sessions4");
        let _ = fs::create_dir_all(&dir);
        let session = create_session(&dir, "To Delete");
        let result = delete_session(&dir, &session.id);
        assert!(result.is_ok());
        assert!(load_session(&dir, &session.id).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_delete_missing_session_returns_error() {
        let dir = std::env::temp_dir().join("wuffagent_test_sessions10");
        let _ = fs::create_dir_all(&dir);
        let result = delete_session(&dir, "nonexistent_id");
        assert!(result.is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_exists_true_after_create() {
        let dir = std::env::temp_dir().join("wuffagent_test_sessions5");
        let _ = fs::create_dir_all(&dir);
        let session = create_session(&dir, "Exists Test");
        assert!(session_exists(&dir, &session.id));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_exists_false_for_missing() {
        let dir = std::env::temp_dir().join("wuffagent_test_sessions6");
        let _ = fs::create_dir_all(&dir);
        assert!(!session_exists(&dir, "nonexistent_session_id"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_exists_false_after_delete() {
        let dir = std::env::temp_dir().join("wuffagent_test_sessions7");
        let _ = fs::create_dir_all(&dir);
        let session = create_session(&dir, "To Delete");
        assert!(session_exists(&dir, &session.id));
        let _ = delete_session(&dir, &session.id);
        assert!(!session_exists(&dir, &session.id));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_missing_session_returns_none() {
        let dir = std::env::temp_dir().join("wuffagent_test_sessions8");
        let _ = fs::create_dir_all(&dir);
        let result = load_session(&dir, "nonexistent_id");
        assert!(result.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_session_creates_file_when_missing() {
        // This test verifies the fix: saving a session whose file was deleted
        // should create a new session automatically instead of erroring.
        let dir = std::env::temp_dir().join("wuffagent_test_sessions9");
        let _ = fs::create_dir_all(&dir);
        let mut session = create_session(&dir, "Original");
        session.add_message(Message {
            role: "user".to_string(),
            content: "Msg1".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        save_session(&dir, &session).unwrap();

        // Delete the session file to simulate corruption/loss
        let session_id = session.id.clone();
        let session_file = dir.join(format!("{}.json", session_id));
        fs::remove_file(&session_file).unwrap();
        assert!(!session_exists(&dir, &session_id));

        // Now create a new session with a different id and save to it
        let mut new_session = create_session(&dir, "Fresh Start");
        new_session.add_message(Message {
            role: "user".to_string(),
            content: "New msg".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        save_session(&dir, &new_session).unwrap();

        // The new session should be loadable
        let loaded = load_session(&dir, &new_session.id).expect("should load fresh session");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].content, "New msg");

        // The old deleted session should not exist
        assert!(!session_exists(&dir, &session_id));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── Session Encryption Tests ──────────────────────────────────────────

    #[test]
    fn test_session_encrypt_decrypt_roundtrip() {
        let dir = std::env::temp_dir().join("wuffagent_test_enc_roundtrip");
        let _ = fs::create_dir_all(&dir);
        let mut session = Session::new("Encrypted Roundtrip");
        session.add_message(Message {
            role: "user".to_string(),
            content: "Hello encrypted world".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        let key = gen_key();
        save_session_encrypted(&dir, &session, &key).unwrap();
        assert!(session_exists(&dir, &session.id));

        let loaded = decrypt_and_load_session(&dir, &session.id, &key).expect("should decrypt");
        assert_eq!(loaded.name, "Encrypted Roundtrip");
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.messages[0].content, "Hello encrypted world");
        assert_eq!(loaded.id, session.id);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_encrypt_decrypt_empty_session() {
        let dir = std::env::temp_dir().join("wuffagent_test_enc_empty");
        let _ = fs::create_dir_all(&dir);
        let session = Session::new("Empty Session");
        let key = gen_key();
        save_session_encrypted(&dir, &session, &key).unwrap();

        let loaded = decrypt_and_load_session(&dir, &session.id, &key).expect("should decrypt empty");
        assert_eq!(loaded.name, "Empty Session");
        assert!(loaded.messages.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_encrypt_decrypt_unicode() {
        let dir = std::env::temp_dir().join("wuffagent_test_enc_unicode");
        let _ = fs::create_dir_all(&dir);
        let mut session = Session::new("Unicode Session");
        session.add_message(Message {
            role: "user".to_string(),
            content: "こんにちは世界 🌍 émojis Ñoño 中文".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        session.add_message(Message {
            role: "assistant".to_string(),
            content: "你好！🎉 مرحبا".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        let key = gen_key();
        save_session_encrypted(&dir, &session, &key).unwrap();

        let loaded = decrypt_and_load_session(&dir, &session.id, &key).expect("should decrypt unicode");
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].content, "こんにちは世界 🌍 émojis Ñoño 中文");
        assert_eq!(loaded.messages[1].content, "你好！🎉 مرحبا");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_decrypt_wrong_key_fails() {
        let dir = std::env::temp_dir().join("wuffagent_test_enc_wrongkey");
        let _ = fs::create_dir_all(&dir);
        let mut session = Session::new("Wrong Key Test");
        session.add_message(Message {
            role: "user".to_string(),
            content: "Secret".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        let key = gen_key();
        let wrong_key = gen_key();
        save_session_encrypted(&dir, &session, &key).unwrap();

        let result = decrypt_and_load_session(&dir, &session.id, &wrong_key);
        assert!(result.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_decrypt_corrupted_data_fails() {
        let dir = std::env::temp_dir().join("wuffagent_test_enc_corrupt");
        let _ = fs::create_dir_all(&dir);
        let mut session = Session::new("Corrupt Test");
        session.add_message(Message {
            role: "user".to_string(),
            content: "Data".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        let key = gen_key();
        save_session_encrypted(&dir, &session, &key).unwrap();

        // Corrupt the file by overwriting with garbage
        let path = dir.join(format!("{}.json", session.id));
        fs::write(&path, "this is not valid base64 !!!").unwrap();

        let result = decrypt_and_load_session(&dir, &session.id, &key);
        assert!(result.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_load_corrupted_file() {
        let dir = std::env::temp_dir().join("wuffagent_test_enc_corrupted_json");
        let _ = fs::create_dir_all(&dir);
        // Write a file that is valid JSON but not a valid Session
        let path = dir.join("corrupted.json");
        fs::write(&path, "{\"id\":\"bad\",\"not_a_session\":true}").unwrap();

        let result = load_session(&dir, "corrupted");
        assert!(result.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_load_encrypted_with_missing_key() {
        let dir = std::env::temp_dir().join("wuffagent_test_enc_missing_key");
        let _ = fs::create_dir_all(&dir);
        let session = Session::new("Missing Key");
        let key = gen_key();
        save_session_encrypted(&dir, &session, &key).unwrap();

        // load_session should return None for encrypted files (no key provided)
        let result = load_session(&dir, &session.id);
        assert!(result.is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_save_and_load_with_messages() {
        let dir = std::env::temp_dir().join("wuffagent_test_enc_multi_msg");
        let _ = fs::create_dir_all(&dir);
        let mut session = Session::new("Multi Message");
        session.add_message(Message {
            role: "user".to_string(),
            content: "First message".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        session.add_message(Message {
            role: "assistant".to_string(),
            content: "Second message".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        session.add_message(Message {
            role: "user".to_string(),
            content: "Third message".to_string(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
        reasoning_content: None,
        });
        let key = gen_key();
        save_session_encrypted(&dir, &session, &key).unwrap();

        let loaded = decrypt_and_load_session(&dir, &session.id, &key).expect("should decrypt multi");
        assert_eq!(loaded.messages.len(), 3);
        assert_eq!(loaded.messages[0].content, "First message");
        assert_eq!(loaded.messages[1].content, "Second message");
        assert_eq!(loaded.messages[2].content, "Third message");
        assert_eq!(loaded.name, "Multi Message");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_list_after_encrypted_save() {
        let dir = std::env::temp_dir().join("wuffagent_test_enc_list");
        let _ = fs::create_dir_all(&dir);
        // Save an encrypted session
        let enc_session = Session::new("Encrypted One");
        let key = gen_key();
        save_session_encrypted(&dir, &enc_session, &key).unwrap();

        // Also save a plain session
        let plain_session = create_session(&dir, "Plain One");

        // list_sessions only reads plain JSON; encrypted files are skipped
        let sessions = list_sessions(&dir);
        // Only the plain session should appear
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "Plain One");

        let _ = fs::remove_dir_all(&dir);
    }
}
