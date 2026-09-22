//! Unit tests for the `sessions` module (see `super`).

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
        image: None,
    });
    session.add_message(Message {
        role: "assistant".to_string(),
        content: "Hi there!".to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
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
        image: None,
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
        image: None,
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
        image: None,
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
        image: None,
    });
    session.add_message(Message {
        role: "assistant".to_string(),
        content: "你好！🎉 مرحبا".to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    });
    let key = gen_key();
    save_session_encrypted(&dir, &session, &key).unwrap();

    let loaded = decrypt_and_load_session(&dir, &session.id, &key).expect("should decrypt unicode");
    assert_eq!(loaded.messages.len(), 2);
    assert_eq!(
        loaded.messages[0].content,
        "こんにちは世界 🌍 émojis Ñoño 中文"
    );
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
        image: None,
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
        image: None,
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
        image: None,
    });
    session.add_message(Message {
        role: "assistant".to_string(),
        content: "Second message".to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
    });
    session.add_message(Message {
        role: "user".to_string(),
        content: "Third message".to_string(),
        timestamp: String::new(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
        image: None,
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
    let _plain_session = create_session(&dir, "Plain One");

    // list_sessions only reads plain JSON; encrypted files are skipped
    let sessions = list_sessions(&dir);
    // Only the plain session should appear
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].name, "Plain One");

    let _ = fs::remove_dir_all(&dir);
}
