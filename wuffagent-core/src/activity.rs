//! Live LLM-activity tracking.
//!
//! An [`ActivityTracker`] (created once, `Arc`-cloned into every call site
//! that opts in) instruments individual LLM calls and streams a whole-snapshot
//! [`AppEvent::LlmActivity`] to the UI, throttled to ~5 Hz per activity. The
//! status bar uses it to show what is actually running — "judge",
//! "agent: coder", "memory", "eval: t1", "processing prompt… 42%", … —
//! instead of a static "streaming".
//!
//! Design rules:
//! - The [`ChatClientAdapter`](crate::llm::ChatClientAdapter) itself does NOT
//!   instrument; call sites opt in explicitly (`labeled()` / [`LabeledLlm`]),
//!   which keeps coverage explicit and avoids double-counting.
//! - [`ActivityHandle`] is RAII: dropping it (also on `?` early-return,
//!   panic, cancellation) removes the entry and emits one final snapshot.
//!   No manual `end()` calls needed.
//! - `tx.send` failures are ignored (the UI is gone).

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;

use crate::llm::LlmClient;
use crate::types::{
    ActivityPhase, AppEvent, LlmActivityInfo, Message, PromptProgress, Usage,
};

/// Minimum interval between two emitted snapshots (llama.cpp can send
/// `prompt_progress` per server tick, >50 Hz; never forward faster).
const EMIT_INTERVAL: Duration = Duration::from_millis(200);

/// One in-flight LLM call.
struct Entry {
    label: String,
    session_id: Option<String>,
    /// Last prompt-progress values; `None` until the server reports any.
    pp: Option<PromptProgress>,
    /// Whether TG has started (first chunk seen).
    generating: bool,
    /// Tokens generated so far.
    tokens_out: u32,
    /// Tokens generated since entering the TG phase (for the TG tps).
    phase_tokens: u32,
    /// When the call started.
    started: Instant,
    /// When the current phase started (reset when TG begins).
    phase_started: Instant,
    started_at: SystemTime,
    /// When this entry last took part in an emitted snapshot.
    last_emit: Option<Instant>,
}

struct TrackerInner {
    next_id: u64,
    entries: HashMap<u64, Entry>,
}

/// Lock helper that recovers from poisoning (a panic under the lock must
/// not kill every LLM call — the snapshot is best-effort UI state).
fn lock(inner: &Mutex<TrackerInner>) -> std::sync::MutexGuard<'_, TrackerInner> {
    inner.lock().unwrap_or_else(|e| e.into_inner())
}

/// Snapshot of one entry for emission.
fn to_info(id: u64, e: &Entry, now: Instant) -> (u64, LlmActivityInfo) {
    let (phase, tps) = if e.generating {
        let dur = now.duration_since(e.phase_started).as_secs_f64();
        let tps = if e.phase_tokens >= 1 && dur >= 0.2 {
            Some(e.phase_tokens as f64 / dur)
        } else {
            None
        };
        (ActivityPhase::Generating, tps)
    } else if let Some(pp) = e.pp {
        (
            ActivityPhase::PromptProcessing {
                processed: pp.processed,
                total: pp.total,
                time_ms: pp.time_ms,
            },
            pp.prompt_tps(),
        )
    } else {
        (ActivityPhase::Thinking, None)
    };
    (
        id,
        LlmActivityInfo {
            label: e.label.clone(),
            session_id: e.session_id.clone(),
            phase,
            tokens_out: e.tokens_out,
            tps,
            started_at: e.started_at,
            duration: now.duration_since(e.started),
        },
    )
}

/// Streams a whole-snapshot [`AppEvent::LlmActivity`] for every in-flight LLM
/// call. Cloned as `Arc<ActivityTracker>` into every site that needs it.
pub struct ActivityTracker {
    inner: Mutex<TrackerInner>,
    tx: mpsc::Sender<AppEvent>,
}

/// RAII scope for one LLM call. Dropping the LAST clone (also on `?`
/// early-return, panic, cancellation) removes the entry and emits one final
/// snapshot.
///
/// `Clone`d freely (e.g. into the stream callback alongside the cloned
/// tracker / `event_tx` in the agent loop); the `refcount` marker makes
/// clones drop independently — an entry only ends when every clone is gone.
#[derive(Clone)]
pub struct ActivityHandle {
    id: u64,
    tracker: Arc<ActivityTracker>,
    refcount: Arc<()>,
}

