//! Unit tests for the `server` module (see `super`).

use super::*;

#[test]
fn test_server_manager_creation() {
    let server = ServerManager::new("llama-server", "test_model.gguf", 18080, 0, 2048, 4);
    assert!(!server.is_running());
    assert_eq!(server.get_error(), None);
}

#[test]
fn test_parse_progress() {
    assert_eq!(parse_progress("loading model ... 100%"), Some(100.0));
    assert_eq!(parse_progress("loading model ... 50%"), Some(50.0));
    assert_eq!(parse_progress("loading model ... 75.5%"), Some(75.5));
    assert_eq!(parse_progress("no percentage here"), None);
    assert_eq!(parse_progress(""), None);
    assert_eq!(parse_progress("100"), None);
}
