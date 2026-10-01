//! System font loading for egui: emoji + bold ("Strong") family.
//!
//! egui's default font stack has no emoji glyphs and no bold weight; this
//! loads the OS emoji font (Segoe UI Emoji on Windows, Apple Color Emoji on
//! macOS) at the FRONT of the proportional family so emoji code points fall
//! through to it, and the OS semibold font (Segoe UI Semibold on Windows,
//! Arial Bold on macOS) as the "Strong" family used for markdown bold runs
//! and headings. On other platforms (or if a file is missing) egui's
//! defaults are returned unchanged (Strong then falls back to the regular
//! proportional fonts).

use std::sync::Arc;

pub fn emoji_fonts() -> egui::FontDefinitions {
    let mut font_data = egui::FontDefinitions::default();
    // Load system emoji font for Windows
    #[cfg(target_os = "windows")]
    {
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\seguiemj.ttf") {
            font_data.font_data.insert(
                "emoji".to_string(),
                Arc::new(egui::FontData::from_owned(data)),
            );
            font_data
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "emoji".to_string());
        }
    }
    // Load system emoji font for macOS
    #[cfg(target_os = "macos")]
    {
        if let Ok(data) = std::fs::read("/System/Library/Fonts/Apple Color Emoji.ttc") {
            font_data.font_data.insert(
                "emoji".to_string(),
                Arc::new(egui::FontData::from_owned(data)),
            );
            font_data
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "emoji".to_string());
        }
    }
    // Bold/strong family for markdown: the OS semibold font first, then the
    // regular proportional fallbacks so glyphs are always resolved (a
    // missing bold file degrades to the normal weight, not to tofu).
    {
        let mut strong: Vec<String> = Vec::new();
        #[cfg(target_os = "windows")]
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\seguisb.ttf") {
            font_data.font_data.insert(
                "seguisb".to_string(),
                Arc::new(egui::FontData::from_owned(data)),
            );
            strong.push("seguisb".to_string());
        }
        #[cfg(target_os = "macos")]
        if let Ok(data) = std::fs::read("/System/Library/Fonts/Supplemental/Arial Bold.ttf") {
            font_data.font_data.insert(
                "arialbold".to_string(),
                Arc::new(egui::FontData::from_owned(data)),
            );
            strong.push("arialbold".to_string());
        }
        strong.extend(
            font_data
                .families
                .get(&egui::FontFamily::Proportional)
                .cloned()
                .unwrap_or_default(),
        );
        font_data
            .families
            .insert(egui::FontFamily::Name("Strong".into()), strong);
    }
    font_data
}
