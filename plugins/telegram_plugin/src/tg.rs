//! Minimal blocking Telegram Bot API client (getUpdates / sendMessage /
//! sendChatAction) + reply chunking.
//!
//! The reqwest `Client` is built ONCE per bot start: a per-request client
//! re-does TLS setup and connection pooling on every call (the
//! llama-integration P1 lesson — keep long-lived clients long-lived).

use serde_json::{json, Value};
use std::sync::atomic::{Ordering as AtomicOrdering};
use std::time::Duration;

/// Error classes the poller reacts to differently.
#[derive(Debug)]
pub enum TgError {
    /// 401 — wrong token; the bot stops itself.
    BadToken,
    /// 409 — another long-poller is active (two WuffAgent builds); back off
    /// 30 s and retry.
    Conflict,
    /// 5xx / network / parse — retry after a short backoff.
    Retry(String),
    /// Other 4xx on send (message too long after chunking, chat not found…)
    /// — reported, not retried.
    Send(String),
}

impl TgError {
    pub fn to_string(&self) -> String {
        match self {
            TgError::BadToken => "401: bad token".into(),
            TgError::Conflict => "409: another poller is active".into(),
            TgError::Retry(s) => s.clone(),
            TgError::Send(s) => s.clone(),
        }
    }
}

/// One parsed `message` update (text messages only — `allowed_updates`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub id: i64,
    pub chat_id: i64,
    pub text: String,
    pub from_first_name: String,
}

pub struct TelegramApi {
    client: reqwest::blocking::Client,
    base: String,
    token: String,
    /// The host vtable (kept here so `stop()` can detach the pipeline event
    /// callback without going back through the process-global `host_api()`).
    pub host: Option<&'static wuffagent_core::tools::types::HostApi>,
    /// Set by the e2e test: log every `getUpdates` request (the test's fake
    /// server responds with `Connection: close`, so a response that never
    /// arrives is visible here as a request with no follow-up body).
    #[allow(dead_code)]
    pub debug_get_updates: Option<&'static std::sync::atomic::AtomicBool>,
}

