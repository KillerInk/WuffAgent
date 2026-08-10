//! Session persistence tests

use tempfile::tempdir;
use wuffagent::sessions::{
    create_session, delete_session, load_session, list_sessions, save_session,
};
use wuffagent::types::Message;

#[test]
fn test_create_and_load_session() {
    let dir = tempdir().unwrap();
    let dir_path = dir.path();

    let session = create_session(dir_path, "Test Session");
    assert_eq!(session.name, "Test Session");
    assert!(!session.id.is_empty());
    assert!(session.messages.is_empty());

    let loaded = load_session(dir_path, &session.id).expect("session should exist");
    assert_eq!(loaded.name, "Test Session");
    assert_eq!(loaded.id, session.id);
}

#[test]
fn test_save_and_reload_with_messages() {
    let dir = tempdir().unwrap();
    let dir_path = dir.path();

    let mut session = create_session(dir_path, "Chat");
    session.add_message(Message {
        role: "user".to_string(),
        content: "Hello".to_string(),
        timestamp: String::new(),
        tool_calls: None,
    });
    session.add_message(Message {
        role: "assistant".to_string(),
        content: "Hi there!".to_string(),
        timestamp: String::new(),
        tool_calls: None,
    });
    save_session(dir_path, &session).unwrap();

    let reloaded = load_session(dir_path, &session.id).unwrap();
    assert_eq!(reloaded.messages.len(), 2);
    assert_eq!(reloaded.messages[0].content, "Hello");
    assert_eq!(reloaded.messages[1].content, "Hi there!");
}

#[test]
fn test_list_sessions() {
    let dir = tempdir().unwrap();
    let dir_path = dir.path();

    let _s1 = create_session(dir_path, "First");
    let _s2 = create_session(dir_path, "Second");
    let _s3 = create_session(dir_path, "Third");

    let sessions = list_sessions(dir_path);
    assert_eq!(sessions.len(), 3);
}

#[test]
fn test_delete_session() {
    let dir = tempdir().unwrap();
    let dir_path = dir.path();

    let session = create_session(dir_path, "To Delete");
    let deleted = delete_session(dir_path, &session.id);
    assert!(deleted);

    assert!(load_session(dir_path, &session.id).is_none());
}

#[test]
fn test_session_file_name_format() {
    let dir = tempdir().unwrap();
    let dir_path = dir.path();

    let session = create_session(dir_path, "My Session");
    let expected_file = format!("{}.json", session.id);
    let session_file = dir_path.join(expected_file);
    assert!(session_file.exists());
}

#[test]
fn test_session_with_empty_name() {
    let dir = tempdir().unwrap();
    let dir_path = dir.path();

    let session = create_session(dir_path, "");
    assert_eq!(session.name, "");
}

#[test]
fn test_session_with_special_chars_in_name() {
    let dir = tempdir().unwrap();
    let dir_path = dir.path();

    let session = create_session(dir_path, "Session: Special/Chars*<>?");
    assert_eq!(session.name, "Session: Special/Chars*<>?");
}
