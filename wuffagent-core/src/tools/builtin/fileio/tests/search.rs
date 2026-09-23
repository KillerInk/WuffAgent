use super::*;

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
    assert!(json["matches"][0].as_str().unwrap().ends_with("a.txt"));
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
