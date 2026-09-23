use super::*;

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
    let names: Vec<&str> = entries[1..]
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
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
