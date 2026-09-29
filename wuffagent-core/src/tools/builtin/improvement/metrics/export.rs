//! `read_metrics` 4c export (phase 4): writes the window's raw metric
//! lines to the export dir as JSON (array of raw lines) or CSV
//! (flattened, `agent` column in fleet mode).

use chrono::{Duration, Utc};
use crate::agents::metrics::MetricsLine;

use super::ReadMetricsTool;

impl ReadMetricsTool {
    /// 4c: writes the window's raw metric lines (ALL kinds — the same
    /// `days` window as the report, not the report's aggregates) to the
    /// export dir. JSON = an array of the raw lines (fleet mode injects an
    /// `agent` key into each line); CSV = flattened (`kind`,`ts` columns,
    /// `agent` after `ts` in fleet mode, then the union of all remaining
    /// fields in first-seen order — nested values such as a run's per-tool
    /// histogram stay compact JSON, missing fields become empty cells).
    /// Returns `(line_count, file_path)`.
    pub(crate) fn export_lines(
        &self,
        agent: Option<&str>,
        days: u64,
        format: &str,
    ) -> Result<(usize, std::path::PathBuf), String> {
        let log = self.log();
        let since = Utc::now() - Duration::days(days as i64);
        let mut rows: Vec<(Option<String>, MetricsLine)> = Vec::new();
        if let Some(name) = agent {
            // Agent mode: no agent column (the scope already says who).
            for line in log.read_all(name) {
                if line.ts() >= since {
                    rows.push((None, line));
                }
            }
        } else {
            // Fleet mode: every metrics file (agents + skills + fleet),
            // each line tagged with its file's agent name.
            for name in log.agent_names() {
                for line in log.read_all(&name) {
                    if line.ts() >= since {
                        rows.push((Some(name.clone()), line));
                    }
                }
            }
        }
        let dir = self.export_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating export dir: {e}"))?;
        let stem = match agent {
            Some(a) => format!("metrics-{a}"),
            None => "metrics-fleet".to_string(),
        };
        let ts = Utc::now().format("%Y%m%d-%H%M%S");
        let path = dir.join(format!("{stem}-{days}d-{ts}.{format}"));
        let values: Vec<serde_json::Value> = rows
            .iter()
            .map(|(a, line)| {
                let mut v = serde_json::to_value(line).expect("MetricsLine serializes");
                if let Some(a) = a {
                    if let Some(obj) = v.as_object_mut() {
                        // Overwrites the line's own `agent` field (Check/Eval)
                        // with the file stem — the same value the field
                        // mirrors, and fixes pre-default empty stems.
                        obj.insert("agent".into(), serde_json::Value::String(a.clone()));
                    }
                }
                v
            })
            .collect();
        let content = if format == "json" {
            serde_json::to_string_pretty(&values).map_err(|e| e.to_string())?
        } else {
            export_csv(&values)
        };
        std::fs::write(&path, content).map_err(|e| e.to_string())?;
        Ok((values.len(), path))
    }

    /// 4c: export directory — the test override, or `~/.wuffagent/exports`.
    fn export_dir(&self) -> std::path::PathBuf {
        self.export_dir
            .clone()
            .unwrap_or_else(|| crate::config::get_wuffagent_home().join("exports"))
    }
}

/// 4c: flattens serialized metric lines into CSV (see `export_lines`).
fn export_csv(values: &[serde_json::Value]) -> String {
    // Union of the remaining field names, in first-seen order.
    let mut rest: Vec<String> = Vec::new();
    for v in values {
        let Some(obj) = v.as_object() else {
            continue;
        };
        for k in obj.keys() {
            if k == "kind" || k == "ts" || k == "agent" {
                continue;
            }
            if !rest.iter().any(|r| r == k) {
                rest.push(k.clone());
            }
        }
    }
    let has_agent = values.iter().any(|v| v.get("agent").is_some());
    let mut header: Vec<String> = vec!["kind".to_string(), "ts".to_string()];
    if has_agent {
        header.push("agent".to_string());
    }
    for r in &rest {
        header.push(r.clone());
    }
    let mut out = header.join(",").to_string();
    out.push('\n');
    for v in values {
        let Some(obj) = v.as_object() else {
            continue;
        };
        let mut row: Vec<String> = vec![
            csv_cell(obj.get("kind").and_then(|v| v.as_str()).unwrap_or("")),
            csv_cell(obj.get("ts").and_then(|v| v.as_str()).unwrap_or("")),
        ];
        if has_agent {
            row.push(csv_cell(obj.get("agent").and_then(|v| v.as_str()).unwrap_or("")));
        }
        for r in &rest {
            match obj.get(r.as_str()) {
                Some(serde_json::Value::Null) | None => row.push(String::new()),
                Some(serde_json::Value::String(s)) => row.push(csv_cell(s)),
                Some(other) => row.push(csv_cell(&other.to_string())),
            }
        }
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

/// 4c: RFC-4180 quoting — quote fields containing a comma, quote, or
/// line break; double inner quotes.
fn csv_cell(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
