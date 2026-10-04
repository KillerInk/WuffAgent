//! Host-side half of the optional plugin host API (P1 of the plugin plan).
//!
//! Core (P0) hands plugins exporting `wuff_tool_host_api` a `HostApi`
//! vtable pointer. This module implements that vtable: each `extern "C"`
//! fn enqueues a [`HostCommand`] on a channel and waits (up to 5 s) for the
//! UI thread to run it once per frame in `ChatApp::process_host_commands`,
//! which sends a one-shot [`HostReply`] back.
//!
//! Threading: vtable fns run on the plugin's own thread (e.g. a Telegram
//! poller). Commands are executed on the UI thread (it owns `ChatApp`); the
//! plugin thread must never block the UI (hence the timeout). [`emit_event`]
//! runs on the UI thread when pipeline events are dispatched and must stay
//! fast and non-blocking (a registered callback only forwards into an mpsc).

use std::ffi::c_void;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use wuffagent_core::tools::types::{HostApi, HostEventCallback, HOST_API_VERSION};

/// Command timeout (host half of the ABI contract: call + wait ≤ 5 s).
const CMD_TIMEOUT: Duration = Duration::from_secs(5);

/// One-shot reply the UI thread sends back to the plugin thread waiting on a
/// [`HostCommand`].
pub enum HostReply {
    /// The command was executed.
    Ok,
    /// The command produced a session id (create/resolve).
    OkId(String),
    /// The command produced a count.
    OkCount(usize),
    /// The command produced an (id, name) pair (get by index).
    OkSession { id: String, name: String },
    /// The command failed (unknown session, timeout, or UI shutting down).
    Err,
}

/// Commands plugins issue through the host API. The UI thread drains these
/// once per frame (fast, non-blocking operations only).
pub enum HostCommand {
    /// Inject a user message into `sid` (auto-creates the session if missing,
    /// per the ABI).
    Inject {
        sid: String,
        text: String,
        reply: mpsc::Sender<HostReply>,
    },
    /// Create a named session; replies with its id.
    Create {
        name: String,
        reply: mpsc::Sender<HostReply>,
    },
    /// Resolve a session by exact id, or by (case-insensitive) name; replies
    /// with the id.
    Resolve {
        query: String,
        reply: mpsc::Sender<HostReply>,
    },
    /// Switch the UI to the session `sid` (so the desktop follows the bot's
    /// active session).
    Switch {
        sid: String,
        reply: mpsc::Sender<HostReply>,
    },
    /// Replies with the number of sessions in the store.
    Count { reply: mpsc::Sender<HostReply> },
    /// Replies with the (id, name) of the `index`-th session (sorted by id —
    /// deterministic across calls).
    Get { index: usize, reply: mpsc::Sender<HostReply> },
}

/// Process-global command sender: the `extern "C"` vtable fns have no
/// `&self`, so this is the only way to reach the UI-thread queue. Installed
/// once by [`init`] before the first plugin load (bootstrap).
static COMMAND_TX: OnceLock<Mutex<Option<mpsc::Sender<HostCommand>>>> = OnceLock::new();

/// The opaque plugin-side `user_data` pointer: a raw pointer is neither
/// `Send` nor `Sync`, but this slot is only ever read/written on the UI
/// thread and passed back verbatim to the plugin's own callback.
struct CbUser(*mut c_void);
unsafe impl Send for CbUser {}
unsafe impl Sync for CbUser {}

/// The single event-callback slot a plugin registers (single-slot per the
/// ABI; re-registration replaces the previous callback).
static EVENT_CB: Mutex<Option<(HostEventCallback, CbUser)>> = Mutex::new(None);

/// A clone of the app's `egui::Context`, registered once from the UI thread
/// (`ChatApp::ui`) so the plugin thread can wake the eframe loop. `eframe`
/// redraws only on input or an explicit `request_repaint`, so without this a
/// host command enqueued while the window is idle would sit in the queue until
/// the next user interaction and time out in [`send_and_wait`].
static REPAINT_CTX: OnceLock<egui::Context> = OnceLock::new();

