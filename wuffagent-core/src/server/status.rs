//! Server status monitor: polls the llama.cpp server's `/slots` and
//! `/props` endpoints to provide live server state to the UI.
//!
//! The monitor runs as a tokio task, emitting `AppEvent::ServerStatus`
//! snapshots at a fixed interval (3 seconds). It is cheap: each poll is
//! two small GET requests that the server handles in microseconds when
//! idle.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;

use crate::types::{AppEvent, ServerStatusInfo, SlotInfo};

/// How often to poll the server (3 seconds).
const POLL_INTERVAL_SECS: u64 = 3;

/// One-shot poll of the server's `/slots` + `/props` endpoints.
///
/// Returns a `ServerStatusInfo` with whatever the server reported.
/// `reachable` is `false` when the server doesn't respond at all.
/// Individual fields are `None`/empty when that endpoint is unavailable
/// (e.g. `/slots` disabled via `--no-slots`, or `/props` on a non-llama.cpp
/// backend).
pub async fn poll_server_status(base_url: &str, api_key: Option<&str>) -> ServerStatusInfo {
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

    status
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
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(POLL_INTERVAL_SECS));
        // The first tick fires immediately; we want the first poll after
        // one interval (give the server time to finish loading its model).
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;

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

            let status = poll_server_status(&base_url, api_key.as_deref()).await;
            let reachable = status.reachable;
            let (busy, total) = status.busy_slots();

            tracing::debug!(
                "ServerMonitor: reachable={}, slots={}/{} busy, n_ctx={:?}",
                reachable, busy, total, status.n_ctx
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


