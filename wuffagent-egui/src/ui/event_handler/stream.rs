use super::super::state::ChatApp;
use wuffagent_core::types::{AppStatus, MessageKind, Usage};

impl ChatApp {
    /// StreamChunk arm of `handle_event`.
    pub(crate) fn handle_stream_chunk(&mut self, content: &str, sid: &str, n_ctx: u32) {
        if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime.chat_state.stream_chunk(content);
            // Live status-bar estimates while generating (content and
            // thinking chunks alike): see `update_live_estimates`.
            runtime.chat_state.update_live_estimates(n_ctx);
        }
    }

    /// StreamPromptProgress arm of `handle_event`.
    pub(crate) fn handle_stream_prompt_progress(&mut self, progress: wuffagent_core::types::PromptProgress, sid: &str) {
        if let Some(runtime) = self.session_store.get_mut(sid) {
            // Live PP progress + speed while the server processes the
            // prompt (llama.cpp `prompt_progress`; counts only
            // non-cached tokens). Replaced by the server-reported
            // final value when the round completes.
            runtime.chat_state.prompt_progress = Some(progress);
            if let Some(tps) = progress.prompt_tps() {
                runtime.chat_state.prompt_tps = Some(tps);
            }
        }
    }

    /// StreamRoundComplete arm of `handle_event`.
    pub(crate) fn handle_stream_round_complete(&mut self, usage: Option<&Usage>, sid: &str, n_ctx: u32) {
        // Intermediate tool round: commit the round's text, keep generating
        // so the next round's chunks keep rendering live.
        if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime.chat_state.commit_stream();
            // This round's prompt processing is done — drop the live
            // progress pill (the next round may start a new one).
            runtime.chat_state.prompt_progress = None;
            // Gauge: prefer the server's exact usage (input+output tokens of
            // this round); fall back to the exact char counter of the
            // stored conversation when the backend omits usage.
            if let Some(u) = usage {
                runtime.chat_state.token_count = u.total_tokens as usize;
                if n_ctx > 0 {
                    runtime.chat_state.context_used = u.total_tokens as f32 / n_ctx as f32 * 100.0;
                }
                // Server speeds (llama.cpp `timings`); None when the
                // backend doesn't report them — that clears stale
                // values from a previous, different backend.
                let t = u.timings.as_ref();
                runtime.chat_state.prompt_tps = t.and_then(|t| t.prompt_per_second);
                runtime.chat_state.gen_tps = t.and_then(|t| t.predicted_per_second);
            } else {
                runtime.refresh_token_gauge(n_ctx);
            }
        }
        // Token tracker: a round just finished and was logged.
        self.usage_panel.mark_dirty();
    }

    /// StreamComplete arm of `handle_event`.
    pub(crate) fn handle_stream_complete(&mut self, content: &str, usage: Option<Usage>, sid: &str, n_ctx: u32) {
        let is_selected = self.selected_session_id.as_deref() == Some(sid);
        if let Some(runtime) = self.session_store.get_mut(sid) {
            // Fallback for backends that never sent StreamChunks
            if runtime.chat_state.stream_buffer.is_empty() && !content.trim().is_empty() {
                runtime.chat_state.append_message("assistant", content);
            } else {
                runtime.chat_state.commit_stream();
            }
            runtime.chat_state.prompt_progress = None;
            runtime.chat_state.is_generating = false;
            runtime.chat_state.current_thinking.clear();
            // The run ended: any live tool card still open (e.g. a tool
            // aborted before it emitted its completion) is stale now.
            runtime.chat_state.active_tools.clear();
            if is_selected {
                self.status = AppStatus::Ready;
            }
            runtime.chat_state.status = AppStatus::Ready;
            if let Some(usage) = usage {
                runtime.chat_state.token_count = usage.total_tokens as usize;
                if n_ctx > 0 {
                    runtime.chat_state.context_used = usage.total_tokens as f32 / n_ctx as f32 * 100.0;
                }
                // Server speeds (llama.cpp `timings`); None when the
                // backend doesn't report them.
                let t = usage.timings.as_ref();
                runtime.chat_state.prompt_tps = t.and_then(|t| t.prompt_per_second);
                runtime.chat_state.gen_tps = t.and_then(|t| t.predicted_per_second);
            } else {
                runtime.refresh_token_gauge(n_ctx);
            }
        }
        // Persist the session after each complete response.
        if let Err(e) = self.save_session_for(sid) {
            tracing::warn!("Failed to save session: {}", e);
        }
        // Start the next queued message (sent while this run was active).
        self.drain_next_queued_message(sid);
        // Token tracker: the run finished and its final round was logged.
        self.usage_panel.mark_dirty();
    }

    /// StreamError arm of `handle_event`.
    pub(crate) fn handle_stream_error(&mut self, error: &str, sid: &str) {
        let cancelled = error == "Cancelled";
        let is_selected = self.selected_session_id.as_deref() == Some(sid);
        if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime.chat_state.stream_chunk(&format!("\n\nStream error: {}", error));
            runtime.chat_state.commit_stream();
            runtime.chat_state.prompt_progress = None;
            runtime.chat_state.is_generating = false;
            // Aborted tools never emit their completion — drop their
            // live cards so the transcript doesn't spin forever.
            runtime.chat_state.active_tools.clear();
            if is_selected {
                self.status = AppStatus::Error(error.to_string());
            }
            runtime.chat_state.status = AppStatus::Error(error.to_string());
        }
        // A failed run (including a user stop surfacing as "Cancelled")
        // never emits StreamComplete, so persist the session here. The
        // agent writes the user turn and each assistant/tool round into
        // the shared conversation store as it runs; saving captures
        // whatever completed so the turn is not lost on reload.
        if let Err(e) = self.save_session_for(sid) {
            tracing::warn!("Failed to save session after error: {}", e);
        }
        // Keep the queue alive: the failed turn is retried as the next
        // turn after an earlier queued message, if any remain. An
        // explicit Stop surfaces as error "Cancelled" — don't resume
        // in that case.
        if !cancelled {
            self.drain_next_queued_message(sid);
        }
    }

    /// StreamThinkingChunk arm of `handle_event`.
    pub(crate) fn handle_thinking_chunk(&mut self, content: &str, sid: &str, n_ctx: u32) {
        tracing::trace!("UI: StreamThinkingChunk received, content_len={}", content.len());
        // Live display + status-bar estimates: thinking tokens are
        // generated tokens, so TG speed and the token gauge must
        // track them too (before this, TG froze for the whole
        // duration of thinking segments).
        if let Some(runtime) = self.session_store.get_mut(sid) {
            runtime.chat_state.stream_thinking_chunk(content);
            runtime.chat_state.update_live_estimates(n_ctx);
        }
    }

    /// StreamThinkingComplete arm of `handle_event`.
    pub(crate) fn handle_thinking_complete(&mut self, sid: &str) {
        tracing::trace!("UI: StreamThinkingComplete received, current_thinking_len={}",
            self.session_store.get(sid).map(|r| r.chat_state.current_thinking.len()).unwrap_or(0));
        // Commit the thinking as a typed message, then clear live state.
        // Note: do NOT commit_stream() here — the round's text stays in
        // stream_buffer and is committed by RoundComplete/StreamComplete,
        // which keeps ordering correct (thinking first, text after) and
        // prevents the StreamComplete fallback from re-appending it.
        if let Some(runtime) = self.session_store.get_mut(sid) {
            let thinking_text = std::mem::take(&mut runtime.chat_state.current_thinking);
            if !thinking_text.is_empty() {
                runtime.chat_state.push_message(MessageKind::Thinking, "assistant", &thinking_text);
            }
        }
    }
}
