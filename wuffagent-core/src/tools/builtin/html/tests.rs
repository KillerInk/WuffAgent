//! Unit tests for the `html` module (see `super`).

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
    assert_eq!(
        extract_text("<strong>Rust</strong> &amp; friends"),
        Some("Rust & friends".to_string())
    );
    assert_eq!(extract_text("  plain  "), Some("plain".to_string()));
}

#[test]
fn test_percent_decode() {
    assert_eq!(
        percent_decode("https%3a%2f%2fgithub.com%2fflosse"),
        "https://github.com/flosse"
    );
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
    assert!(
        !text.contains("<not text>"),
        "script content must be removed: {text}"
    );
    assert!(
        !text.contains("color:red"),
        "style content must be removed: {text}"
    );
    assert!(
        !text.contains("\n\n\n"),
        "blank runs must be collapsed: {text:?}"
    );
    assert!(text.contains("Line one & Line two"), "got: {text}");
    assert_eq!(text.lines().last(), Some("Block three"));
}
