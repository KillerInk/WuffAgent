use super::*;

// ─── CRLF / BOM handling ────────────────────────────────────────────────────

const BOM: &str = "\u{feff}";

#[test]
fn test_read_file_bom_stripped_and_reported() {
    let dir = temp_dir("bomread");
    let p = dir.join("bom.txt");
    fs::write(&p, format!("{BOM}hello\r\nworld\r\n")).unwrap();
    let out = read_file(p.to_str().unwrap(), None, None, false).unwrap();
    let json = success_json(out);
    assert_eq!(json["content"].as_str().unwrap(), "hello\nworld");
    assert_eq!(json["total_lines"].as_u64().unwrap(), 2);
    assert!(json["bom"].as_bool().unwrap(), "BOM must be reported");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_no_bom_flag() {
    let dir = temp_dir("nombom");
    let p = dir.join("n.txt");
    write(p.to_str().unwrap(), "plain\n");
    let json = success_json(read_file(p.to_str().unwrap(), None, None, false).unwrap());
    assert!(!json["bom"].as_bool().unwrap());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_line_numbers_with_bom() {
    let dir = temp_dir("bomnums");
    let p = dir.join("b.txt");
    fs::write(&p, format!("{BOM}first\r\nsecond\r\n")).unwrap();
    let json = success_json(read_file(p.to_str().unwrap(), None, None, true).unwrap());
    let content = json["content"].as_str().unwrap().to_string();
    assert!(
        content.starts_with("     1 | first"),
        "BOM must not leak into numbered output: {content:?}"
    );
    assert!(content.contains("     2 | second"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_file_preserves_crlf_and_bom() {
    let dir = temp_dir("bomwrite");
    let p = dir.join("w.txt");
    fs::write(&p, format!("{BOM}old one\r\nold two\r\n")).unwrap();
    // Model-style payload: clean LF lines, no BOM.
    write_file(p.to_str().unwrap(), "new one\nnew two\n").unwrap();
    let bytes = fs::read(p.to_str().unwrap()).unwrap();
    assert_eq!(bytes, format!("{BOM}new one\r\nnew two\r\n").into_bytes());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_file_lf_file_stays_lf_no_bom_added() {
    let dir = temp_dir("lfwrite");
    let p = dir.join("l.txt");
    fs::write(&p, "old\n").unwrap();
    write_file(p.to_str().unwrap(), "new\n").unwrap();
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"new\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_file_converts_crlf_content_for_lf_file() {
    let dir = temp_dir("crlf2lf");
    let p = dir.join("c.txt");
    fs::write(&p, "old\n").unwrap();
    write_file(p.to_str().unwrap(), "one\r\ntwo\r\n").unwrap();
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"one\ntwo\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_file_new_file_written_verbatim() {
    let dir = temp_dir("newwrite");
    let p = dir.join("n.txt");
    // No existing file: nothing to preserve, mixed payload stays mixed.
    write_file(p.to_str().unwrap(), "x\ny\r\n").unwrap();
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"x\ny\r\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_file_bytes_written_reflects_conversion() {
    let dir = temp_dir("bomwritebytes");
    let p = dir.join("w.txt");
    fs::write(&p, format!("{BOM}old\r\n")).unwrap();
    let json = success_json(write_file(p.to_str().unwrap(), "new\n").unwrap());
    // 3 BOM bytes + "new" + CRLF.
    assert_eq!(json["bytes_written"].as_u64().unwrap(), 3 + 3 + 2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_append_file_matches_crlf() {
    let dir = temp_dir("appcrlf");
    let p = dir.join("a.txt");
    fs::write(&p, "one\r\n").unwrap();
    append_file(p.to_str().unwrap(), "two\nthree\n").unwrap();
    assert_eq!(
        fs::read(p.to_str().unwrap()).unwrap(),
        b"one\r\ntwo\r\nthree\r\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_append_file_converts_crlf_payload_for_lf_file() {
    let dir = temp_dir("applf");
    let p = dir.join("l.txt");
    fs::write(&p, "one\n").unwrap();
    append_file(p.to_str().unwrap(), "two\r\n").unwrap();
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"one\ntwo\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_append_file_inserts_missing_trailing_newline() {
    let dir = temp_dir("appnl");
    let p = dir.join("n.txt");
    fs::write(&p, "one").unwrap();
    append_file(p.to_str().unwrap(), "two\n").unwrap();
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"one\ntwo\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_append_file_inserts_crlf_for_missing_trailing_newline() {
    let dir = temp_dir("appnlcrlf");
    let p = dir.join("n.txt");
    fs::write(&p, "one\r\ntwo").unwrap();
    append_file(p.to_str().unwrap(), "three\n").unwrap();
    assert_eq!(
        fs::read(p.to_str().unwrap()).unwrap(),
        b"one\r\ntwo\r\nthree\r\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_append_file_to_new_file_verbatim() {
    let dir = temp_dir("appnew");
    let p = dir.join("new.txt");
    append_file(p.to_str().unwrap(), "fresh\n").unwrap();
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"fresh\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_append_file_empty_content_no_separator() {
    let dir = temp_dir("appempty");
    let p = dir.join("e.txt");
    fs::write(&p, "one").unwrap();
    append_file(p.to_str().unwrap(), "").unwrap();
    // Empty payload must not insert a line break on its own.
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"one");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_preserves_bom_and_crlf() {
    let dir = temp_dir("diffbom");
    let p = dir.join("b.txt");
    fs::write(&p, format!("{BOM}hello\r\nworld\r\n")).unwrap();
    let diff = "<<<<<<< SEARCH\nhello\n=======\nHELLO\n>>>>>>> REPLACE\n";
    apply_diff(p.to_str().unwrap(), diff).unwrap();
    let bytes = fs::read(p.to_str().unwrap()).unwrap();
    assert_eq!(bytes, format!("{BOM}HELLO\r\nworld\r\n").into_bytes());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_search_first_line_of_bom_file() {
    // The model re-types the first line without the (invisible) BOM; the
    // match must still succeed and the BOM must survive the write.
    let dir = temp_dir("diffbomline");
    let p = dir.join("bl.txt");
    fs::write(&p, format!("{BOM}alpha\nbeta\n")).unwrap();
    let diff = "<<<<<<< SEARCH\nalpha\n=======\nALPHA\n>>>>>>> REPLACE\n";
    apply_diff(p.to_str().unwrap(), diff).unwrap();
    let bytes = fs::read(p.to_str().unwrap()).unwrap();
    assert_eq!(bytes, format!("{BOM}ALPHA\nbeta\n").into_bytes());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_diff_payload_with_bom() {
    let dir = temp_dir("diffpaybom");
    let p = dir.join("p.txt");
    fs::write(&p, "alpha\nbeta\n").unwrap();
    let diff = format!("{BOM}<<<<<<< SEARCH\nalpha\n=======\nALPHA\n>>>>>>> REPLACE\n");
    apply_diff(p.to_str().unwrap(), &diff).unwrap();
    assert_eq!(read_string(p.to_str().unwrap()), "ALPHA\nbeta\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_lf_file_with_stray_crlf_stays_lf() {
    // One stray CRLF must not flip an otherwise-LF file to CRLF: only the
    // replacement is touched, the original stray CRLF stays as-is.
    let dir = temp_dir("diffstray");
    let p = dir.join("s.txt");
    fs::write(&p, "x\r\ny\nz\n").unwrap();
    let diff = "<<<<<<< SEARCH\nz\n=======\nZ\n>>>>>>> REPLACE\n";
    apply_diff(p.to_str().unwrap(), diff).unwrap();
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"x\r\ny\nZ\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_utf16_clear_error() {
    let dir = temp_dir("utf16read");
    let p = dir.join("u.txt");
    fs::write(&p, b"\xFF\xFEh\0e\0l\0l\0o\0").unwrap();
    let err = read_file(p.to_str().unwrap(), None, None, false).unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("UTF-16"), "msg: {msg}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_utf16_clear_error_leaves_file_untouched() {
    let dir = temp_dir("utf16diff");
    let p = dir.join("u.txt");
    let original = b"\xFF\xFEh\0e\0l\0l\0o\0";
    fs::write(&p, original).unwrap();
    let diff = "<<<<<<< SEARCH\nhello\n=======\nx\n>>>>>>> REPLACE\n";
    let err = apply_diff(p.to_str().unwrap(), diff).unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("UTF-16"), "msg: {msg}");
    assert_eq!(&fs::read(p.to_str().unwrap()).unwrap(), original);
    let _ = fs::remove_dir_all(&dir);
}
