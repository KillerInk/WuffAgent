//! Unit tests for the `search` module (see `super`).

use super::*;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("wuff_search_content_{tag}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write(p: &std::path::Path, s: &str) {
    fs::write(p, s).unwrap();
}

fn params(pairs: &[(&str, serde_json::Value)]) -> ToolParams {
    let mut m = std::collections::HashMap::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v.clone());
    }
    ToolParams { values: m }
}

/// search_content returns a bare raw-text string (not a JSON object) —
/// extract it directly.
fn success_str(out: ToolOutput) -> String {
    match out {
        ToolOutput::Success(serde_json::Value::String(s)) => s,
        other => panic!("expected a raw string result, got {other:?}"),
    }
}

#[test]
fn test_search_single_file_substring() {
    let dir = temp_dir("single");
    let p = dir.join("f.txt");
    write(&p, "hello world\nfoo bar\nbaz foo\n");
    let raw = success_str(
        search_content("foo", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    let path = p.to_str().unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(
        lines[0],
        &format!("[search_content foo in {path}: 2 matches in 1 file]")
    );
    assert_eq!(lines[1], &format!("{path}:2: foo bar"));
    assert_eq!(lines[2], &format!("{path}:3: baz foo"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_directory_recursive() {
    let dir = temp_dir("dir");
    // Join one component at a time so the expected path string uses the
    // platform separator (matching walk_dir's fs::read_dir paths) —
    // join("sub/deep") keeps the '/' on Windows.
    let sub = dir.join("sub").join("deep");
    fs::create_dir_all(&sub).unwrap();
    write(&dir.join("a.txt"), "needle here\n");
    write(&sub.join("b.txt"), "no match\nneedle again\n");
    write(&sub.join("c.txt"), "nothing\n");
    let raw = success_str(
        search_content("needle", dir.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    let a = dir.join("a.txt").to_str().unwrap().to_string();
    let b = sub.join("b.txt").to_str().unwrap().to_string();
    let header = raw.lines().next().unwrap();
    assert!(
        header.contains("2 matches in 3 files"),
        "header: {header}"
    );
    assert!(raw.contains(&format!("{a}:1: needle here")), "raw: {raw}");
    assert!(raw.contains(&format!("{b}:2: needle again")), "raw: {raw}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_glob_filter() {
    let dir = temp_dir("glob");
    write(&dir.join("a.txt"), "target\n");
    write(&dir.join("b.rs"), "target\n");
    write(&dir.join("c.md"), "target\n");
    let raw = success_str(
        search_content(
            "target",
            dir.to_str().unwrap(),
            Some("*.txt"),
            false,
            true,
            0,
            100,
        )
        .unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "raw: {raw}");
    let l = lines[1];
    assert!(l.ends_with("a.txt:1: target"), "line: {l}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_regex() {
    let dir = temp_dir("regex");
    let p = dir.join("r.txt");
    write(&p, "error 404 found\neror once\nno digits\nline 123 ok\n");
    let raw = success_str(
        search_content("e+r+o+r", p.to_str().unwrap(), None, true, true, 0, 100).unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 3, "raw: {raw}");
    let (l1, l2) = (lines[1], lines[2]);
    assert!(l1.ends_with(":1: error 404 found"), "line: {l1}");
    assert!(l2.ends_with(":2: eror once"), "line: {l2}");

    let raw = success_str(
        search_content("\\d+", p.to_str().unwrap(), None, true, true, 0, 100).unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 3, "raw: {raw}");
    let (l1, l2) = (lines[1], lines[2]);
    assert!(l1.ends_with(":1: error 404 found"), "line: {l1}");
    assert!(l2.ends_with(":4: line 123 ok"), "line: {l2}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_case_insensitive() {
    let dir = temp_dir("case");
    let p = dir.join("c.txt");
    write(&p, "Foo bar\nFOO baz\nfoo qux\n");
    // Case-sensitive by default: only exact "Foo".
    let raw = success_str(
        search_content("Foo", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    let header = raw.lines().next().unwrap();
    assert!(header.contains("1 match in 1 file"), "header: {header}");
    // Case-insensitive: all three.
    let raw = success_str(
        search_content("foo", p.to_str().unwrap(), None, false, false, 0, 100).unwrap(),
    );
    let header = raw.lines().next().unwrap();
    assert!(
        header.contains("3 matches in 1 file"),
        "header: {header}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_max_results_truncated() {
    let dir = temp_dir("maxres");
    let p = dir.join("m.txt");
    let content: Vec<String> = (1..=5).map(|i| format!("match line {i}")).collect();
    write(&p, &content.join("\n"));
    let raw = success_str(
        search_content("match", p.to_str().unwrap(), None, false, true, 0, 2).unwrap(),
    );
    let header = raw.lines().next().unwrap();
    assert!(header.contains("2 matches in 1 file"), "header: {header}");
    assert!(header.contains("truncated"), "header: {header}");
    // Exactly at the limit is not truncated.
    let raw = success_str(
        search_content("match", p.to_str().unwrap(), None, false, true, 0, 5).unwrap(),
    );
    let header = raw.lines().next().unwrap();
    assert!(
        header.contains("5 matches in 1 file"),
        "header: {header}"
    );
    assert!(!header.contains("truncated"), "header: {header}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_context_lines() {
    let dir = temp_dir("ctx");
    let p = dir.join("ctx.txt");
    write(&p, "l1\nl2\ntarget\nl4\nl5\n");
    let p_str = p.to_str().unwrap();
    let raw = success_str(
        search_content("target", p_str, None, false, true, 1, 100).unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 4, "raw: {raw}");
    let (c1, m, c2) = (lines[1], lines[2], lines[3]);
    assert_eq!(c1, &format!("    {p_str}:2: l2"), "ctx line: {c1}");
    assert_eq!(m, &format!("{p_str}:3: target"), "match: {m}");
    assert_eq!(c2, &format!("    {p_str}:4: l4"), "ctx line: {c2}");
    // Without context lines: just the header and the match.
    let raw = success_str(
        search_content("target", p_str, None, false, true, 0, 100).unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "raw: {raw}");
    let m = lines[1];
    assert_eq!(m, &format!("{p_str}:3: target"), "match: {m}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_binary_skipped() {
    let dir = temp_dir("binary");
    let p = dir.join("bin.dat");
    fs::write(&p, b"\x00\x01needle\x02").unwrap();
    let raw = success_str(
        search_content("needle", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    let header = raw.lines().next().unwrap();
    assert!(
        header.contains("0 matches in 1 file"),
        "the binary file was searched but matched nothing; header: {header}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_git_dir_skipped() {
    let dir = temp_dir("git");
    let gitdir = dir.join(".git");
    fs::create_dir_all(&gitdir).unwrap();
    write(&gitdir.join("config"), "secret needle\n");
    write(&dir.join("a.txt"), "visible needle\n");
    let raw = success_str(
        search_content("needle", dir.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "raw: {raw}");
    let l = lines[1];
    assert!(l.ends_with("a.txt:1: visible needle"), "line: {l}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_missing_path_fails() {
    let dir = temp_dir("missing");
    let err = search_content(
        "x",
        dir.join("nope").to_str().unwrap(),
        None,
        false,
        true,
        0,
        100,
    )
    .unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_invalid_regex_fails() {
    let dir = temp_dir("badre");
    let p = dir.join("f.txt");
    write(&p, "abc\n");
    let err =
        search_content("([unclosed", p.to_str().unwrap(), None, true, true, 0, 100).unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_content_tool_executes() {
    let dir = temp_dir("tool");
    let p = dir.join("t.txt");
    write(&p, "alpha\nbeta alpha\ngamma\n");
    let p_str = p.to_str().unwrap();
    let tool = SearchContentTool::new();
    assert_eq!(tool.name(), "search_content");
    let raw = success_str(
        tool.execute(params(&[
            ("pattern", serde_json::json!("alpha")),
            ("path", serde_json::json!(p_str)),
        ]))
        .unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 3, "raw: {raw}");
    let (l1, l2) = (lines[1], lines[2]);
    assert!(l1.ends_with(":1: alpha"), "line: {l1}");
    assert!(l2.ends_with(":2: beta alpha"), "line: {l2}");
    // Optional params via the tool: regex + case-insensitive + context.
    let raw = success_str(
        tool.execute(params(&[
            ("pattern", serde_json::json!("g.m.a")),
            ("path", serde_json::json!(p_str)),
            ("regex", serde_json::json!(true)),
            ("case_sensitive", serde_json::json!(false)),
            ("context_lines", serde_json::json!(1)),
        ]))
        .unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    // header + one context line (before) + the match
    assert_eq!(lines.len(), 3, "raw: {raw}");
    let (c1, m) = (lines[1], lines[2]);
    assert!(c1.starts_with("    "), "context line must be indented: {c1}");
    assert_eq!(m, &format!("{p_str}:3: gamma"), "match: {m}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_content_tool_requires_pattern() {
    let tool = SearchContentTool::new();
    let err = tool
        .execute(params(&[("path", serde_json::json!("."))]))
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidParams(_)));
}

// ─── CRLF / BOM handling ────────────────────────────────────────────────────

#[test]
fn test_search_bom_file_line_anchored() {
    let dir = temp_dir("bom");
    let p = dir.join("b.txt");
    fs::write(&p, "\u{feff}needle here\nother needle\n").unwrap();
    // ^-anchored regex must match on line 1 despite the BOM.
    let raw = success_str(
        search_content("^needle", p.to_str().unwrap(), None, true, true, 0, 100).unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "raw: {raw}");
    let l = lines[1];
    assert!(l.ends_with(":1: needle here"), "line: {l}");
    assert!(!l.contains('\u{feff}'), "BOM leaked into output: {l:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_bom_file_substring_first_line() {
    let dir = temp_dir("bomsub");
    let p = dir.join("s.txt");
    fs::write(&p, "\u{feff}unique_token\nrest\n").unwrap();
    let raw = success_str(
        search_content(
            "unique_token",
            p.to_str().unwrap(),
            None,
            false,
            true,
            0,
            100,
        )
        .unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "raw: {raw}");
    let l = lines[1];
    assert!(l.ends_with(":1: unique_token"), "line: {l}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_crlf_file_clean_output() {
    let dir = temp_dir("crlfsearch");
    let p = dir.join("c.txt");
    fs::write(&p, "alpha\r\nbeta alpha\r\ngamma\r\n").unwrap();
    let raw = success_str(
        search_content("alpha", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 3, "raw: {raw}");
    for l in &lines[1..] {
        assert!(!l.contains('\r'), "CR leaked into output: {l:?}");
    }
    let l = lines[2];
    assert!(l.ends_with(":2: beta alpha"), "line: {l}");
    let _ = fs::remove_dir_all(&dir);
}
