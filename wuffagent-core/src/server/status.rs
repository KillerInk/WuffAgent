//! Server status monitor: polls the llama.cpp server's `/health`,
//! `/slots`, `/props`, and (when `--metrics` is on) `/metrics` endpoints
//! to provide live server state to the UI. `GET /v1/models` is fetched
//! ONCE per monitor lifetime for the model's trained context size
//! (`n_ctx_train` → status-bar tooltip "n_ctx 4096 / train 32768").
//!
//! The monitor runs as a tokio task, emitting `AppEvent::ServerStatus`
//! snapshots at a fixed interval (3 seconds). It emits while the server
//! is reachable AND on the two transition edges (up → down: alert the UI
//! with a `reachable: false` snapshot; down → up: recovery). While
//! continuously unreachable (no local server running) it stays silent.
//! It is cheap: each poll is a handful of small GET requests that the
//! server handles in microseconds when idle.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;

use crate::types::{AppEvent, ServerMetrics, ServerStatusInfo, SlotInfo};

/// How often to poll the server (3 seconds).
const POLL_INTERVAL_SECS: u64 = 3;

/// Parse the small Prometheus text subset emitted by llama.cpp's
/// `/metrics` endpoint into a [`ServerMetrics`].
///
/// The server renders `# HELP` / `# TYPE` comment lines followed by
/// `llamacpp:<name> <value>` series lines (verified against b11126
/// `tools/server/server-task.cpp`, `server_task_result_metrics::to_metrics`).
/// Labeled series (speculative decoding) and unknown names are ignored.
/// `None` when no recognizable `llamacpp:` series is present.
pub fn parse_metrics_text(text: &str) -> Option<ServerMetrics> {
    let mut metrics = ServerMetrics::default();
    let mut found = false;

    for line in text.lines() {
        let line = line.trim();
        // Skip comments (# HELP / # TYPE) and labeled series (we only use
        // the unlabeled gauges/counters WuffAgent displays).
        if line.starts_with('#') || line.contains('{') {
            continue;
        }
        let Some((name, value)) = line.split_once(' ') else {
            continue;
        };
        let Some(name) = name.strip_prefix("llamacpp:") else {
            continue;
        };
        // Only the four metrics WuffAgent displays are recognized; a body of
        // unknown llamacpp: series (or non-llamacpp lines) still yields None.
        let recognized = matches!(
            name,
            "prompt_tokens_seconds"
                | "predicted_tokens_seconds"
                | "requests_processing"
                | "n_tokens_max"
        );
        if !recognized {
            continue;
        }
        let Ok(value) = value.parse::<f64>() else {
            continue;
        };
        match name {
            "prompt_tokens_seconds" => metrics.prompt_tps = Some(value),
            "predicted_tokens_seconds" => metrics.predicted_tps = Some(value),
            "requests_processing" => {
                metrics.requests_processing = Some(value.round() as u32);
            }
            "n_tokens_max" => metrics.n_tokens_max = Some(value.round() as u32),
            _ => unreachable!("recognized names are the only ones here"),
        }
        found = true;
    }

    if found {
        Some(metrics)
    } else {
        None
    }
}

