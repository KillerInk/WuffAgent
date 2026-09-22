//! Server context-overflow error details: the `exceed_context_size_error`
//! (HTTP 400) payload that lets a caller derive the true chars/token ratio
//! from a failing request and force-trim. Pure code motion from the old
//! client facade (C3).

/// Details from a server `exceed_context_size_error` (HTTP 400): the prompt
/// size that was sent and the server's actual context window. Lets the caller
/// derive the true chars/token ratio from the failing request and force-trim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextOverflow {
    pub n_prompt: u32,
    pub n_ctx: u32,
}

/// Parse the message of a server `exceed_context_size_error` (HTTP 400)
/// response into the reported prompt size and context window, so the caller
/// can force-trim to fit and retry. Returns `None` when the message does not
/// carry such an error.
///
/// Takes the raw message string (not a client `Error`) so this brick stays
/// free of a `client` dependency; `client::parse_context_overflow` is the
/// thin adapter that unwraps `Error::Http` first.
pub fn parse_context_overflow_msg(msg: &str) -> Option<ContextOverflow> {
    // Error format: "Server returned 400 Bad Request: {json body}[trailing text]"
    let body_start = msg.find('{')?;
    let body_end = msg.rfind('}')?;
    if body_end <= body_start {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&msg[body_start..=body_end]).ok()?;
    let e = v.get("error")?;
    if e.get("type").and_then(|t| t.as_str()) != Some("exceed_context_size_error") {
        return None;
    }
    let n_prompt = e.get("n_prompt_tokens")?.as_u64()? as u32;
    let n_ctx = e
        .get("n_ctx")
        .and_then(|t| t.as_u64())
        .unwrap_or(n_prompt as u64) as u32;
    Some(ContextOverflow { n_prompt, n_ctx })
}
