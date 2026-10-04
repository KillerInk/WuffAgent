//! The Telegram bot: an in-process long-poll (no extra exe).
//!
//! Thread model (all started by `start`, all signalled by `stop`):
//!
//! * **poller thread** — blocks in `getUpdates` (long-poll, up to
//!   `poll_timeout_secs`), forwards each batch over an mpsc channel. Handles
//!   401 (stop), 409 (30 s backoff), 5xx/network (5 s backoff).
//! * **worker thread** — consumes update batches + the event relay: slash
//!   commands, free-text dispatch (`HostApi::inject_user_message`), reply
//!   accumulation from pipeline events, `sendMessage` (chunked).
//!
//! The pipeline event callback runs on the WuffAgent UI thread and does
//! exactly one thing: forward `(kind, session_id, payload)` into an mpsc
//! (`EVENT_TX`) — no allocation-heavy work, no host calls.
//!
//! Session routing: each chat id has a persisted pointer
//! (`chat_id -> (session_id, session_name)`). One in-flight turn per chat;
//! a second free-text message while busy is queued (max 1 per chat). Events
//! are matched against the in-flight `(chat_id, session_id)` pair recorded
//! at send time, so a `/use` mid-turn does not misroute the old turn's
//! trailing events.

use crate::config::{BotStateFile, ChatState, Config};
use crate::log;
use crate::tg::{self, TelegramApi, TgError, Update};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use wuffagent_core::tools::types::{HostApi, HostEventCallback};

/// Pipeline event relayed to the worker (kind per the host-bridge contract:
/// 0 = chunk, 1 = complete, 2 = error, 3 = round-complete).
#[derive(Clone, Debug)]
pub struct Event {
    pub kind: u32,
    pub session_id: String,
    pub payload: String,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}


fn bytes_to_string(p: *const u8, len: usize) -> String {
    if p.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: the host copies the string for the duration of the call.
    let slice = unsafe { std::slice::from_raw_parts(p, len) };
    String::from_utf8_lossy(slice).into_owned()
}

