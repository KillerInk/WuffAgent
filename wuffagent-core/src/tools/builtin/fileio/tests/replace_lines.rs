//! `replace_lines`: line-range replacement (line-based alternative to
//! apply_diff) — ranges, delete, validation errors, verify guard,
//! CRLF/BOM/trailing-newline preservation.

use super::*;

fn rl(p: &str, start: usize, end: usize, new: &str) -> Result<ToolOutput, ToolError> {
    replace_lines(p, start, end, new, None)
}

#[test]
fn test_replace_lines_single() {
    let dir = temp_dir("rl1");
    let p = dir.join("a.txt");
    write(p.to_str().unwrap(), "one\ntwo\nthree\n");
    rl(p.to_str().unwrap(), 2, 2, "TWO").unwrap();
    assert_eq!(read_string(p.to_str().unwrap()), "one\nTWO\nthree\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_multi_line_range() {
    let dir = temp_dir("rl2");
    let p = dir.join("b.txt");
    write(p.to_str().unwrap(), "a\nb\nc\nd\ne\n");
    let json = success_json(rl(p.to_str().unwrap(), 2, 4, "X\nY").unwrap());
    assert_eq!(json["lines_replaced"].as_u64().unwrap(), 3);
    assert_eq!(json["lines_inserted"].as_u64().unwrap(), 2);
    assert_eq!(json["verified"].as_bool().unwrap(), false);
    assert_eq!(read_string(p.to_str().unwrap()), "a\nX\nY\ne\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_first_and_last_lines() {
    let dir = temp_dir("rl3");
    let p = dir.join("c.txt");
    write(p.to_str().unwrap(), "head\nmid\ntail\n");
    rl(p.to_str().unwrap(), 1, 1, "HEAD").unwrap();
    rl(p.to_str().unwrap(), 3, 3, "TAIL").unwrap();
    assert_eq!(read_string(p.to_str().unwrap()), "HEAD\nmid\nTAIL\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_delete() {
    let dir = temp_dir("rl4");
    let p = dir.join("d.txt");
    write(p.to_str().unwrap(), "keep\ngone\ngone2\nkeep2\n");
    let json = success_json(rl(p.to_str().unwrap(), 2, 3, "").unwrap());
    assert_eq!(json["lines_replaced"].as_u64().unwrap(), 2);
    assert_eq!(json["lines_inserted"].as_u64().unwrap(), 0);
    assert_eq!(read_string(p.to_str().unwrap()), "keep\nkeep2\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_start_after_end_fails() {
    let dir = temp_dir("rl5");
    let p = dir.join("e.txt");
    let original = "a\nb\n";
    write(p.to_str().unwrap(), original);
    let err = rl(p.to_str().unwrap(), 3, 2, "x").unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("after end_line"), "msg: {msg}");
    assert_eq!(read_string(p.to_str().unwrap()), original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_zero_start_fails() {
    let dir = temp_dir("rl6");
    let p = dir.join("f.txt");
    let original = "a\nb\n";
    write(p.to_str().unwrap(), original);
    let err = rl(p.to_str().unwrap(), 0, 1, "x").unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("start_line must be >= 1"), "msg: {msg}");
    assert_eq!(read_string(p.to_str().unwrap()), original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_end_beyond_eof_reports_total_and_tail() {
    let dir = temp_dir("rl7");
    let p = dir.join("g.txt");
    let original = "a\nb\nc\n";
    write(p.to_str().unwrap(), original);
    let err = rl(p.to_str().unwrap(), 1, 99, "x").unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("has 3 lines"), "msg: {msg}");
    assert!(msg.contains("last lines:"), "msg: {msg}");
    assert!(msg.contains("3: c"), "msg: {msg}");
    assert_eq!(read_string(p.to_str().unwrap()), original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_empty_file_fails() {
    let dir = temp_dir("rl8");
    let p = dir.join("h.txt");
    fs::write(&p, "").unwrap();
    let err = rl(p.to_str().unwrap(), 1, 1, "x").unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("empty"), "msg: {msg}");
    assert!(msg.contains("write_file"), "msg: {msg}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_verify_pass() {
    let dir = temp_dir("rl9");
    let p = dir.join("i.txt");
    write(
        p.to_str().unwrap(),
        "fn a() {\n    let x = 1;\n}\nfn a() {\n    let x = 2;\n}\n",
    );
    let json = success_json(
        replace_lines(
            p.to_str().unwrap(),
            2,
            2,
            "    let x = 10;",
            Some("let x = 1;"),
        )
        .unwrap(),
    );
    assert_eq!(json["verified"].as_bool().unwrap(), true);
    assert_eq!(
        read_string(p.to_str().unwrap()),
        "fn a() {\n    let x = 10;\n}\nfn a() {\n    let x = 2;\n}\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_verify_mismatch_fails_and_reports_target() {
    let dir = temp_dir("rl10");
    let p = dir.join("j.txt");
    let original = "alpha\nbeta\ngamma\n";
    write(p.to_str().unwrap(), original);
    let err = replace_lines(p.to_str().unwrap(), 2, 3, "x", Some("not there"))
        .unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("verify_contains"), "msg: {msg}");
    assert!(msg.contains("beta\ngamma"), "msg: {msg}");
    assert_eq!(read_string(p.to_str().unwrap()), original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_preserves_crlf() {
    let dir = temp_dir("rl11");
    let p = dir.join("k.txt");
    fs::write(&p, "one\r\ntwo\r\nthree\r\n").unwrap();
    rl(p.to_str().unwrap(), 2, 2, "TWO").unwrap();
    assert_eq!(
        fs::read(p.to_str().unwrap()).unwrap(),
        b"one\r\nTWO\r\nthree\r\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_multiline_payload_crlf_normalized_on_lf_file() {
    let dir = temp_dir("rl12");
    let p = dir.join("l.txt");
    fs::write(&p, "a\nb\n").unwrap();
    // CRLF payload into an LF file must not introduce CR bytes.
    rl(p.to_str().unwrap(), 2, 2, "X\r\nY").unwrap();
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"a\nX\nY\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_multiline_payload_on_crlf_file() {
    let dir = temp_dir("rl13");
    let p = dir.join("m.txt");
    fs::write(&p, "a\r\nb\r\n").unwrap();
    rl(p.to_str().unwrap(), 2, 2, "X\nY").unwrap();
    assert_eq!(
        fs::read(p.to_str().unwrap()).unwrap(),
        b"a\r\nX\r\nY\r\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_preserves_bom() {
    let dir = temp_dir("rl14");
    let p = dir.join("n.txt");
    fs::write(&p, format!("{UTF8_BOM}alpha\nbeta\n")).unwrap();
    rl(p.to_str().unwrap(), 1, 1, "ALPHA").unwrap();
    assert_eq!(
        fs::read(p.to_str().unwrap()).unwrap(),
        format!("{UTF8_BOM}ALPHA\nbeta\n").into_bytes()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_no_trailing_newline_stays_absent() {
    let dir = temp_dir("rl15");
    let p = dir.join("o.txt");
    fs::write(&p, "a\nb").unwrap();
    rl(p.to_str().unwrap(), 1, 1, "A").unwrap();
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"A\nb");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_whole_file() {
    let dir = temp_dir("rl16");
    let p = dir.join("q.txt");
    write(p.to_str().unwrap(), "a\nb\n");
    rl(p.to_str().unwrap(), 1, 2, "X\nY\nZ").unwrap();
    assert_eq!(read_string(p.to_str().unwrap()), "X\nY\nZ\n");
    let _ = fs::remove_dir_all(&dir);
}
