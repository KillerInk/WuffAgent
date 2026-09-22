//! LLM request methods for ChatClient: send/complete/stream + tool-call
//! warning checks. Split out of the client facade (C1).

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::trimming::message_char_count;
use crate::types::{Message, ToolCall, Usage};

use super::estimate_conversation_tokens;
use super::http::{self, build_request, ChatRequest, send_message};
use super::sse::{self, ToolCallTracker};
use super::{parse_context_overflow, ChatClient, Error};

impl ChatClient {
    // ── HTTP methods ──────────────────────────────────────────────────────────

    pub async fn send_message(&self, prompt: &str) -> Result<(String, Option<Usage>), Error> {
        self.send_message_with_tools(prompt, None).await
    }

    /// Send a non-streaming request built from an explicit message list.
    ///
    /// Unlike `send_message` (which prepends `self.system_prompt` and appends
    /// the prompt to `self.conversation`), this uses the given messages
    /// verbatim — the caller is responsible for including the system prompt,
    /// history, and user turn in the right order.
    pub async fn complete_messages(
        &self,
        messages: &[Message],
        tools: Option<&[crate::tools::ToolDefinition]>,
    ) -> Result<(String, Option<Usage>), Error> {
        let mut msgs = messages.to_vec();
        let request = ChatRequest {
            model: "local".to_string(),
            messages: msgs.clone(),
            stream: false,
            tools: tools.map(|t| t.to_vec()),
            reasoning_effort: self.reasoning_effort.as_wire_value().map(|s| s.to_string()),
            stream_options: Some(http::StreamOptions {
                include_usage: true,
            }),
            return_progress: None,
        };
        self.note_prompt_chars(message_char_count(&msgs));
        let result = send_message(
            &self.http_client,
            &self.url(),
            self.settings.api_key().as_deref(),
            &request,
        )
        .await;

        // Backstop: the estimator is a heuristic — if the server still
        // rejects the request as over-context, force-trim the message list
        // to 80% of the reported prompt size and retry once.
        let result = match result {
            Err(e) if parse_context_overflow(&e).is_some() => {
                if let Some(ov) = parse_context_overflow(&e) {
                    tracing::warn!(
                        "complete_messages exceeded context ({} tokens, n_ctx={}); force-trimming and retrying",
                        ov.n_prompt, ov.n_ctx
                    );
                    let target = self.overflow_retry_char_budget(&ov);
                    Self::trim_to_token_budget_messages(&mut msgs, target);
                    let request2 = ChatRequest {
                        model: "local".to_string(),
                        messages: msgs.clone(),
                        stream: false,
                        tools: tools.map(|t| t.to_vec()),
                        reasoning_effort: self
                            .reasoning_effort
                            .as_wire_value()
                            .map(|s| s.to_string()),
                        stream_options: Some(http::StreamOptions {
                            include_usage: true,
                        }),
                        return_progress: None,
                    };
                    self.note_prompt_chars(message_char_count(&msgs));
                    match send_message(
                        &self.http_client,
                        &self.url(),
                        self.settings.api_key().as_deref(),
                        &request2,
                    )
                    .await
                    {
                        // Full result flows to the common tail below, where
                        // the usage is logged and the ratio calibrated.
                        Ok(ok) => {
                            self.calibrate_from_usage(ok.usage.as_ref());
                            Ok(ok)
                        }
                        Err(re) => Err(re),
                    }
                } else {
                    return Err(e);
                }
            }
            other => other,
        };

        let r = result?;
        self.record_usage(
            r.usage.as_ref(),
            r.model.as_deref(),
            r.tool_calls,
            r.thinking_chars,
        );
        self.calibrate_from_usage(r.usage.as_ref());
        Ok((r.content, r.usage))
    }