/// Install the command channel. Must be called before the first plugin load
/// (bootstrap); vtable calls made before it return errors/defaults. Returns
/// the receiver half for the UI to drain.
pub fn init() -> mpsc::Receiver<HostCommand> {
    let (tx, rx) = mpsc::channel();
    *COMMAND_TX.get_or_init(Default::default).lock().unwrap() = Some(tx);
    rx
}

/// Register the app's `egui::Context` so [`send_and_wait`] can wake the eframe
/// loop with `request_repaint` when a command is enqueued. Called from the UI
/// thread every frame; the `OnceLock` makes it a no-op after the first call.
pub fn set_repaint_ctx(ctx: &egui::Context) {
    if REPAINT_CTX.get().is_none() {
        let _ = REPAINT_CTX.set(ctx.clone());
    }
}

/// The vtable passed to `ToolRegistry::set_host_api` at bootstrap. The table
/// is process-lifetime (`static`), satisfying the host-side lifetime promise
/// documented on `HostApi`.
pub fn host_api() -> &'static HostApi {
    static API: HostApi = HostApi {
        version: HOST_API_VERSION,
        inject_user_message: inject_user_message,
        create_session: create_session,
        resolve_session: resolve_session,
        switch_session: switch_session,
        session_count: session_count,
        get_session: get_session,
        register_event_callback: register_event_callback,
    };
    &API
}

/// Emit a pipeline event to the registered plugin callback (UI thread; fast
/// and non-blocking — no-op when nothing is registered).
///
/// Kinds: 0 = stream-chunk text, 1 = final complete content, 2 = error
/// message, 3 = round-complete marker (payload empty).
///
/// NOTE: the callback runs while the callback slot is locked — a callback
/// that re-enters `register_event_callback` would deadlock (the ABI asks for
/// a fast, forwarding-only callback).
pub fn emit_event(kind: u32, session_id: &str, payload: &str) {
    let slot = EVENT_CB.lock().unwrap();
    if let Some((cb, user_data)) = &*slot {
        // `cb`/`user_data` come from the plugin's own registration call and
        // are valid for its lifetime; the byte slices live for the duration
        // of this call.
        let res = std::panic::catch_unwind(|| {
            cb(
                kind,
                session_id.as_ptr(),
                session_id.len(),
                payload.as_ptr(),
                payload.len(),
                user_data.0,
            );
        });
        // A plugin callback panicking must not take down the UI frame loop.
        if let Err(payload) = res {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_string());
            tracing::error!(error = %msg, "plugin event callback panicked (callback kept; it will run again on the next event)");
        }
    }
}

// ─── vtable implementations ─────────────────────────────────────────────────

/// UTF-8 byte buffer → `String` (null/empty → `""`, invalid bytes → U+FFFD;
/// the host never aborts on malformed plugin input).
fn bytes_to_string(ptr: *const u8, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: the plugin passes a valid buffer of `len` bytes for the call.
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf8_lossy(slice).into_owned()
}

/// Copy `s` into `out` as a NUL-terminated C string, truncated to fit.
/// Returns `false` on a null buffer or `cap < 64` (per the ABI).
fn write_nul(out: *mut u8, cap: usize, s: &str) -> bool {
    if out.is_null() || cap < 64 {
        return false;
    }
    let bytes = s.as_bytes();
    let n = bytes.len().min(cap - 1);
    // SAFETY: `out` is valid for `cap` writes and `bytes` for `n` reads for
    // the duration of the call.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, n);
        *out.add(n) = 0;
    }
    true
}

