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

fn success_json(out: ToolOutput) -> serde_json::Value {
    match out {
        ToolOutput::Success(v) => v,
        other => panic!("expected Success, got {other:?}"),
    }
}

#[test]
fn test_search_single_file_substring() {
    let dir = temp_dir("single");
    let p = dir.join("f.txt");
    write(&p, "hello world\nfoo bar\nbaz foo\n");
    let json = success_json(
        search_content("foo", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
    assert!(!json["truncated"].as_bool().unwrap());
    let matches = json["matches"].as_array().unwrap();
    assert_eq!(matches[0]["line"].as_u64().unwrap(), 2);
    assert_eq!(matches[0]["text"].as_str().unwrap(), "foo bar");
    assert_eq!(matches[1]["line"].as_u64().unwrap(), 3);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_directory_recursive() {
    let dir = temp_dir("dir");
    let sub = dir.join("sub/deep");
    fs::create_dir_all(&sub).unwrap();
    write(&dir.join("a.txt"), "needle here\n");
    write(&sub.join("b.txt"), "no match\nneedle again\n");
    write(&sub.join("c.txt"), "nothing\n");
    let json = success_json(
        search_content("needle", dir.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
    assert_eq!(json["files_searched"].as_u64().unwrap(), 3);
    let paths: Vec<&str> = json["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("a.txt")));
    assert!(paths.iter().any(|p| p.ends_with("b.txt")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_glob_filter() {
    let dir = temp_dir("glob");
    write(&dir.join("a.txt"), "target\n");
    write(&dir.join("b.rs"), "target\n");
    write(&dir.join("c.md"), "target\n");
    let json = success_json(
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
    assert_eq!(json["total_matches"].as_u64().unwrap(), 1);
    assert!(json["matches"][0]["path"]
        .as_str()
        .unwrap()
        .ends_with("a.txt"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_regex() {
    let dir = temp_dir("regex");
    let p = dir.join("r.txt");
    write(&p, "error 404 found\neror once\nno digits\nline 123 ok\n");
    let json = success_json(
        search_content("e+r+o+r", p.to_str().unwrap(), None, true, true, 0, 100).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
    let lines: Vec<u64> = json["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["line"].as_u64().unwrap())
        .collect();
    assert_eq!(lines, vec![1, 2]);

    let json = success_json(
        search_content("\\d+", p.to_str().unwrap(), None, true, true, 0, 100).unwrap(),
    );
    let lines: Vec<u64> = json["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["line"].as_u64().unwrap())
        .collect();
    assert_eq!(lines, vec![1, 4]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_case_insensitive() {
    let dir = temp_dir("case");
    let p = dir.join("c.txt");
    write(&p, "Foo bar\nFOO baz\nfoo qux\n");
    // Case-sensitive by default: only exact "Foo".
    let json = success_json(
        search_content("Foo", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 1);
    // Case-insensitive: all three.
    let json = success_json(
        search_content("foo", p.to_str().unwrap(), None, false, false, 0, 100).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 3);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_max_results_truncated() {
    let dir = temp_dir("maxres");
    let p = dir.join("m.txt");
    let content: Vec<String> = (1..=5).map(|i| format!("match line {i}")).collect();
    write(&p, &content.join("\n"));
    let json = success_json(
        search_content("match", p.to_str().unwrap(), None, false, true, 0, 2).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
    assert!(json["truncated"].as_bool().unwrap());
    // Exactly at the limit is not truncated.
    let json = success_json(
        search_content("match", p.to_str().unwrap(), None, false, true, 0, 5).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 5);
    assert!(!json["truncated"].as_bool().unwrap());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_context_lines() {
    let dir = temp_dir("ctx");
    let p = dir.join("ctx.txt");
    write(&p, "l1\nl2\ntarget\nl4\nl5\n");
    let json = success_json(
        search_content("target", p.to_str().unwrap(), None, false, true, 1, 100).unwrap(),
    );
    let m = &json["matches"][0];
    assert_eq!(
        m["context_before"].as_array().unwrap(),
        &vec![serde_json::json!("l2")]
    );
    assert_eq!(
        m["context_after"].as_array().unwrap(),
        &vec![serde_json::json!("l4")]
    );
    // Without context lines the keys are absent.
    let json = success_json(
        search_content("target", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    assert!(json["matches"][0].get("context_before").is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_binary_skipped() {
    let dir = temp_dir("binary");
    let p = dir.join("bin.dat");
    fs::write(&p, b"\x00\x01needle\x02").unwrap();
    let json = success_json(
        search_content("needle", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 0);
    assert_eq!(
        json["files_searched"].as_u64().unwrap(),
        0,
        "binary files are not counted"
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
    let json = success_json(
        search_content("needle", dir.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 1);
    assert!(json["matches"][0]["path"]
        .as_str()
        .unwrap()
        .ends_with("a.txt"));
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
    let tool = SearchContentTool::new();
    assert_eq!(tool.name(), "search_content");
    let json = success_json(
        tool.execute(params(&[
            ("pattern", serde_json::json!("alpha")),
            ("path", serde_json::json!(p.to_str().unwrap())),
        ]))
        .unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
    // Optional params via the tool: regex + case-insensitive + context.
    let json = success_json(
        tool.execute(params(&[
            ("pattern", serde_json::json!("g.m.a")),
            ("path", serde_json::json!(p.to_str().unwrap())),
            ("regex", serde_json::json!(true)),
            ("case_sensitive", serde_json::json!(false)),
            ("context_lines", serde_json::json!(1)),
        ]))
        .unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 1);
    assert!(json["matches"][0].get("context_before").is_some());
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
    let json = success_json(
        search_content("^needle", p.to_str().unwrap(), None, true, true, 0, 100).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 1);
    assert_eq!(json["matches"][0]["line"].as_u64().unwrap(), 1);
    let text = json["matches"][0]["text"].as_str().unwrap();
    assert_eq!(text, "needle here");
    assert!(
        !text.starts_with('\u{feff}'),
        "BOM leaked into output: {text:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_bom_file_substring_first_line() {
    let dir = temp_dir("bomsub");
    let p = dir.join("s.txt");
    fs::write(&p, "\u{feff}unique_token\nrest\n").unwrap();
    let json = success_json(
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
    assert_eq!(json["total_matches"].as_u64().unwrap(), 1);
    assert_eq!(json["matches"][0]["text"].as_str().unwrap(), "unique_token");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_crlf_file_clean_output() {
    let dir = temp_dir("crlfsearch");
    let p = dir.join("c.txt");
    fs::write(&p, "alpha\r\nbeta alpha\r\ngamma\r\n").unwrap();
    let json = success_json(
        search_content("alpha", p.to_str().unwrap(), None, false, true, 0, 100).unwrap(),
    );
    assert_eq!(json["total_matches"].as_u64().unwrap(), 2);
    for m in json["matches"].as_array().unwrap() {
        let text = m["text"].as_str().unwrap();
        assert!(!text.contains('\r'), "CR leaked into output: {text:?}");
    }
    assert_eq!(json["matches"][1]["line"].as_u64().unwrap(), 2);
    let _ = fs::remove_dir_all(&dir);
}
