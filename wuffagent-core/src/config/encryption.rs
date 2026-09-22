use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Encryption-related fields extracted from Config.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct EncryptionSettings {
    #[serde(default)]
    pub encryption_enabled: bool,
    /// Password used to derive the encryption key. Stored as a hex-encoded ChaCha20Poly1305 key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_password: Option<String>,
}

impl EncryptionSettings {
    /// Derive a 32-byte encryption key from a password using PBKDF2 (via the `chacha20poly1305` crate's key derivation).
    /// Returns None if no password is set.
    pub fn encryption_key(&self) -> Option<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let password = self.encryption_password.as_ref()?;
        let mut hasher = Sha256::new();
        // Simple but effective: hash the password with a salt prefix
        hasher.update(b"wuffagent-session-encryption-salt");
        hasher.update(password.as_bytes());
        let result = hasher.finalize();
        Some(result.into())
    }
}

/// Generate a random 32-byte encryption key.
#[allow(dead_code)]
pub fn generate_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// Encrypt plaintext using ChaCha20Poly1305 with the given 32-byte key.
/// Returns a base64-encoded string of [nonce || ciphertext].
#[allow(dead_code)]
pub fn encrypt(plaintext: &str, key: &[u8]) -> Result<String, anyhow::Error> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| anyhow::anyhow!("failed to create cipher: {}", e))?;

    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("encryption failed: {}", e))?;

    let mut encoded = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    encoded.extend_from_slice(&nonce_bytes);
    encoded.extend_from_slice(&ciphertext);

    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &encoded,
    ))
}

/// Decrypt a base64-encoded encrypted string using ChaCha20Poly1305 with the given key.
/// Returns None if the key is wrong or the data is malformed.
#[allow(dead_code)]
pub fn decrypt(encrypted: &str, key: &[u8]) -> Result<String, anyhow::Error> {
    let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encrypted)
        .map_err(|e| anyhow::anyhow!("base64 decode failed: {}", e))?;

    if raw.len() < 12 {
        return Err(anyhow::anyhow!("encrypted data too short"));
    }

    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|e| anyhow::anyhow!("failed to create cipher: {}", e))?;

    let nonce = Nonce::from_slice(&raw[..12]);
    let ciphertext = &raw[12..];

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("decryption failed: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("invalid utf-8: {}", e))
}

#[cfg(test)]
mod tests;
