//! `McpManager` — owns all live MCP server connections and mirrors their
//! tools into the shared `ToolRegistry`.
//!
//! Concurrency model:
//! - The manager owns a **dedicated 2-worker tokio runtime**. All I/O
//!   (process spawn, stdio, HTTP, handshakes) happens on it, so it never
//!   contends with the UI runtime (the UI thread is inside `#[tokio::main]`,
//!   where `block_on` is not allowed).
//! - Async core methods (`connect_async` & co.) run on that runtime.
//! - The sync facade (`connect_sync` & co.) wraps them with
//!   `runtime.block_on` — safe from plain threads (UI helper threads,
//!   bootstrap), NOT from inside a runtime context.
//! - The UI panel runs mutating ops on helper threads (MaintenanceJob
//!   pattern) and polls results with `try_recv`.
//! - `McpServerState` is guarded by a plain std `RwLock`; guards are never
//!   held across await points (needed values are cloned out first).
//!
//! The async core (connect/disconnect/call/refresh) lives in `ops.rs`.

mod ops;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::config::McpServerConfig;
use crate::tools::registry::ToolRegistry;
use crate::tools::types::ToolMetadata;

use super::client::McpClient;
use super::jsonrpc::McpToolInfo;
use super::tool::{mcp_tool_name, McpTool};
use super::McpError;

/// Runtime connection state of one MCP server.
#[derive(Clone, Debug, PartialEq)]
pub enum McpServerStatus {
    /// Configured but not connected.
    Configured,
    /// Handshake / tool listing in progress.
    Connecting,
    /// Connected; `tool_count` tools are currently registered.
    Connected { tool_count: usize },
    /// Last operation failed (message explains why).
    Error(String),
}

/// Live state of one configured MCP server.
#[derive(Clone)]
pub struct McpServerState {
    pub config: McpServerConfig,
    pub status: McpServerStatus,
    /// Tools discovered via `tools/list` (after the allowlist filter).
    pub tools: Vec<McpToolInfo>,
    /// Per-tool enable flag (default true for newly discovered tools).
    pub tool_enabled: HashMap<String, bool>,
    /// Live connection, if any.
    pub client: Option<Arc<McpClient>>,
}

impl McpServerState {
    fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            status: McpServerStatus::Configured,
            tools: Vec::new(),
            tool_enabled: HashMap::new(),
            client: None,
        }
    }
}

/// Snapshot of one server for the UI (cheap to clone, no locks inside).
#[derive(Clone, Debug)]
pub struct McpServerSnapshot {
    pub name: String,
    pub transport_summary: String,
    pub enabled: bool,
    pub timeout_secs: u64,
    pub status: McpServerStatus,
    pub tools: Vec<McpToolSnapshot>,
}

#[derive(Clone, Debug)]
pub struct McpToolSnapshot {
    /// Raw tool name as reported by the server.
    pub name: String,
    /// Registry name (`mcp__<server>__<tool>`).
    pub full_name: String,
    pub description: String,
    /// Currently registered/available to agents.
    pub enabled: bool,
}

struct McpInner {
    registry: Arc<ToolRegistry>,
    servers: RwLock<HashMap<String, McpServerState>>,
    /// `None` after `shutdown()` (the runtime was moved to a plain thread to
    /// be dropped there, since dropping a runtime on the UI thread — inside a
    /// runtime context — panics).
    runtime: Mutex<Option<Arc<tokio::runtime::Runtime>>>,
}

/// Cloneable handle to the MCP subsystem. Cheap to share with `McpTool`
/// instances and the UI.
#[derive(Clone)]
pub struct McpManager {
    inner: Arc<McpInner>,
    /// Set once `shutdown()` has started; `connect` refuses new work after.
    shutting_down: Arc<AtomicBool>,
}