impl std::fmt::Debug for ActivityHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityHandle").field("id", &self.id).finish_non_exhaustive()
    }
}

impl Drop for ActivityHandle {
    fn drop(&mut self) {
        // `refcount` is still alive inside Drop: strong_count == 1 means this
        // is the last clone.
        if Arc::strong_count(&self.refcount) == 1 {
            self.tracker.drop_handle(self.id);
        }
    }
}

impl ActivityTracker {
    pub fn new(tx: mpsc::Sender<AppEvent>) -> Self {
        Self {
            inner: Mutex::new(TrackerInner {
                next_id: 0,
                entries: HashMap::new(),
            }),
            tx,
        }
    }

    /// Start tracking one LLM call; the returned handle must outlive the
    /// call. `session_id == None` marks a background/global call.
    ///
    /// `self: &Arc<Self>` so the handle can own a clone (callers always hold
    /// the tracker in an `Arc`).
    pub fn begin(self: &Arc<Self>, label: &str, session_id: Option<String>) -> ActivityHandle {
        let now = Instant::now();
        let (id, snapshot) = {
            let mut inner = lock(&self.inner);
            let id = inner.next_id;
            inner.next_id += 1;
            inner.entries.insert(
                id,
                Entry {
                    label: label.to_string(),
                    session_id,
                    pp: None,
                    generating: false,
                    tokens_out: 0,
                    phase_tokens: 0,
                    started: now,
                    phase_started: now,
                    started_at: SystemTime::now(),
                    last_emit: None,
                },
            );
            (id, Self::take_snapshot(&mut inner, now))
        };
        self.send(snapshot);
        ActivityHandle {
            id,
            tracker: Arc::clone(self),
            refcount: Arc::new(()),
        }
    }

    /// Feed one prompt-processing progress update.
    pub fn pp(&self, handle: &ActivityHandle, progress: &PromptProgress) {
        self.update(handle.id, false, |e, _| {
            e.pp = Some(*progress);
        });
    }

    /// Count `n_tokens` generated tokens.
    pub fn tg(&self, handle: &ActivityHandle, n_tokens: u32) {
        self.update(handle.id, false, |e, now| {
            e.tokens_out = e.tokens_out.saturating_add(n_tokens);
            if !e.generating {
                e.generating = true;
                e.phase_tokens = 0;
                e.phase_started = now;
            }
            e.phase_tokens = e.phase_tokens.saturating_add(n_tokens);
        });
    }

    /// Mark the call finished (overrides the token count when `Some`) and
    /// force one immediate snapshot. The entry stays until the handle drops.
    pub fn finish(&self, handle: &ActivityHandle, tokens_out: Option<u32>) {
        self.update(handle.id, true, |e, _| {
            if let Some(n) = tokens_out {
                e.tokens_out = n;
            }
        });
    }

    /// Remove an entry (RAII drop) and force one final snapshot.
    fn drop_handle(&self, id: u64) {
        let snapshot = {
            let mut inner = lock(&self.inner);
            if inner.entries.remove(&id).is_none() {
                return;
            }
            Self::take_snapshot(&mut inner, Instant::now())
        };
        self.send(snapshot);
    }

    /// Apply an update to one entry; emit a snapshot when the entry's
    /// per-activity throttle allows (or `force` is set — terminal events
    /// always emit immediately).
    fn update(&self, id: u64, force: bool, f: impl FnOnce(&mut Entry, Instant)) {
        let emit = {
            let mut inner = lock(&self.inner);
            let now = Instant::now();
            match inner.entries.get_mut(&id) {
                Some(e) => {
                    f(e, now);
                    force || e.last_emit.is_none_or(|t| now.duration_since(t) >= EMIT_INTERVAL)
                }
                None => false, // handle already dropped: nothing to track
            }
        };
        if emit {
            let snapshot = {
                let mut inner = lock(&self.inner);
                Self::take_snapshot(&mut inner, Instant::now())
            };
            self.send(snapshot);
        }
    }

    /// Build the full snapshot (stable order by id) and stamp every entry's
    /// `last_emit` so the emitted event is the last one for ≤200 ms.
    fn take_snapshot(inner: &mut TrackerInner, now: Instant) -> Vec<LlmActivityInfo> {
        for e in inner.entries.values_mut() {
            e.last_emit = Some(now);
        }
        let mut v: Vec<(u64, LlmActivityInfo)> = inner
            .entries
            .iter()
            .map(|(id, e)| to_info(*id, e, now))
            .collect();
        v.sort_by_key(|(id, _)| *id);
        v.into_iter().map(|(_, info)| info).collect()
    }

