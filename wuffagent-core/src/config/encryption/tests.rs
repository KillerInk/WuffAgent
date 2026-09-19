//! Unit tests for the `encryption` module (see `super`).

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
