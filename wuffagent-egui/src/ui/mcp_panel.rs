//! MCP (Model Context Protocol) management panel.
//!
//! Lists configured MCP servers with live status, allows connecting /
//! disconnecting / refreshing, toggling individual tools, and adding /
//! editing / deleting server entries. Mutating operations that block on the
//! MCP runtime (connect, disconnect, remove) run on a helper thread and the
//! result is polled per frame via `try_recv` (the memory-panel MaintenanceJob
//! pattern) — the UI thread is inside the main runtime context, where
//! `block_on` panics. Config changes are persisted to `config.mcp_servers`
//! and saved after every successful change.

use eframe::egui;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wuffagent_core::config::{Config, McpServerConfig, McpTransport};
use wuffagent_core::tools::mcp::{McpManager, McpServerSnapshot, McpServerStatus, McpToolSnapshot};
use super::theme::Theme;

/// MCP panel: server list + status + add/edit/delete + per-tool toggles.
pub struct McpPanel {
    pub show_panel: bool,
    /// In-flight blocking operation (runs on a helper thread; polled per
    /// frame). Only one at a time.
    job: Option<McpJob>,
    /// Add/edit dialog state (None = closed).
    editing: Option<ServerEditState>,
    /// Server name awaiting delete confirmation.
    pending_delete: Option<String>,
    /// Status message (success/error), cleared when the panel closes.
    message: Option<String>,
    /// Server names whose tool list is currently expanded.
    expanded: Vec<String>,
}

/// An in-flight MCP operation. The op runs on a plain helper thread (the UI
/// thread cannot `block_on` a runtime); the outcome message is delivered via
/// `rx` and collected in `draw` each frame.
struct McpJob {
    label: String,
    started_at: Instant,
    rx: mpsc::Receiver<Result<String, String>>,
}

enum McpOp {
    Connect(String),
    Disconnect(String),
    Refresh(String),
    Remove(String),
    /// Toggle the server-level enabled flag (connects/disconnects to match —
    /// blocking, hence a job).
    SetEnabled(String, bool),
}

impl Default for McpPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl McpPanel {
    pub fn new() -> Self {
        Self {
            show_panel: false,
            job: None,
            editing: None,
            pending_delete: None,
            message: None,
            expanded: Vec::new(),
        }
    }