fn c_string(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Typed wrapper over the host vtable (field-of-fn-type calls need the
/// parenthesized form; this keeps that noise in one place).
pub struct Host {
    pub api: &'static HostApi,
}

impl Host {
    pub fn new(api: &'static HostApi) -> Self {
        Self { api }
    }

    pub fn inject(&self, sid: &str, text: &str) -> bool {
        (self.api.inject_user_message)(sid.as_ptr(), sid.len(), text.as_ptr(), text.len())
    }

    pub fn create(&self, name: &str) -> Option<String> {
        let mut out = [0u8; 64];
        (self.api.create_session)(name.as_ptr(), name.len(), out.as_mut_ptr(), out.len())
            .then(|| c_string(&out))
    }

    pub fn resolve(&self, query: &str) -> Option<String> {
        let mut out = [0u8; 64];
        (self.api.resolve_session)(query.as_ptr(), query.len(), out.as_mut_ptr(), out.len())
            .then(|| c_string(&out))
    }

    pub fn switch(&self, sid: &str) -> bool {
        (self.api.switch_session)(sid.as_ptr(), sid.len())
    }

    pub fn count(&self) -> usize {
        (self.api.session_count)()
    }

    pub fn get(&self, index: usize) -> Option<(String, String)> {
        let mut id = [0u8; 64];
        let mut name = [0u8; 64];
        (self.api
            .get_session)(index, id.as_mut_ptr(), id.len(), name.as_mut_ptr(), name.len())
            .then(|| (c_string(&id), c_string(&name)))
    }

    /// All sessions as `(id, name)` pairs (sorted by id — the host-side
    /// `get_session` returns them sorted).
    pub fn list(&self) -> Vec<(String, String)> {
        (0..self.count()).filter_map(|i| self.get(i)).collect()
    }

    pub fn register_event_callback(&self, cb: HostEventCallback, user_data: *mut c_void) {
        (self.api.register_event_callback)(cb, user_data)
    }
}

struct Inflight {
    chat_id: i64,
    session_id: String,
}

pub struct BotState {
    pub cfg: Config,
    pub dir: PathBuf,
    pub api: TelegramApi,
    /// chat_id -> session pointer (seeded from the state file at start).
    chats: Mutex<BTreeMap<i64, ChatState>>,
    /// chat_id -> in-flight turn's session (recorded at send time).
    inflight: Mutex<Vec<Inflight>>,
    /// chat_id -> one queued free-text message (max 1 per chat).
    queued: Mutex<BTreeMap<i64, String>>,
    /// The chat whose turn was injected last (`use_session` target).
    active_chat: Mutex<Option<i64>>,
    /// chat_id -> accumulated streaming reply text.
    buffers: Mutex<BTreeMap<i64, String>>,
    /// Last seen Telegram update_id (persisted; the getUpdates offset).
    last_update_id: Mutex<i64>,
    stop: AtomicBool,
    running: AtomicBool,
    last_error: Mutex<String>,
    last_poll: Mutex<Instant>,
}

static BOT: Mutex<Option<Arc<BotState>>> = Mutex::new(None);
static EVENT_TX: Mutex<Option<mpsc::Sender<Event>>> = Mutex::new(None);
/// Serializes `start_inner` (its wait-then-set is not atomic) — without it
/// two concurrent starts could both pass the `running()` check, and the
/// loser's `OnceLock::set` failure would leave a bot with dead threads.
static START_LOCK: Mutex<()> = Mutex::new(());

pub fn running() -> bool {
    BOT.lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|b| b.running.load(Ordering::Relaxed))
        .unwrap_or(false)
}


/// Start the bot threads. `dir` is where `telegram.json`/`telegram-state.json`
/// live (`~/.wuffagent` in normal operation; a temp dir in tests).
pub fn start(cfg: Config, dir: PathBuf) -> Result<String, String> {
    start_inner(&cfg, dir, true)
}

/// Build the API client for a config; a test override (`set_api_for_testing`)
/// wins when set — the e2e test injects a pool-less `reqwest` client so its
/// per-request timeout overrides (see `TelegramApi::get_updates`) actually
/// apply (reqwest's builder `.timeout()` is silently ignored by a pooled
/// connection whose idle time is unbounded).
pub fn set_api_for_testing(api: Option<fn(&Config) -> TelegramApi>) {
    *TEST_API_OVERRIDE.lock().unwrap() = api;
}

fn build_api(cfg: &Config) -> TelegramApi {
    let f = *TEST_API_OVERRIDE.lock().unwrap();
    match f {
        Some(f) => f(cfg),
        None => TelegramApi::new(&cfg.token, cfg.api_base.as_deref(), cfg.poll_timeout_secs),
    }
}

static TEST_API_OVERRIDE: Mutex<Option<fn(&Config) -> TelegramApi>> = Mutex::new(None);
/// Test flag: when true, `try_autostart` is a no-op (the e2e test sets it
/// before `wuff_tool_host_api` so its explicit `bot::start` is the only
/// path — autostart would race it and the test's stop-wait loop would hang
/// on a bot the test didn't start).
pub static AUTOSTART_DISABLED: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

fn start_inner(cfg: &Config, dir: PathBuf, log_started: bool) -> Result<String, String> {
    cfg.validate().map_err(|e| format!("config: {e}"))?;
    let _guard = START_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if BOT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some()
    {
        return Err("bot already registered (stop it first)".into());
    }
    let api_ref = crate::host_api()
        .ok_or_else(|| "host API unavailable (WuffAgent build too old?) — bot needs session access".to_string())?;
    let host = Host::new(api_ref);

    std::fs::create_dir_all(&dir).map_err(|err| format!("create {}: {err}", dir.display()))?;
    let file = BotStateFile::load(&dir);

    // Replace any previous instance's event channel, then its host callback:
    // the callback holds a `Sender` clone (the worker's `Rx` keeps it
    // connected until the worker exits), so replacing it before spawning the
    // new threads is what makes `stop()` actually release the event relay.
    let (event_tx, event_rx) = mpsc::channel::<Event>();
    *EVENT_TX.lock().unwrap() = Some(event_tx);

    // Register the pipeline event callback (single slot; replaces previous).
    host.register_event_callback(telegram_event_cb, std::ptr::null_mut());

    let mut api = build_api(&cfg);
    api.host = Some(api_ref);
    let state = Arc::new(BotState {
        api,
        chats: Mutex::new(
            file
                .chats
                .into_iter()
                .filter_map(|(k, v)| k.parse::<i64>().ok().map(|id| (id, v)))
                .collect(),
        ),
        inflight: Mutex::new(Vec::new()),
        queued: Mutex::new(BTreeMap::new()),
        active_chat: Mutex::new(None),
        buffers: Mutex::new(BTreeMap::new()),
        last_update_id: Mutex::new(file.last_update_id),
        stop: AtomicBool::new(false),
        running: AtomicBool::new(true),
        last_error: Mutex::new(String::new()),
        last_poll: Mutex::new(Instant::now()),
        cfg: cfg.clone(),
        dir,
    });
    // Unreachable under START_LOCK (the check above), kept as a belt-and-braces
    // guard: if it ever fires, roll back the event wiring.
    let mut bot_slot = BOT.lock().unwrap_or_else(|p| p.into_inner());
    debug_assert!(bot_slot.is_none());
    *bot_slot = Some(state.clone());
    drop(bot_slot);

    let (updates_tx, updates_rx) = mpsc::channel::<Vec<Update>>();
    let poller_state = state.clone();
    std::thread::Builder::new()
        .name("telegram-poller".into())
        .spawn(move || poller_thread(poller_state, updates_tx))
        .map_err(|e| format!("spawn poller: {e}"))?;
    let worker_state = state.clone();
    std::thread::Builder::new()
        .name("telegram-worker".into())
        .spawn(move || worker_thread(worker_state, updates_rx, event_rx))
        .map_err(|e| {
            state.stop.store(true, Ordering::Relaxed);
            format!("spawn worker: {e}")
        })?;

    if log_started {
        log::log(
            &state.dir,
            &format!(
                "started ({} chat(s) in allowlist, {} persisted chat pointer(s), offset {})",
                state.cfg.allow_chat_ids.len(),
                state.chats.lock().unwrap().len(),
                state.last_update_id.lock().unwrap(),
            ),
        );
    }
    Ok(format!("Telegram bot started (token prefix {:?})", token_prefix(&state.cfg.token)))
}

/// Signal both threads to stop. The poller finishes its current long-poll
/// (≤ `poll_timeout_secs` — shortened to 2 s once stop is requested), so the
/// threads exit within a couple of seconds.
pub fn stop() -> String {
    let state = BOT.lock().unwrap_or_else(|p| p.into_inner()).take();
    match state {
        Some(state) if state.running.load(Ordering::Relaxed) => {
            state.stop.store(true, Ordering::Relaxed);
            log::log(&state.dir, "stop requested");
            // Detach the pipeline event callback now (not when the worker
            // exits, which can take up to a poll cycle): it holds a
            // `Sender` clone the UI calls per pipeline event, and keeping it
            // up would relay events to a dead relay until then.
            if let Some(api) = state.api.host {
                Host::new(api).register_event_callback(telegram_event_cb, std::ptr::null_mut());
            }
            "Telegram bot stopping (threads exit within a few seconds)".into()
        }
        Some(_) => "Telegram bot is not running (slot cleared)".into(),
        None => "Telegram bot was never started".into(),
    }
}

pub fn token_prefix(token: &str) -> String {
    match token.split_once(':') {
        Some((id, _)) => format!("{id}."),
        None => "??.".to_string(),
    }
}

pub fn status_json() -> serde_json::Value {
    let Some(state) = BOT.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
        return serde_json::json!({ "running": false, "note": "never started" });
    };
    let last_poll = *state.last_poll.lock().unwrap();
    serde_json::json!({
        "running": state.running.load(Ordering::Relaxed),
        "token_prefix": token_prefix(&state.cfg.token),
        "allow_chat_ids": state.cfg.allow_chat_ids,
        "last_error": state.last_error.lock().unwrap().clone(),
        "last_poll_secs_ago": last_poll.elapsed().as_secs(),
        "chats": state.chats.lock().unwrap().len(),
    })
}

