//! Mid-run user injections for `run_llm_loop` (extracted A3).

use crate::types::Message;

use super::Agent;

impl Agent {
    /// Drain the mid-run injection channel at a round boundary.
    ///
    /// Messages the user sent while this run is active arrive on the
    /// injection channel (the UI pushes them in at send time instead of
    /// queueing them behind the whole run). The model can only see new input
    /// at an LLM round boundary, so the top of the loop — just before the
    /// next LLM call — is the earliest point one can land: append each
    /// message to the current turn (request list + shared store) and let the
    /// next round react to it. Must be drained BEFORE the handoff/restart
    /// checks so a message sent during a long tool call (e.g. a `restart`
    /// build) is recorded in the store and survives the handoff snapshot /
    /// process relaunch.
    ///
    /// Each injected text is also appended to `original_request` so the
    /// verification judge grades against the FULL request.
    pub(crate) fn drain_injections(
        &self,
        messages: &mut Vec<Message>,
        original_request: &mut String,
    ) {
        let Some(holder) = &self.injection_rx else {
            return;
        };
        let rx = holder.lock().unwrap();
        while let Ok(injected) = rx.try_recv() {
            let image = injected
                .image
                .as_ref()
                .and_then(crate::types::image_source_data_uri);
            let user_msg = Message {
                role: "user".to_string(),
                content: injected.text.clone(),
                timestamp: crate::types::format_timestamp(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                image,
            };
            tracing::info!(
                "[AGENT] Agent '{}' injecting user message sent mid-run into the running turn: {}",
                self.config.name,
                injected.text
            );
            messages.push(user_msg.clone());
            self.record_in_store(&user_msg);
            // The injected message is part of this turn's request now.
            original_request.push_str(&format!(
                "\n[User added while the agent was working: {}]",
                injected.text
            ));
        }
    }
}
