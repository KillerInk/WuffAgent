//! Unit tests for the `progress` module (see `super`).

use super::*;

#[test]
fn test_parse_progress() {
    assert_eq!(parse_progress("loading model ... 100%"), Some(100.0));
    assert_eq!(parse_progress("loading model ... 50%"), Some(50.0));
    assert_eq!(parse_progress("loading model ... 75.5%"), Some(75.5));
    assert_eq!(parse_progress("no percentage here"), None);
    assert_eq!(parse_progress(""), None);
    assert_eq!(parse_progress("100"), None);
}
