//! Unit tests for the `web_search` module (see `super`).

    use super::*;

    const BING_HTML: &str = include_str!("../../../../tests/fixtures/bing.html");
    const YAHOO_HTML: &str = include_str!("../../../../tests/fixtures/yahoo.html");
    const DDG_HTML: &str = include_str!("../../../../tests/fixtures/ddg.html");
    // Second Yahoo capture (2026-09, live): guards against markup drift —
    // if this ever fails, re-probe the live page and update the parser.
    const YAHOO_HTML_2026_09: &str =
        include_str!("../../../../tests/fixtures/yahoo-live-2026-09.html");

    // Fixture query was "rust web framework" — first result on both engines:
    const EXPECTED_URL: &str = "https://github.com/flosse/rust-web-framework-comparison";

    #[test]
    fn test_parse_bing_results_happy() {
        let results = parse_bing_results(BING_HTML, 10);
        assert!(results.len() >= 3, "expected >=3 results, got {}", results.len());

        let first = &results[0];
        assert_eq!(first["url"].as_str(), Some(EXPECTED_URL));
        assert!(
            first["title"].as_str().unwrap_or("").contains("Rust web framework comparison"),
            "unexpected title: {}",
            first["title"]
        );
        assert!(!first["snippet"].as_str().unwrap_or("").is_empty());
    }

    #[test]
    fn test_parse_yahoo_results_happy() {
        let results = parse_yahoo_results(YAHOO_HTML, 10);
        assert!(results.len() >= 3, "expected >=3 results, got {}", results.len());

        let first = &results[0];
        assert_eq!(first["url"].as_str(), Some(EXPECTED_URL));
        assert!(
            first["title"].as_str().unwrap_or("").contains("Rust web framework comparison"),
            "unexpected title: {}",
            first["title"]
        );
        assert!(!first["snippet"].as_str().unwrap_or("").is_empty());
    }

    #[test]
    fn test_parse_yahoo_results_2026_09_capture() {
        // Live capture from 8 months later: same query, same first result —
        // proves the parser survives Yahoo markup drift between captures.
        let results = parse_yahoo_results(YAHOO_HTML_2026_09, 10);
        assert!(results.len() >= 3, "expected >=3 results, got {}", results.len());
        assert_eq!(results[0]["url"].as_str(), Some(EXPECTED_URL));
    }

    #[test]
    fn test_parse_ddg_challenge_page_is_empty() {
        // ddg.html is the bot-challenge page: no <article> results at all.
        assert!(parse_duckduckgo_results(DDG_HTML, 10).is_empty());
        assert!(DDG_HTML.contains("anomaly-modal"));
    }

    #[test]
    fn test_parsers_empty_input() {
        assert!(parse_bing_results("", 10).is_empty());
        assert!(parse_yahoo_results("", 10).is_empty());
        assert!(parse_duckduckgo_results("", 10).is_empty());
    }

    #[test]
    fn test_parse_ddg_synthetic_happy_path() {
        // Real-world attribute order (class before href) on both anchors.
        let html = r#"<html><body>
<article class="result results_links results_links_deep web-result">
<h2 class="result__title"><a rel="nofollow" class="result__a" href="/l/?uddg=https%3A%2F%2Fexample.com%2Fpage1&rut=abc123">Example <b>Page</b> One</a></h2>
<div class="result__snippet">Snippet one &amp; more.</div>
</article>
<article class="result results_links results_links_deep web-result">
<h2 class="result__title"><a rel="nofollow" class="result__a" href="/l/?uddg=https%3A%2F%2Fexample.org%2Ftwo&rut=def456">Example Page Two</a></h2>
<div class="result__snippet">Snippet two.</div>
</article>
</body></html>"#;

        let results = parse_duckduckgo_results(html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"].as_str(), Some("Example Page One"));
        assert_eq!(results[0]["url"].as_str(), Some("https://example.com/page1"));
        assert_eq!(results[0]["snippet"].as_str(), Some("Snippet one & more."));
        assert_eq!(results[1]["title"].as_str(), Some("Example Page Two"));
        assert_eq!(results[1]["url"].as_str(), Some("https://example.org/two"));
    }

    #[test]
    fn test_clean_ddg_url() {
        assert_eq!(
            clean_ddg_url("/l/?uddg=https%3A%2F%2Fexample.com%2Fx&rut=123"),
            "https://example.com/x"
        );
        // Non-redirect URLs pass through.
        assert_eq!(clean_ddg_url("https://plain.example/"), "https://plain.example/");
    }

    #[test]
    fn test_run_chain_failover() {
        let tool = WebSearchTool::new();
        let ddg_results = vec![
            serde_json::json!({"title": "t1", "url": "https://a.example", "snippet": ""}),
            serde_json::json!({"title": "t2", "url": "https://b.example", "snippet": ""}),
        ];

        let (backend, results) = tool
            .run_chain(
                &[
                    SearchBackend::Bing,
                    SearchBackend::Yahoo,
                    SearchBackend::DuckDuckGo,
                ],
                "failover-happy-test",
                |b| match b {
                    SearchBackend::Bing => Err("blocked by bot detection".to_string()),
                    SearchBackend::Yahoo => Ok(Vec::new()),
                    SearchBackend::DuckDuckGo => Ok(ddg_results.clone()),
                    _ => unreachable!(),
                },
            )
            .expect("chain should succeed on the third backend");

        assert_eq!(backend, SearchBackend::DuckDuckGo);
        assert_eq!(results, ddg_results);
    }

    #[test]
    fn test_run_chain_first_backend_wins() {
        let tool = WebSearchTool::new();
        let bing_results = vec![serde_json::json!({"title": "t", "url": "https://x", "snippet": ""})];

        let (backend, results) = tool
            .run_chain(
                &[SearchBackend::Bing, SearchBackend::Yahoo, SearchBackend::DuckDuckGo],
                "failover-first-wins-test",
                |b| {
                    if matches!(b, SearchBackend::Bing) {
                        Ok(bing_results.clone())
                    } else {
                        unreachable!("chain must stop at the first success")
                    }
                },
            )
            .expect("chain should succeed on the first backend");

        assert_eq!(backend, SearchBackend::Bing);
        assert_eq!(results, bing_results);
    }

    #[test]
    fn test_run_chain_all_fail() {
        let tool = WebSearchTool::new();
        let failures = tool
            .run_chain(
                &[
                    SearchBackend::Bing,
                    SearchBackend::Yahoo,
                    SearchBackend::DuckDuckGo,
                ],
                "failover-all-fail-test",
                |b| match b {
                    SearchBackend::Bing => Err("timeout".to_string()),
                    SearchBackend::Yahoo => Ok(Vec::new()),
                    SearchBackend::DuckDuckGo => Err("blocked by bot detection".to_string()),
                    _ => unreachable!(),
                },
            )
            .expect_err("chain should fail when every backend fails");

        assert_eq!(failures.len(), 3);
        assert!(failures[0].starts_with("bing: timeout"), "{}", failures[0]);
        assert!(failures[1].starts_with("yahoo: no results (blocked)"), "{}", failures[1]);
        assert!(
            failures[2].starts_with("duckduckgo: blocked by bot detection"),
            "{}",
            failures[2]
        );
    }
