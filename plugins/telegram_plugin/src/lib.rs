//! WuffAgent plugin — the Telegram bot (in-process; no separate exe).
//!
//! Talk to WuffAgent from Telegram: free text is injected into a per-chat
//! session, and the bot can create (`/new`), list (`/sessions`), and
//! switch (`/use`) sessions through the host API. See `README.md` for setup
//! and the command reference.
//!
//! Exports (the plugin ABI + the host-API hook):
//!
//!   * `wuff_tool_abi_version`   — must equal `PLUGIN_ABI_VERSION`
//!   * `wuff_tool_metadata`      — `*const ToolMetadata` (leaked box)
//!   * `wuff_tool_create`        — the `telegram` tool instance
//!   * `wuff_tool_host_api`      — OPTIONAL: receives the host vtable
//!     pointer (`*const HostApi`). Plugins without this symbol work as
//!     tool-only plugins (the loader just skips the hook); with it, the
//!     bot can create/join/switch sessions. The pointer is valid for the
//!     process lifetime (the egui side keeps it in a `&'static`).

pub mod bot;
pub mod config;
pub mod log;
pub mod tg;

use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;
use wuffagent_core::tools::types::{
    HostApi, PluginTool, Tool, ToolError, ToolMetadata, ToolOutput, ToolParams, ToolResult,
};

/// The host vtable pointer handed to us by the loader (null before the
/// loader calls `wuff_tool_host_api`, e.g. if the app build is too old).
static HOST_API: AtomicPtr<HostApi> = AtomicPtr::new(std::ptr::null_mut());
/// Test override for the host vtable (the e2e test sets it BEFORE calling
/// `wuff_tool_host_api` so `host_api()` sees the stub instead of null — in a
/// plugin binary the loader calls `wuff_tool_host_api` before the test body
/// runs, so the real loader path would leave `HOST_API` null and the
/// worker thread would exit immediately on "host API unavailable").
pub static TEST_HOST_OVERRIDE: Mutex<Option<&'static HostApi>> = Mutex::new(None);

/// The parsed `~/.wuffagent/telegram.json`, read once per start (never
/// cached across starts, so config edits apply on the next start).
fn load_config() -> Result<(config::Config, std::path::PathBuf), String> {
    let dir = wuffagent_core::config::get_wuffagent_home();
    Ok((config::Config::load(&dir)?, dir))
}

/// The plugin's tool: the bot's control surface.
pub struct TelegramTool;

impl Tool for TelegramTool {
    fn name(&self) -> &str {
        "telegram"
    }

    fn description(&self) -> &str {
        "Control the Telegram bot. Actions: status (bot state), start, stop, use_session (point the active chat at a session), new_session, list_sessions, send (send a text message to a chat id)."
    }