    fn send(&self, snapshot: Vec<LlmActivityInfo>) {
        let _ = self.tx.send(AppEvent::LlmActivity {
            activities: snapshot,
        });
    }
}

/// A [`LlmClient`] that instruments every call with an [`ActivityTracker`]
/// (begin → per-chunk `tg` → `finish`, drop on return). The wrapped client
/// is unchanged; use this at call sites that hold a `dyn LlmClient` +
/// tracker (or via [`ChatClientAdapter::labeled`](crate::llm::ChatClientAdapter::labeled)).
pub struct LabeledLlm {
    inner: Arc<dyn LlmClient>,
    tracker: Arc<ActivityTracker>,
    label: String,
    session_id: Option<String>,
}

impl LabeledLlm {
    pub fn new(
        inner: Arc<dyn LlmClient>,
        tracker: Arc<ActivityTracker>,
        label: impl Into<String>,
        session_id: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner,
            tracker,
            label: label.into(),
            session_id,
        })
    }
}

#[async_trait]
impl LlmClient for LabeledLlm {
    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        let handle = self.tracker.begin(&self.label, self.session_id.clone());
        let result = self.inner.complete(messages).await;
        self.tracker.finish(&handle, None);
        result
    }

    async fn complete_with_usage(
        &self,
        messages: &[Message],
    ) -> Result<(String, Option<Usage>), String> {
        let handle = self.tracker.begin(&self.label, self.session_id.clone());
        let result = self.inner.complete_with_usage(messages).await;
        self.tracker.finish(&handle, None);
        result
    }

    async fn stream(
        &self,
        messages: &[Message],
        mut chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
    ) -> Result<String, String> {
        let handle = self.tracker.begin(&self.label, self.session_id.clone());
        let tracker = Arc::clone(&self.tracker);
        let chunk_handle = handle.clone();
        let handler = Box::new(move |chunk: String| {
            chunk_handler(chunk);
            tracker.tg(&chunk_handle, 1);
        });
        let result = self.inner.stream(messages, handler).await;
        self.tracker.finish(&handle, None);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(processed: u32, total: u32, time_ms: f64) -> PromptProgress {
        PromptProgress {
            total,
            cache: 0,
            processed,
            time_ms,
        }
    }

    /// Drain all currently queued `LlmActivity` snapshots (the channel is fed
    /// only by the tracker under test, so no other events can arrive).
    fn drain(rx: &mpsc::Receiver<AppEvent>) -> Vec<Vec<LlmActivityInfo>> {
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(AppEvent::LlmActivity { activities }) => out.push(activities),
                Ok(_) => panic!("unexpected non-LlmActivity event"),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    #[test]
    fn raii_drop_without_finish_removes_entry_and_emits_final_snapshot() {
        let (tx, rx) = mpsc::channel::<AppEvent>();
        let tracker = Arc::new(ActivityTracker::new(tx));

        let h = tracker.begin("judge", Some("s1".into()));
        tracker.pp(&h, &progress(10, 100, 50.0));
        drop(h);

        let snaps = drain(&rx);
        // begin snapshot + throttled pp (first pp always emits) + drop.
        assert!(snaps.len() >= 2);
        assert_eq!(snaps.last().unwrap(), &Vec::<LlmActivityInfo>::new());
        // no further emissions after the drop
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn finish_forces_immediate_snapshot_with_token_count() {
        let (tx, rx) = mpsc::channel::<AppEvent>();
        let tracker = Arc::new(ActivityTracker::new(tx));
        let h = tracker.begin("memory", None);
        let _ = drain(&rx);

        tracker.tg(&h, 5);
        // tg within the throttle window right after begin: may or may not
        // emit; drain either way.
        let _ = drain(&rx);
        tracker.tg(&h, 5);
        tracker.finish(&h, Some(42));

        let snaps = drain(&rx);
        let last = snaps.last().unwrap().clone();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].label, "memory");
        assert_eq!(last[0].tokens_out, 42);
        assert_eq!(last[0].phase, ActivityPhase::Generating);
        drop(h);
        assert_eq!(drain(&rx).last().unwrap().len(), 0);
    }

    #[test]
    fn throttle_bounds_rapid_pp_updates() {
        let (tx, rx) = mpsc::channel::<AppEvent>();
        let tracker = Arc::new(ActivityTracker::new(tx));
        let h = tracker.begin("agent: coder", Some("s9".into()));
        let _ = drain(&rx); // begin snapshot

        for i in 0..50 {
            tracker.pp(&h, &progress(i + 1, 1000, 10.0));
        }
        let snaps = drain(&rx);
        // begin already drained and stamped last_emit: 50 rapid pp calls
        // (<< 200 ms total) fit inside the throttle window → at most 1
        // extra emission (if the test was slow enough to cross a window).
        assert!(snaps.len() <= 1, "emitted {snaps:?}");
        assert!(snaps.iter().all(|s| s[0].label == "agent: coder"));

        // finish always emits immediately, even inside the window.
        tracker.finish(&h, None);
        let snaps = drain(&rx);
        assert!(snaps.last().unwrap()[0].label == "agent: coder");
        drop(h);
    }

    #[test]
    fn snapshot_contains_all_concurrent_entries_independently() {
        let (tx, rx) = mpsc::channel::<AppEvent>();
        let tracker = Arc::new(ActivityTracker::new(tx));
        let _ = drain(&rx); // no entries yet — begin emits below

        let a = tracker.begin("judge", None);
        let b = tracker.begin("memory", None);
        tracker.pp(&a, &progress(10, 100, 1000.0));
        tracker.tg(&b, 7);
        let _ = drain(&rx);

        // Force emission by finishing a; the snapshot must carry BOTH.
        tracker.finish(&a, None);
        let snaps = drain(&rx);
        let last = snaps.last().unwrap().clone();
        assert_eq!(last.len(), 2);
        let judge = last.iter().find(|i| i.label == "judge").unwrap();
        let memory = last.iter().find(|i| i.label == "memory").unwrap();
        assert!(matches!(
            judge.phase,
            ActivityPhase::PromptProcessing {
                processed: 10,
                total: 100,
                ..
            }
        ));
        assert_eq!(memory.phase, ActivityPhase::Generating);
        assert_eq!(memory.tokens_out, 7);

        drop(a);
        drop(b);
        let snaps = drain(&rx);
        assert!(snaps.last().unwrap().is_empty());
    }

    /// Minimal mock LlmClient: returns a canned response and counts calls.
    #[derive(Default)]
    struct MockLlm {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl LlmClient for MockLlm {
        async fn complete(&self, _messages: &[Message]) -> Result<String, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("mock".to_string())
        }
        async fn stream(
            &self,
            _messages: &[Message],
            mut chunk_handler: Box<dyn FnMut(String) + Send + Sync + 'static>,
        ) -> Result<String, String> {
            chunk_handler("a".into());
            chunk_handler("b".into());
            Ok("ab".to_string())
        }
    }

    #[tokio::test]
    async fn labeled_llm_instruments_complete_and_stream() {
        let (tx, rx) = mpsc::channel::<AppEvent>();
        let tracker = Arc::new(ActivityTracker::new(tx));
        let inner: Arc<dyn LlmClient> = Arc::new(MockLlm::default());
        let client = LabeledLlm::new(inner, tracker, "improvement", None);
        let _ = drain(&rx);

        // complete: begin snapshot carries the label + session_id (None),
        // finish snapshot is terminal.
        let out = client.complete(&[]).await.unwrap();
        assert_eq!(out, "mock");
        let snaps = drain(&rx);
        // begin + finish + drop: the terminal (drop) snapshot is empty.
        assert!(snaps.len() >= 3);
        assert!(snaps
            .iter()
            .all(|s| {
                s.len() <= 1
                    && s.iter().all(|i| {
                        i.label == "improvement" && i.session_id.is_none()
                    })
            }));

        // stream: two chunks → tg(1) twice → Generating + tokens_out 2.
        let chunks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let chunks_cb = chunks.clone();
        let out = client
            .stream(
                &[],
                Box::new(move |_| {
                    chunks_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }),
            )
            .await
            .unwrap();
        assert_eq!(out, "ab");
        assert_eq!(
            chunks.load(std::sync::atomic::Ordering::SeqCst),
            2
        );
        let snaps = drain(&rx);
        let last = snaps.last().unwrap();
        // terminal snapshot after stream: empty (handle dropped) — but the
        // finish snapshot (forced) must have shown Generating with 2 tokens.
        assert!(snaps
            .iter()
            .any(|s| s.iter().any(|i| {
                i.phase == ActivityPhase::Generating && i.tokens_out == 2
            })));
        assert!(last.is_empty());
    }
}
