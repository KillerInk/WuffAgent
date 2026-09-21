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

/// Live-tail logic: lines are tracked across chunk boundaries, CRLF endings
/// are normalized, blank lines are dropped, and the tail reports only the
/// most recent lines (latest-tail semantics for the UI).
#[test]
fn pipe_state_tracks_recent_lines() {
    let mut st = PipeState::default();
    // One line split across two chunk boundaries.
    st.feed(0, b"alpha\n");
    st.feed(0, b"bet");
    st.feed(0, b"a\n");
    // A blank line is dropped; stderr feeds the same rolling window.
    st.feed(0, b"\n");
    st.feed(1, b"err line\n");
    assert_eq!(st.take_tail(6).unwrap(), "alpha\nbeta\nerr line");
    assert!(!st.stdout.is_empty() && !st.stderr.is_empty());

    // CRLF is normalized so Windows output lines don't carry stray \r.
    let mut st2 = PipeState::default();
    st2.feed(0, b"win\r\nline2\r\n");
    assert_eq!(st2.take_tail(6).unwrap(), "win\nline2");
}

/// The tail is capped to the most recent lines even when many are fed.
#[test]
fn pipe_state_tail_is_capped() {
    let mut st = PipeState::default();
    for i in 0..50u32 {
        st.feed(0, format!("line {i}\n").as_bytes());
    }
    let tail = st.take_tail(6).unwrap();
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 6);
    assert_eq!(lines.first().copied(), Some("line 44"));
    assert_eq!(lines.last().copied(), Some("line 49"));
}

/// Progress path end-to-end (no process): a sink receives nothing for a
/// no-op, and the default `execute_with_progress` still runs the tool.
#[test]
fn tool_progress_none_is_quiet() {
    let sink = ToolProgress::none();
    let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sink2 = ToolProgress {
        on_progress: Some({
            let c = std::sync::Arc::clone(&called);
            std::sync::Arc::new(move |_| {
                c.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        }),
    };
    sink.report("ignored");
    assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    sink2.report("seen");
    assert!(called.load(std::sync::atomic::Ordering::SeqCst));
}

/// End-to-end: a command that streams output over ~1s delivers progress
/// reports (latest-tail, each ≤ a handful of lines) while the full output is
/// still captured in the final result.
#[test]
fn execute_with_progress_streams_live_output() {
    let config = ShellConfig {
        enabled: true,
        allowed_commands: vec![".*".to_string()],
        timeout_ms: 20_000,
        ..Default::default()
    };
    let tool = ShellTool::new(config);

    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let r = std::sync::Arc::clone(&received);
    let sink = ToolProgress {
        on_progress: Some(std::sync::Arc::new(move |text: &str| {
            r.lock().unwrap().push(text.to_string());
        })),
    };

    // Emit 25 lines at ~60ms intervals (≈1.5s) so the 100ms report throttle
    // fires several times.
    //
    // Windows: the shell tool splits the command on whitespace into argv, and
    // PowerShell 5.1's unquoted -Command stops at `;` while `$var` expansion
    // mangles anything else — so the script is ONE quoted token with no `$`
    // and no `-ms` alias (unreliable in this PS version): the outer quote
    // pair survives argv re-quoting and PS strips it, leaving a clean script.
    #[cfg(windows)]
    let cmd = r#"powershell -NoProfile -c "1..25 | ForEach-Object { Start-Sleep -Milliseconds 60; Write-Output 'line' }""#;
    #[cfg(not(windows))]
    let cmd = r#"sh -c 'i=0; while [ $i -lt 25 ]; do echo line; sleep 0.06; i=$((i+1)); done'""#;

    let mut params = ToolParams::new();
    params.values.insert("command".to_string(), serde_json::Value::String(cmd.to_string()));
    let out = tool
        .execute_with_progress(params, &sink)
        .expect("command should succeed");
    let s = format!("{out}");
    let v: serde_json::Value = serde_json::from_str(&s).expect("output is JSON: {s}");
    assert_eq!(v.get("exit_code").and_then(|c| c.as_u64()), Some(0));
    // All 25 lines were captured in the final output.
    let stdout_field = v.get("stdout").and_then(|o| o.as_str()).unwrap_or("");
    let line_count = stdout_field.lines().filter(|l| l.trim() == "line").count();
    assert_eq!(
        line_count, 25,
        "expected 25 captured lines, got {line_count}: {s}"
    );

    let reports = received.lock().unwrap();
    assert!(
        !reports.is_empty(),
        "expected at least one live progress report"
    );
    for rep in reports.iter() {
        // Latest-tail: a small window, never the whole transcript.
        assert!(
            rep.lines().count() <= 8,
            "report too large ({} lines): {rep}",
            rep.lines().count()
        );
    }
    // The final report should include the last lines produced.
    let last = reports.last().expect("at least one report");
    assert!(last.contains("line"), "last report: {last}");
}