    fn parameters_schema(&self) -> wuffagent_core::tools::types::ToolSchema {
        use wuffagent_core::tools::types::{FieldSchema, JsonSchema};
        let mut props = HashMap::new();
        let f = |_name: &str, desc: &str| {
            FieldSchema {
                type_name: "string".to_string(),
                description: desc.to_string(),
                nullable: true,
            }
        };
        props.insert("action".to_string(), f("action", "status | start | stop | use_session | new_session | list_sessions | send"));
        props.insert("query".to_string(), f("query", "use_session: session id or (case-insensitive) name fragment"));
        props.insert("name".to_string(), f("name", "new_session: session name"));
        props.insert("chat_id".to_string(), f("chat_id", "send: Telegram chat id (decimal string)"));
        props.insert("text".to_string(), f("text", "send: the message text"));
        let required = vec!["action".to_string()];
        wuffagent_core::tools::types::ToolSchema {
            name: "telegram".to_string(),
            description: "Control the Telegram bot".to_string(),
            input_type: Some(JsonSchema {
                type_name: "object".to_string(),
                properties: Some(props),
                required,
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> ToolResult<ToolOutput> {
        let action = params
            .get::<String>("action")
            .ok_or_else(|| ToolError::InvalidParams("action is required".into()))?;
        let result: Result<serde_json::Value, String> = match action.as_str() {
            "status" => Ok(bot::status_json()),
            "start" => load_config()
                .and_then(|(cfg, dir)| bot::start(cfg, dir))
                .map(|msg| serde_json::json!({ "ok": true, "message": msg })),
            "stop" => Ok(serde_json::json!({ "ok": true, "message": bot::stop() })),
            "use_session" => params
                .get::<String>("query")
                .ok_or_else(|| "query is required for use_session".to_string())
                .and_then(|query| bot::use_session(&query)),
            "new_session" => params
                .get::<String>("name")
                .ok_or_else(|| "name is required for new_session".to_string())
                .and_then(|name| bot::new_session(&name)),
            "list_sessions" => bot::list_sessions(),
            "send" => match params.get::<String>("chat_id") {
                Some(chat_raw) => match chat_raw.parse::<i64>() {
                    Ok(chat_id) => params
                        .get::<String>("text")
                        .ok_or_else(|| "text is required for send".to_string())
                        .and_then(|text| bot::send_message(chat_id, &text)),
                    Err(_) => Err(format!("chat_id {chat_raw:?} is not a decimal integer")),
                },
                None => Err("chat_id is required for send".to_string()),
            },
            other => Err(format!("unknown action {other:?} (status | start | stop | use_session | new_session | list_sessions | send)")),
        };
        match result {
            Ok(v) => Ok(ToolOutput::Success(v)),
            Err(e) => Ok(ToolOutput::Error(e)),
        }
    }
}

/// `wuff_tool_abi_version` — must equal
/// `wuffagent_core::tools::types::PLUGIN_ABI_VERSION` or the loader rejects
/// the plugin (stale DLLs fail cleanly instead of corrupting the heap).
#[no_mangle]
pub extern "C" fn wuff_tool_abi_version() -> u32 {
    wuffagent_core::tools::types::PLUGIN_ABI_VERSION
}

/// `wuff_tool_metadata` — leaked box; the loader clones the pointee.
#[no_mangle]
pub extern "C" fn wuff_tool_metadata() -> *const ToolMetadata {
    let box_meta: Box<ToolMetadata> = Box::new(ToolMetadata {
        name: "telegram".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        description: "Telegram bot: talk to WuffAgent from Telegram (sessions create/join/switch)".to_string(),
        dependencies: vec![],
    });
    Box::leak(box_meta)
}

/// `wuff_tool_create` — one tool instance per registry entry.
#[no_mangle]
pub extern "C" fn wuff_tool_create() -> PluginTool {
    PluginTool::from_box(Box::new(TelegramTool) as Box<dyn Tool>)
}

/// Auto-start once the loader hands us the host vtable: if a valid
/// `telegram.json` exists, start the bot immediately (README contract:
/// install → load → talk). Failures (missing/invalid config, bot already
/// running) are logged, not fatal — the `telegram` tool's `start` action
/// stays as the manual path. Called exactly once at plugin load, on the
/// loader thread (no deadlock: the bot spawns its own threads and returns).
/// The e2e test disables this (its explicit `bot::start` is the controlled
/// path — autostart would race it and the test's stop-wait loop would hang
/// on a bot the test didn't start).
fn try_autostart() {
    if *bot::AUTOSTART_DISABLED.lock().unwrap() {
        return;
    }
    let dir = wuffagent_core::config::get_wuffagent_home();
    let log_line = |msg: String| log::log(&dir, &msg);
    let cfg = match config::Config::load(&dir) {
        Ok(c) => c,
        Err(e) => {
            log_line(format!("autostart skipped: {e}"));
            return;
        }
    };
    match bot::start(cfg, dir.clone()) {
        Ok(msg) => log_line(format!("autostart: {msg}")),
        Err(e) => log_line(format!("autostart failed: {e}")),
    }
}

/// `wuff_tool_host_api` — OPTIONAL hook: the loader passes the host vtable
/// pointer (or null, if the host build predates the host API). Stores the
/// pointer (valid for the process lifetime) and, for a non-null pointer,
/// attempts an auto-start (see `try_autostart`).
#[no_mangle]
pub extern "C" fn wuff_tool_host_api(ptr: *const HostApi) {
    HOST_API.store(ptr as *mut HostApi, Ordering::Relaxed);
    if !ptr.is_null() {
        try_autostart();
    }
}

/// The host vtable pointer as `&'static` (null-checked; the egui side keeps
/// the vtable alive in a `&'static` for the process lifetime). A test
/// override (`TEST_HOST_OVERRIDE`) wins when set — the e2e test sets it
/// before `wuff_tool_host_api` so the worker thread sees the stub.
pub fn host_api() -> Option<&'static HostApi> {
    if let Some(h) = *TEST_HOST_OVERRIDE.lock().unwrap() {
        return Some(h);
    }
    let p = HOST_API.load(Ordering::Relaxed);
    if p.is_null() {
        None
    } else {
        // SAFETY: non-null pointer stored by the loader from a
        // `&'static HostApi` that outlives the process.
        Some(unsafe { &*p })
    }
}


