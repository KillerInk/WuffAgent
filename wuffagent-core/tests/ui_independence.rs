//! Guards that `wuffagent-core` stays **independent of the UI layer**
//! (`wuffagent-egui`).
//!
//! The layering is one-way: `wuffagent-egui -> wuffagent-core`. Core must never
//! depend on `egui`/`eframe` — it models app state (`AppStatus`, `AppEvent`,
//! `ChatMessage`), the LLM client, tools, memory, sessions and the llama-server
//! process, and hands display concerns (colours, layout, widgets) to the UI.
//!
//! The compiler already rejects any `egui::` / `eframe::` path in core without
//! the dependency, so this test guards the ROOT CAUSE: a UI crate being added
//! to `wuffagent-core/Cargo.toml`. The moment someone adds `egui` (or `eframe`)
//! as a dependency here, this test fails and the layering violation is caught
//! at build time instead of creeping in silently.

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