    pub async fn send_message_with_tools(
        &self,
        prompt: &str,
        tools: Option<&[crate::tools::ToolDefinition]>,
    ) -> Result<(String, Option<Usage>), Error> {
        let request = build_request(
            &self.system_prompt,
            &self.conversation,
            prompt,
            false,
            tools,
            self.reasoning_effort,
            self.n_ctx(),
        );
        self.note_prompt_chars(message_char_count(&request.messages));
        let result = send_message(
            &self.http_client,
            &self.url(),
            self.settings.api_key().as_deref(),
            &request,
        )
        .await;

        // Backstop: the estimator is a heuristic — if the server still
        // rejects the request as over-context, force-trim to 80% of the
        // reported prompt size and retry once.
        let result = match result {
            Err(e) if parse_context_overflow(&e).is_some() => {
                if let Some(ov) = parse_context_overflow(&e) {
                    tracing::warn!(
                        "request exceeded context ({} tokens, n_ctx={}); force-trimming and retrying",
                        ov.n_prompt, ov.n_ctx
                    );
                    let target = self.overflow_retry_char_budget(&ov);
                    self.trim_conversation(self.max_messages);
                    self.trim_to_token_budget(target);
                    let request2 = build_request(
                        &self.system_prompt,
                        &self.conversation,
                        prompt,
                        false,
                        tools,
                        self.reasoning_effort,
                        self.n_ctx(),
                    );
                    self.note_prompt_chars(message_char_count(&request2.messages));
                    let retry = send_message(
                        &self.http_client,
                        &self.url(),
                        self.settings.api_key().as_deref(),
                        &request2,
                    )
                    .await;
                    match retry {
                        // Full result flows to the common tail below, where
                        // the usage is logged and the ratio calibrated.
                        Ok(ok) => {
                            self.calibrate_from_usage(ok.usage.as_ref());
                            Ok(ok)
                        }
                        Err(re) => Err(re),
                    }
                } else {
                    return Err(e);
                }
            }
            other => other,
        };
        let r = result?;
        self.record_usage(
            r.usage.as_ref(),
            r.model.as_deref(),
            r.tool_calls,
            r.thinking_chars,
        );
        let (content, usage) = (r.content, r.usage);

        // Update conversation history
        let mut conv = self.conversation.lock().unwrap();
        conv.push(Message {
            role: "user".to_string(),
            content: prompt.to_string(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
        conv.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });
        drop(conv);

        // Calibrate the chars/token ratio from the server's real count so
        // subsequent trim budgets track the actual tokenizer.
        self.calibrate_from_usage(usage.as_ref());

        // Only trim once the context limit is reached (same policy as the
        // streaming chat loop): while the estimated token count stays below
        // 90% of n_ctx, the full history is kept.
        let n_ctx = self.n_ctx();
        if n_ctx > 0 {
            if estimate_conversation_tokens(&self.conversation) > self.trim_trigger_chars() {
                let target_chars = self.trim_target_chars();
                self.trim_conversation(self.max_messages);
                self.trim_to_token_budget(target_chars);
            }
        } else if self.max_messages > 0 {
            self.trim_conversation(self.max_messages);
        }

        Ok((content, usage))
    }

    /// Stream a request built from an EXPLICIT message list, without touching
    /// the client's own conversation. The agent engine keeps its own message
    /// history and uses this to retain full control (system prompt, assistant
    /// tool-call messages, tool results, reasoning round-trip).
    ///
    /// Thinking/reasoning chunks are delivered via `callback` with
    /// `is_thinking == true`; content chunks with `false`.
    ///
    /// Returns the accumulated assistant message (content, reasoning_content,
    /// tool_calls) plus the usage reported by the server.
    ///
    /// `on_tool_call_ready` fires (mid-stream) as soon as a tool call is
    /// complete enough to execute — the model has moved past it (text or the
    /// next tool call) and its arguments look like complete JSON. Agents use
    /// this to start executing tools while the model keeps reasoning.
    ///
    /// `on_prompt_progress` fires per server tick with llama.cpp's live
    /// prompt-processing progress (the request sets `return_progress: true`;
    /// other backends simply never invoke it).
    pub async fn stream_with_messages_arc(
        client: &Arc<Self>,
        messages: &[Message],
        tools: Option<&[crate::tools::ToolDefinition]>,
        callback: impl FnMut(String, bool) -> Result<(), Error> + Send + Sync + 'static,
        on_tool_call_ready: impl FnMut(ToolCall) + Send + Sync + 'static,
        on_prompt_progress: impl FnMut(crate::types::PromptProgress) + Send + Sync + 'static,
        cancel_token: Option<&CancellationToken>,
    ) -> Result<(Message, Option<Usage>), Error> {
        let http_client = client.stream_http_client.clone();
        let base_url = client.url();
        let api_key = client.settings.api_key();

        let request = ChatRequest {
            model: "local".to_string(),
            messages: messages.to_vec(),
            stream: true,
            tools: tools.map(|t| t.to_vec()),
            reasoning_effort: client
                .reasoning_effort
                .as_wire_value()
                .map(|s| s.to_string()),
            stream_options: Some(http::StreamOptions {
                include_usage: true,
            }),
            // Ask llama.cpp for live prompt-processing progress chunks.
            return_progress: Some(true),
        };
        let body = serde_json::to_string(&request)?;

        let mut builder = http_client
            .post(format!("{}/v1/chat/completions", base_url))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(body);
        if let Some(ref key) = api_key {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }

        let resp = builder.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Http(format!("Server returned {}: {}", status, text)));
        }

