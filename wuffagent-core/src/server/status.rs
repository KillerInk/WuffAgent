//! Server status monitor: polls the llama.cpp server's `/slots`,
//! `/props`, and (when `--metrics` is on) `/metrics` endpoints to
//! provide live server state to the UI. `GET /v1/models` is fetched ONCE
//! per monitor lifetime for the model's trained context size
//! (`n_ctx_train` → status-bar tooltip "n_ctx 4096 / train 32768").
//!
//! The monitor runs as a tokio task, emitting `AppEvent::ServerStatus`
//! snapshots at a fixed interval (3 seconds). It is cheap: each poll is
//! a handful of small GET requests that the server handles in
//! microseconds when idle.

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

/// One-shot poll of the server's `/slots` + `/props` endpoints, plus
/// `GET /metrics` when `metrics_enabled` is true (server started with
/// `--metrics`; the endpoint returns 501 "not supported" otherwise, which
/// we treat as `metrics: None` — no error).
///
/// Returns a `ServerStatusInfo` with whatever the server reported.
/// `reachable` is `false` when the server doesn't respond at all.
/// Individual fields are `None`/empty when that endpoint is unavailable
/// (e.g. `/slots` disabled via `--no-slots`, or `/props` on a non-llama.cpp
/// backend).
pub async fn poll_server_status(
    base_url: &str,
    api_key: Option<&str>,
    metrics_enabled: bool,
) -> ServerStatusInfo {
    let http_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let mut status = ServerStatusInfo::default();

    // 1. GET /slots — per-slot state.
    {
        let mut builder = http_client.get(format!("{}/slots", base_url));
        if let Some(ref key) = api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }
        if let Ok(resp) = builder.send().await {
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
    }

    // 2. GET /props — n_ctx + model info.
    {
        let mut builder = http_client.get(format!("{}/props", base_url));
        if let Some(ref key) = api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }
        if let Ok(resp) = builder.send().await {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    status.reachable = true;
                    // Extract n_ctx from default_generation_settings.n_ctx
                    // (top-level fallback for non-llama.cpp backends).
                    if status.n_ctx.is_none() {
                        if let Ok(props) = serde_json::from_str::<serde_json::Value>(&text) {
                            status.n_ctx = props
                                .get("default_generation_settings")
                                .and_then(|s| s.get("n_ctx"))
                                .or_else(|| props.get("n_ctx"))
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32);
                        }
                    }
                    // Extract model name from `model` field (if present).
                    if let Ok(props) = serde_json::from_str::<serde_json::Value>(&text) {
                        status.model = props
                            .get("model")
                            .and_then(|m| m.as_str())
                            .map(|s| s.to_string());
                    }
                }
            }
        }
    }

    // 3. GET /metrics — live throughput gauges (only when the server was
    // started with --metrics; otherwise the endpoint 501s and we leave
    // `metrics: None`).
    if metrics_enabled {
        let mut builder = http_client.get(format!("{}/metrics", base_url));
        if let Some(ref key) = api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }
        if let Ok(resp) = builder.send().await {
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
pub async fn fetch_n_ctx_train(base_url: &str, api_key: Option<&str>) -> Option<u32> {
    let http_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();

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

            let mut status =
                poll_server_status(&base_url, api_key.as_deref(), metrics_enabled).await;
            let reachable = status.reachable;
            let (busy, total) = status.busy_slots();

            // /v1/models fetch for the trained context window: retry every
            // tick until it lands, then keep the value for good (it is
            // static for the server process).
            if n_ctx_train.is_none() {
                n_ctx_train = fetch_n_ctx_train(&base_url, api_key.as_deref()).await;
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

            // Only emit when the server is reachable (avoids spamming the
            // UI with "unreachable" events when no local server is running).
            if reachable {
                if event_tx
                    .send(AppEvent::ServerStatus { status })
                    .is_err()
                {
                    // Receiver dropped (UI shutting down) — stop polling.
                    break;
                }
            }
        }
    })
}