/// Send `cmd` to the UI thread and wait (≤ 5 s) for the one-shot reply.
fn send_and_wait(cmd: HostCommand, reply_rx: mpsc::Receiver<HostReply>) -> Option<HostReply> {
    let tx = COMMAND_TX
        .get()
        .and_then(|slot| slot.lock().unwrap().clone())?;
    if tx.send(cmd).is_err() {
        return None;
    }
    // Wake the eframe loop so the UI thread drains this command promptly even
    // when the window is idle: `eframe::run_native` only redraws on input or an
    // explicit repaint request, so an inject arriving during idle would
    // otherwise wait out the full `CMD_TIMEOUT` and be mis-read as a failure.
    if let Some(ctx) = REPAINT_CTX.get() {
        ctx.request_repaint();
    }
    reply_rx.recv_timeout(CMD_TIMEOUT).ok()
}

/// ABI: inject a user message into a session (auto-creates it if missing);
/// `false` on timeout/shutdown.
extern "C" fn inject_user_message(
    session_id: *const u8,
    session_len: usize,
    text: *const u8,
    text_len: usize,
) -> bool {
    let (reply_tx, reply_rx) = mpsc::channel();
    match send_and_wait(
        HostCommand::Inject {
            sid: bytes_to_string(session_id, session_len),
            text: bytes_to_string(text, text_len),
            reply: reply_tx,
        },
        reply_rx,
    ) {
        Some(HostReply::Ok) => true,
        _ => false,
    }
}

/// ABI: create a session; copies its id (NUL-terminated) into `out_id`
/// (`out_cap >= 64` required).
extern "C" fn create_session(
    name: *const u8,
    name_len: usize,
    out_id: *mut u8,
    out_cap: usize,
) -> bool {
    let (reply_tx, reply_rx) = mpsc::channel();
    match send_and_wait(
        HostCommand::Create {
            name: bytes_to_string(name, name_len),
            reply: reply_tx,
        },
        reply_rx,
    ) {
        Some(HostReply::OkId(id)) => write_nul(out_id, out_cap, &id),
        _ => false,
    }
}

/// ABI: resolve a session by exact id or case-insensitive name.
extern "C" fn resolve_session(
    query: *const u8,
    query_len: usize,
    out_id: *mut u8,
    out_cap: usize,
) -> bool {
    let (reply_tx, reply_rx) = mpsc::channel();
    match send_and_wait(
        HostCommand::Resolve {
            query: bytes_to_string(query, query_len),
            reply: reply_tx,
        },
        reply_rx,
    ) {
        Some(HostReply::OkId(id)) => write_nul(out_id, out_cap, &id),
        _ => false,
    }
}

/// ABI: switch the UI to a session so the desktop follows the plugin's
/// active session; `false` when the session is unknown or on timeout.
extern "C" fn switch_session(session_id: *const u8, session_len: usize) -> bool {
    let (reply_tx, reply_rx) = mpsc::channel();
    match send_and_wait(
        HostCommand::Switch {
            sid: bytes_to_string(session_id, session_len),
            reply: reply_tx,
        },
        reply_rx,
    ) {
        Some(HostReply::Ok) => true,
        _ => false,
    }
}

/// ABI: number of sessions in the store (0 on timeout/shutdown).
extern "C" fn session_count() -> usize {
    let (reply_tx, reply_rx) = mpsc::channel();
    match send_and_wait(HostCommand::Count { reply: reply_tx }, reply_rx) {
        Some(HostReply::OkCount(n)) => n,
        _ => 0,
    }
}

/// ABI: (id, name) of the `index`-th session (sorted by id); `false` when
/// out of range.
extern "C" fn get_session(
    index: usize,
    out_id: *mut u8,
    id_cap: usize,
    out_name: *mut u8,
    name_cap: usize,
) -> bool {
    let (reply_tx, reply_rx) = mpsc::channel();
    match send_and_wait(HostCommand::Get { index, reply: reply_tx }, reply_rx) {
        Some(HostReply::OkSession { id, name }) => {
            write_nul(out_id, id_cap, &id) && write_nul(out_name, name_cap, &name)
        }
        _ => false,
    }
}