impl McpManager {
    /// Create the manager with its own dedicated 2-worker runtime.
    /// Building (as opposed to `block_on`/dropping) a runtime is allowed from
    /// inside a runtime context, so this can run in `bootstrap()`.
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("mcp-worker")
            .enable_all()
            .build()
            .expect("failed to build the dedicated MCP tokio runtime");
        Self {
            inner: Arc::new(McpInner {
                registry,
                servers: RwLock::new(HashMap::new()),
                runtime: Mutex::new(Some(Arc::new(runtime))),
            }),
            shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }

    // ── Runtime access ─────────────────────────────────────────────────────

    /// The dedicated runtime, if not yet shut down (for `McpTool::execute`).
    pub fn runtime(&self) -> Option<Arc<tokio::runtime::Runtime>> {
        self.inner.runtime.lock().unwrap().clone()
    }

    fn runtime_or_error(&self) -> Result<Arc<tokio::runtime::Runtime>, McpError> {
        self.runtime()
            .ok_or_else(|| McpError::Other("MCP runtime is shut down".to_string()))
    }

    // ── Read-only state access (any thread, no runtime blocking) ──────────

    pub fn get_config(&self, name: &str) -> Result<McpServerConfig, McpError> {
        let servers = self.inner.servers.read().unwrap();
        servers
            .get(name)
            .map(|s| s.config.clone())
            .ok_or_else(|| McpError::Other(format!("unknown MCP server '{name}'")))
    }

    pub fn timeout_secs(&self, server: &str) -> u64 {
        let servers = self.inner.servers.read().unwrap();
        servers
            .get(server)
            .map(|s| s.config.timeout_secs)
            .unwrap_or(60)
            .max(5)
    }

