//! Session persistence tests

use std::path::PathBuf;
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
        tool_calls: None,
    });
    session.add_message(Message {
        role: "assistant".to_string(),
        content: "Hi there!".to_string(),
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

    let s1 = create_session(dir_path, "First");
    let s2 = create_session(dir_path, "Second");

    let listed = list_sessions(dir_path);
    assert_eq!(listed.len(), 2);
    // Listed in reverse chronological order (newest first)
    assert_eq!(listed[0].name, "Second");
    assert_eq!(listed[1].name, "First");
}

#[test]
fn test_delete_session() {
    let dir = tempdir().unwrap();
    let dir_path = dir.path();

    let session = create_session(dir_path, "To Delete");
    assert!(delete_session(dir_path, &session.id));
    assert!(load_session(dir_path, &session.id).is_none());
    // Deleting again returns false
    assert!(!delete_session(dir_path, &session.id));
}

#[test]
fn test_list_sessions_empty_dir() {
    let dir = tempdir().unwrap();
    let sessions = list_sessions(dir.path());
    assert!(sessions.is_empty());
}
