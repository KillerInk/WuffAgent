//! End-to-end test with a **fake Telegram server** (in-test `TcpListener`
//! serving canned `getUpdates`/`sendMessage` JSON) and a **fake host**
//! (in-test `HostApi` vtable over a shared `Mutex`).
//!
//! Exercises, through the real plugin threads + real host-API calls:
//! free text -> `inject_user_message` round trip + event-callback reply
//! accumulation -> `sendMessage`; `/new` -> `create_session` + switch;
//! `/use` -> `resolve_session` + switch; `/sessions` -> list.
//!
//! (The plugin's `wuff_tool_host_api` is called directly instead of going
//! through the `cdylib` loader — same entry point the loader uses.)

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use wuffagent_core::tools::types::{HostApi, HostEventCallback, HOST_API_VERSION};

/// The fake host's session store.
#[derive(Default)]
struct HostState {
    /// id -> name, ids are "s1", "s2", ... (insertion order = sorted order).
    sessions: BTreeMap<String, String>,
    selected: Option<String>,
    /// session id -> injected user texts (in order).
    transcript: BTreeMap<String, Vec<String>>,
    next_id: usize,
    cb: Option<HostEventCallback>,
}

static STATE: Mutex<HostState> = Mutex::new(HostState {
    sessions: BTreeMap::new(),
    selected: None,
    transcript: BTreeMap::new(),
    next_id: 0,
    cb: None,
});
static HOST: OnceLock<HostApi> = OnceLock::new();

fn read_cstr(p: *const u8, len: usize) -> String {
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(p, len) }).into_owned()
}

fn write_cstr(out: *mut u8, out_cap: usize, bytes: &[u8]) {
    let n = bytes.len().min(out_cap.saturating_sub(1));
    unsafe {
        for (i, b) in bytes[..n].iter().enumerate() {
            *out.add(i) = *b;
        }
        *out.add(n) = 0;
    }
}

extern "C" fn c_inject(sid: *const u8, sid_len: usize, text: *const u8, text_len: usize) -> bool {
    let mut st = STATE.lock().unwrap();
    let sid = read_cstr(sid, sid_len);
    if !st.sessions.contains_key(&sid) {
        return false;
    }
    st.transcript.entry(sid.clone()).or_default().push(read_cstr(text, text_len));
    true
}

extern "C" fn c_create(name: *const u8, name_len: usize, out: *mut u8, out_cap: usize) -> bool {
    let mut st = STATE.lock().unwrap();
    st.next_id += 1;
    let id = format!("s{}", st.next_id);
    st.sessions.insert(id.clone(), read_cstr(name, name_len));
    write_cstr(out, out_cap, id.as_bytes());
    true
}

extern "C" fn c_resolve(query: *const u8, query_len: usize, out: *mut u8, out_cap: usize) -> bool {
    let mut st = STATE.lock().unwrap();
    let q = read_cstr(query, query_len);
    // id first, then case-insensitive name substring.
    let id = st
        .sessions
        .keys()
        .find(|k| k.as_str() == q.as_str())
        .or_else(|| {
            let ql = q.to_lowercase();
            st.sessions
                .iter()
                .find(|(_, n)| n.to_lowercase().contains(&ql))
                .map(|(k, _)| k)
        })
        .cloned();
    let Some(id) = id else { return false };
    write_cstr(out, out_cap, id.as_bytes());
    true
}

extern "C" fn c_switch(sid: *const u8, sid_len: usize) -> bool {
    let mut st = STATE.lock().unwrap();
    let sid = read_cstr(sid, sid_len);
    if !st.sessions.contains_key(&sid) {
        return false;
    }
    st.selected = Some(sid);
    true
}

extern "C" fn c_count() -> usize {
    STATE.lock().unwrap().sessions.len()
}

extern "C" fn c_get(index: usize, id_out: *mut u8, id_cap: usize, name_out: *mut u8, name_cap: usize) -> bool {
    let st = STATE.lock().unwrap();
    let Some((id, name)) = st.sessions.iter().nth(index) else {
        return false;
    };
    write_cstr(id_out, id_cap, id.as_bytes());
    write_cstr(name_out, name_cap, name.as_bytes());
    true
}

extern "C" fn c_register_cb(cb: HostEventCallback, _user: *mut std::ffi::c_void) {
    STATE.lock().unwrap().cb = Some(cb);
}

