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

/// Magic bytes written at the start of an encrypted session file.
const ENCRYPTION_MARKER: &[u8] = b"WUFFENC";

pub fn load_session(dir: &Path, id: &str) -> Option<Session> {
    let path = dir.join(format!("{}.json", id));
    if path.exists() {
        if let Ok(bytes) = fs::read(&path) {
            if bytes.len() > ENCRYPTION_MARKER.len() && &bytes[..ENCRYPTION_MARKER.len()] == ENCRYPTION_MARKER {
                // Encrypted file — return None to signal that decrypt_and_load_session is needed
                return None;
            }
        }
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
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    sessions
}

pub fn delete_session(dir: &Path, id: &str) -> bool {
    let path = dir.join(format!("{}.json", id));
    if fs::remove_file(&path).is_err() {
        return false;
    }
    // Also delete all backup files for this session
    let pattern = format!("{}.json.bak_", id);
    if dir.exists() {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                if let Some(name) = file_name.to_str() {
                    if name.starts_with(&pattern) {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
    true
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
            let mut raw = self.clone();
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