/// One-shot poll of the server's `/health` + `/slots` + `/props`
/// endpoints, plus `GET /metrics` when `metrics_enabled` is true (server
/// started with `--metrics`; the endpoint returns 501 "not supported"
/// otherwise, which we treat as `metrics: None` — no error).
///
/// Returns a `ServerStatusInfo` with whatever the server reported.
/// `reachable` is `false` when the server doesn't respond at all.
///
/// `/health` is the most reliable reachability signal (always served by
/// llama.cpp; `/slots`/`/props` may 404 or 503 on some builds/configs):
/// 200 (a slot is free) or 503 (all slots busy / model still loading)
/// both mark `reachable = true`.
///
/// Individual fields are `None`/empty when that endpoint is unavailable
/// (e.g. `/slots` disabled via `--no-slots`, or `/props` on a non-llama.cpp
/// backend).
/// `client` is shared for the whole monitor lifetime (perf pass: building a
/// `reqwest::Client` sets up the pool / TLS context, and a fresh one per
/// 3 s poll threw away keep-alive connections; the 2 s connect / 5 s
/// request timeouts live on the shared client).
pub async fn poll_server_status(
    http_client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    metrics_enabled: bool,
) -> ServerStatusInfo {
    let mut status = ServerStatusInfo {
        base_url: base_url.to_string(),
        ..Default::default()
    };

    // GET /health + /slots + /props + /metrics — all four fired in PARALLEL
    // (perf follow-up: sequential awaits added up to 4× the single-request
    // latency per poll; with the shared client's 5 s request timeout a slow
    // poll could otherwise take ~20 s). /health is the liveness probe (200
    // or 503 both mean the process is up; `/slots`/`/props` may be
    // unavailable on some builds/configs). /metrics is always requested
    // (the endpoint 501s when the server was not started with --metrics)
    // and its body is only parsed when metrics are enabled.
    let auth = |b: reqwest::RequestBuilder| -> reqwest::RequestBuilder {
        match api_key {
            Some(key) => b.header("Authorization", format!("Bearer {}", key)),
            None => b,
        }
    };
    let (health_res, slots_res, props_res, metrics_res) = futures::join!(
        auth(http_client.get(format!("{}/health", base_url))).send(),
        auth(http_client.get(format!("{}/slots", base_url))).send(),
        auth(http_client.get(format!("{}/props", base_url))).send(),
        auth(http_client.get(format!("{}/metrics", base_url))).send(),
    );

    // /health — liveness probe.
    if let Ok(resp) = health_res {
        if matches!(resp.status().as_u16(), 200 | 503) {
            status.reachable = true;
        }
        // Drain the body so the connection can be pooled.
        let _ = resp.text().await;
    }

    // /slots — per-slot state.
    if let Ok(resp) = slots_res {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                match serde_json::from_str::<Vec<SlotInfo>>(&text) {
                    Ok(slots) => {
                        status.slots = slots;
                        status.reachable = true;
                    }
                    Err(e) => {
                        tracing::debug!("ServerMonitor: failed to parse /slots: {}", e);
                    }
                }
            }
        }
    }

    // /props — n_ctx + model info.
    if let Ok(resp) = props_res {
        if resp.status().is_success() {
            if let Ok(text) = resp.text().await {
                status.reachable = true;
                // Parse ONCE — both n_ctx and model come from this body.
                if let Ok(props) = serde_json::from_str::<serde_json::Value>(&text) {
                    // n_ctx from default_generation_settings.n_ctx
                    // (top-level fallback for non-llama.cpp backends).
                    if status.n_ctx.is_none() {
                        status.n_ctx = props
                            .get("default_generation_settings")
                            .and_then(|s| s.get("n_ctx"))
                            .or_else(|| props.get("n_ctx"))
                            .and_then(|v| v.as_u64())
                            .map(|v| v as u32);
                    }
                    // Extract model name from `model` field (if present).
                    status.model = props
                        .get("model")
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string());
                }
            }
        }
    }

    // /metrics — Prometheus-text subset (only when the server was started
    // with --metrics; the endpoint 501s otherwise → `metrics: None`).
    if metrics_enabled {
        if let Ok(resp) = metrics_res {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    status.metrics = parse_metrics_text(&text);
                }
            }
        }
    }

    status
}

