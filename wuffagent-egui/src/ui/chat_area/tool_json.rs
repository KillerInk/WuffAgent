//! JSON tool-result rendering (split out of `tool_cards.rs`): smart
//! field extraction for shell/file/search results, key-value lists, and the
//! JSON → display string conversion.

use eframe::egui;

use crate::ui::state::ChatApp;
use crate::ui::theme::Theme;

impl ChatApp {
    /// Render a tool result that is valid JSON with smart field extraction.
    /// Called only with the tool card already expanded, so long content
    /// (e.g. file reads) is shown directly in a height-capped scroll area.
    pub(super) fn draw_tool_json_result(&self, ui: &mut egui::Ui, json: &serde_json::Value, raw: &str, theme: &Theme) {
        // Shell output: {exit_code, stdout, stderr, duration_ms, truncated} —
        // render a status line plus stdout/stderr as line-by-line code blocks
        // (one giant wrapped label is unreadable for command output).
        if json.get("exit_code").is_some() {
            let exit_code = json.get("exit_code").and_then(|v| v.as_u64()).unwrap_or(0);
            let dur = json.get("duration_ms").and_then(|v| v.as_u64());
            let truncated = json.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false);
            ui.horizontal(|ui| {
                if exit_code == 0 {
                    ui.colored_label(theme.success, "✓");
                } else {
                    ui.colored_label(theme.warning, "✗");
                }
                ui.label(egui::RichText::new(format!("exit code {}", exit_code))
                    .color(if exit_code == 0 { theme.text_dim } else { theme.warning })
                    .size(11.0));
                if let Some(d) = dur {
                    ui.label(egui::RichText::new(format!("· {}", Self::format_duration(d)))
                        .color(theme.text_dim)
                        .size(10.5));
                }
                if truncated {
                    ui.label(egui::RichText::new("· output truncated")
                        .color(theme.text_dim)
                        .size(10.5));
                }
            });
            let stream_block = |ui: &mut egui::Ui, title: &str, value: &str, color: egui::Color32| {
                if value.trim().is_empty() {
                    return;
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new(title)
                    .color(theme.text_dim)
                    .size(10.0)
                    .monospace());
                ui.add_space(2.0);
                Self::code_block(ui, theme, |ui| {
                    let lines: Vec<&str> = value.lines().collect();
                    const MAX_LINES: usize = 300;
                    ui.vertical(|ui| {
                        for line in &lines[..lines.len().min(MAX_LINES)] {
                            ui.add(Self::breaking_label(
                                line,
                                egui::FontId::monospace(11.5),
                                color,
                                false,
                            ));
                        }
                        if lines.len() > MAX_LINES {
                            ui.add_space(2.0);
                            ui.label(egui::RichText::new(format!(
                                "… ({} more lines)",
                                lines.len() - MAX_LINES
                            ))
                            .color(theme.text_dim)
                            .size(10.5));
                        }
                    });
                });
            };
            stream_block(
                ui,
                "stdout",
                json.get("stdout").and_then(|v| v.as_str()).unwrap_or(""),
                theme.code_text,
            );
            stream_block(
                ui,
                "stderr",
                json.get("stderr").and_then(|v| v.as_str()).unwrap_or(""),
                theme.warning,
            );
            return;
        }
        // Check for common structured patterns
        if let Some(path) = json.get("path").and_then(|v| v.as_str()) {
            // Has a path field — likely a file operation result
            let is_file_read = json.get("content").is_some();
            let is_file_write = json.get("bytes_written").is_some() || json.get("success").is_some();
            let is_dir_list = json.get("entries").is_some();

            if is_dir_list {
                // Directory listing: show path badge + entries list
                self.draw_tool_path_badge(ui, path, theme);
                ui.add_space(4.0);
                if let Some(entries) = json.get("entries").and_then(|v| v.as_array()) {
                    let max_entries = 50;
                    let display_entries: Vec<&serde_json::Value> = entries.iter().take(max_entries).collect();
                    Self::code_block(ui, theme, |ui| {
                            for entry in display_entries {
                                // New list_dir returns {name, type, size} objects;
                                // older sessions stored plain strings.
                                let line = if let Some(s) = entry.as_str() {
                                    s.to_string()
                                } else {
                                    let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                    let typ = entry.get("type").and_then(|v| v.as_str()).unwrap_or("file");
                                    let size = entry.get("size").and_then(|v| v.as_u64())
                                        .map(|n| format!("  {} bytes", n))
                                        .unwrap_or_default();
                                    format!("{}  [{}{}]", name, typ, size)
                                };
                                ui.add(Self::breaking_label(
                                    line,
                                    egui::FontId::monospace(13.0),
                                    theme.code_text,
                                    false,
                                ));
                            }
                            if entries.len() > max_entries {
                                ui.label(egui::RichText::new(format!("... and {} more entries", entries.len() - max_entries))
                                    .color(theme.text_dim)
                                    .size(10.0));
                            }
                        });
                }
            } else if is_file_read {
                // File read: path badge + content in a height-capped scroll area
                // (the surrounding tool card already controls expand/collapse).
                self.draw_tool_path_badge(ui, path, theme);
                ui.add_space(4.0);
                if let Some(content) = json.get("content").and_then(|v| v.as_str()) {
                    Self::code_block(ui, theme, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                ui.add(Self::breaking_label(
                                    content,
                                    egui::FontId::monospace(12.5),
                                    theme.code_text,
                                    false,
                                ));
                            });
                    });
                }
            } else if is_file_write {
                // File write: show compact success badge
                self.draw_tool_path_badge(ui, path, theme);
                ui.add_space(4.0);
                if let Some(bytes) = json.get("bytes_written").and_then(|v| v.as_u64()) {
                    ui.horizontal(|ui| {
                        ui.colored_label(theme.success, format!("✓ Written {} byte{}", bytes, if bytes == 1 { "" } else { "s" }));
                    });
                } else if let Some(success) = json.get("success").and_then(|v| v.as_bool()) {
                    if success {
                        ui.colored_label(theme.success, "✓ File written successfully");
                    }
                }
            } else {
                // Generic JSON with path — render as structured key-value
                self.draw_tool_path_badge(ui, path, theme);
                ui.add_space(4.0);
                self.draw_tool_json_kv(ui, json, theme);
            }
        } else {
            // No path field — check for other common patterns
            if let Some(expr) = json.get("expression").and_then(|v| v.as_str()) {
                if let Some(result) = json.get("result").and_then(|v| v.as_f64()) {
                    // Calculation result
                    // The expression wraps at the block width; the result goes
                    // on its own line so a long expression cannot push it past
                    // the right edge.
                    ui.add(Self::breaking_label(
                        expr,
                        egui::FontId::monospace(13.0),
                        theme.text_secondary,
                        false,
                    ));
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("= ").color(theme.text_dim));
                        ui.colored_label(theme.success, format!("{}", result));
                    });
                } else {
                    self.draw_tool_json_kv(ui, json, theme);
                }
            } else if json.is_object() {
                // Generic JSON object — render as key-value pairs
                self.draw_tool_json_kv(ui, json, theme);
            } else {
                // Array or scalar — fall back to plain rendering
                self.draw_tool_plain_result(ui, raw, theme);
            }
        }
    }

    /// Render JSON as a key-value list.
    fn draw_tool_json_kv(&self, ui: &mut egui::Ui, json: &serde_json::Value, theme: &Theme) {
        Self::code_block(ui, theme, |ui| {
                match json {
                    serde_json::Value::Object(map) => {
                        for (key, value) in map {
                            ui.horizontal(|ui| {
                                ui.add(Self::breaking_label(
                                    format!("{}:", key),
                                    egui::FontId::monospace(11.0),
                                    theme.text_secondary,
                                    false,
                                ));
                                let val_str = Self::json_value_to_string(value);
                                ui.add(Self::breaking_label(
                                    val_str,
                                    egui::FontId::monospace(11.0),
                                    theme.code_text,
                                    false,
                                ));
                            });
                        }
                    }
                    serde_json::Value::Array(arr) => {
                        for item in arr {
                            ui.add(Self::breaking_label(
                                Self::json_value_to_string(item),
                                egui::FontId::monospace(11.0),
                                theme.code_text,
                                false,
                            ));
                        }
                    }
                    other => {
                        ui.add(Self::breaking_label(
                            Self::json_value_to_string(other),
                            egui::FontId::monospace(11.0),
                            theme.code_text,
                            false,
                        ));
                    }
                }
            });
    }

    /// Convert a JSON value to a display string.
    fn json_value_to_string(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => "null".to_string(),
            serde_json::Value::Array(arr) => {
                let items: Vec<String> = arr.iter().map(Self::json_value_to_string).collect();
                format!("[{}]", items.join(", "))
            }
            serde_json::Value::Object(map) => {
                let pairs: Vec<String> = map.iter()
                    .map(|(k, v)| format!("{}: {}", k, Self::json_value_to_string(v)))
                    .collect();
                format!("{{{}}}", pairs.join(", "))
            }
        }
    }
}
