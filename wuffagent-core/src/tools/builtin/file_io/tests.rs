//! Unit tests for the `file_io` module (see `super`).

use super::*;
use std::io::Read;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wuff_file_io_{}_{}", tag, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write(p: &str, s: &str) {
    fs::write(p, s).unwrap();
}

fn read_string(path: &str) -> String {
    let mut buf = String::new();
    let mut file = fs::File::open(path).unwrap();
    file.read_to_string(&mut buf).unwrap();
    buf
}

fn params(pairs: &[(&str, &str)]) -> ToolParams {
    let mut m = HashMap::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), serde_json::json!(v));
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
fn test_read_file_full() {
    let dir = temp_dir("read");
    let p = dir.join("a.txt");
    write(p.to_str().unwrap(), "line1\nline2\nline3\n");
    let out = read_file(p.to_str().unwrap(), None, None, false).unwrap();
    let json = success_json(out);
    let content = json["content"].as_str().unwrap();
    assert!(content.contains("line1") && content.contains("line3"));
    assert_eq!(json["total_lines"].as_u64().unwrap(), 3);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_line_range() {
    let dir = temp_dir("range");
    let p = dir.join("r.txt");
    write(p.to_str().unwrap(), "a\nb\nc\nd\n");
    let out = read_file(p.to_str().unwrap(), Some(2), Some(3), false).unwrap();
    let json = success_json(out);
    assert_eq!(json["content"].as_str().unwrap(), "b\nc");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_line_numbers() {
    let dir = temp_dir("nums");
    let p = dir.join("n.txt");
    write(p.to_str().unwrap(), "hello\n");
    let out = read_file(p.to_str().unwrap(), None, None, true).unwrap();
    let json = success_json(out);
    let content = json["content"].as_str().unwrap();
    assert!(content.contains("1 | hello"), "got: {content}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_truncates_large_file() {
    let dir = temp_dir("bigread");
    let p = dir.join("big.txt");
    // 1200 lines x 300 bytes = 360 KB > 256 KB cap.
    let line = "x".repeat(299) + "\n";
    write(p.to_str().unwrap(), &line.repeat(1200));
    let out = read_file(p.to_str().unwrap(), None, None, false).unwrap();
    let json = success_json(out);
    assert_eq!(json["total_lines"].as_u64().unwrap(), 1200);
    assert!(json["truncated"].as_bool().unwrap());
    assert!(json["lines_returned"].as_u64().unwrap() < 1200);
    let content = json["content"].as_str().unwrap();
    assert!(
        content.len() < 256 * 1024 + 310,
        "content exceeds cap: {}",
        content.len()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_total_lines_with_range() {
    let dir = temp_dir("rangetotal");
    let p = dir.join("rt.txt");
    let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    write(p.to_str().unwrap(), &content);
    let out = read_file(p.to_str().unwrap(), Some(10), Some(20), false).unwrap();
    let json = success_json(out);
    assert_eq!(json["total_lines"].as_u64().unwrap(), 100);
    assert_eq!(json["lines_returned"].as_u64().unwrap(), 11);
    assert!(!json["truncated"].as_bool().unwrap());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_long_line_trimmed() {
    let dir = temp_dir("longline");
    let p = dir.join("long.txt");
    write(p.to_str().unwrap(), &format!("short\n{}\ntail\n", "y".repeat(50_000)));
    let out = read_file(p.to_str().unwrap(), None, None, false).unwrap();
    let json = success_json(out);
    assert!(!json["truncated"].as_bool().unwrap());
    let lines: Vec<&str> = json["content"].as_str().unwrap().lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[1].ends_with('…'));
    assert!(lines[1].len() < 10_100);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_and_read_roundtrip() {
    let dir = temp_dir("rt");
    let p = dir.join("rt.txt");
    write_file(p.to_str().unwrap(), "data\nmore\n").unwrap();
    let out = read_file(p.to_str().unwrap(), None, None, false).unwrap();
    let json = success_json(out);
    assert_eq!(json["content"].as_str().unwrap(), "data\nmore");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_append_file() {
    let dir = temp_dir("app");
    let p = dir.join("app.txt");
    write_file(p.to_str().unwrap(), "one\n").unwrap();
    append_file(p.to_str().unwrap(), "two\n").unwrap();
    // read_file is line-based, so compare the raw file bytes for the
    // exact round-trip (including the trailing newline).
    assert_eq!(read_string(p.to_str().unwrap()), "one\ntwo\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_list_dir() {
    let dir = temp_dir("ls");
    let p = dir.join("x.txt");
    write(p.to_str().unwrap(), "x");
    let out = list_dir(dir.to_str().unwrap()).unwrap();
    let json = success_json(out);
    let entries = json["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"].as_str().unwrap(), "x.txt");
    assert_eq!(entries[0]["type"].as_str().unwrap(), "file");
    assert_eq!(entries[0]["size"].as_u64().unwrap(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_list_dir_dirs_first_and_symlinks() {
    let dir = temp_dir("lsorder");
    write(dir.join("b.txt").to_str().unwrap(), "bb");
    write(dir.join("a.txt").to_str().unwrap(), "aaaaa");
    fs::create_dir_all(dir.join("sub")).unwrap();
    // Best-effort symlink (requires privileges on Windows).
    let link = dir.join("link.txt");
    let made = {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("a.txt"), &link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(dir.join("a.txt"), &link).is_ok()
        }
    };
    let out = list_dir(dir.to_str().unwrap()).unwrap();
    let json = success_json(out);
    let entries = json["entries"].as_array().unwrap();
    // The directory must come first.
    assert_eq!(entries[0]["name"].as_str().unwrap(), "sub");
    assert_eq!(entries[0]["type"].as_str().unwrap(), "dir");
    assert!(entries[0].get("size").is_none());
    // Files follow, sorted by name, with sizes.
    let names: Vec<&str> = entries[1..].iter().map(|e| e["name"].as_str().unwrap()).collect();
    let mut sorted_names = names.clone();
    sorted_names.sort();
    assert_eq!(names, sorted_names);
    let a = entries.iter().find(|e| e["name"] == "a.txt").unwrap();
    assert_eq!(a["size"].as_u64().unwrap(), 5);
    if made {
        let l = entries.iter().find(|e| e["name"] == "link.txt").unwrap();
        assert_eq!(l["type"].as_str().unwrap(), "symlink");
        assert!(l.get("size").is_none());
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_file_creates_parent_dirs() {
    let dir = temp_dir("parent");
    let p = dir.join("a/b/c/deep.txt");
    let out = write_file(p.to_str().unwrap(), "nested").unwrap();
    let json = success_json(out);
    assert_eq!(json["bytes_written"].as_u64().unwrap(), 6);
    assert_eq!(read_string(p.to_str().unwrap()), "nested");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_file_replaces_existing_without_temp_leftovers() {
    let dir = temp_dir("atomic");
    let p = dir.join("f.txt");
    write_file(p.to_str().unwrap(), "version one\n").unwrap();
    write_file(p.to_str().unwrap(), "version two\n").unwrap();
    assert_eq!(read_string(p.to_str().unwrap()), "version two\n");
    // No .tmp. files may remain in the directory.
    let leftovers: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_files_glob() {
    let dir = temp_dir("glob");
    let p = dir.join("match.txt");
    write(p.to_str().unwrap(), "m");
    let pattern = dir.join("*").to_string_lossy().to_string();
    let out = search_files(&pattern, None).unwrap();
    let json = success_json(out);
    assert_eq!(json["count"].as_u64().unwrap(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_files_skips_git_and_target() {
    let dir = temp_dir("skipdirs");
    fs::create_dir_all(dir.join(".git")).unwrap();
    fs::create_dir_all(dir.join("target/debug")).unwrap();
    write(dir.join("a.txt").to_str().unwrap(), "a");
    write(dir.join(".git/config").to_str().unwrap(), "c");
    write(dir.join("target/debug/b.txt").to_str().unwrap(), "b");
    // Single level: .git and target themselves are filtered out.
    let pattern = dir.join("*").to_string_lossy().to_string();
    let out = search_files(&pattern, None).unwrap();
    let json = success_json(out);
    assert_eq!(json["count"].as_u64().unwrap(), 1);
    assert_eq!(json["skipped"].as_u64().unwrap(), 2);
    assert!(json["matches"][0]
        .as_str()
        .unwrap()
        .ends_with("a.txt"));
    // Recursive: nothing under .git or target may appear.
    let pattern2 = dir.join("**").to_string_lossy().to_string();
    let out2 = search_files(&pattern2, None).unwrap();
    let json2 = success_json(out2);
    for m in json2["matches"].as_array().unwrap() {
        let s = m.as_str().unwrap().replace('\\', "/");
        let comps: Vec<&str> = s.split('/').collect();
        assert!(!comps.contains(&".git"), "matched {s} under .git");
        assert!(!comps.contains(&"target"), "matched {s} under target");
    }
    assert!(json2["skipped"].as_u64().unwrap() >= 3);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_search_files_max_results() {
    let dir = temp_dir("cap");
    for i in 0..600 {
        write(dir.join(format!("f{i:03}.txt")).to_str().unwrap(), "x");
    }
    let pattern = dir.join("*").to_string_lossy().to_string();
    let out = search_files(&pattern, None).unwrap();
    let json = success_json(out);
    assert_eq!(json["count"].as_u64().unwrap(), 500);
    assert!(json["truncated"].as_bool().unwrap());
    let out = search_files(&pattern, Some(42)).unwrap();
    let json = success_json(out);
    assert_eq!(json["count"].as_u64().unwrap(), 42);
    assert!(json["truncated"].as_bool().unwrap());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_validate_path_rejects_sensitive() {
    assert!(validate_path("C:/Windows").is_err());
    assert!(validate_path("/etc/passwd").is_err());
    assert!(validate_path("../escape").is_err());
    assert!(validate_path("").is_err());
}

#[test]
fn test_validate_path_allows_normal() {
    let dir = temp_dir("ok");
    let p = dir.join("file.txt");
    write(p.to_str().unwrap(), "ok");
    assert!(validate_path(p.to_str().unwrap()).is_ok());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_apply_diff_single_block() {
    let dir = temp_dir("diff1");
    let p = dir.join("d.txt");
    write(p.to_str().unwrap(), "fn main() {\n    println!(\"old\");\n}\n");
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
    let ToolError::Execution(msg) = &err else { panic!("{err:?}") };
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
    let ToolError::Execution(msg) = &err else { panic!("{err:?}") };
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
    let ToolError::Execution(msg) = &err else { panic!("{err:?}") };
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
    assert_eq!(content, "ALPHA\r\nBETA\r\ngamma\r\n", "content: {content:?}");
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
fn test_mkdir_recursive() {
    let dir = temp_dir("mkdir");
    let nested = dir.join("a/b/c");
    mkdir(nested.to_str().unwrap(), true).unwrap();
    assert!(nested.is_dir());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_mkdir_nonrecursive_missing_parent_fails() {
    let dir = temp_dir("mkdirnr");
    let nested = dir.join("a/b");
    let err = mkdir(nested.to_str().unwrap(), false).unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_delete_file() {
    let dir = temp_dir("del");
    let p = dir.join("d.txt");
    write(p.to_str().unwrap(), "x");
    delete(p.to_str().unwrap(), false).unwrap();
    assert!(!p.exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_delete_dir_nonempty_guard_and_recursive() {
    let dir = temp_dir("deldir");
    let sub = dir.join("sub");
    fs::create_dir_all(&sub).unwrap();
    let inner = sub.join("in.txt");
    write(inner.to_str().unwrap(), "x");
    // Non-recursive delete of a non-empty dir must fail and keep contents.
    assert!(delete(sub.to_str().unwrap(), false).is_err());
    assert!(inner.exists(), "file must survive a failed non-recursive delete");
    delete(sub.to_str().unwrap(), true).unwrap();
    assert!(!sub.exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_copy_file_and_dir() {
    let dir = temp_dir("cp");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    let f = src.join("f.txt");
    write(f.to_str().unwrap(), "data");
    let nested = src.join("n");
    fs::create_dir_all(&nested).unwrap();
    write(nested.join("n.txt").to_str().unwrap(), "nested");
    let dest = dir.join("dest");
    copy(src.to_str().unwrap(), dest.to_str().unwrap()).unwrap();
    assert_eq!(fs::read_to_string(dest.join("f.txt")).unwrap(), "data");
    assert_eq!(fs::read_to_string(dest.join("n/n.txt")).unwrap(), "nested");
    // Plain file copy.
    let dest2 = dir.join("dest2.txt");
    copy(f.to_str().unwrap(), dest2.to_str().unwrap()).unwrap();
    assert_eq!(read_string(dest2.to_str().unwrap()), "data");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_copy_missing_source_fails() {
    let dir = temp_dir("cpnf");
    let err = copy(
        dir.join("nope").to_str().unwrap(),
        dir.join("nope2").to_str().unwrap(),
    )
    .unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_move_file_and_dir() {
    let dir = temp_dir("mv");
    let p = dir.join("m.txt");
    write(p.to_str().unwrap(), "x");
    let p2 = dir.join("m2.txt");
    move_item(p.to_str().unwrap(), p2.to_str().unwrap()).unwrap();
    assert!(!p.exists());
    assert_eq!(read_string(p2.to_str().unwrap()), "x");

    let d1 = dir.join("d1");
    fs::create_dir_all(&d1).unwrap();
    write(d1.join("x.txt").to_str().unwrap(), "y");
    let d2 = dir.join("d2");
    move_item(d1.to_str().unwrap(), d2.to_str().unwrap()).unwrap();
    assert!(!d1.exists());
    assert_eq!(fs::read_to_string(d2.join("x.txt")).unwrap(), "y");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_move_missing_source_fails() {
    let dir = temp_dir("mvnf");
    let err =
        move_item(dir.join("nope").to_str().unwrap(), dir.join("nope2").to_str().unwrap())
            .unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_info_file_and_dir() {
    let dir = temp_dir("info");
    let p = dir.join("i.txt");
    write(p.to_str().unwrap(), "12345");
    let json = success_json(file_info(p.to_str().unwrap()).unwrap());
    assert_eq!(json["size"].as_u64().unwrap(), 5);
    assert!(json["is_file"].as_bool().unwrap());
    assert!(!json["is_dir"].as_bool().unwrap());
    assert!(json["permissions"].as_str().is_some());
    let dir_json = success_json(file_info(dir.to_str().unwrap()).unwrap());
    assert!(dir_json["is_dir"].as_bool().unwrap());
    assert!(!dir_json["is_file"].as_bool().unwrap());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_file_info_missing_fails() {
    let dir = temp_dir("infonf");
    let err = file_info(dir.join("nope").to_str().unwrap()).unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
    let _ = fs::remove_dir_all(&dir);
}

// ─── Named tool wiring tests ───────────────────────────────────────────

#[test]
fn test_read_file_tool_executes() {
    let dir = temp_dir("toolread");
    let p = dir.join("t.txt");
    write(p.to_str().unwrap(), "abc\n");
    let tool = ReadFileTool::new();
    assert_eq!(tool.name(), "read_file");
    let mut prms = params(&[("path", p.to_str().unwrap())]);
    prms.values.insert("line_numbers".to_string(), serde_json::json!(false));
    let out = tool.execute(prms).unwrap();
    let json = success_json(out);
    assert_eq!(json["content"].as_str().unwrap(), "abc");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_file_tool_requires_content() {
    let tool = WriteFileTool::new();
    let err = tool.execute(params(&[("path", "x.txt")])).unwrap_err();
    let ToolError::InvalidParams(msg) = &err else { panic!("{err:?}") };
    assert!(msg.contains("content"));
}

#[test]
fn test_apply_diff_tool_executes() {
    let dir = temp_dir("tooldiff");
    let p = dir.join("td.txt");
    write(p.to_str().unwrap(), "before\n");
    let tool = ApplyDiffTool::new();
    assert_eq!(tool.name(), "apply_diff");
    let diff = "<<<<<<< SEARCH\nbefore\n=======\nafter\n>>>>>>> REPLACE\n";
    let out = tool
        .execute(params(&[("path", p.to_str().unwrap()), ("diff", diff)]))
        .unwrap();
    let json = success_json(out);
    assert_eq!(json["blocks_applied"].as_u64().unwrap(), 1);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_mkdir_tool_executes() {
    let dir = temp_dir("toolmkdir");
    let nested = dir.join("x/y");
    let tool = MkdirTool::new();
    assert_eq!(tool.name(), "mkdir");
    let out = tool
        .execute(params(&[("path", nested.to_str().unwrap())]))
        .unwrap();
    let json = success_json(out);
    assert!(json["created"].as_bool().unwrap());
    assert!(nested.is_dir());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_delete_tool_executes() {
    let dir = temp_dir("tooldel");
    let p = dir.join("t.txt");
    write(p.to_str().unwrap(), "x");
    let tool = DeleteTool::new();
    assert_eq!(tool.name(), "delete");
    let out = tool
        .execute(params(&[("path", p.to_str().unwrap())]))
        .unwrap();
    let json = success_json(out);
    assert!(json["deleted"].as_bool().unwrap());
    assert!(!p.exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_copy_move_fileinfo_tools_executes() {
    let dir = temp_dir("toolcmi");
    let p = dir.join("a.txt");
    write(p.to_str().unwrap(), "data");

    let cp = CopyTool::new();
    assert_eq!(cp.name(), "copy");
    let p2 = dir.join("b.txt");
    cp.execute(params(&[
        ("src", p.to_str().unwrap()),
        ("dest", p2.to_str().unwrap()),
    ]))
    .unwrap();
    assert_eq!(read_string(p2.to_str().unwrap()), "data");

    let mv = MoveTool::new();
    assert_eq!(mv.name(), "move");
    let p3 = dir.join("c.txt");
    mv.execute(params(&[
        ("src", p2.to_str().unwrap()),
        ("dest", p3.to_str().unwrap()),
    ]))
    .unwrap();
    assert!(!p2.exists());
    assert_eq!(read_string(p3.to_str().unwrap()), "data");

    let info = FileInfoTool::new();
    assert_eq!(info.name(), "file_info");
    let json = success_json(
        info.execute(params(&[("path", p.to_str().unwrap())]))
            .unwrap(),
    );
    assert_eq!(json["size"].as_u64().unwrap(), 4);
    assert!(json["is_file"].as_bool().unwrap());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_tool_names_unique() {
    let read = ReadFileTool::new();
    let write = WriteFileTool::new();
    let append = AppendFileTool::new();
    let list = ListDirTool::new();
    let search = SearchFilesTool::new();
    let diff = ApplyDiffTool::new();
    let mkdir = MkdirTool::new();
    let del = DeleteTool::new();
    let cp = CopyTool::new();
    let mv = MoveTool::new();
    let info = FileInfoTool::new();
    let names = vec![
        read.name(),
        write.name(),
        append.name(),
        list.name(),
        search.name(),
        diff.name(),
        mkdir.name(),
        del.name(),
        cp.name(),
        mv.name(),
        info.name(),
    ];
    let set: std::collections::HashSet<&str> = names.iter().copied().collect();
    assert_eq!(set.len(), 11, "tool names must be unique: {names:?}");
}


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
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"one\r\ntwo\r\nthree\r\n");
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
    assert_eq!(fs::read(p.to_str().unwrap()).unwrap(), b"one\r\ntwo\r\nthree\r\n");
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