/// Extract `n_ctx_train` from a parsed `GET /v1/models` response body.
///
/// b11126 wraps the model list under `data` (OAI-compat,
/// `server-models.cpp` `get_router_models`) with the field at
/// `meta.n_ctx_train` (from the loaded-model info); a bare array or a
/// top-level `n_ctx_train` also work. `None` when absent.
pub fn n_ctx_train_from_models(models: &serde_json::Value) -> Option<u32> {
    let arr = models
        .get("data")
        .map(|v| v.as_array())
        .flatten()
        .or_else(|| models.as_array())?;
    let first = arr.first()?;
    first
        .get("meta")
        .and_then(|m| m.get("n_ctx_train"))
        .or_else(|| first.get("n_ctx_train"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
}

/// Fetch the model's trained context window (`n_ctx_train`) once from
/// `GET /v1/models` → `data[0].meta.n_ctx_train` (b11126
/// `server-models.cpp`, `get_router_models`). Returns `None` when the
/// endpoint or the field is unavailable (e.g. non-llama.cpp backend).
pub async fn fetch_n_ctx_train(
    http_client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
) -> Option<u32> {
    let mut builder = http_client.get(format!("{}/v1/models", base_url));
    if let Some(ref key) = api_key {
        builder = builder.header("Authorization", format!("Bearer {}", key));
    }
    let resp = builder.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    let models = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    n_ctx_train_from_models(&models)
}

/// Spawn the server status monitor task.
///
/// Polls the server every 3 seconds and emits `AppEvent::ServerStatus`
/// snapshots via the provided event channel. The task runs until the
/// `active` flag is set to `false` (e.g. on app shutdown or server stop).
///
/// Returns a `JoinHandle` so the caller can await the task's completion
/// if needed (e.g. on shutdown).
pub fn spawn_server_monitor(
    base_url: String,
    api_key: Option<String>,
    event_tx: mpsc::Sender<AppEvent>,
    active: Arc<AtomicBool>,
    metrics_enabled: bool,
) -> tokio::task::JoinHandle<()> {
    // One client for the whole monitor lifetime (perf pass: was a fresh
    // Client per 3 s poll + one per n_ctx_train retry).
    let http_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(POLL_INTERVAL_SECS));
        // The first tick fires immediately; we want the first poll after
        // one interval (give the server time to finish loading its model).
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;

        // `n_ctx_train` is static per server process — fetch it from /v1/models
        // once, then keep it across snapshots. While it is None (first ticks
        // before the model is loaded, or a transient failure) we keep
        // retrying every tick; once it lands we never fetch again.
        let mut n_ctx_train: Option<u32> = None;
        let mut prev_reachable = false;

        loop {
            // Check if we should stop.
            if !active.load(std::sync::atomic::Ordering::SeqCst) {
                tracing::debug!("ServerMonitor: deactivated, stopping");
                break;
            }

            interval.tick().await;

            // Re-check after the tick (the flag may have been cleared).
            if !active.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }

            let mut status = poll_server_status(
                &http_client,
                &base_url,
                api_key.as_deref(),
                metrics_enabled,
            )
            .await;
            let reachable = status.reachable;
            let (busy, total) = status.busy_slots();

            // /v1/models fetch for the trained context window: retry every
            // tick until it lands, then keep the value for good (it is
            // static for the server process).
            if n_ctx_train.is_none() {
                n_ctx_train =
                    fetch_n_ctx_train(&http_client, &base_url, api_key.as_deref()).await;
                if n_ctx_train.is_some() {
                    tracing::info!(
                        n_ctx_train = ?n_ctx_train,
                        "ServerMonitor: fetched model trained context from /v1/models"
                    );
                }
            }
            status.n_ctx_train = n_ctx_train;

            tracing::debug!(
                "ServerMonitor: reachable={}, slots={}/{} busy, n_ctx={:?}, metrics={:?}",
                reachable, busy, total, status.n_ctx, status.metrics
            );

            // Emit while reachable (periodic snapshots) plus the two
            // transition edges: up → down (alert the UI the server went
            // away — the snapshot carries `reachable: false`) and down → up
            // (recovery). Stay silent while CONTINUOUSLY unreachable
            // (no local server running) to avoid spamming the event bus.
            if reachable || prev_reachable {
                if !reachable {
                    tracing::warn!("ServerMonitor: server at {base_url} went DOWN");
                }
                if event_tx
                    .send(AppEvent::ServerStatus { status })
                    .is_err()
                {
                    // Receiver dropped (UI shutting down) — stop polling.
                    break;
                }
            }
            prev_reachable = reachable;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract the shared `/slots`-parsing logic from `poll_server_status`
    /// so it can be unit-tested without a live server.
    fn parse_slots_body(
        status: u16,
        text: &str,
        _metrics_enabled: bool,
    ) -> ServerStatusInfo {
        let mut s = ServerStatusInfo {
            base_url: "http://127.0.0.1:9999".into(),
            ..Default::default()
        };
        if matches!(status, 200 | 503) {
            s.reachable = true;
        }
        if status == 200 {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
                s.slots = v
                    .get("slots")
                    .and_then(|s| s.as_array())
                    .and_then(|arr| serde_json::to_value(arr).ok())
                    .and_then(|v| serde_json::from_value::<Vec<SlotInfo>>(v).ok())
                    .unwrap_or_default();
            }
        }
        s
    }

    #[test]
    fn status_slots_200_parses() {
        let st = parse_slots_body(
            200,
            r#"{"slots":[{"id":0,"is_processing":false,"n_ctx":4096}]}"#,
            false,
        );
        assert!(st.reachable);
        assert_eq!(st.slots.len(), 1);
        assert!(!st.slots[0].is_processing);
    }

    #[test]
    fn status_slots_404_is_unreachable() {
        // /slots disabled via --no-slots → 404; the server may still be up
        // (via /health or /props), so the /slots step alone reports
        // unreachable.
        let st = parse_slots_body(404, "", false);
        assert!(!st.reachable);
        assert!(st.slots.is_empty());
    }

    #[test]
    fn status_503_when_model_loading() {
        // /slots 503 → reachable (server up, model still loading), no slots.
        let st = parse_slots_body(503, "", true);
        assert!(st.reachable);
        assert!(st.slots.is_empty());
        let st = parse_slots_body(
            200,
            r#"{"slots":[{"id":0,"is_processing":true,"n_ctx":4096}]}"#,
            true,
        );
        assert!(st.reachable);
        assert_eq!(st.slots.len(), 1);
        assert!(st.slots[0].is_processing);
        assert_eq!(st.slots[0].n_ctx, 4096);
    }
}
