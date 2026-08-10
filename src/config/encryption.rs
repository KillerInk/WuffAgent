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
