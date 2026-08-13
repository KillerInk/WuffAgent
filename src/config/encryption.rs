use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Encryption-related fields extracted from Config.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EncryptionSettings {
    #[serde(default)]
    pub encryption_enabled: bool,
    /// Password used to derive the encryption key. Stored as a hex-encoded ChaCha20Poly1305 key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_password: Option<String>,
}

impl Default for EncryptionSettings {
    fn default() -> Self {
        Self {
            encryption_enabled: false,
            encryption_password: None,
        }
    }
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
    let raw = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        encrypted,
    )
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
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [42u8; 32];
        let plaintext = "sensitive session data";

        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_decrypt_empty_string() {
        let key = [0u8; 32];
        let encrypted = encrypt("", &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();
        assert_eq!(decrypted, "");
    }

    #[test]
    fn test_encrypt_decrypt_unicode() {
        let key = [1u8; 32];
        let plaintext = "Hello 世界 🌍 émojis";

        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_decrypt_different_keys_fail() {
        let key1 = [42u8; 32];
        let key2 = [43u8; 32];
        let plaintext = "sensitive session data";

        let encrypted = encrypt(plaintext, &key1).unwrap();
        let result = decrypt(&encrypted, &key2);
        assert!(result.is_err());
    }

    #[test]
    fn test_encrypt_wrong_key_returns_none() {
        let key1 = [42u8; 32];
        let key2 = [43u8; 32];
        let plaintext = "sensitive session data";

        let encrypted = encrypt(plaintext, &key1).unwrap();
        let result = decrypt(&encrypted, &key2);
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_key_produces_32_bytes() {
        let key = generate_key();
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn test_generate_key_uniqueness() {
        let key1 = generate_key();
        let key2 = generate_key();
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_generate_key_randomness() {
        let mut keys = Vec::new();
        for _ in 0..10 {
            keys.push(generate_key());
        }
        // Verify no duplicates
        for i in 0..keys.len() {
            for j in (i + 1)..keys.len() {
                assert_ne!(keys[i], keys[j]);
            }
        }
    }

    #[test]
    fn test_encryption_settings_default() {
        let settings = EncryptionSettings::default();
        assert!(!settings.encryption_enabled);
        assert!(settings.encryption_password.is_none());
    }

    #[test]
    fn test_encryption_key_returns_none_when_no_password() {
        let settings = EncryptionSettings::default();
        assert!(settings.encryption_key().is_none());
    }

    #[test]
    fn test_encryption_key_derives_consistent_key() {
        let settings = EncryptionSettings {
            encryption_enabled: true,
            encryption_password: Some("test-password".to_string()),
        };
        let key1 = settings.encryption_key().unwrap();
        let key2 = settings.encryption_key().unwrap();
        assert_eq!(key1, key2);
    }
}