    /// Snapshot of all servers for the UI (called once per frame).
    pub fn snapshot(&self) -> Vec<McpServerSnapshot> {
        let servers = self.inner.servers.read().unwrap();
        let mut out: Vec<McpServerSnapshot> = servers
            .iter()
            .map(|(name, state)| McpServerSnapshot {
                name: name.clone(),
                transport_summary: state.config.transport_summary(),
                enabled: state.config.enabled,
                timeout_secs: state.config.timeout_secs.max(5),
                status: state.status.clone(),
                tools: state
                    .tools
                    .iter()
                    .map(|t| McpToolSnapshot {
                        name: t.name.clone(),
                        full_name: mcp_tool_name(name, &t.name),
                        description: t.description.clone(),
                        enabled: state.tool_enabled.get(&t.name).copied().unwrap_or(true),
                    })
                    .collect(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// `(connected, total)` server counts for the status bar.
    pub fn connected_counts(&self) -> (usize, usize) {
        let servers = self.inner.servers.read().unwrap();
        let connected = servers
            .values()
            .filter(|s| matches!(s.status, McpServerStatus::Connected { .. }))
            .count();
        (connected, servers.len())
    }

    // ── Config sync (any thread, no runtime blocking) ─────────────────────

    /// Reconcile live state with the configured server list (startup).
    /// Adds missing servers, drops removed ones (killing live child
    /// processes via background tasks) and updates changed configs.
    pub fn sync_from_config(&self, configs: &[McpServerConfig]) {
        let names: Vec<String> = configs.iter().map(|c| c.name.clone()).collect();
        let mut servers = self.inner.servers.write().unwrap();
        let keys: Vec<String> = servers.keys().cloned().collect();
        for name in keys {
            if !names.contains(&name) {
                if servers[name.as_str()].client.is_some() {
                    let manager = self.clone();
                    let name2 = name.clone();
                    if let Some(rt) = self.inner.runtime.lock().unwrap().clone() {
                        let _ = rt.spawn(async move {
                            let _ = manager.disconnect_async(&name2).await;
                        });
                    }
                }
                servers.remove(&name);
            }
        }
        for cfg in configs {
            match servers.get_mut(&cfg.name) {
                Some(state) if state.config != *cfg => {
                    state.config = cfg.clone();
                    tracing::info!(target: "mcp", server = %cfg.name, "MCP server config updated (applies on next connect)");
                }
                Some(_) => {}
                None => {
                    servers.insert(cfg.name.clone(), McpServerState::new(cfg.clone()));
                }
            }
        }
    }

    /// Connect every `enabled` server (fire-and-forget; failures only log).
    /// Safe to call from inside a runtime context (spawns, never blocks).
    pub fn auto_connect_enabled(&self) {
        let names: Vec<String> = {
            let servers = self.inner.servers.read().unwrap();
            servers
                .values()
                .filter(|s| s.config.enabled)
                .map(|s| s.config.name.clone())
                .collect()
        };
        let Some(rt) = self.inner.runtime.lock().unwrap().clone() else {
            return;
        };
        for name in names {
            let manager = self.clone();
            let _ = rt.spawn(async move {
                match manager.connect_async(&name).await {
                    Ok(count) => {
                        tracing::info!(target: "mcp", server = %name, tools = count, "MCP server auto-connected")
                    }
                    Err(e) => {
                        tracing::warn!(target: "mcp", server = %name, error = %e, "MCP auto-connect failed")
                    }
                }
            });
        }
    }

    /// Add or replace a server entry in live state (does not connect and
    /// does not persist — the app owns `config.json`).
    pub fn upsert_server(&self, config: McpServerConfig) -> Result<(), McpError> {
        let mut servers = self.inner.servers.write().unwrap();
        match servers.get_mut(&config.name) {
            Some(state) => state.config = config,
            None => {
                servers.insert(config.name.clone(), McpServerState::new(config));
            }
        }
        Ok(())
    }

    /// Disconnect every server (kill all child processes).
    async fn disconnect_all_async(&self) {
        let names: Vec<String> = {
            let servers = self.inner.servers.read().unwrap();
            servers.keys().cloned().collect()
        };
        for name in names {
            if let Err(e) = self.disconnect_async(&name).await {
                tracing::debug!(target: "mcp", server = %name, error = %e, "disconnect during shutdown");
            }
        }
    }

    fn set_status(&self, name: &str, status: McpServerStatus) {
        let mut servers = self.inner.servers.write().unwrap();
        if let Some(state) = servers.get_mut(name) {
            state.status = status;
        }
    }

    // ── Registry bridging ─────────────────────────────────────────────────

    /// Register one MCP tool in the shared registry. Returns the registry
    /// name on success; `None` on collision/invalidity (logged).
    fn register_tool(&self, server: &str, info: &McpToolInfo) -> Option<String> {
        let full_name = mcp_tool_name(server, &info.name);
        if full_name.len() > 64 || full_name.is_empty() {
            tracing::warn!(target: "mcp", server, tool = %info.name, "MCP tool name too long, skipping");
            return None;
        }
        let tool = McpTool::new(self.clone(), server, info);
        let entry = crate::tools::registry::ToolEntry {
            tool: Arc::new(tool),
            metadata: ToolMetadata {
                name: full_name.clone(),
                version: "mcp".to_string(),
                description: format!("[MCP: {server}] {}", info.description),
                dependencies: vec![],
            },
            loaded_at: Instant::now(),
        };
        match self.inner.registry.register(entry) {
            Ok(()) => Some(full_name),
            Err(e) => {
                tracing::warn!(target: "mcp", server, tool = %info.name, error = %e, "Failed to register MCP tool (name collision?)");
                None
            }
        }
    }

    fn unregister_tool(&self, server: &str, tool: &str) {
        let full_name = mcp_tool_name(server, tool);
        if let Err(e) = self.inner.registry.unregister(&full_name) {
            tracing::debug!(target: "mcp", server, tool, error = %e, "MCP tool was not registered");
        }
    }

    // ── Sync facade (plain threads only: UI helper threads, bootstrap) ────
    //
    // Each method blocks its (helper) thread on the dedicated runtime until
    // the async op finishes. Never call from inside a tokio runtime context.

    pub fn connect_sync(&self, name: &str) -> Result<usize, McpError> {
        let rt = self.runtime_or_error()?;
        let manager = self.clone();
        rt.block_on(manager.connect_async(name))
    }

    pub fn disconnect_sync(&self, name: &str) -> Result<(), McpError> {
        let rt = self.runtime_or_error()?;
        let manager = self.clone();
        rt.block_on(manager.disconnect_async(name))
    }

    /// Enable or disable one tool: registers or unregisters it in the shared
    /// registry. Synchronous and cheap — safe to call directly from the UI
    /// thread.
    pub fn set_tool_enabled(
        &self,
        server: &str,
        tool: &str,
        enabled: bool,
    ) -> Result<(), McpError> {
        let info = {
            let mut servers = self.inner.servers.write().unwrap();
            let state = servers
                .get_mut(server)
                .ok_or_else(|| McpError::Other(format!("unknown MCP server '{server}'")))?;
            let info = state
                .tools
                .iter()
                .find(|t| t.name == tool)
                .cloned()
                .ok_or_else(|| {
                    McpError::Other(format!("unknown tool '{tool}' on server '{server}'"))
                })?;
            state.tool_enabled.insert(tool.to_string(), enabled);
            info
        };
        if enabled {
            self.register_tool(server, &info);
        } else {
            self.unregister_tool(server, tool);
        }
        Ok(())
    }

    /// Remove a server: disconnect (kills the child process) and drop state.
    pub fn remove_server_sync(&self, name: &str) -> Result<(), McpError> {
        let rt = self.runtime_or_error()?;
        let manager = self.clone();
        rt.block_on(async move {
            let _ = manager.disconnect_async(name).await;
            let mut servers = manager.inner.servers.write().unwrap();
            servers.remove(name);
            Ok(())
        })
    }

    /// Toggle the server's `enabled` flag and connect/disconnect to match.
    pub fn set_server_enabled_sync(&self, name: &str, enabled: bool) -> Result<(), McpError> {
        {
            let mut servers = self.inner.servers.write().unwrap();
            if let Some(state) = servers.get_mut(name) {
                state.config.enabled = enabled;
            }
        }
        if enabled {
            self.connect_sync(name)?;
        } else {
            self.disconnect_sync(name)?;
        }
        Ok(())
    }

    /// Re-list tools over the live connection (sync facade).
    pub fn refresh_tools_sync(&self, name: &str) -> Result<usize, McpError> {
        let rt = self.runtime_or_error()?;
        let manager = self.clone();
        rt.block_on(manager.refresh_tools_async(name))
    }

    // ── Shutdown ───────────────────────────────────────────────────────────

    /// Disconnect all servers and drop the dedicated runtime on a plain
    /// thread (a runtime cannot be dropped on the UI thread, which is inside
    /// a runtime context). Idempotent and non-blocking.
    pub fn shutdown(&self) {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::Release);
        let rt = self.inner.runtime.lock().unwrap().take();
        let Some(rt) = rt else {
            return;
        };
        let rt_for_fallback = rt.clone();
        let manager = self.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("mcp-shutdown".to_string())
            .spawn(move || {
                // Kill all child processes while the runtime still runs.
                rt.block_on(async move {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(5),
                        manager.disconnect_all_async(),
                    )
                    .await;
                });
                // Dropping the last runtime handle waits for worker tasks
                // (the stdio reader tasks exit once their stdout hits EOF).
                // This is a plain thread — safe (RuntimeOnThread pattern).
                drop(rt);
                tracing::info!(target: "mcp", "MCP runtime shut down");
            })
        {
            // Extremely rare (thread creation failed): tear down here instead.
            let manager_fb = self.clone();
            tracing::warn!(target: "mcp", error = %e, "Failed to start MCP shutdown thread; dropping runtime here");
            rt_for_fallback.block_on(async move {
                let _ =
                    tokio::time::timeout(Duration::from_secs(2), manager_fb.disconnect_all_async())
                        .await;
            });
            drop(rt_for_fallback);
        }
    }
}