    /// Draw the MCP panel window. No-op when not shown.
    ///
    /// Takes disjoint references into app state (manager + `&mut Config` so
    /// changes can be persisted) — the panel is itself a field of `ChatApp`.
    pub fn draw(
        &mut self,
        ctx: &egui::Context,
        mcp: &Arc<McpManager>,
        config: &mut Config,
    ) {
        // Collect a finished job (even while the window is closed).
        if let Some(job) = &mut self.job {
            let label = job.label.clone();
            let outcome = job.rx.try_recv();
            match outcome {
                Ok(Ok(msg)) => {
                    self.message = Some(msg);
                    self.job = None;
                }
                Ok(Err(e)) => {
                    self.message = Some(format!("✗ {label} failed: {e}"));
                    self.job = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.message = Some(format!("✗ {label} failed (worker exited)"));
                    self.job = None;
                }
            }
        }

        if !self.show_panel {
            return;
        }
        let theme = Theme::from_name(&config.theme);
        let servers = mcp.snapshot();
        let connected = servers
            .iter()
            .filter(|s| matches!(s.status, McpServerStatus::Connected { .. }))
            .count();

        egui::Window::new("MCP Servers")
            .collapsible(true)
            .resizable(true)
            .default_size([660.0, 520.0])
            .show(ctx, |ui| {
                ui.visuals_mut().panel_fill = theme.surface;

                ui.heading("MCP Servers");
                ui.label(
                    egui::RichText::new(format!(
                        "{} configured · {} connected",
                        servers.len(),
                        connected
                    ))
                    .color(theme.text_secondary),
                );
                ui.separator();

                // Status / result message
                if let Some(msg) = &self.message {
                    let color = if msg.starts_with("✓") {
                        theme.success
                    } else {
                        theme.warning
                    };
                    ui.add(egui::Label::new(egui::RichText::new(msg).color(color)).wrap());
                    ui.separator();
                }

                if servers.is_empty() {
                    ui.label("No MCP servers configured yet. Add one to expose its tools to the agents.");
                }

                // Server list
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for snap in &servers {
                        self.draw_server_row(ui, &theme, snap, mcp, config);
                        ui.add_space(4.0);
                    }
                });

                ui.separator();

                // Add/edit dialog (driven by self.editing).
                if self.editing.is_some() {
                    self.draw_edit_dialog(ui, &theme, mcp, config);
                }

                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new("＋ Add server").fill(theme.primary))
                        .clicked()
                    {
                        self.editing = Some(ServerEditState::new());
                    }
                    if let Some(job) = &self.job {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} in progress… ({}s)",
                                job.label,
                                job.started_at.elapsed().as_secs()
                            ))
                            .color(theme.text_dim)
                            .small(),
                        );
                    }
                    if ui.button("Close").clicked() {
                        self.show_panel = false;
                        self.message = None;
                    }
                });
            });
    }

    /// One server row: status, name, enable toggle, actions, expandable tools.
    fn draw_server_row(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        snap: &McpServerSnapshot,
        mcp: &Arc<McpManager>,
        config: &mut Config,
    ) {
        let (dot, status_text, status_color) = match &snap.status {
            McpServerStatus::Connected { .. } => ("●", "connected", theme.success),
            McpServerStatus::Connecting => ("◌", "connecting…", theme.accent),
            McpServerStatus::Configured => {
                if snap.enabled {
                    ("○", "configured", theme.text_secondary)
                } else {
                    ("⊘", "disabled", theme.text_dim)
                }
            }
            McpServerStatus::Error(_) => ("⚠", "error", theme.warning),
        };

        let is_expanded = self.expanded.contains(&snap.name);
        let fill = if is_expanded {
            theme.surface_light
        } else {
            theme.surface
        };

        egui::Frame::new()
            .fill(fill)
            .corner_radius(4)
            .inner_margin(egui::Margin::same(6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(dot).color(status_color).strong());
                    ui.label(
                        egui::RichText::new(format!("{status_text} ·"))
                            .color(status_color)
                            .small(),
                    );
                    // Click the name to expand/collapse the tool list.
                    if ui
                        .label(
                            egui::RichText::new(&snap.name)
                                .color(theme.text_primary)
                                .strong(),
                        )
                        .clicked()
                    {
                        if let Some(pos) = self.expanded.iter().position(|n| n == &snap.name) {
                            self.expanded.remove(pos);
                        } else {
                            self.expanded.push(snap.name.clone());
                        }
                    }
                    ui.label(
                        egui::RichText::new(truncate(&snap.transport_summary, 48))
                            .color(theme.text_dim)
                            .small(),
                    );
                    if let McpServerStatus::Error(err) = &snap.status {
                        ui.label(
                            egui::RichText::new(truncate(err, 60))
                                .color(theme.warning)
                                .small(),
                        );
                    }
                });

                // Controls row
                let busy = self.job.is_some();
                let name = snap.name.clone();
                ui.horizontal(|ui| {
                    // Enable/disable (server-level; connects/disconnects to
                    // match, so it runs as a blocking job).
                    let mut enabled = snap.enabled;
                    if ui.add(egui::Checkbox::new(&mut enabled, "enabled")).changed() {
                        self.start_job(mcp, McpOp::SetEnabled(name.clone(), enabled));
                        if enabled {
                            if let Some(slot) = config.mcp_servers.iter_mut().find(|c| c.name == name) {
                                slot.enabled = true;
                            }
                            self.persist(config, format!("✓ {name} enabled"));
                        } else {
                            if let Some(slot) = config.mcp_servers.iter_mut().find(|c| c.name == name) {
                                slot.enabled = false;
                            }
                            self.persist(config, format!("✓ {name} disabled"));
                        }
                    }

                    match snap.status {
                        McpServerStatus::Connected { .. } => {
                            if ui
                                .add_enabled(!busy, egui::Button::new("Disconnect").fill(theme.surface_light))
                                .clicked()
                            {
                                self.start_job(mcp, McpOp::Disconnect(name.clone()));
                            }
                            if ui
                                .add_enabled(!busy, egui::Button::new("↻ Refresh tools").fill(theme.surface_light))
                                .clicked()
                            {
                                self.start_job(mcp, McpOp::Refresh(name.clone()));
                            }
                        }
                        McpServerStatus::Connecting => {}
                        McpServerStatus::Configured | McpServerStatus::Error(_) => {
                            if ui
                                .add_enabled(!busy, egui::Button::new("Connect").fill(theme.primary))
                                .clicked()
                            {
                                self.start_job(mcp, McpOp::Connect(name.clone()));
                            }
                        }
                    }

                    if ui
                        .add_enabled(!busy, egui::Button::new("Edit").fill(theme.surface_light))
                        .clicked()
                    {
                        let existing = config
                            .mcp_servers
                            .iter()
                            .find(|c| c.name == name)
                            .cloned()
                            .unwrap_or_else(|| McpServerConfig {
                                name: name.clone(),
                                ..Default::default()
                            });
                        self.editing = Some(ServerEditState::from_config(existing));
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Delete").fill(theme.surface_light))
                        .clicked()
                    {
                        self.pending_delete = Some(name);
                    }
                });

                // Expanded tool list
                if is_expanded {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Tools").color(theme.text_secondary).strong());
                    if snap.tools.is_empty() {
                        ui.label(
                            egui::RichText::new("No tools discovered (connect, then refresh).")
                                .color(theme.text_dim)
                                .small(),
                        );
                    } else {
                        for tool in &snap.tools {
                            self.draw_tool_row(ui, theme, &snap.name, tool, mcp);
                        }
                    }
                }
            });

        // Delete confirmation dialog (driven by pending_delete == this name).
        if self.pending_delete.as_deref() == Some(&snap.name) {
            let name = snap.name.clone();
            let mut confirmed = false;
            egui::Window::new("Confirm delete")
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label(format!("Remove MCP server '{name}'?"));
                    ui.label(
                        egui::RichText::new(
                            "Its tools will be unregistered and the process (if any) killed.",
                        )
                        .color(theme.text_secondary),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.pending_delete = None;
                        }
                        if ui.add(egui::Button::new("Delete").fill(theme.error)).clicked() {
                            confirmed = true;
                        }
                    });
                });
            if confirmed {
                self.pending_delete = None;
                self.start_job(mcp, McpOp::Remove(name));
            }
        }
    }

    /// A discovered tool with a per-tool enable checkbox.
    fn draw_tool_row(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        server: &str,
        tool: &McpToolSnapshot,
        mcp: &Arc<McpManager>,
    ) {
        let mut enabled = tool.enabled;
        let mut changed = false;
        ui.horizontal(|ui| {
            changed = ui.add(egui::Checkbox::new(&mut enabled, "")).changed();
            ui.label(
                egui::RichText::new(&tool.name)
                    .monospace()
                    .color(theme.text_primary),
            );
            if !tool.description.is_empty() {
                ui.label(
                    egui::RichText::new(truncate(&tool.description, 60))
                        .color(theme.text_dim)
                        .small(),
                );
            }
        });
        if changed {
            self.on_tool_toggled(server, &tool.name, enabled, mcp);
        }
    }

    /// Apply a tool enable/disable toggle (fast, non-blocking state change).
    fn on_tool_toggled(&mut self, server: &str, tool: &str, enabled: bool, mcp: &McpManager) {
        if mcp.set_tool_enabled(server, tool, enabled).is_ok() {
            self.message = Some(format!(
                "✓ tool '{}' {} on '{}'",
                tool,
                if enabled { "enabled" } else { "disabled" },
                server
            ));
        }
    }

    /// Add/edit server dialog (driven by self.editing).
    ///
    /// The window closure only borrows the edit state; Save/Cancel set flags
    /// that are handled AFTER the window closes (avoids holding `&mut
    /// self.editing` across `apply_edit`).
    fn draw_edit_dialog(
        &mut self,
        ui: &mut egui::Ui,
        theme: &Theme,
        mcp: &Arc<McpManager>,
        config: &mut Config,
    ) {
        let title = if self.editing.as_ref().map(|e| e.existing.is_some()).unwrap_or(false) {
            "Edit MCP server"
        } else {
            "Add MCP server"
        };
        let mut save_requested = false;
        let mut cancel_requested = false;

        egui::Window::new(title)
            .collapsible(false)
            .resizable(true)
            .default_size([520.0, 480.0])
            .show(ui.ctx(), |ui| {
                ui.visuals_mut().panel_fill = theme.surface;
                let Some(edit) = self.editing.as_mut() else {
                    return;
                };
                let is_new = edit.existing.is_none();

                ui.horizontal(|ui| {
                    ui.label("Name:");
                    // Renaming an existing server would orphan its state, so
                    // the name is fixed in edit mode.
                    ui.add_enabled(
                        is_new,
                        egui::TextEdit::singleline(&mut edit.name)
                            .desired_width(200.0)
                            .hint_text("my-server"),
                    );
                    if is_new {
                        ui.label(
                            egui::RichText::new("(tools appear as mcp__<name>__<tool>)")
                                .color(theme.text_dim)
                                .small(),
                        );
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Transport:");
                    if ui
                        .selectable_label(edit.transport_kind == TransportKind::Stdio, "Stdio (child process)")
                        .clicked()
                    {
                        edit.transport_kind = TransportKind::Stdio;
                    }
                    if ui
                        .selectable_label(edit.transport_kind == TransportKind::Http, "HTTP (Streamable)")
                        .clicked()
                    {
                        edit.transport_kind = TransportKind::Http;
                    }
                });

                ui.separator();

                if edit.transport_kind == TransportKind::Stdio {
                    ui.horizontal(|ui| {
                        ui.label("Command:");
                        ui.add(egui::TextEdit::singleline(&mut edit.command).desired_width(320.0));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Args:");
                        ui.add(
                            egui::TextEdit::singleline(&mut edit.args)
                                .hint_text("space separated")
                                .desired_width(320.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Working dir:");
                        ui.add(egui::TextEdit::singleline(&mut edit.working_dir).desired_width(320.0));
                    });
                    ui.label("Env (one KEY=VALUE per line):");
                    ui.add(
                        egui::TextEdit::multiline(&mut edit.env)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label("URL:");
                        ui.add(
                            egui::TextEdit::singleline(&mut edit.url)
                                .hint_text("https://host/mcp")
                                .desired_width(360.0),
                        );
                    });
                    ui.label("Headers (one 'Key: value' per line):");
                    ui.add(
                        egui::TextEdit::multiline(&mut edit.headers)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                }

                ui.separator();

                let timeout_changed = ui
                    .add(
                        egui::DragValue::new(&mut edit.timeout_secs)
                            .range(1..=600)
                            .suffix(" s call timeout"),
                    )
                    .changed();
                let enabled_changed = ui
                    .add(egui::Checkbox::new(&mut edit.enabled, "Enabled (auto-connect at startup)"))
                    .changed();
                ui.label("Allowed tools (comma separated, empty = all):");
                let allowed_changed = ui
                    .add(egui::TextEdit::singleline(&mut edit.allowed_tools).desired_width(f32::INFINITY))
                    .changed();

                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new("Save").fill(theme.primary)).clicked()
                        || (is_new
                            && (enabled_changed || timeout_changed || allowed_changed))
                    {
                        save_requested = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_requested = true;
                    }
                });
            });

        // Handle after the window closure released its borrow of self.editing.
        if save_requested {
            self.apply_edit(mcp, config);
        } else if cancel_requested {
            self.editing = None;
        }
    }

    /// Validate + persist the dialog state into manager + config.
    fn apply_edit(&mut self, mcp: &Arc<McpManager>, config: &mut Config) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let cfg = match edit.build() {
            Some(c) => c,
            None => {
                self.message = Some("✗ Invalid server config (check name / command / url)".to_string());
                self.editing = Some(edit);
                return;
            }
        };

        let is_new = edit.existing.is_none();
        match mcp.upsert_server(cfg.clone()) {
            Ok(()) => {
                if is_new {
                    config.mcp_servers.push(cfg.clone());
                } else if let Some(slot) =
                    config.mcp_servers.iter_mut().find(|c| c.name == cfg.name)
                {
                    *slot = cfg.clone();
                }
                if let Err(e) = config.save() {
                    self.message = Some(format!("✗ Saved in memory, but config file write failed: {e}"));
                    return;
                }
                // Connect right away if enabled and not already connected.
                let snap = mcp.snapshot().into_iter().find(|s| s.name == cfg.name);
                let should_connect = cfg.enabled
                    && matches!(
                        snap.map(|s| s.status),
                        None | Some(McpServerStatus::Configured) | Some(McpServerStatus::Error(_))
                    );
                if should_connect {
                    self.start_job(mcp, McpOp::Connect(cfg.name.clone()));
                } else {
                    self.message = Some(format!("✓ Server '{}' saved", cfg.name));
                }
            }
            Err(e) => {
                self.message = Some(format!("✗ Could not save server: {e}"));
                self.editing = Some(edit);
            }
        }
    }

    /// Start a blocking MCP op on a helper thread (no-op if one is running).
    fn start_job(&mut self, mcp: &Arc<McpManager>, op: McpOp) {
        if self.job.is_some() {
            self.message = Some("✗ Another MCP operation is already in progress".to_string());
            return;
        }
        let label = match &op {
            McpOp::Connect(n) => format!("connecting {n}"),
            McpOp::Disconnect(n) => format!("disconnecting {n}"),
            McpOp::Refresh(n) => format!("refreshing {n}"),
            McpOp::Remove(n) => format!("removing {n}"),
            McpOp::SetEnabled(n, e) => {
                format!("{} {n}", if *e { "enabling" } else { "disabling" })
            }
        };
        let mcp = mcp.clone();
        let (tx, rx) = mpsc::channel();
        let timeout = match &op {
            McpOp::Disconnect(_) | McpOp::Remove(_) => Duration::from_secs(10),
            _ => Duration::from_secs(30),
        };
        let spawned = std::thread::Builder::new()
            .name("mcp-ui-op".to_string())
            .spawn(move || {
                let outcome: Result<String, String> = match op {
                    McpOp::Connect(name) => {
                        let m = mcp.clone();
                        let n = name.clone();
                        timeout_op(timeout, move || m.connect_sync(&n).map_err(|e| e.to_string()))
                            .map(|cnt| format!("✓ {name} connected ({cnt} tools registered)"))
                    }
                    McpOp::Disconnect(name) => {
                        let m = mcp.clone();
                        let n = name.clone();
                        timeout_op(timeout, move || m.disconnect_sync(&n).map_err(|e| e.to_string()))
                            .map(|_| format!("✓ {name} disconnected"))
                    }
                    McpOp::Refresh(name) => {
                        let m = mcp.clone();
                        let n = name.clone();
                        timeout_op(
                            timeout,
                            move || m.refresh_tools_sync(&n).map_err(|e| e.to_string()),
                        )
                        .map(|cnt| format!("✓ {name}: {cnt} tools registered"))
                    }
                    McpOp::Remove(name) => {
                        let m = mcp.clone();
                        let n = name.clone();
                        timeout_op(
                            timeout,
                            move || m.remove_server_sync(&n).map_err(|e| e.to_string()),
                        )
                        .map(|_| format!("✓ {name} removed"))
                    }
                    McpOp::SetEnabled(name, enabled) => {
                        let m = mcp.clone();
                        let n = name.clone();
                        let en = enabled;
                        timeout_op(
                            timeout,
                            move || m.set_server_enabled_sync(&n, en).map_err(|e| e.to_string()),
                        )
                        .map(|_| {
                            format!(
                                "✓ {name} {}",
                                if enabled { "enabled" } else { "disabled" }
                            )
                        })
                    }
                };
                let _ = tx.send(outcome);
            });
        match spawned {
            Ok(_) => {
                self.job = Some(McpJob {
                    label,
                    started_at: Instant::now(),
                    rx,
                });
            }
            Err(e) => self.message = Some(format!("✗ Failed to start MCP worker thread: {e}")),
        }
    }

    /// Persist a config change; surface a message on failure.
    fn persist(&mut self, config: &mut Config, message: String) {
        match config.save() {
            Ok(()) => self.message = Some(message),
            Err(e) => self.message = Some(format!("✗ Config save failed: {e}")),
        }
    }
}