/// ABI: register the event callback (single slot; replaces the previous one).
extern "C" fn register_event_callback(cb: HostEventCallback, user_data: *mut c_void) {
    *EVENT_CB.lock().unwrap() = Some((cb, CbUser(user_data)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_bytes_to_string_handles_null_and_invalid() {
        assert_eq!(bytes_to_string(std::ptr::null(), 5), "");
        let s = "héllo";
        let b = s.as_bytes();
        assert_eq!(bytes_to_string(b.as_ptr(), b.len()), s);
        // Invalid UTF-8 is lossy-replaced, never a panic.
        assert_eq!(bytes_to_string(b"ab\xff".as_ptr(), 3), "ab\u{fffd}");
    }

    #[test]
    fn test_write_nul() {
        let mut buf = [0u8; 64];
        assert!(write_nul(buf.as_mut_ptr(), 64, "abc"));
        assert_eq!(&buf[..4], b"abc\0");

        // Too-small buffer → `false`, untouched (per the ABI).
        let mut small = [0u8; 8];
        assert!(!write_nul(small.as_mut_ptr(), 8, "abc"));
        assert_eq!(small, [0u8; 8]);

        // Truncation to cap-1 bytes + NUL.
        let long = "abcdefghij".repeat(8); // 80 chars
        let mut buf2 = [0xffu8; 64];
        assert!(write_nul(buf2.as_mut_ptr(), 64, &long));
        assert_eq!(&buf2[..63], &long.as_bytes()[..63]);
        assert_eq!(buf2[63], 0);
    }

    #[test]
    fn test_command_roundtrip_through_vtable() {
        let rx = init();
        let api = host_api();
        assert_eq!(api.version, HOST_API_VERSION);

        // A handler thread plays the UI thread: drains up to 3 commands and
        // answers each on its one-shot reply channel.
        let handle = std::thread::spawn(move || {
            let mut handled = 0usize;
            while handled < 3 {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(cmd) => {
                        handled += 1;
                        match cmd {
                            HostCommand::Count { reply } => {
                                let _ = reply.send(HostReply::OkCount(3));
                            }
                            HostCommand::Get { index, reply } => {
                                let _ = reply.send(HostReply::OkSession {
                                    id: format!("id-{index}"),
                                    name: "n".into(),
                                });
                            }
                            HostCommand::Create { name, reply } => {
                                let _ = reply.send(HostReply::OkId(format!("created-{name}")));
                            }
                            _ => {}
                        }
                    }
                    Err(_) => break,
                }
            }
            handled
        });

        // Each call goes: plugin-thread vtable fn → command queue → handler
        // → one-shot reply (all within the 5 s timeout).
        assert_eq!((api.session_count)(), 3);

        let mut id = [0u8; 64];
        let mut name = [0u8; 64];
        assert!((api.get_session)(7, id.as_mut_ptr(), 64, name.as_mut_ptr(), 64));
        assert_eq!(String::from_utf8_lossy(&id).split('\0').next(), Some("id-7"));
        assert_eq!(String::from_utf8_lossy(&name).split('\0').next(), Some("n"));

        let mut out = [0u8; 64];
        assert!((api.create_session)(b"t".as_ptr(), 1, out.as_mut_ptr(), 64));
        assert_eq!(
            String::from_utf8_lossy(&out).split('\0').next(),
            Some("created-t")
        );

        assert_eq!(handle.join().unwrap(), 3);
    }

    #[test]
    fn test_event_callback_slot() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::as_ptr(&calls) as *mut c_void;

        extern "C" fn cb(
            _kind: u32,
            _sid: *const u8,
            _sid_len: usize,
            _payload: *const u8,
            _payload_len: usize,
            user_data: *mut c_void,
        ) {
            let n = unsafe { &*(user_data as *const AtomicUsize) };
            n.fetch_add(1, Ordering::SeqCst);
        }

        register_event_callback(cb, counter);
        emit_event(0, "s1", "chunk");
        emit_event(1, "s1", "done");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
