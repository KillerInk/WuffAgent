use super::*;

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
    assert!(
        inner.exists(),
        "file must survive a failed non-recursive delete"
    );
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
    let err = move_item(
        dir.join("nope").to_str().unwrap(),
        dir.join("nope2").to_str().unwrap(),
    )
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
