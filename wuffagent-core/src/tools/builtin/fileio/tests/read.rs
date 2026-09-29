use super::*;

#[test]
fn test_read_file_full() {
    let dir = temp_dir("read");
    let p = dir.join("a.txt");
    write(p.to_str().unwrap(), "line1\nline2\nline3\n");
    let raw = success_str(read_file(p.to_str().unwrap(), None, None, false).unwrap());
    let path = p.to_str().unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines[0], &format!("[read_file {path}: lines 1-3 of 3]"));
    assert_eq!(&lines[1..], &["line1", "line2", "line3"]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_line_range() {
    let dir = temp_dir("range");
    let p = dir.join("r.txt");
    write(p.to_str().unwrap(), "a\nb\nc\nd\n");
    let raw = success_str(read_file(p.to_str().unwrap(), Some(2), Some(3), false).unwrap());
    let path = p.to_str().unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines[0], &format!("[read_file {path}: lines 2-3 of 4]"));
    assert_eq!(&lines[1..], &["b", "c"]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_line_numbers() {
    let dir = temp_dir("nums");
    let p = dir.join("n.txt");
    write(p.to_str().unwrap(), "hello\n");
    let raw = success_str(read_file(p.to_str().unwrap(), None, None, true).unwrap());
    let path = p.to_str().unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines[0], &format!("[read_file {path}: lines 1-1 of 1]"));
    assert_eq!(lines[1], "     1 | hello");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_truncates_large_file() {
    let dir = temp_dir("bigread");
    let p = dir.join("big.txt");
    // 1200 lines x 300 bytes = 360 KB > 256 KB cap.
    let line = "x".repeat(299) + "\n";
    write(p.to_str().unwrap(), &line.repeat(1200));
    let raw = success_str(read_file(p.to_str().unwrap(), None, None, false).unwrap());
    let header = raw.lines().next().unwrap();
    assert!(header.contains(" of 1200"), "header: {header}");
    assert!(header.contains("truncated"), "header: {header}");
    // Content (minus the header) must stay under the byte cap.
    let content_len = raw.len() - header.len() - 1;
    assert!(
        content_len < 256 * 1024 + 310,
        "content exceeds cap: {content_len}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_read_file_total_lines_with_range() {
    let dir = temp_dir("rangetotal");
    let p = dir.join("rt.txt");
    let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    write(p.to_str().unwrap(), &content);
    let raw = success_str(read_file(p.to_str().unwrap(), Some(10), Some(20), false).unwrap());
    let path = p.to_str().unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines[0], &format!("[read_file {path}: lines 10-20 of 100]"));
    assert_eq!(lines.len(), 12); // header + 11 lines
    assert_eq!(lines[1], "line 10");
    assert_eq!(lines[11], "line 20");
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
    let raw = success_str(read_file(p.to_str().unwrap(), None, None, false).unwrap());
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 4); // header + 3 lines
    assert_eq!(lines[1], "short");
    assert!(lines[2].ends_with('…'));
    assert!(lines[2].len() < 10_100);
    assert_eq!(lines[3], "tail");
    let _ = fs::remove_dir_all(&dir);
}