/// Run a blocking op with a hard timeout on a detached thread (last-resort
/// safety net against a wedged MCP runtime; the ops also have internal
/// timeouts — handshake 10s, per-call timeout from the server config).
fn timeout_op<T: Send + 'static>(
    dur: Duration,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(dur) {
        Ok(res) => res,
        Err(_) => Err(format!("timed out after {}s", dur.as_secs())),
    }
}
#[derive(Clone, Copy, PartialEq)]
enum TransportKind {
    Stdio,
    Http,
}

/// Editable add/edit dialog state.
struct ServerEditState {
    /// When editing, the original name (the name field is fixed).
    existing: Option<String>,
    name: String,
    transport_kind: TransportKind,
    command: String,
    args: String,
    env: String,
    working_dir: String,
    url: String,
    headers: String,
    enabled: bool,
    timeout_secs: u64,
    allowed_tools: String,
}

impl ServerEditState {
    fn new() -> Self {
        Self {
            existing: None,
            name: String::new(),
            transport_kind: TransportKind::Stdio,
            command: String::new(),
            args: String::new(),
            env: String::new(),
            working_dir: String::new(),
            url: String::new(),
            headers: String::new(),
            enabled: true,
            timeout_secs: 60,
            allowed_tools: String::new(),
        }
    }

