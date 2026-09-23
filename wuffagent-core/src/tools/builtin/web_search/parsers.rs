//! HTML/JSON result parsers for the web search backends (Bing, Yahoo,
//! DuckDuckGo). Split out of `web_search/mod.rs` (Phase D size split).
//!
//! The Bing/Yahoo parsers are covered by OFFLINE tests against saved HTML
//! fixtures in `wuffagent-core/tests/fixtures/` — keep the fixtures current
//! when provider markup changes.

use serde_json::Value;

use crate::tools::builtin::html;

/// Minimal parser for Bing HTML results.
///
/// One organic result per `<li class="b_algo"` item; the anchor `href` is a
/// `bing.com/ck/a` redirect whose `u=a1<base64>` query param carries the real
/// URL.
pub(crate) fn parse_bing_results(html: &str, limit: usize) -> Vec<Value> {
    let mut results = Vec::new();

    for seg in html.split("<li class=\"b_algo\"").skip(1).take(limit) {
        // URL: `u=a1<base64>` — the base64 value ends at the next `&`.
        let url = seg
            .find("u=a1")
            .and_then(|i| {
                let rest = &seg[i + "u=a1".len()..];
                let end = rest.find('&').unwrap_or(rest.len());
                Some(&rest[..end])
            })
            .and_then(decode_bing_url);
        let Some(url) = url else { continue };

        // Title: first <h2 …><a …>TITLE</a> in the item.
        let title = seg
            .find("<h2")
            .and_then(|h2| {
                let h2seg = &seg[h2..];
                h2seg
                    .find("<a")
                    .and_then(|a| h2seg[a..].find('>').map(|gt| &h2seg[a + gt + 1..]))
            })
            .and_then(|t| t.find("</a>").map(|e| &t[..e]))
            .and_then(html::extract_text)
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }

        // Snippet: <div class="b_caption"><p …>…</p>
        let snippet = seg
            .find("class=\"b_caption\"")
            .and_then(|i| seg[i..].find('>').map(|gt| &seg[i + gt + 1..]))
            .and_then(|s| s.find("</p>").map(|e| &s[..e]))
            .and_then(html::extract_text)
            .unwrap_or_default();

        results.push(serde_json::json!({
            "title": title,
            "url": url,
            "snippet": snippet,
        }));
    }

    results
}

/// Decode Bing's `u=a1<base64>` redirect target; `None` when undecodable or
/// not an http(s) URL.
fn decode_bing_url(b64: &str) -> Option<String> {
    use base64::Engine;
    let b64 = b64.trim();
    // Bing's `u=a1` values are unpadded (length ≡ 2 mod 4), so the
    // padding-indifferent engines are required (`STANDARD` rejects them).
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(b64)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b64))
        .ok()?;
    let url = String::from_utf8_lossy(&bytes).into_owned();
    if url.starts_with("http://") || url.starts_with("https://") {
        Some(url)
    } else {
        None
    }
}

/// Minimal parser for Yahoo HTML results.
///
/// One organic result per `<div class="compTitle options-toggle">` segment —
/// the title anchor, the h3, and the `compText aAbs` snippet all live in the
/// same segment. The real URL is the percent-encoded `RU=` param of the
/// `r.search.yahoo.com` redirect (up to `/RK=`).
pub(crate) fn parse_yahoo_results(html: &str, limit: usize) -> Vec<Value> {
    let mut results = Vec::new();

    for seg in html
        .split("<div class=\"compTitle options-toggle\">")
        .skip(1)
        .take(limit)
    {
        // URL: first href in the segment; RU=<percent-encoded>/RK=.
        let url = seg
            .find("href=\"http")
            .and_then(|i| {
                let rest = &seg[i + "href=\"".len()..];
                rest.find('"').map(|e| &rest[..e])
            })
            .and_then(|href| {
                href.find("RU=").and_then(|ru| {
                    let after = &href[ru + "RU=".len()..];
                    let end = after.find("/RK=").unwrap_or(after.len());
                    Some(&after[..end])
                })
            })
            .map(html::percent_decode)
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"));
        let Some(url) = url else { continue };

        // Title: <h3 …><span …>TITLE</span>
        let title = seg
            .find("<h3")
            .and_then(|h3| {
                let h3seg = &seg[h3..];
                h3seg
                    .find("<span")
                    .and_then(|sp| h3seg[sp..].find('>').map(|gt| &h3seg[sp + gt + 1..]))
            })
            .and_then(|t| t.find("</span>").map(|e| &t[..e]))
            .and_then(html::extract_text)
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }

        // Snippet: <div class="compText aAbs"><p …>…</p>
        let snippet = seg
            .find("compText aAbs")
            .and_then(|i| {
                seg[i..]
                    .find("<p")
                    .and_then(|p| seg[i + p..].find('>').map(|gt| &seg[i + p + gt + 1..]))
            })
            .and_then(|s| s.find("</p>").map(|e| &s[..e]))
            .and_then(html::extract_text)
            .unwrap_or_default();

        results.push(serde_json::json!({
            "title": title,
            "url": url,
            "snippet": snippet,
        }));
    }

    results
}

/// Minimal parser for DuckDuckGo HTML results.
pub(super) fn parse_duckduckgo_results(html: &str, limit: usize) -> Vec<Value> {
    let mut results = Vec::new();

    // DuckDuckGo HTML results use <article class="result"> elements.
    let rows: Vec<&str> = html.split("<article").skip(1).take(limit).collect();

    for row in rows {
        // Title: text of <a class="result__a">…</a> (attribute order is not
        // relied on: the anchor tag ends at the first `>` after the class).
        let title = row
            .find("class=\"result__a\"")
            .and_then(|i| row[i..].find('>').map(|gt| &row[i + gt + 1..]))
            .and_then(|t| t.find("</a>").map(|e| &t[..e]))
            .and_then(html::extract_text)
            .unwrap_or_default();

        // Extract URL from the first href attribute.
        let url = html::extract_between(row, "href=\"", "\"")
            .or_else(|| {
                row.find("href='").and_then(|i| {
                    let rest = &row[i + "href='".len()..];
                    rest.find('\'').map(|e| &rest[..e])
                })
            })
            .map(clean_ddg_url)
            .unwrap_or_default();

        // Snippet: text of <div class="result__snippet">…</div>
        let snippet = row
            .find("class=\"result__snippet\"")
            .and_then(|i| row[i..].find('>').map(|gt| &row[i + gt + 1..]))
            .and_then(|s| s.find("</").map(|e| &s[..e]))
            .and_then(html::extract_text)
            .unwrap_or_default();

        if !title.is_empty() {
            results.push(serde_json::json!({
                "title": title,
                "url": url,
                "snippet": snippet,
            }));
        }
    }

    results
}

/// DuckDuckGo wraps URLs in redirect links — extract the real URL from the
/// `uddg=` param (value ends at the next `&`).
pub(super) fn clean_ddg_url(url: &str) -> String {
    if let Some(eq_pos) = url.find("uddg=") {
        let encoded = &url[eq_pos + "uddg=".len()..];
        let encoded = encoded.split('&').next().unwrap_or("");
        if !encoded.is_empty() {
            return html::percent_decode(encoded);
        }
    }
    url.to_string()
}