impl TelegramApi {
    pub fn new(token: &str, api_base: Option<&str>, poll_timeout_secs: u64) -> Self {
        let base = api_base
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|| "https://api.telegram.org".into());
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(poll_timeout_secs + 15))
                .build()
                .expect("reqwest client build"),
            base,
            token: token.to_string(),
            host: None,
            debug_get_updates: None,
        }
    }

    /// Build with an explicit client (tests inject a pool-less client).
    pub fn with_client(client: reqwest::blocking::Client, base: String, token: &str) -> Self {
        Self {
            client,
            base,
            token: token.to_string(),
            host: None,
            debug_get_updates: None,
        }
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base, self.token, method)
    }

    /// Long-poll `getUpdates`; `timeout` is the hold (seconds).
    pub fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Update>, TgError> {
        // (Debug hook: `debug_get_updates` — the e2e test sets it to trace
        // every poll; the test's fake server responds with `Connection:
        // close`, so a request with no follow-up body is the smoking gun for
        // a stuck poll.)
        if let Some(d) = &self.debug_get_updates {
            if d.load(AtomicOrdering::Relaxed) {
                eprintln!("[tg] getUpdates offset={offset} hold={timeout_secs}s");
            }
        }
        // Per-request timeout override: `timeout_secs` is the server-side
        // hold, but the CLIENT must not hang on a connection that never
        // completes (e.g. a `connect` that is accepted and never answered) —
        // the reqwest builder timeout only covers the normal request
        // lifetime, not a stuck connect.
        let resp = self
            .client
            .get(self.url("getUpdates"))
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", timeout_secs.to_string()),
                ("allowed_updates", "[\"message\"]".to_string()),
            ])
            .timeout(std::time::Duration::from_secs(timeout_secs + 10))
            .send()
            .map_err(|e| TgError::Retry(e.to_string()))?;
        // NOTE: no debug logging between `.send()` and the body drain here —
        // a log line in that gap would itself be the hang (the test's fake
        // server responds with `Connection: close` and reqwest reads the
        // whole body before `.send()` returns; the drain is the only safe
        // spot to observe).
        // Drain the body into a string first (same reason as
        // `send_message`): the body must be fully read before the
        // connection is handed back to the pool.
        let status = resp.status().as_u16();
        let body_text = resp
            .text()
            .map_err(|e| TgError::Retry(format!("getUpdates body: {e}")))?;
        let body: Value = serde_json::from_str(&body_text)
            .map_err(|e| TgError::Retry(format!("getUpdates body: {e}")))?;
        match status {
            200..=299 => {}
            401 => return Err(TgError::BadToken),
            409 => return Err(TgError::Conflict),
            _ => return Err(TgError::Retry(format!("getUpdates HTTP {status}"))),
        }
        let result = body
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for u in result {
            let Some(msg) = u.get("message") else { continue };
            let Some(chat) = msg.get("chat") else { continue };
            let Some(chat_id) = chat.get("id").and_then(Value::as_i64) else { continue };
            let id = u.get("update_id").and_then(Value::as_i64).unwrap_or(0);
            let text = msg
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let from_first_name = msg
                .get("from")
                .and_then(|f| f.get("first_name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            out.push(Update {
                id,
                chat_id,
                text,
                from_first_name,
            });
        }
        Ok(out)
    }

    /// Plain-text `sendMessage` (no HTML/MarkdownV2, preview disabled).
    pub fn send_message(&self, chat_id: i64, text: &str) -> Result<(), TgError> {
        self.send_message_timeout(chat_id, text, std::time::Duration::from_secs(15))
    }

    fn send_message_timeout(
        &self,
        chat_id: i64,
        text: &str,
        timeout: std::time::Duration,
    ) -> Result<(), TgError> {
        let resp = self
            .client
            .post(self.url("sendMessage"))
            .json(&json!({
                "chat_id": chat_id,
                "text": text,
                "disable_web_page_preview": true,
            }))
            .timeout(timeout)
            .send()
            .map_err(|e| TgError::Retry(e.to_string()))?;
        // Drain the response body into a STRING (not `resp.json()`): the
        // body must be fully read before the connection is handed back to
        // the pool, otherwise a pooled connection can carry a half-read body
        // into the next request and the response bytes get interleaved —
        // which makes the follow-up `.json()` parse fail (the body is
        // consumed by the NEXT response) and, worse, can deadlock on a
        // connection that is never closed.
        // Drain the body BEFORE reading the status — `Response::text` takes
        // `self` by value, so the status must be captured first.
        let status = resp.status().as_u16();
        let body_text = resp
            .text()
            .map_err(|e| TgError::Retry(format!("sendMessage body: {e}")))?;
        let body: Value = serde_json::from_str(&body_text)
            .map_err(|e| TgError::Retry(format!("sendMessage body: {e}")))?;
        match status {
            200..=299 => Ok(()),
            401 => Err(TgError::BadToken),
            429 => Err(TgError::Retry("429: rate limited".into())),
            s => Err(TgError::Send(format!(
                "HTTP {s}: {}",
                body
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("no description")
            ))),
        }
    }

    /// Best-effort "typing…" indicator (shown up to 10 s); errors ignored.
    /// The explicit per-request timeout is the same reason as in
    /// [`Self::get_updates`]: the builder timeout does not cover a stuck
    /// connect, and this runs on the WORKER thread — a hang would stall
    /// every subsequent update/event for the bot's whole lifetime.
    pub fn send_chat_action(&self, chat_id: i64) {
        let _ = self
            .client
            .post(self.url("sendChatAction"))
            .json(&json!({ "chat_id": chat_id, "action": "typing" }))
            .timeout(std::time::Duration::from_secs(10))
            .send();
    }
}