    fn from_config(cfg: McpServerConfig) -> Self {
        let mut s = Self::new();
        s.existing = Some(cfg.name.clone());
        s.name = cfg.name;
        s.enabled = cfg.enabled;
        s.timeout_secs = cfg.timeout_secs;
        s.allowed_tools = cfg.allowed_tools.join(", ");
        match cfg.transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                working_dir,
            } => {
                s.transport_kind = TransportKind::Stdio;
                s.command = command;
                s.args = args.join(" ");
                s.env = env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                s.working_dir = working_dir.unwrap_or_default();
            }
            McpTransport::Http { url, headers } => {
                s.transport_kind = TransportKind::Http;
                s.url = url;
                s.headers = headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
        s
    }

    /// Validate + build the config. None if invalid.
    fn build(&self) -> Option<McpServerConfig> {
        let name = wuffagent_core::tools::mcp::sanitize_name_part(&self.name.trim());
        if name.is_empty() {
            return None;
        }
        let transport = match self.transport_kind {
            TransportKind::Stdio => {
                let command = self.command.trim();
                if command.is_empty() {
                    return None;
                }
                McpTransport::Stdio {
                    command: command.to_string(),
                    args: split_space_separated(&self.args),
                    env: parse_env_lines(&self.env),
                    working_dir: if self.working_dir.trim().is_empty() {
                        None
                    } else {
                        Some(self.working_dir.trim().to_string())
                    },
                }
            }
            TransportKind::Http => {
                let url = self.url.trim();
                if !url.starts_with("http") {
                    return None;
                }
                McpTransport::Http {
                    url: url.to_string(),
                    headers: parse_header_lines(&self.headers),
                }
            }
        };
        Some(McpServerConfig {
            name,
            transport,
            enabled: self.enabled,
            timeout_secs: self.timeout_secs.clamp(1, 600),
            allowed_tools: self
                .allowed_tools
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        })
    }
}

fn split_space_separated(s: &str) -> Vec<String> {
    s.split_whitespace().map(|t| t.to_string()).collect()
}

fn parse_env_lines(s: &str) -> HashMap<String, String> {
    s.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

fn parse_header_lines(s: &str) -> HashMap<String, String> {
    s.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (k, v) = line.split_once(':')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Truncate a string to at most `n` chars, appending an ellipsis if cut.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
