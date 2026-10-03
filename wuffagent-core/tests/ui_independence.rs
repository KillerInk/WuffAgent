//! Guards that `wuffagent-core` stays **independent of the UI layer**
//! (`wuffagent-egui`).
//!
//! The layering is one-way: `wuffagent-egui -> wuffagent-core`. Core must never
//! depend on `egui`/`eframe` — it models app state (`AppStatus`, `AppEvent`,
//! `ChatMessage`), the LLM client, tools, memory, sessions and the llama-server
//! process, and hands display concerns (colours, layout, widgets) to the UI.
//!
//! The compiler already rejects UI-crate paths in core without the dependency,
//! so these tests guard the ROOT CAUSE: a UI crate being added to
//! `wuffagent-core/Cargo.toml` (test 1), and any stray reference to the UI
//! namespace in core's own source (test 2). Either failure is caught at build
//! time instead of the coupling creeping in silently.

use std::path::Path;

/// Core's own manifest (this test lives in `wuffagent-core/tests/`, so
/// `CARGO_MANIFEST_DIR` is the `wuffagent-core/` directory).
const CORE_MANIFEST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");

/// UI crates that core must not depend on.
const UI_CRATES: &[&str] = &["egui", "eframe"];

#[test]
fn core_does_not_depend_on_ui_crates() {
    let toml = std::fs::read_to_string(CORE_MANIFEST)
        .expect("read wuffagent-core/Cargo.toml (CARGO_MANIFEST_DIR should point at wuffagent-core/)");

    let mut offenders: Vec<String> = Vec::new();
    for line in toml.lines() {
        let line = line.trim();
        // Skip blanks and comments. The manifest legitimately *mentions* "egui"
        // in a comment (the `image` format set matches the UI file picker), so
        // only non-comment lines are treated as dependencies.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // A dependency entry starts with the crate name followed by '=':
        //   egui = "0.36"          or   egui = { version = "0.36", ... }
        // This holds in any section ([dependencies], target.*.dependencies, ...).
        for ui_crate in UI_CRATES {
            let is_dep_entry = line
                .strip_prefix(ui_crate)
                .is_some_and(|rest| rest.trim_start().starts_with('='));
            if is_dep_entry {
                offenders.push(format!("{ui_crate}: `{line}`"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "wuffagent-core must stay independent of the UI layer (one-way egui -> core).\n\
         Found UI dependency(ies) in wuffagent-core/Cargo.toml:\n  {}",
        offenders.join("\n  ")
    );
}

/// Defense in depth: core's own source must never reference the `egui` or
/// `eframe` namespace, even if a UI dependency somehow sneaks past test 1.
/// Conservative substring scan (also matches comments and strings) over
/// `wuffagent-core/src/**/*.rs`.
fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read_dir core src") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn core_source_does_not_reference_ui_crates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = root.join("src");
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_rs_files(&src, &mut files);
    assert!(!files.is_empty(), "expected .rs files under wuffagent-core/src");

    let mut hits: Vec<String> = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read core .rs file");
        for (idx, line) in text.lines().enumerate() {
            if line.contains("egui::") || line.contains("eframe::") {
                let rel = file.strip_prefix(root).unwrap_or(file);
                hits.push(format!("{}:{}: {}", rel.display(), idx + 1, line.trim()));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "wuffagent-core source must not reference the UI crates (one-way egui -> core). Offenders:\n  {}",
        hits.join("\n  ")
    );
}
