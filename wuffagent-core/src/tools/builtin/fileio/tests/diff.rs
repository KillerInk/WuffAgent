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
