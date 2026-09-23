use super::*;

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
    write(
        p.to_str().unwrap(),
        &format!("short\n{}\ntail\n", "y".repeat(50_000)),
    );
    let out = read_file(p.to_str().unwrap(), None, None, false).unwrap();
    let json = success_json(out);
    assert!(!json["truncated"].as_bool().unwrap());
    let lines: Vec<&str> = json["content"].as_str().unwrap().lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[1].ends_with('…'));
    assert!(lines[1].len() < 10_100);
    let _ = fs::remove_dir_all(&dir);
}