fn host() -> &'static HostApi {
    HOST.get_or_init(|| HostApi {
            version: HOST_API_VERSION,
            inject_user_message: c_inject,
            create_session: c_create,
            resolve_session: c_resolve,
            switch_session: c_switch,
            session_count: c_count,
            get_session: c_get,
            register_event_callback: c_register_cb,
        }
    )
}

/// Emit a pipeline event exactly like the host bridge would (UI thread).
fn emit(kind: u32, sid: &str, payload: &str) {
    let cb = STATE.lock().unwrap().cb.expect("bot registered an event callback");
    cb(
        kind,
        sid.as_ptr(),
        sid.len(),
        payload.as_ptr(),
        payload.len(),
        std::ptr::null_mut(),
    );
}

/// The fake Telegram server. `updates` is fed to `getUpdates` (long-poll
/// holds up to ~800 ms); `sent` records `sendMessage` bodies.
struct FakeTg {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl FakeTg {
    fn start(
        updates: Arc<Mutex<VecDeque<serde_json::Value>>>,
        sent: Arc<Mutex<Vec<(i64, String)>>>,
    ) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let loop_stop = stop.clone();
        thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let stop2 = loop_stop;
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline && !stop2.load(Ordering::Relaxed) {
                let Ok((mut sock, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                };
                sock.set_nonblocking(false).unwrap();
                let u = updates.clone();
                let s = sent.clone();
                thread::spawn(move || {
                    let _ = handle_conn(&mut sock, &u, &s);
                });
            }
            // Drain-accept until the test drops us.
            while !stop2.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(20));
            }
        });
        Self { port, stop }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn read_request(sock: &mut impl Read) -> Option<(String, Vec<u8>)> {
    let mut buf = [0u8; 4096];
    let mut all = Vec::new();
    loop {
        let n = sock.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        all.extend_from_slice(&buf[..n]);
        if let Some(pos) = find_header_end(&all) {
            let head = String::from_utf8_lossy(&all[..pos]).into_owned();
            let head = head.trim_end().to_string();
            let cl = head
                .lines()
                .filter_map(|l| {
                    let lower = l.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .next()
                .unwrap_or(0);
            let body_start = pos + 4;
            while all.len() < body_start + cl {
                let n = sock.read(&mut buf).ok()?;
                if n == 0 {
                    break;
                }
                all.extend_from_slice(&buf[..n]);
            }
            let line = head.lines().next().unwrap_or("").to_string();
            let body = all[body_start.min(all.len())..(body_start + cl).min(all.len())].to_vec();
            return Some((line, body));
        }
        if all.len() > 65536 {
            return None;
        }
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn respond(sock: &mut impl Write, body: &str) {
    let _ = write!(
        sock,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = sock.flush();
}

fn handle_conn(sock: &mut std::net::TcpStream, updates: &Arc<Mutex<VecDeque<serde_json::Value>>>, sent: &Arc<Mutex<Vec<(i64, String)>>>) -> bool {
    let Some((line, body)) = read_request(sock) else {
        return false;
    };
    let _dbg = line
        .split_whitespace()
        .nth(1)
        .map(|p| p.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or(""))
        .unwrap_or("");
    eprintln!("[fake-tg] request: {_dbg}");
    let is_get = line.starts_with("GET");
    let method = line
        .split_whitespace()
        .nth(1)
        .map(|p| p.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or(""))
        .unwrap_or("");
    if is_get && method == "getUpdates" {
        // Long-poll: hold up to ~800 ms, RETURNING AS SOON AS the first
        // update appears (not a fixed 800 ms hold). A fixed hold makes the
        // test racy: the fake server's 800 ms hold vs the worker's
        // 100 ms update drain means an event emitted while the worker is
        // mid-long-poll can be delayed by up to 800 ms — long enough for the
        // test's 5 s wait to expire before the reply is sent.
        let deadline = Instant::now() + Duration::from_millis(800);
        let mut batch = Vec::new();
        loop {
            if let Some(u) = updates.lock().unwrap().pop_front() {
                batch.push(u);
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        respond(sock, &format!("{{\"ok\":true,\"result\":{}}}", serde_json::json!(batch)));
        return true;
    }
    if !is_get {
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        match method {
            "sendMessage" => {
                let chat_id = v.get("chat_id").and_then(|c| c.as_i64()).unwrap_or(-1);
                let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                sent.lock().unwrap().push((chat_id, text));
                respond(
                    sock,
                    r#"{"ok":true,"result":{"message_id":1,"chat":{"id":0}}}"#,
                );
            }
            "sendChatAction" => {
                respond(sock, r#"{"ok":true}"#);
            }
            _ => respond(sock, r#"{"ok":false,"description":"unknown"}"#),
        }
        return true;
    }
    respond(sock, r#"{"ok":false,"description":"bad request"}"#);
    true
}

fn wait_until(mut cond: impl FnMut() -> bool, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    cond()
}

fn sent_texts(sent: &Arc<Mutex<Vec<(i64, String)>>>) -> Vec<String> {
    sent.lock().unwrap().iter().map(|(_, t)| t.clone()).collect()
}

/// Enqueue an incoming Telegram message.
fn push_update(updates: &Arc<Mutex<VecDeque<serde_json::Value>>>, id: i64, chat_id: i64, text: &str) {
    updates.lock().unwrap().push_back(serde_json::json!({
        "update_id": id,
        "message": {
            "message_id": id,
            "chat": { "id": chat_id },
            "from": { "first_name": "Tester" },
            "text": text,
        }
    }));
}

#[test]
fn fake_telegram_end_to_end() {
    let updates = Arc::new(Mutex::new(VecDeque::new()));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let server = FakeTg::start(updates.clone(), sent.clone());

    let dir = std::env::temp_dir().join(format!(
        "tg_plugin_e2e_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Redirect the app home for THIS test process (the plugin's copy of
    // wuffagent-core honors `set_wuffagent_home_for_testing`): the autostart
    // triggered by `wuff_tool_host_api` below reads `telegram.json` from the
    // app home, and we must not let a real `~/.wuffagent/telegram.json` on a
    // dev box win (or write the test's state/log into the user's dir).
    wuffagent_core::config::set_wuffagent_home_for_testing(Some(dir.clone()));

    // Write the test's `telegram.json` (pointing at the fake server) BEFORE
    // handing the plugin the host vtable: `wuff_tool_host_api` also triggers
    // an autostart attempt (the loader contract), which must pick up THIS
    // config — otherwise a valid `~/.wuffagent/telegram.json` on a dev box
    // would win and the autostarted bot would poll the real API with the
    // test's throwaway token (401 → self-stop → the explicit start below
    // races it).
    telegram_plugin::config::Config {
        token: "1:2".into(),
        allow_chat_ids: vec![42],
        default_session: "Telegram".into(),
        chunk_chars: 4096,
        api_base: Some(format!("http://127.0.0.1:{}", server.port)),
        poll_timeout_secs: 1,
    }
    .write_to(&dir)
    .expect("test config writes");

    // Hand the plugin the host vtable (what the loader does via
    // `wuff_tool_host_api`).
    // The test's fake server closes every connection right after the
    // response, so a POOLED connection never gets observed idle time:
    // reqwest's builder-level `.timeout()` is then ignored (the idle check
    // passes unconditionally) and only the per-request `.timeout()`
    // overrides (see `TelegramApi::get_updates`) can bound the request —
    // without them, a response that never comes hangs the worker thread
    // forever. Inject a pool-less client (and the matching builder timeout
    // as a backstop) so the per-request overrides do the work.
    telegram_plugin::bot::set_api_for_testing(Some(|cfg| {
        let client = reqwest::blocking::Client::builder()
            .pool_idle_timeout(std::time::Duration::ZERO)
            .timeout(std::time::Duration::from_secs(cfg.poll_timeout_secs + 15))
            .build()
            .unwrap();
        telegram_plugin::tg::TelegramApi::with_client(
            client,
            cfg.api_base.clone().unwrap_or_else(|| "https://api.telegram.org".into()),
            &cfg.token,
        )
    }));
    // Override the host vtable so `host_api()` returns the stub (in a plugin
    // binary the loader calls `wuff_tool_host_api` before the test body runs,
    // but here we call it explicitly — the override is set BEFORE that call
    // so `host_api()` sees the stub instead of null; without it the worker
    // thread exits immediately on "host API unavailable" and no reply is
    // ever sent). Also disable autostart (the explicit `bot::start` below
    // is the only path — autostart would race it and the stop-wait loop
    // would hang on a bot the test didn't start).
    telegram_plugin::TEST_HOST_OVERRIDE.lock().unwrap().replace(host());
    *telegram_plugin::bot::AUTOSTART_DISABLED.lock().unwrap() = true;

    telegram_plugin::wuff_tool_host_api(host());
    // Autostart is disabled (set above) so the explicit `bot::start` below
    // is the only path — no autostarted bot to stop and no race.

    let cfg = telegram_plugin::config::Config {
        token: "1:2".into(),
        allow_chat_ids: vec![42],
        default_session: "Telegram".into(),
        chunk_chars: 4096,
        api_base: Some(format!("http://127.0.0.1:{}", server.port)),
        poll_timeout_secs: 1,
    };
    let start_res = telegram_plugin::bot::start(cfg.clone(), dir.clone());
    println!("start result: {start_res:?}");
    let start_msg = start_res.expect("bot starts");
    println!("bot: {start_msg}");
    assert!(telegram_plugin::bot::running());

    // 1. Free text -> inject into the auto-created default session.
    push_update(&updates, 1, 42, "hello from telegram");
    assert!(
        wait_until(|| {
            let Ok(st) = STATE.try_lock() else {
                eprintln!("[test] STATE busy");
                return false;
            };
            st.transcript
                .values()
                .any(|ts| ts.iter().any(|t| t == "hello from telegram"))
        }, 5),
        "free text was not injected; sent so far: {:?}; sessions: {:?}; chats-state: {:?}",
        sent_texts(&sent),
        STATE.lock().unwrap().sessions,
        STATE.lock().unwrap().transcript
    );
    let injected_sid = STATE
        .lock()
        .unwrap()
        .transcript
        .keys()
        .next()
        .cloned()
        .expect("a session was injected into");
    assert_eq!(STATE.lock().unwrap().sessions.get(&injected_sid).map(String::as_str), Some("Telegram"));

    // 2. Simulate the pipeline finishing -> event callback -> reply.
    emit(0, &injected_sid, "Hello ");
    emit(0, &injected_sid, "from WuffAgent");
    emit(1, &injected_sid, "Hello from WuffAgent");
    assert!(
        wait_until(
            || sent_texts(&sent).iter().any(|t| t == "Hello from WuffAgent"),
            5
        ),
        "reply not sent; got: {:?}",
        sent_texts(&sent)
    );
    assert_eq!(sent.lock().unwrap().iter().filter(|(c, _)| *c == 42).count() >= 1, true);

    // 3. /new creates + switches.
    push_update(&updates, 2, 42, "/new fix-thing");
    assert!(
        wait_until(
            || {
                let st = STATE.lock().unwrap();
                st.sessions.values().any(|n| n == "fix-thing")
                    && st.selected.as_deref() == st
                        .sessions
                        .keys()
                        .find(|k| st.sessions.get(*k) == Some(&"fix-thing".to_string()))
                        .map(String::as_str)
            },
            5
        ),
        "/new did not create+switch; sessions: {:?}",
        STATE.lock().unwrap().sessions
    );
    let fix_id = STATE
        .lock()
        .unwrap()
        .sessions
        .iter()
        .find(|(_, n)| *n == "fix-thing")
        .map(|(id, _)| id.clone())
        .unwrap();
    assert!(
        wait_until(
            || sent_texts(&sent).iter().any(|t| t.contains("created session") && t.contains("fix-thing")),
            5
        ),
        "/new reply missing; got: {:?}",
        sent_texts(&sent)
    );

    // 4. /sessions lists both.
    push_update(&updates, 3, 42, "/sessions");
    assert!(
        wait_until(
            || {
                sent_texts(&sent)
                    .iter()
                    .any(|t| t.contains("Telegram (") && t.contains(&fix_id))
            },
            5
        ),
        "/sessions reply missing; got: {:?}",
        sent_texts(&sent)
    );

    // 5. /use switches back to the first session (by id).
    push_update(&updates, 4, 42, &format!("/use {injected_sid}"));
    assert!(
        wait_until(
            || {
                let st = STATE.lock().unwrap();
                st.selected.as_deref() == Some(injected_sid.as_str())
            },
            5
        ),
        "/use did not switch; selected: {:?}",
        STATE.lock().unwrap().selected
    );
    assert!(
        wait_until(
            || sent_texts(&sent).iter().any(|t| t.contains("using session")),
            5
        ),
        "/use reply missing; got: {:?}",
        sent_texts(&sent)
    );

    // 6. The state file persisted the chat pointer.
    let state_file = std::fs::read_to_string(dir.join(telegram_plugin::config::STATE_FILE))
        .expect("state file exists");
    assert!(state_file.contains(&injected_sid), "state: {state_file}");

    let stop_msg = telegram_plugin::bot::stop();
    println!("bot: {stop_msg}");
    assert!(wait_until(|| !telegram_plugin::bot::running(), 10), "bot stopped");
    server.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
