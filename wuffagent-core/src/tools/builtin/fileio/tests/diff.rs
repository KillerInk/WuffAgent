use super::*;

#[test]
fn test_apply_diff_single_block() {
    let dir = temp_dir("diff1");
    let p = dir.join("d.txt");
    write(
        p.to_str().unwrap(),
        "fn main() {\n    println!(\"old\");\n}\n",
    );
    let diff = "<<<<<<< SEARCH\n    println!(\"old\");\n=======\n    println!(\"new\");\n>>>>>>> REPLACE\n";
    let out = apply_diff(p.to_str().unwrap(), diff).unwrap();
    let json = success_json(out);
    assert_eq!(json["blocks_applied"].as_u64().unwrap(), 1);
    let content = read_string(p.to_str().unwrap());
    assert!(content.contains("\"new\"") && !content.contains("\"old\""));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_multi_block() {
    let dir = temp_dir("diffm");
    let p = dir.join("m.txt");
    write(p.to_str().unwrap(), "alpha\nbeta\ngamma\n");
    let diff = "<<<<<<< SEARCH\nalpha\n=======\nALPHA\n>>>>>>> REPLACE\n<<<<<<< SEARCH\ngamma\n=======\nGAMMA\n>>>>>>> REPLACE\n";
    let out = apply_diff(p.to_str().unwrap(), diff).unwrap();
    let json = success_json(out);
    assert_eq!(json["blocks_applied"].as_u64().unwrap(), 2);
    assert_eq!(read_string(p.to_str().unwrap()), "ALPHA\nbeta\nGAMMA\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_delete_only() {
    let dir = temp_dir("diffdel");
    let p = dir.join("del.txt");
    write(p.to_str().unwrap(), "keep\ngone\nkeep2\n");
    let diff = "<<<<<<< SEARCH\ngone\n\n=======\n>>>>>>> REPLACE\n";
    apply_diff(p.to_str().unwrap(), diff).unwrap();
    assert_eq!(read_string(p.to_str().unwrap()), "keep\nkeep2\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_not_found_fails() {
    let dir = temp_dir("diffnf");
    let p = dir.join("nf.txt");
    let original = "unchanged\n";
    write(p.to_str().unwrap(), original);
    let diff = "<<<<<<< SEARCH\nmissing\n=======\nx\n>>>>>>> REPLACE\n";
    let err = apply_diff(p.to_str().unwrap(), diff).unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("not found"), "msg: {msg}");
    assert_eq!(read_string(p.to_str().unwrap()), original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_ambiguous_fails() {
    let dir = temp_dir("diffamb");
    let p = dir.join("amb.txt");
    let original = "dup\ndup\ndup\n";
    write(p.to_str().unwrap(), original);
    let diff = "<<<<<<< SEARCH\ndup\n=======\nunique\n>>>>>>> REPLACE\n";
    let err = apply_diff(p.to_str().unwrap(), diff).unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("3 places"), "msg: {msg}");
    assert_eq!(read_string(p.to_str().unwrap()), original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_malformed_fails() {
    let dir = temp_dir("diffmal");
    let p = dir.join("mal.txt");
    let original = "content\n";
    write(p.to_str().unwrap(), original);
    let err = apply_diff(p.to_str().unwrap(), "just some text").unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("No search/replace blocks"), "msg: {msg}");
    assert_eq!(read_string(p.to_str().unwrap()), original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_preserves_crlf() {
    let dir = temp_dir("diffcrlf");
    let p = dir.join("w.txt");
    fs::write(&p, "hello\r\nworld\r\n").unwrap();
    let diff = "<<<<<<< SEARCH\nhello\n=======\nHELLO\n>>>>>>> REPLACE\n";
    apply_diff(p.to_str().unwrap(), diff).unwrap();
    let bytes = fs::read(p.to_str().unwrap()).unwrap();
    assert!(bytes.windows(2).any(|w| w == b"\r\n"), "CRLF not preserved");
    assert!(bytes.starts_with(b"HELLO"), "replacement not applied");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_context_anchoring() {
    let dir = temp_dir("diffctx");
    let p = dir.join("ctx.txt");
    write(
        p.to_str().unwrap(),
        "fn foo() {\n    let x = 1;\n    let y = 2;\n}\nfn foo() {\n    let x = 1;\n    let y = 3;\n}\n",
    );
    let diff = "<<<<<<< SEARCH\n    let y = 3;\n=======\n    let y = 30;\n>>>>>>> REPLACE\n";
    apply_diff(p.to_str().unwrap(), diff).unwrap();
    let content = read_string(p.to_str().unwrap());
    assert!(content.contains("let y = 30;"), "content: {content}");
    assert!(content.contains("let y = 2;"), "content: {content}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_crlf_multiline() {
    // Multi-line SEARCH block (LF) against a CRLF file must match and
    // the file must stay CRLF after the edit.
    let dir = temp_dir("diffcrlfm");
    let p = dir.join("crlfm.txt");
    fs::write(&p, "alpha\r\nbeta\r\ngamma\r\n").unwrap();
    let diff = "<<<<<<< SEARCH\nalpha\nbeta\n=======\nALPHA\nBETA\n>>>>>>> REPLACE\n";
    let out = apply_diff(p.to_str().unwrap(), diff).unwrap();
    let json = success_json(out);
    assert_eq!(json["blocks_applied"].as_u64().unwrap(), 1);
    let content = read_string(p.to_str().unwrap());
    assert_eq!(
        content, "ALPHA\r\nBETA\r\ngamma\r\n",
        "content: {content:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_crlf_diff_payload() {
    // A diff payload with CRLF line endings must also work on a CRLF file.
    let dir = temp_dir("diffcrlfd");
    let p = dir.join("crlfd.txt");
    fs::write(&p, "one\r\ntwo\r\n").unwrap();
    let diff = "<<<<<<< SEARCH\r\none\r\ntwo\r\n=======\r\nONE\r\n>>>>>>> REPLACE\r\n";
    apply_diff(p.to_str().unwrap(), diff).unwrap();
    let content = read_string(p.to_str().unwrap());
    assert_eq!(content, "ONE\r\n", "content: {content:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_lf_file_stays_lf() {
    // Editing an LF file must not introduce CR bytes.
    let dir = temp_dir("difflf");
    let p = dir.join("lf.txt");
    fs::write(&p, "a\nb\nc\n").unwrap();
    let diff = "<<<<<<< SEARCH\na\nb\n=======\nA\nB\n>>>>>>> REPLACE\n";
    apply_diff(p.to_str().unwrap(), diff).unwrap();
    let bytes = fs::read(p.to_str().unwrap()).unwrap();
    assert_eq!(bytes, b"A\nB\nc\n", "LF file must not gain CR bytes");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_trailing_whitespace_fallback() {
    // File lines carry trailing spaces the model's SEARCH text lacks:
    // exact match fails, tolerant whole-line match applies (fuzzy_blocks=1).
    let dir = temp_dir("difffuzzy");
    let p = dir.join("f.txt");
    write(p.to_str().unwrap(), "fn main() {\n    let x = 5;   \n    let y = 3;\n}\n");
    // A single search line is a prefix of the file line (exact substring),
    // so include the next line: the 2-line block is NOT an exact substring
    // (file line 2 carries trailing spaces), but IS a unique tolerant match.
    let diff = "<<<<<<< SEARCH\n    let x = 5;\n    let y = 3;\n=======\n    let x = 6;\n    let y = 3;\n>>>>>>> REPLACE\n";
    let out = apply_diff(p.to_str().unwrap(), diff).unwrap();
    let json = success_json(out);
    assert_eq!(json["blocks_applied"].as_u64().unwrap(), 1);
    assert_eq!(json["fuzzy_blocks"].as_u64().unwrap(), 1);
    assert_eq!(
        read_string(p.to_str().unwrap()),
        "fn main() {\n    let x = 6;\n    let y = 3;\n}\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_tolerant_ambiguous_fails() {
    // "a"/"b" pairs occur twice (ignoring trailing spaces) -> ambiguous, file untouched.
    let dir = temp_dir("difffuzzyamb");
    let p = dir.join("fa.txt");
    let original = "a   \nb\t\na  \nb \n";
    write(p.to_str().unwrap(), original);
    let diff = "<<<<<<< SEARCH\na\nb\n=======\nX\n>>>>>>> REPLACE\n";
    let err = apply_diff(p.to_str().unwrap(), diff).unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("multiple places"), "msg: {msg}");
    assert!(msg.contains("1, 3"), "msg: {msg}");
    assert_eq!(read_string(p.to_str().unwrap()), original);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_not_found_shows_closest_region() {
    // Not-found error must point at the closest real region with line numbers.
    let dir = temp_dir("diffnf2");
    let p = dir.join("nf2.txt");
    write(p.to_str().unwrap(), "line one\nfn foo() {\n    body\n}\nline five\n");
    let diff = "<<<<<<< SEARCH\nfn foo() {\n    bodx\n=======\nfn foo() {}\n>>>>>>> REPLACE\n";
    let err = apply_diff(p.to_str().unwrap(), diff).unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("not found"), "msg: {msg}");
    assert!(msg.contains("2: fn foo() {"), "msg: {msg}");
    assert!(msg.contains("3:     body"), "msg: {msg}");
    // File untouched.
    assert!(read_string(p.to_str().unwrap()).contains("fn foo() {"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_ambiguous_shows_line_numbers() {
    let dir = temp_dir("diffamb2");
    let p = dir.join("amb2.txt");
    write(p.to_str().unwrap(), "dup\ndup\ndup\n");
    let diff = "<<<<<<< SEARCH\ndup\n=======\nunique\n>>>>>>> REPLACE\n";
    let err = apply_diff(p.to_str().unwrap(), diff).unwrap_err();
    let ToolError::Execution(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("3 places"), "msg: {msg}");
    assert!(msg.contains("lines 1, 2, 3"), "msg: {msg}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_fuzzy_replace_last_line_keeps_trailing_newline() {
    // SEARCH line has MORE trailing spaces than the file line: not an exact
    // substring, but a unique tolerant match -> applied, trailing \n kept.
    let dir = temp_dir("difffuzzylast");
    let p = dir.join("fl.txt");
    write(p.to_str().unwrap(), "a\nlast\n");
    let diff = "<<<<<<< SEARCH\nlast   \n=======\nLAST\n>>>>>>> REPLACE\n";
    apply_diff(p.to_str().unwrap(), diff).unwrap();
    assert_eq!(read_string(p.to_str().unwrap()), "a\nLAST\n");
    let _ = fs::remove_dir_all(&dir);
}
