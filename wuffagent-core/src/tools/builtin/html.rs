//! Shared HTML helpers for the hand-rolled search/page parsers.
//!
//! Kept dependency-light on purpose (project style: no HTML parser crate);
//! `regex` is only used for the multi-line block removal in [`html_to_text`].

/// Extract text between two markers (returns the slice strictly between them).
pub(crate) fn extract_between<'a>(input: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let idx = input.find(start)? + start.len();
    let rest = &input[idx..];
    let end_idx = rest.find(end)?;
    Some(&rest[..end_idx])
}

/// Strip HTML tags (`<...>`), keeping the text content.
pub(crate) fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for c in input.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Strip HTML tags and entities to get plain text.
pub(crate) fn extract_text(input: &str) -> Option<String> {
    let stripped = html_unescape(&strip_tags(input));
    Some(stripped.trim().to_string())
}

/// Simple HTML entity decoder (named entities only; bare `&` is kept).
pub(crate) fn html_unescape(input: &str) -> String {
    const ENTITIES: [(&str, &str); 6] = [
        ("quot", "\""),
        ("apos", "'"),
        ("nbsp", " "),
        ("amp", "&"),
        ("lt", "<"),
        ("gt", ">"),
    ];
    let mut result = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('&') {
        let after = &rest[pos + 1..];
        match ENTITIES.iter().find(|(name, _)| {
            after.len() > name.len() && after.starts_with(name) && after.as_bytes()[name.len()] == b';'
        }) {
            Some((name, replacement)) => {
                result.push_str(&rest[..pos]);
                result.push_str(replacement);
                rest = &after[name.len() + 1..];
            }
            None => {
                result.push_str(&rest[..=pos]);
                rest = after;
            }
        }
    }
    result.push_str(rest);
    result
}

/// ponytail: minimal percent-decode (+ = space, like urlencoding), no dep needed
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'+' {
            out.push(b' ');
        } else if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Convert an HTML document to readable plain text: remove
/// `<script>`/`<style>`/`<noscript>`/`<head>` blocks, drop all tags,
/// unescape entities, and collapse runs of blank lines.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();
    for (open, close) in [
        ("script", "</script>"),
        ("style", "</style>"),
        ("noscript", "</noscript>"),
        ("head", "</head>"),
    ] {
        // Matches `<tag>` or `<tag attrs>` through the closing tag; the
        // `(?:\s[^>]*)?` keeps `<header>`/`<styleX>`-style lookalikes safe.
        let re = regex::Regex::new(&format!(r"(?is)<{open}(?:\s[^>]*)?>.*?{close}"))
            .expect("html_to_text block regex");
        s = re.replace_all(&s, "").into_owned();
    }

    let text = html_unescape(&strip_tags(&s));

    // Collapse blank-line runs to a single blank line (no leading/trailing blanks).
    let mut out: Vec<String> = Vec::new();
    let mut blanks = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blanks += 1;
        } else {
            if !out.is_empty() && blanks >= 1 {
                out.push(String::new());
            }
            out.push(trimmed.to_string());
            blanks = 0;
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_between() {
        assert_eq!(
            extract_between("<a href=\"x\">t</a>", "href=\"", "\""),
            Some("x")
        );
        assert_eq!(extract_between("nope", "x", "y"), None);
    }

    #[test]
    fn test_strip_tags_and_extract_text() {
        assert_eq!(strip_tags("<b>web</b> framework"), "web framework");
        assert_eq!(extract_text("<strong>Rust</strong> &amp; friends"), Some("Rust & friends".to_string()));
        assert_eq!(extract_text("  plain  "), Some("plain".to_string()));
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("https%3a%2f%2fgithub.com%2fflosse"), "https://github.com/flosse");
        // '+' decodes to space (query-string style, like urlencoding)
        assert_eq!(percent_decode("a+b%c3%b6"), "a +bö");
    }

    #[test]
    fn test_html_to_text() {
        let html = r#"<html><head><title>T</title><style>body{color:red}</style></head>
<body><h1>Hello <b>World</b></h1><script>var x = "<not text>";</script><p>Line one &amp; Line two</p><div>


Block three</div></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Hello World"), "got: {text}");
        assert!(!text.contains("<not text>"), "script content must be removed: {text}");
        assert!(!text.contains("color:red"), "style content must be removed: {text}");
        assert!(!text.contains("\n\n\n"), "blank runs must be collapsed: {text:?}");
        assert!(text.contains("Line one & Line two"), "got: {text}");
        assert_eq!(text.lines().last(), Some("Block three"));
    }
}