/// `use_session` tool action: resolve a session and point the active
/// Telegram chat at it (no-op chat switch when no telegram turn is active —
/// the resolved id is still reported).
pub fn use_session(query: &str) -> Result<serde_json::Value, String> {
    let host = crate::host_api().map(Host::new).ok_or("host API unavailable")?;
    let id = host.resolve(query).ok_or_else(|| format!("no session matching {query:?}"))?;
    let name = host
        .list()
        .iter()
        .find_map(|(sid, n)| (sid == &id).then_some(n.clone()));
    let state = BOT.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let switched_chat = if let Some(state) = &state {
        if state.running.load(Ordering::Relaxed) {
            let chat = *state.active_chat.lock().unwrap();
            if let Some(chat) = chat {
                set_chat(state, &host, chat, &id, name.as_deref().unwrap_or(&id));
                Some(chat)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    Ok(serde_json::json!({
        "session_id": id,
        "session_name": name,
        "switched_chat": switched_chat,
    }))
}

/// `new_session` tool action.
pub fn new_session(name: &str) -> Result<serde_json::Value, String> {
    let host = crate::host_api().map(Host::new).ok_or("host API unavailable")?;
    let id = host.create(name).ok_or_else(|| "session creation failed (host busy?)".to_string())?;
    Ok(serde_json::json!({ "session_id": id, "name": name }))
}

/// `list_sessions` tool action.
pub fn list_sessions() -> Result<serde_json::Value, String> {
    let host = crate::host_api().map(Host::new).ok_or("host API unavailable")?;
    let count = host.count();
    let sessions: Vec<serde_json::Value> = (0..count)
        .filter_map(|i| host.get(i))
        .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
        .collect();
    Ok(serde_json::json!({ "count": count, "sessions": sessions }))
}

/// `send` tool action.
pub fn send_message(chat_id: i64, text: &str) -> Result<serde_json::Value, String> {
    let Some(state) = BOT.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
        return Err("bot not started".into());
    };
    let chunks = tg::chunk_text(text, state.cfg.chunk_chars);
    for chunk in &chunks {
        state
            .api
            .send_message(chat_id, chunk)
            .map_err(|e| format!("sendMessage to {chat_id}: {}", e.to_string()))?;
    }
    Ok(serde_json::json!({ "sent_chunks": chunks.len() }))
}

/// The pipeline event callback — runs on the WuffAgent **UI thread**.
/// Contract: forward to the mpsc and return; no other work.
pub extern "C" fn telegram_event_cb(
    kind: u32,
    sid: *const u8,
    sid_len: usize,
    payload: *const u8,
    payload_len: usize,
    _user_data: *mut c_void,
) {
    let ev = Event {
        kind,
        session_id: bytes_to_string(sid, sid_len),
        payload: bytes_to_string(payload, payload_len),
    };
    if let Some(tx) = EVENT_TX.lock().unwrap().clone() {
        let _ = tx.send(ev);
    }
}

fn set_error(state: &BotState, msg: &str) {
    log::log(&state.dir, msg);
    *state.last_error.lock().unwrap() = msg.to_string();
    // The plugin's `tracing!` goes to its own (unset) global dispatcher and
    // is silently dropped (README: dual-linking note) — mirror the error to
    // the console so it is visible when running from a terminal.
    eprintln!("telegram_plugin error: {msg}");
}

fn set_chat(state: &BotState, host: &Host, chat_id: i64, sid: &str, name: &str) {
    state
        .chats
        .lock()
        .unwrap()
        .insert(chat_id, ChatState {
            session_id: sid.to_string(),
            session_name: name.to_string(),
        });
    let _ = host.switch(sid); // desktop follows along (best effort)
    save_state(state);
}

fn save_state(state: &BotState) {
    let chats = state.chats.lock().unwrap();
    let last = *state.last_update_id.lock().unwrap();
    let file = BotStateFile {
        chats: chats
            .iter()
            .map(|(id, cs)| (id.to_string(), cs.clone()))
            .collect(),
        last_update_id: last,
    };
    if let Err(e) = file.save(&state.dir) {
        eprintln!("telegram_plugin: state save: {e}");
    }
}

fn poller_thread(state: Arc<BotState>, updates_tx: mpsc::Sender<Vec<Update>>) {
    loop {
        if state.stop.load(Ordering::Relaxed) {
            break;
        }
        // Shorten the hold once stop is requested so shutdown is quick.
        let hold = if state.stop.load(Ordering::Relaxed) {
            2
        } else {
            state.cfg.poll_timeout_secs
        };
        let offset = *state.last_update_id.lock().unwrap();
        match state.api.get_updates(offset, hold) {
            Ok(updates) => {
                *state.last_poll.lock().unwrap() = Instant::now();
                *state.last_error.lock().unwrap() = String::new();
                if updates.is_empty() {
                    continue;
                }
                let next = tg::next_offset(&updates, offset);
                *state.last_update_id.lock().unwrap() = next;
                save_state(&state);
                if updates_tx.send(updates).is_err() {
                    break; // worker gone
                }
            }
            Err(TgError::BadToken) => {
                set_error(&state, "401: bad token — bot stopped");
                state.stop.store(true, Ordering::Relaxed);
                break;
            }
            Err(TgError::Conflict) => {
                set_error(
                    &state,
                    "409: another poller is active (another WuffAgent build running?) — retrying in 30 s",
                );
                sleep_interruptible(&state, Duration::from_secs(30));
            }
            Err(e) => {
                set_error(&state, &format!("getUpdates: {} — retrying in 5 s", e.to_string()));
                sleep_interruptible(&state, Duration::from_secs(5));
            }
        }
    }
    state.running.store(false, Ordering::Relaxed);
}

fn sleep_interruptible(state: &BotState, d: Duration) {
    let mut left = d;
    while left > Duration::ZERO {
        if state.stop.load(Ordering::Relaxed) {
            break;
        }
        let step = left.min(Duration::from_millis(250));
        std::thread::sleep(step);
        left -= step;
    }
}

fn worker_thread(
    state: Arc<BotState>,
    updates_rx: mpsc::Receiver<Vec<Update>>,
    event_rx: mpsc::Receiver<Event>,
) {
    let host = match crate::host_api().map(Host::new) {
        Some(h) => h,
        None => {
            state.running.store(false, Ordering::Relaxed);
            return;
        }
    };
    let mut updates_disconnected = false;
    let mut events_disconnected = false;
    loop {
        // Drain relayed pipeline events first (fast path keeps replies flowing
        // even while a long-poll is in flight). The `try_recv` loop is
        // re-checked after EVERY handled event — a `handle_event` that takes
        // a 2 s send-retry must not starve the drain of events queued behind
        // it (they would only be picked up after the next 100 ms update
        // timeout, and a reply can be missed entirely in the meantime).
        if !events_disconnected {
            loop {
                match event_rx.try_recv() {
                    Ok(ev) => {
                        handle_event(&state, &host, ev);
                        continue;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        events_disconnected = true;
                        break;
                    }
                }
            }
        }
        if updates_disconnected && events_disconnected {
            break;
        }
        if !updates_disconnected {
            match updates_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(batch) => {
                    for u in batch {
                        handle_update(&state, &host, &u);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => updates_disconnected = true,
            }
        }
        if state.stop.load(Ordering::Relaxed) {
            break;
        }
    }
    *EVENT_TX.lock().unwrap() = None;
    state.running.store(false, Ordering::Relaxed);
}

/// Slash-command grammar. Free text (no leading `/`) -> `Unknown(text)`.
pub enum Command {
    New(Option<String>),
    Sessions,
    Use(String),
    Current,
    Help,
    Unknown(String),
}

pub fn parse_command(text: &str) -> Command {
    let text = text.trim();
    if !text.starts_with('/') {
        return Command::Unknown(text.to_string());
    }
    let (head, rest) = match text.split_once(' ') {
        Some((h, r)) => (h, r.trim()),
        None => (text, ""),
    };
    // Tolerate the Telegram bot mention suffix (/new@MyBot).
    let head = head.split('@').next().unwrap_or(head).to_lowercase();
    match head.as_str() {
        "/new" => Command::New(if rest.is_empty() { None } else { Some(rest.to_string()) }),
        "/sessions" => Command::Sessions,
        "/use" => {
            if rest.is_empty() {
                Command::Unknown(text.to_string())
            } else {
                Command::Use(rest.to_string())
            }
        }
        "/current" => Command::Current,
        "/help" => Command::Help,
        _ => Command::Unknown(text.to_string()),
    }
}

pub fn is_allowed(allow: &[i64], chat_id: i64) -> bool {
    allow.contains(&chat_id)
}

fn handle_update(state: &Arc<BotState>, host: &Host, u: &Update) {
    if !is_allowed(&state.cfg.allow_chat_ids, u.chat_id) {
        log::log(
            &state.dir,
            &format!("update {} from chat {} ignored (not in allowlist)", u.id, u.chat_id),
        );
        return;
    }
    log::log(
        &state.dir,
        &format!("update {} from {}: {:?}", u.id, u.chat_id, u.text),
    );
    match parse_command(&u.text) {
        Command::New(name) => {
            let name = name.unwrap_or_else(|| format!("Telegram-{}", now_secs()));
            match host.create(&name) {
                Some(id) => {
                    set_chat(state, host, u.chat_id, &id, &name);
                    send_reply(state, u.chat_id, &format!("created session “{name}” (id {id})"));
                }
                None => send_reply(state, u.chat_id, "could not create the session (host busy?) — try again"),
            }
        }
        Command::Sessions => {
            let count = host.count();
            let mut lines = Vec::new();
            for i in 0..count.min(25) {
                if let Some((id, name)) = host.get(i) {
                    lines.push(format!("{i}. {name} ({id})"));
                }
            }
            if count > 25 {
                lines.push(format!("…and {} more", count - 25));
            }
            if lines.is_empty() {
                lines.push("(no sessions)".into());
            } else {
                lines.insert(0, "sessions:".into());
            }
            send_reply(state, u.chat_id, &lines.join("\n"));
        }
        Command::Use(query) => match host.resolve(&query) {
            Some(id) => {
                let name = host
                    .list()
                    .iter()
                    .find_map(|(sid, n)| (sid == &id).then_some(n.clone()))
                    .unwrap_or_else(|| id.clone());
                set_chat(state, host, u.chat_id, &id, &name);
                send_reply(state, u.chat_id, &format!("using session “{name}” (id {id})"));
            }
            None => send_reply(state, u.chat_id, &format!("no session matching {query:?}")),
        },
        Command::Current => {
            let current = state.chats.lock().unwrap().get(&u.chat_id).cloned();
            let msg = match current {
                Some(cs) => format!("current: “{}” (id {})", cs.session_name, cs.session_id),
                None => "current: (none — send a message to start the default session)".to_string(),
            };
            send_reply(state, u.chat_id, &msg);
        }
        Command::Help => {
            send_reply(
                state,
                u.chat_id,
                "commands:\n/new [name] — new session and use it\n/use <name or id> — switch session\n/sessions — list sessions\n/current — show this chat's session\n/help — this help\n\nanything else is sent to the current session",
            );
        }
        Command::Unknown(text) => {
            if text.starts_with('/') {
                send_reply(state, u.chat_id, &format!("unknown command {text:?} — try /help"));
            } else {
                dispatch_free_text(state, host, u.chat_id, &text);
            }
        }
    }
    drain_queued(state, host);
}

fn dispatch_free_text(state: &Arc<BotState>, host: &Host, chat_id: i64, text: &str) {
    if state.inflight.lock().unwrap().iter().any(|f| f.chat_id == chat_id) {
        let mut queued = state.queued.lock().unwrap();
        if queued.contains_key(&chat_id) {
            send_reply(state, chat_id, "still working — try again in a moment");
        } else {
            queued.insert(chat_id, text.to_string());
            send_reply(state, chat_id, "got it (I'll continue with it once the current request is done)");
        }
        return;
    }
    // Resolve (or create) this chat's session. The entry is CLONED out first:
    // the guard's temporary would otherwise stay alive across the whole match
    // (the `Some(cs)` arm borrows the map), and re-locking `chats` in the
    // `None` arm would self-deadlock on the non-reentrant mutex.
    let existing = state.chats.lock().unwrap().get(&chat_id).cloned();
    let (sid, name) = match existing {
        Some(cs) => (cs.session_id, cs.session_name),
        None => match host.create(&state.cfg.default_session) {
            Some(id) => {
                // Insert without the extra switch/save churn of set_chat — the
                // pointer is new; the switch is a no-op nicety either way.
                // (Safe: `existing` was cloned out, so no `chats` guard is
                // held here.)
                state.chats.lock().unwrap().insert(
                    chat_id,
                    ChatState {
                        session_id: id.clone(),
                        session_name: state.cfg.default_session.clone(),
                    },
                );
                save_state(state);
                (id, state.cfg.default_session.clone())
            }
            None => {
                send_reply(state, chat_id, "could not start a session (host busy?) — try /new");
                return;
            }
        },
    };
    let inj_ok = host.inject(&sid, text);
    if !inj_ok {
        // The session may have been deleted in the GUI — clear the pointer.
        state.chats.lock().unwrap().remove(&chat_id);
        save_state(state);
        send_reply(
            state,
            chat_id,
            &format!("could not reach session “{name}” (deleted in WuffAgent?) — send /new to start fresh"),
        );
        return;
    }
    state
        .inflight
        .lock()
        .unwrap()
        .push(Inflight {
            chat_id,
            session_id: sid,
        });
    *state.active_chat.lock().unwrap() = Some(chat_id);
    state.buffers.lock().unwrap().remove(&chat_id);
    state.api.send_chat_action(chat_id);
}

fn handle_event(state: &Arc<BotState>, host: &Host, ev: Event) {
    // Route the event to the chat whose in-flight turn is in THIS session
    // (recorded at send time — a /use mid-turn cannot misroute trailing events).
    let chat = state
        .inflight
        .lock()
        .unwrap()
        .iter()
        .find(|f| f.session_id == ev.session_id)
        .map(|f| f.chat_id);
    let Some(chat) = chat else { return };
    match ev.kind {
        0 => {
            state
                .buffers
                .lock()
                .unwrap()
                .entry(chat)
                .or_default()
                .push_str(&ev.payload);
        }
        1 => {
            let accumulated = state
                .buffers
                .lock()
                .unwrap()
                .remove(&chat)
                .unwrap_or_default();
            let final_text = if ev.payload.is_empty() {
                accumulated
            } else if accumulated.is_empty() {
                ev.payload.clone()
            } else {
                // The complete event's content is the final text; the chunks
                // are the same stream — prefer the complete payload, fall back
                // to the buffer. (They should be equal; dedupe defensively.)
                if ev.payload.contains(&accumulated) || accumulated.contains(&ev.payload) {
                    ev.payload
                } else {
                    format!("{}\n\n{}", accumulated, ev.payload)
                }
            };
            finish_turn(state, host, chat);
            if !final_text.trim().is_empty() {
                send_reply(state, chat, &final_text);
            }
        }
        2 => {
            finish_turn(state, host, chat);
            send_reply(state, chat, &format!("⚠️ run failed: {}", ev.payload));
        }
        3 => {} // round-complete: nothing to send (the final answer arrives with kind 1)
        _ => {}
    }
    drain_queued(state, host);
}

fn finish_turn(state: &BotState, _host: &Host, chat: i64) {
    state
        .inflight
        .lock()
        .unwrap()
        .retain(|f| f.chat_id != chat);
    let mut active = state.active_chat.lock().unwrap();
    if *active == Some(chat) {
        *active = None;
    }
    state.buffers.lock().unwrap().remove(&chat);
}

fn drain_queued(state: &Arc<BotState>, host: &Host) {
    // Sorted iteration = deterministic order across chats. Snapshot each map
    // separately (never hold queued + inflight at once — dispatch takes them
    // in the opposite order).
    let queued_snapshot: Vec<(i64, String)> = state
        .queued
        .lock()
        .unwrap()
        .iter()
        .map(|(c, t)| (*c, t.clone()))
        .collect();
    let pending: Vec<(i64, String)> = {
        let inflight = state.inflight.lock().unwrap();
        queued_snapshot
            .into_iter()
            .filter(|(chat, _)| !inflight.iter().any(|f| f.chat_id == *chat))
            .collect()
    };
    for (chat, text) in pending {
        state.queued.lock().unwrap().remove(&chat);
        dispatch_free_text(state, host, chat, &text);
    }
}

fn send_reply(state: &BotState, chat_id: i64, text: &str) {
    let dir = state.dir.clone();
    let chunks = tg::chunk_text(text, state.cfg.chunk_chars);
    for chunk in chunks {
        match state.api.send_message(chat_id, &chunk) {
            Ok(()) => {}
            // One retry for transient failures (rate limit / 5xx / network).
            Err(TgError::Retry(_)) => {
                log::log(
                    &dir,
                    &format!("sendMessage to {chat_id} failed (retrying in 2 s): {chunk}"),
                );
                std::thread::sleep(Duration::from_secs(2));
                match state.api.send_message(chat_id, &chunk) {
                    Ok(()) => {}
                    Err(e) => set_error(state, &format!("sendMessage to {chat_id}: {}", e.to_string())),
                }
            }
            Err(e) => set_error(state, &format!("sendMessage to {chat_id}: {}", e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_command_variants() {
        assert!(matches!(parse_command("/new"), Command::New(None)));
        assert!(matches!(
            parse_command("/new fix-thing"),
            Command::New(Some(s)) if s == "fix-thing"
        ));
        assert!(matches!(parse_command("/sessions"), Command::Sessions));
        assert!(matches!(
            parse_command("/use 42"),
            Command::Use(s) if s == "42"
        ));
        assert!(matches!(parse_command("/current"), Command::Current));
        assert!(matches!(parse_command("/help"), Command::Help));
        // No argument -> not a valid /use (falls to unknown-command path).
        assert!(matches!(parse_command("/use"), Command::Unknown(_)));
        // Bot mention suffix tolerated.
        assert!(matches!(
            parse_command("/new@MyBot x"),
            Command::New(Some(s)) if s == "x"
        ));
        // Case-insensitive command head.
        assert!(matches!(parse_command("/SESSIONS"), Command::Sessions));
        // Free text.
        assert!(matches!(
            parse_command("hello there"),
            Command::Unknown(s) if s == "hello there"
        ));
        assert!(matches!(parse_command("/unknowncmd"), Command::Unknown(_)));
    }

    #[test]
    fn test_allowlist_filter() {
        let allow = vec![1, 2, 3];
        assert!(is_allowed(&allow, 2));
        assert!(!is_allowed(&allow, 4));
        assert!(!is_allowed(&[], 1), "empty allowlist answers nobody");
    }

    #[test]
    fn test_token_prefix() {
        assert_eq!(token_prefix("123456:ABC"), "123456.");
        assert_eq!(token_prefix("no-colon"), "??.");
    }

    #[test]
    fn test_start_requires_host_api_and_running_flag() {
        // host_api() is process-global; make sure it is not set by a sibling
        // test (tests in one binary share statics — this test only checks the
        // not-set branch when the e2e binary is separate, which it is).
        let cfg = Config {
            token: "1:2".into(),
            allow_chat_ids: vec![1],
            default_session: "T".into(),
            chunk_chars: 4096,
            api_base: None,
            poll_timeout_secs: 1,
        };
        let dir = std::env::temp_dir().join(format!("tg_bot_start_{}", std::process::id()));
        match crate::host_api() {
            None => {
                let err = start(cfg, dir).unwrap_err();
                assert!(err.contains("host API"), "got: {err}");
            }
            Some(_) => {
                // Another test set the host API; exercise the running flag.
                if running() {
                    assert!(start(cfg, dir).unwrap_err().contains("already running"));
                } else {
                    assert!(start(cfg.clone(), dir.clone()).is_ok());
                    assert!(running());
                    let err = start(cfg, dir).unwrap_err();
                    assert!(err.contains("already running"));
                    let _ = stop();
                }
            }
        }
    }
}
