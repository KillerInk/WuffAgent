use super::super::state::ChatApp;
use wuffagent_core::types::MessageKind;

impl ChatApp {
    /// ToolCallWarning arm of `handle_event`.
    pub(crate) fn handle_tool_call_warning(&mut self, tool_name: &str, message: &str) {
        tracing::warn!(tool_name, message, "Tool call warning");
    }

    /// ToolCallStart arm of `handle_event`.
    pub(crate) fn handle_tool_call_start(&mut self, tool_name: &str, call_id: &str, args_preview: String, sid: &str) {
        tracing::debug!(tool_name, call_id, "Tool call started");
        if let Some(runtime) = self.session_store.get_mut(sid) {
            // Open a live card for this call.
            if !runtime
                .chat_state
                .active_tools
                .iter()
                .any(|t| t.call_id == call_id)
            {
                runtime.chat_state.active_tools.push(wuffagent_core::sessions::ActiveTool {
                    tool_name: tool_name.to_string(),
                    call_id: call_id.to_string(),
                    args_preview,
                    started_at: std::time::Instant::now(),
                    live_output: String::new(),
                });
            }
        }
    }

    /// ToolCallProgress arm of `handle_event`.
    pub(crate) fn handle_tool_call_progress(&mut self, tool_name: &str, call_id: &str, text: String, sid: &str) {
        // Live output tail for the running tool card (latest-tail:
        // replace, don't append).
        if let Some(runtime) = self.session_store.get_mut(sid) {
            if let Some(active) = runtime
                .chat_state
                .active_tools
                .iter_mut()
                .find(|t| t.call_id == call_id)
            {
                active.live_output = text;
            }
            let _ = tool_name;
        }
    }

    /// ToolCallComplete arm of `handle_event`.
    pub(crate) fn handle_tool_call_complete(&mut self, tool_name: &str, call_id: &str, result: String, sid: &str) {
        tracing::debug!(tool_name, result, "Tool call complete");
        // Close the live card, capturing the args preview and duration
        // so the persisted message can show both.
        let (args_preview, duration_ms) = self
            .session_store
            .get(sid)
            .and_then(|r| {
                r.chat_state
                    .active_tools
                    .iter()
                    .find(|t| t.call_id == call_id)
                    .map(|t| {
                        (
                            t.args_preview.clone(),
                            t.started_at.elapsed().as_millis() as u64,
                        )
                    })
            })
            .unwrap_or_default();
        if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime
                .chat_state
                .active_tools
                .retain(|t| t.call_id != call_id);
            let header = if args_preview.is_empty() {
                wuffagent_core::tools::tool_call_header(tool_name, &result)
            } else {
                format!("🔧 {}: {}", tool_name, args_preview)
            };
            let content = if duration_ms > 0 {
                format!("{}||{}||{}||{}", header, call_id, result, duration_ms)
            } else {
                format!("{}||{}||{}", header, call_id, result)
            };
            runtime.chat_state.push_message(MessageKind::Tool, "tool", &content);
        }
    }

    /// ToolCallError arm of `handle_event`.
    pub(crate) fn handle_tool_call_error(&mut self, tool_name: &str, call_id: &str, error: String, sid: &str) {
        tracing::warn!(tool_name, error, "Tool call error");
        // Close the live card for errors too (duration + args preview).
        let (args_preview, duration_ms) = self
            .session_store
            .get(sid)
            .and_then(|r| {
                r.chat_state
                    .active_tools
                    .iter()
                    .find(|t| t.call_id == call_id)
                    .map(|t| {
                        (
                            t.args_preview.clone(),
                            t.started_at.elapsed().as_millis() as u64,
                        )
                    })
            })
            .unwrap_or_default();
        if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime
                .chat_state
                .active_tools
                .retain(|t| t.call_id != call_id);
            let header = if args_preview.is_empty() {
                format!("Tool '{}' error: {}", tool_name, error)
            } else {
                format!("✗ {}: {} — {}", tool_name, args_preview, error)
            };
            let content = if duration_ms > 0 {
                format!("{}||{}||{}||{}", header, call_id, error, duration_ms)
            } else {
                format!("{}||{}||{}||", header, call_id, error)
            };
            runtime.chat_state.push_message(MessageKind::Tool, "tool", &content);
        }
    }
}
