use super::*;

// ─── Named tool wiring tests ───────────────────────────────────────────

#[test]
fn test_read_file_tool_executes() {
    let dir = temp_dir("toolread");
    let p = dir.join("t.txt");
    write(p.to_str().unwrap(), "abc\n");
    let tool = ReadFileTool::new();
    assert_eq!(tool.name(), "read_file");
    let mut prms = params(&[("path", p.to_str().unwrap())]);
    prms.values
        .insert("line_numbers".to_string(), serde_json::json!(false));
    let out = tool.execute(prms).unwrap();
    let json = success_json(out);
    assert_eq!(json["content"].as_str().unwrap(), "abc");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_write_file_tool_requires_content() {
    let tool = WriteFileTool::new();
    let err = tool.execute(params(&[("path", "x.txt")])).unwrap_err();
    let ToolError::InvalidParams(msg) = &err else {
        panic!("{err:?}")
    };
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
fn test_replace_lines_tool_executes() {
    let dir = temp_dir("toolrl");
    let p = dir.join("tr.txt");
    write(p.to_str().unwrap(), "one\ntwo\nthree\n");
    let tool = ReplaceLinesTool::new();
    assert_eq!(tool.name(), "replace_lines");
    let mut prms = ToolParams {
        values: std::collections::HashMap::new(),
    };
    prms.values.insert("path".to_string(), serde_json::json!(p.to_str().unwrap()));
    prms.values.insert("start_line".to_string(), serde_json::json!(2));
    prms.values.insert("end_line".to_string(), serde_json::json!(2));
    prms.values.insert("new_content".to_string(), serde_json::json!("TWO"));
    prms.values
        .insert("verify_contains".to_string(), serde_json::json!("two"));
    let out = tool.execute(prms).unwrap();
    let json = success_json(out);
    assert_eq!(json["lines_replaced"].as_u64().unwrap(), 1);
    assert_eq!(json["verified"].as_bool().unwrap(), true);
    assert_eq!(read_string(p.to_str().unwrap()), "one\nTWO\nthree\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_replace_lines_tool_missing_start_line() {
    let tool = ReplaceLinesTool::new();
    let err = tool
        .execute(params(&[("path", "x.txt"), ("end_line", "1"), ("new_content", "a")]))
        .unwrap_err();
    let ToolError::InvalidParams(msg) = &err else {
        panic!("{err:?}")
    };
    assert!(msg.contains("start_line"), "msg: {msg}");
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