        // Throwaway conversation seeded with one empty assistant message; the
        // SSE layer accumulates content / reasoning_content / tool_calls into it.
        let local_conv: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(vec![Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: crate::types::format_timestamp(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        }]));

        let mut boxed_cb = Box::new(callback);
        let mut boxed_ready = Box::new(on_tool_call_ready);
        let mut boxed_pp = Box::new(on_prompt_progress);
        let mut ready_tracker = ToolCallTracker::default();
        let (usage, model) = sse::stream_message(
            resp,
            &local_conv,
            &mut boxed_cb,
            &mut boxed_ready,
            &mut boxed_pp,
            &mut ready_tracker,
            cancel_token,
        )
        .await?;

        let msg = local_conv.lock().unwrap().pop().unwrap_or_else(|| Message {
            role: "assistant".to_string(),
            content: String::new(),
            timestamp: String::new(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            image: None,
        });

        // The accumulated message carries what the SSE layer saw: the
        // assistant's tool calls and its thinking text.
        let tool_calls = msg.tool_calls.as_ref().map(|t| t.len() as u32).unwrap_or(0);
        let thinking_chars = msg
            .reasoning_content
            .as_ref()
            .map(|r| r.chars().count() as u64)
            .unwrap_or(0);
        client.record_usage(usage.as_ref(), model.as_deref(), tool_calls, thinking_chars);
        Ok((msg, usage))
    }

    // ── Tool call helpers ─────────────────────────────────────────────────────

    /// Check for malformed tool calls in the conversation and return warnings.
    /// A tool call is considered malformed if its arguments are not valid JSON.
    pub fn check_tool_call_warnings(&self) -> Vec<(String, String)> {
        let conv = self.conversation.lock().unwrap();
        let mut warnings = Vec::new();

        for msg in conv.iter() {
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    // Try to parse the arguments as JSON
                    if tc.function.arguments.is_empty() {
                        warnings.push((tc.function.name.clone(), "Empty arguments".to_string()));
                    } else if !tc.function.arguments.starts_with('{') {
                        warnings.push((
                            tc.function.name.clone(),
                            "Invalid JSON: arguments don't start with '{'".to_string(),
                        ));
                    } else if serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                        .is_err()
                    {
                        warnings.push((
                            tc.function.name.clone(),
                            format!(
                                "Malformed JSON arguments: {}",
                                &tc.function.arguments[..tc.function.arguments.len().min(50)]
                            ),
                        ));
                    }
                }
            }
        }

        warnings
    }
}
