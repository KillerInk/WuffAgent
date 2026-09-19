//! Unit tests for the shell tool (see `super`).

use super::*;

#[test]
fn test_dangerous_command_blocked() {
    let config = ShellConfig {
        enabled: true,
        allowed_commands: vec![".*".to_string()], // allow all
        ..Default::default()
    };
    let tool = ShellTool::new(config);

    assert!(tool.is_command_allowed("rm -rf /").is_err());
    assert!(tool.is_command_allowed("del C:\\").is_err());
}

#[test]
fn test_disabled_shell_blocks() {
    let config = ShellConfig {
        enabled: false,
        ..Default::default()
    };
    let tool = ShellTool::new(config);

    assert!(tool.is_command_allowed("echo hello").is_err());
}

#[test]
fn test_allowlist_matching() {
    let config = ShellConfig {
        enabled: true,
        allowed_commands: vec!["cargo build.*".to_string(), "git.*".to_string()],
        ..Default::default()
    };
    let tool = ShellTool::new(config);

    assert!(tool.is_command_allowed("cargo build").is_ok());
    assert!(tool.is_command_allowed("git status").is_ok());
    assert!(tool.is_command_allowed("rm -rf /tmp").is_err()); // not in allowlist
}

#[test]
fn test_new_dangerous_patterns() {
    let config = ShellConfig {
        enabled: true,
        allowed_commands: Vec::new(),
        ..Default::default()
    };
    let tool = ShellTool::new(config);

    // Patterns added in the security expansion
    assert!(tool.is_command_allowed("rm -rf ~/").is_err());
    assert!(tool.is_command_allowed("rm -rf /*").is_err());
    assert!(tool.is_command_allowed("> /dev/sda").is_err());
    assert!(tool.is_command_allowed("remove-item -recurse -force c:\\").is_err());
    assert!(tool.is_command_allowed("$(rm -rf /)").is_err());
    assert!(tool.is_command_allowed("shutdown -h now").is_err());
    assert!(tool.is_command_allowed("kill -9 1").is_err());
}
