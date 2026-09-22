//! Unit tests for the `search` module (see `super`).

use super::*;

#[test]
fn test_backend_chain_auto() {
    let chain = SearchBackend::Auto.chain();
    assert_eq!(
        chain,
        vec![
            SearchBackend::Bing,
            SearchBackend::Yahoo,
            SearchBackend::DuckDuckGo,
        ]
    );
    let labels: Vec<String> = chain.iter().map(|b| b.label()).collect();
    assert_eq!(labels, vec!["bing", "yahoo", "duckduckgo"]);
}

#[test]
fn test_backend_chain_explicit_is_itself() {
    assert_eq!(SearchBackend::Bing.chain(), vec![SearchBackend::Bing]);
    assert_eq!(
        SearchBackend::DuckDuckGo.chain(),
        vec![SearchBackend::DuckDuckGo]
    );
    assert_eq!(
        SearchBackend::SearXNG {
            base_url: "http://x".into()
        }
        .chain(),
        vec![SearchBackend::SearXNG {
            base_url: "http://x".into()
        }]
    );
}

#[test]
fn test_backend_label() {
    assert_eq!(SearchBackend::Auto.label(), "auto");
    assert_eq!(SearchBackend::Bing.label(), "bing");
    assert_eq!(SearchBackend::Yahoo.label(), "yahoo");
    assert_eq!(SearchBackend::DuckDuckGo.label(), "duckduckgo");
    assert_eq!(
        SearchBackend::SearXNG {
            base_url: "u".into()
        }
        .label(),
        "searxng"
    );
    assert_eq!(
        SearchBackend::Brave {
            api_key: "k".into()
        }
        .label(),
        "brave"
    );
}

#[test]
fn test_search_config_default_backend_is_auto() {
    assert_eq!(SearchConfig::default().backend, SearchBackend::Auto);
    assert_eq!(SearchBackend::default(), SearchBackend::Auto);
}

/// Env-var tests mutate process-wide env vars, so they run sequentially in
/// a single test and restore everything afterwards.
#[test]
fn test_resolve_with_env() {
    std::env::remove_var("WUFFAGENT_SEARCH_BACKEND");
    std::env::remove_var("WUFFAGENT_SEARXNG_URL");

    // Unset → config value.
    let cfg_ddg = SearchConfig {
        backend: SearchBackend::DuckDuckGo,
        ..SearchConfig::default()
    };
    assert_eq!(
        SearchBackend::resolve_with_env(&cfg_ddg),
        SearchBackend::DuckDuckGo
    );

    // WUFFAGENT_SEARCH_BACKEND=bing → Bing.
    std::env::set_var("WUFFAGENT_SEARCH_BACKEND", "bing");
    assert_eq!(
        SearchBackend::resolve_with_env(&cfg_ddg),
        SearchBackend::Bing
    );

    // PascalCase tolerated.
    std::env::set_var("WUFFAGENT_SEARCH_BACKEND", "Yahoo");
    assert_eq!(
        SearchBackend::resolve_with_env(&cfg_ddg),
        SearchBackend::Yahoo
    );

    // =searxng + WUFFAGENT_SEARXNG_URL=http://x → SearXNG { "http://x" }.
    std::env::set_var("WUFFAGENT_SEARCH_BACKEND", "searxng");
    std::env::set_var("WUFFAGENT_SEARXNG_URL", "http://x");
    assert_eq!(
        SearchBackend::resolve_with_env(&cfg_ddg),
        SearchBackend::SearXNG {
            base_url: "http://x".into()
        }
    );

    // searxng without env URL, config has one → configured URL.
    std::env::remove_var("WUFFAGENT_SEARXNG_URL");
    let cfg_searxng = SearchConfig {
        backend: SearchBackend::SearXNG {
            base_url: "http://cfg".into(),
        },
        ..SearchConfig::default()
    };
    assert_eq!(
        SearchBackend::resolve_with_env(&cfg_searxng),
        SearchBackend::SearXNG {
            base_url: "http://cfg".into()
        }
    );

    // searxng with no URL anywhere → keep configured backend.
    assert_eq!(
        SearchBackend::resolve_with_env(&cfg_ddg),
        SearchBackend::DuckDuckGo
    );

    // WUFFAGENT_SEARXNG_URL overrides a configured SearXNG backend too.
    std::env::remove_var("WUFFAGENT_SEARCH_BACKEND");
    std::env::set_var("WUFFAGENT_SEARXNG_URL", "http://env");
    assert_eq!(
        SearchBackend::resolve_with_env(&cfg_searxng),
        SearchBackend::SearXNG {
            base_url: "http://env".into()
        }
    );

    // Unknown backend value → config value unchanged.
    std::env::remove_var("WUFFAGENT_SEARXNG_URL");
    std::env::set_var("WUFFAGENT_SEARCH_BACKEND", "bogus");
    assert_eq!(
        SearchBackend::resolve_with_env(&cfg_ddg),
        SearchBackend::DuckDuckGo
    );

    std::env::remove_var("WUFFAGENT_SEARCH_BACKEND");
    std::env::remove_var("WUFFAGENT_SEARXNG_URL");
}

#[test]
fn test_legacy_backend_names_still_deserialize() {
    // Old config files store the plain PascalCase variant names.
    let b: SearchBackend = serde_json::from_str("\"DuckDuckGo\"").unwrap();
    assert_eq!(b, SearchBackend::DuckDuckGo);
    let b: SearchBackend =
        serde_json::from_str(r#"{"SearXNG": {"base_url": "http://s"}}"#).unwrap();
    assert_eq!(
        b,
        SearchBackend::SearXNG {
            base_url: "http://s".into()
        }
    );
    let b: SearchBackend = serde_json::from_str(r#"{"Brave": {"api_key": "k"}}"#).unwrap();
    assert_eq!(
        b,
        SearchBackend::Brave {
            api_key: "k".into()
        }
    );
    // New variants round-trip.
    let b: SearchBackend = serde_json::from_str("\"Auto\"").unwrap();
    assert_eq!(b, SearchBackend::Auto);
}
