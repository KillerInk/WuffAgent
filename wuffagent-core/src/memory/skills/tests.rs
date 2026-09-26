//! Tests for the skill store (K1).

use super::*;
use std::path::PathBuf;

/// A unique temp dir per test (created on demand).
fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wuffagent_skills_test_{}_{}",
        std::process::id(),
        tag
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn test_save_read_roundtrip() {
    let store = SkillStore::new(temp_root("rt"));
    store
        .save("demo-skill", "What it does", "When to use it", "Step 1: do this\nStep 2: do that")
        .unwrap();
    let skill = store.read("demo-skill").expect("skill must exist");
    assert_eq!(skill.name, "demo-skill");
    assert_eq!(skill.description, "What it does");
    assert_eq!(skill.when_to_use, "When to use it");
    assert!(skill.body.contains("Step 1: do this"));
    assert!(skill.body.contains("Step 2: do that"));
    assert!(skill.modified_at.is_some(), "mtime must be stamped");
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_crud_save_list_read_delete() {
    let store = SkillStore::new(temp_root("crud"));
    store.save("alpha", "a desc", "a when", "alpha body").unwrap();
    store.save("beta", "b desc", "b when", "beta body").unwrap();

    let list = store.list();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].name, "alpha", "list must be sorted");
    assert_eq!(list[1].name, "beta");
    assert_eq!(list[0].description, "a desc");

    assert!(store.delete("alpha").unwrap(), "delete of existing → true");
    assert_eq!(store.list().len(), 1);
    assert!(store.read("alpha").is_none());
    assert!(!store.delete("alpha").unwrap(), "delete of missing → false");
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_save_overwrites_existing() {
    let store = SkillStore::new(temp_root("ow"));
    store.save("s", "v1", "when", "first version").unwrap();
    store.save("s", "v2", "when", "second version").unwrap();
    let skill = store.read("s").unwrap();
    assert_eq!(skill.description, "v2");
    assert!(skill.body.contains("second version"));
    assert!(!skill.body.contains("first version"));
    // No temp file left behind.
    assert!(!store.path("s").with_extension("md.tmp").exists());
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_invalid_names_rejected() {
    let store = SkillStore::new(temp_root("names"));
    let bad = ["", "1abc", "has space", "bad!", "-leading", "über"];
    for name in bad {
        let err = store.save(name, "d", "w", "body").unwrap_err();
        assert!(err.contains("name"), "name {name:?} must be rejected, got: {err}");
    }
    let long = "a".repeat(65);
    assert!(store.save(&long, "d", "w", "body").is_err());
    // Exactly 64 is fine.
    let max = "a".repeat(64);
    assert!(store.save(&max, "d", "w", "body").is_ok());
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_uppercase_name_normalized() {
    let store = SkillStore::new(temp_root("upper"));
    assert!(store.save("MY-COOL", "d", "w", "body").is_ok());
    assert!(store.read("my-cool").is_some(), "name must be lowercased");
    // A space is still invalid after normalization.
    assert!(store.save("My Skill", "d", "w", "body").is_err());
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_empty_body_rejected() {
    let store = SkillStore::new(temp_root("empty"));
    assert!(store.save("s", "d", "w", "   \n  ").is_err());
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_empty_store() {
    let store = SkillStore::new(temp_root("empty-store"));
    assert!(store.list().is_empty());
    assert_eq!(store.prompt_block(), "");
    assert!(store.read("nope").is_none());
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_handwritten_file_without_frontmatter() {
    let root = temp_root("no-fm");
    fs::write(root.join("loose.md"), "just a body, no frontmatter\n").unwrap();
    let store = SkillStore::new(root.clone());
    let skill = store.read("loose").expect("must parse body-only");
    assert!(skill.description.is_empty());
    assert!(skill.body.contains("just a body"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_corrupt_frontmatter_skipped() {
    let root = temp_root("corrupt");
    // Unterminated frontmatter → skipped by list(), read → None.
    fs::write(root.join("bad.md"), "---\nname: bad\ndescription: oops\n").unwrap();
    fs::write(
        root.join("good.md"),
        "---\nname: good\ndescription: fine\nwhen_to_use: now\n---\nbody\n",
    )
    .unwrap();
    // A non-.md file is ignored.
    fs::write(root.join("notes.txt"), "ignore me").unwrap();
    let store = SkillStore::new(root.clone());
    assert!(store.read("bad").is_none());
    let list = store.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "good");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_metadata_flattened_to_one_line() {
    let store = SkillStore::new(temp_root("flat"));
    store
        .save("s", "line1\nline2\ttabbed", "when  \n here", "body")
        .unwrap();
    let skill = store.read("s").unwrap();
    assert_eq!(skill.description, "line1 line2 tabbed");
    assert_eq!(skill.when_to_use, "when here");
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_prompt_block_content_and_cap() {
    let store = SkillStore::new(temp_root("block"));
    for i in 0..25 {
        store
            .save(&format!("skill-{i:02}"), &format!("desc {i}"), &format!("when {i}"), "body")
            .unwrap();
    }
    let block = store.prompt_block();
    assert!(block.starts_with("\n═══ SKILLS ═══"));
    assert!(block.contains("read_skill"));
    // Capped at 20: the first 20 (sorted) are present, the rest are not.
    assert!(block.contains("skill-00"));
    assert!(block.contains("skill-19"));
    assert!(!block.contains("skill-20"));
    assert!(!block.contains("skill-24"));
    // Lines carry when_to_use and description.
    assert!(block.contains("skill-00 — use: when 0: desc 0"));
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_default_store_honors_testing_override() {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = temp_root("override");
    set_skills_dir_for_testing(Some(dir.clone()));
    let store = SkillStore::default();
    assert_eq!(store.root(), dir.as_path());
    store.save("via-default", "d", "w", "body").unwrap();
    let block = build_skills_prompt_block();
    assert!(block.contains("via-default"));
    set_skills_dir_for_testing(None);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_validate_skill_name() {
    assert!(validate_skill_name("ok-name1").is_ok());
    assert!(validate_skill_name("a").is_ok());
    assert!(validate_skill_name("").is_err());
    assert!(validate_skill_name("9lives").is_err());
    assert!(validate_skill_name("has space").is_err());
    assert!(validate_skill_name("under_score").is_err());
}

// ── 3c: version history ─────────────────────────────────────────────────────

#[test]
fn test_save_snapshots_previous_version() {
    let store = SkillStore::new(temp_root("hist-save"));
    store.save("s", "v1", "when", "first version").unwrap();
    assert!(store.list_skill_history("s").is_empty(), "first save: nothing to snapshot");
    store.save("s", "v2", "when", "second version").unwrap();
    let list = store.list_skill_history("s");
    assert_eq!(list.len(), 1, "overwriting v2 snapshots v1");
    assert_eq!(list[0].extension().map(|e| e == "md"), Some(true));
    store.save("s", "v3", "when", "third version").unwrap();
    let list = store.list_skill_history("s");
    assert_eq!(list.len(), 2, "overwriting v3 snapshots v2");
    // Newest first: [0] holds the v2 content (snapshotted last).
    let newest = fs::read_to_string(&list[0]).unwrap();
    assert!(newest.contains("second version"));
    assert!(store.read("s").unwrap().body.contains("third version"));
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_first_save_no_snapshot() {
    let store = SkillStore::new(temp_root("hist-first"));
    store.save("s", "v1", "when", "body").unwrap();
    assert!(
        store.list_skill_history("s").is_empty(),
        "a first save has nothing to snapshot"
    );
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_revert_skill_restores_and_stays_reversible() {
    let store = SkillStore::new(temp_root("hist-revert"));
    store.save("s", "v1", "when", "first version").unwrap();
    store.save("s", "v2", "when", "second version").unwrap();
    store.save("s", "v3", "when", "third version").unwrap();
    let hist = store.list_skill_history("s");
    assert_eq!(hist.len(), 2); // [v2, v1] newest first
    // Revert to the OLDEST snapshot (v1).
    let restored = store.revert_skill("s", &hist[1]).expect("revert to v1");
    assert!(restored.body.contains("first version"));
    assert!(store.read("s").unwrap().body.contains("first version"));
    // The revert itself snapshotted the pre-revert (v3) state, so history
    // grew by one and the newest entry IS v3 (forward again is possible).
    let hist2 = store.list_skill_history("s");
    assert_eq!(hist2.len(), 3);
    let newest = store.revert_skill("s", &hist2[0]).expect("revert to v3");
    assert!(newest.body.contains("third version"));
    assert!(store.read("s").unwrap().body.contains("third version"));
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_delete_snapshots_so_retired_skill_restores() {
    let store = SkillStore::new(temp_root("hist-delete"));
    store.save("s", "v1", "when", "body").unwrap();
    assert!(store.delete("s").unwrap());
    assert!(store.read("s").is_none());
    let hist = store.list_skill_history("s");
    assert_eq!(hist.len(), 1, "delete snapshots the file before removal");
    let restored = store.revert_skill("s", &hist[0]).expect("restore retired skill");
    assert!(restored.body.contains("body"));
    assert!(store.read("s").is_some(), "skill is back");
    // Delete it again (the restore made it exist), then a delete of the now
    // missing skill is a plain false and snapshots nothing new.
    assert!(store.delete("s").unwrap());
    let before = store.list_skill_history("s").len();
    assert!(!store.delete("s").unwrap());
    assert_eq!(store.list_skill_history("s").len(), before, "missing delete snapshots nothing");
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_list_skill_history_newest_first_and_unknown_empty() {
    let store = SkillStore::new(temp_root("hist-order"));
    store.save("s", "v1", "when", "one").unwrap();
    store.save("s", "v2", "when", "two").unwrap();
    store.save("s", "v3", "when", "three").unwrap();
    let hist = store.list_skill_history("s");
    assert_eq!(hist.len(), 2);
    // Newest first: the last-written snapshot (containing v2) is [0].
    let newest_content = fs::read_to_string(&hist[0]).unwrap();
    assert!(newest_content.contains("two"));
    let oldest_content = fs::read_to_string(&hist[1]).unwrap();
    assert!(oldest_content.contains("one"));
    // Another skill's snapshots are not mixed in.
    store.save("t", "v1", "when", "other").unwrap();
    store.save("t", "v2", "when", "other2").unwrap();
    assert_eq!(store.list_skill_history("s").len(), 2);
    assert_eq!(store.list_skill_history("t").len(), 1);
    assert!(store.list_skill_history("ghost").is_empty());
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_revert_skill_rejects_bad_snapshots() {
    let store = SkillStore::new(temp_root("hist-validate"));
    store.save("s", "v1", "when", "one").unwrap();
    store.save("s", "v2", "when", "two").unwrap();
    // A snapshot of another skill is rejected (name mismatch).
    store.save("t", "v1", "when", "other").unwrap();
    store.save("t", "v2", "when", "other2").unwrap();
    let other = store.list_skill_history("t").pop().unwrap();
    let err = store.revert_skill("s", &other).unwrap_err();
    assert!(err.contains("does not belong"), "got: {err}");
    // A file outside the history dir is rejected.
    let err = store.revert_skill("s", &store.path("s")).unwrap_err();
    assert!(err.contains("not under the history directory"), "got: {err}");
    // A missing file is rejected.
    let missing = store.history_dir().join("s-1.md");
    let err = store.revert_skill("s", &missing).unwrap_err();
    assert!(err.contains("does not exist"), "got: {err}");
    assert_eq!(
        store.list_skill_history("s").len(),
        1,
        "failed reverts must not snapshot"
    );
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_history_prunes_to_keep_cap() {
    let store = SkillStore::new(temp_root("hist-prune"));
    // HISTORY_KEEP + 5 overwrites → the 5 oldest snapshots are pruned.
    for i in 0..(HISTORY_KEEP + 5) {
        store.save("s", &format!("v{i}"), "when", &format!("body {i}")).unwrap();
    }
    let hist = store.list_skill_history("s");
    assert_eq!(hist.len(), HISTORY_KEEP, "oldest snapshots are pruned");
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn test_history_dir_not_listed_as_skill() {
    let store = SkillStore::new(temp_root("hist-dir"));
    store.save("s", "v1", "when", "one").unwrap();
    store.save("s", "v2", "when", "two").unwrap();
    // The history/ subdir (and its files) must not appear in list().
    let list = store.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "s");
    let _ = fs::remove_dir_all(store.root());
}
