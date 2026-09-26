//! System emoji font loading for egui.
//!
//! egui's default font stack has no emoji glyphs; this loads the OS emoji
//! font (Segoe UI Emoji on Windows, Apple Color Emoji on macOS) and puts it
//! at the FRONT of the proportional family so emoji code points fall through
//! to it. On other platforms (or if the file is missing) egui's defaults are
//! returned unchanged.

use std::sync::Arc;

pub fn emoji_fonts() -> egui::FontDefinitions {
    let mut font_data = egui::FontDefinitions::default();
    // Load system emoji font for Windows
    #[cfg(target_os = "windows")]
    {
        if let Ok(data) = std::fs::read("C:\\Windows\\Fonts\\seguiemj.ttf") {
            font_data
                .font_data
                .insert("emoji".to_string(), Arc::new(egui::FontData::from_owned(data)));
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
            font_data
                .font_data
                .insert("emoji".to_string(), Arc::new(egui::FontData::from_owned(data)));
            font_data
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .insert(0, "emoji".to_string());
        }
    }
    font_data
}