/// Split a reply into chunks of at most `max_chars` characters:
/// 1. split on newlines (CRLF kept intact inside a line),
/// 2. pack lines greedily into chunks,
/// 3. hard-split (at char boundaries) any single line longer than the max.
///
/// Empty input -> one empty chunk.
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let max = max_chars.max(1);
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in text.split_inclusive('\n') {
        let pieces: Vec<String> = if line.chars().count() > max {
            let mut out = Vec::new();
            let mut piece = String::new();
            for ch in line.chars() {
                if piece.chars().count() == max {
                    out.push(std::mem::take(&mut piece));
                }
                piece.push(ch);
            }
            if !piece.is_empty() {
                out.push(piece);
            }
            out
        } else {
            vec![line.to_string()]
        };
        for piece in pieces {
            if current.chars().count() + piece.chars().count() > max {
                if !current.is_empty() {
                    chunks.push(std::mem::take(&mut current));
                }
                current = piece; // always <= max after the hard split
            } else {
                current.push_str(&piece);
            }
        }
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Next `getUpdates` offset after receiving `updates` (max update_id + 1,
/// or `prev` when the batch was empty).
pub fn next_offset(updates: &[Update], prev: i64) -> i64 {
    updates
        .iter()
        .map(|u| u.id)
        .max()
        .map(|m| m + 1)
        .unwrap_or(prev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_boundaries() {
        // Exactly at the limit -> one chunk.
        let s = "a".repeat(4096);
        let c = chunk_text(&s, 4096);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].chars().count(), 4096);

        // Four over the limit -> two chunks, total preserved.
        let s = "a".repeat(4100);
        let c = chunk_text(&s, 4096);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].chars().count(), 4096);
        assert_eq!(c[1].chars().count(), 4);
        assert_eq!(c.concat(), s);
    }

    #[test]
    fn test_chunk_crlf_lines_stay_together() {
        // CRLF lines: the \r stays glued to its line, lines pack greedily.
        let s = "hello\r\nworld\r\n";
        let c = chunk_text(&s, 4096);
        assert_eq!(c, vec![s.to_string()]);

        // A line pair that only fits together at the boundary.
        let a = "x".repeat(2000);
        let b = "y".repeat(2000);
        let s = format!("{a}\r\n{b}\r\n");
        let c = chunk_text(&s, 4096);
        // 2002 + 2002 = 4004 <= 4096 -> one chunk.
        assert_eq!(c.len(), 1);
        assert!(c[0].contains("\r\n"));

        // Force a split between the two lines.
        let a = "x".repeat(3000);
        let b = "y".repeat(3000);
        let s = format!("{a}\r\n{b}\r\n");
        let c = chunk_text(&s, 4096);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0], format!("{a}\r\n"));
        assert_eq!(c[1], format!("{b}\r\n"));
    }

    #[test]
    fn test_chunk_single_long_line_hard_split() {
        let s = "b".repeat(100);
        let c = chunk_text(&s, 30);
        assert_eq!(c.len(), 4);
        assert!(c.iter().all(|p| p.chars().count() <= 30));
        assert_eq!(c.concat(), s);
    }

    #[test]
    fn test_chunk_empty_and_multibyte() {
        assert_eq!(chunk_text("", 100), vec![String::new()]);
        // Multibyte: char-count based, not byte based.
        let s = "é".repeat(200); // 2 bytes each
        let c = chunk_text(&s, 100);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].chars().count(), 100);
        assert_eq!(c.concat(), s);
    }

    #[test]
    fn test_next_offset() {
        let u = |id: i64| Update {
            id,
            chat_id: 1,
            text: String::new(),
            from_first_name: String::new(),
        };
        assert_eq!(next_offset(&[], 5), 5, "empty batch keeps prev");
        assert_eq!(next_offset(&[u(3), u(7), u(5)], 1), 8, "max id + 1");
        assert_eq!(next_offset(&[u(2)], 10), 3, "batch max still wins");
    }
}
