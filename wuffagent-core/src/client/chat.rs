//! LLM request methods for ChatClient: send/complete/stream + tool-call
//! warning checks. Split out of the client facade (C1).

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::trimming::message_char_count;
use crate::types::{Message, ToolCall, Usage};

use super::estimate_conversation_tokens;
use super::http::{self, build_request, send_message};
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
        let (reasoning_effort, chat_template_kwargs) = http::reasoning_wire(self.reasoning_effort());
        let (reasoning_format, reasoning_budget_tokens) = self.reasoning_budget();
        let stream_options = http::StreamOptions { include_usage: true };
        // P2: serialize the BORROWED view — the owned ChatRequest deep-cloned
        // the whole history + tools into fields that were serialized and
        // dropped immediately (and re-cloned again on the overflow retry).
        let request = http::ChatRequestRef {
            model: "local",
            messages: &msgs,
            stream: false,
            tools,
            reasoning_effort: reasoning_effort.as_deref(),
            chat_template_kwargs,
            stream_options: Some(&stream_options),
            return_progress: None,
            // Clone: the value must survive for the overflow-retry request
            // below (the first request moves the original).
            reasoning_format: reasoning_format.clone(),
            reasoning_budget_tokens,
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
                    let (reasoning_effort, chat_template_kwargs) =
                        http::reasoning_wire(self.reasoning_effort());
                    // P2: borrowed view again (see first request above).
                    let request2 = http::ChatRequestRef {
                        model: "local",
                        messages: &msgs,
                        stream: false,
                        tools,
                        reasoning_effort: reasoning_effort.as_deref(),
                        chat_template_kwargs,
                        stream_options: Some(&stream_options),
                        return_progress: None,
                        // The first request moved the value (it was dropped
                        // after being serialized) — copy it for the retry.
                        reasoning_format: reasoning_format.clone(),
                        reasoning_budget_tokens,
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
        let (reasoning_format, reasoning_budget_tokens) = self.reasoning_budget();
        let request = build_request(
            &self.session.system_prompt(),
            self.session.conversation(),
            prompt,
            false,
            tools,
            self.reasoning_effort(),
            self.n_ctx(),
            reasoning_format.as_deref(),
            reasoning_budget_tokens,
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
                        &self.session.system_prompt(),
                        self.session.conversation(),
                        prompt,
                        false,
                        tools,
                        self.reasoning_effort(),
                        self.n_ctx(),
                        reasoning_format.as_deref(),
                        reasoning_budget_tokens,
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
        let mut conv = self.session.conversation().lock().unwrap();
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
            if estimate_conversation_tokens(self.session.conversation()) > self.trim_trigger_chars() {
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

        let (reasoning_effort, chat_template_kwargs) =
            http::reasoning_wire(client.reasoning_effort());
        let (reasoning_format, reasoning_budget_tokens) = client.reasoning_budget();
        let stream_options = http::StreamOptions { include_usage: true };
        // P2: serialize the BORROWED view — no per-round deep clone of the
        // full `messages` history or the `tools` definitions (the owned
        // request cloned both, then was serialized and dropped).
        let request = http::ChatRequestRef {
            model: "local",
            messages,
            stream: true,
            tools,
            reasoning_effort: reasoning_effort.as_deref(),
            chat_template_kwargs,
            stream_options: Some(&stream_options),
            // Ask llama.cpp for live prompt-processing progress chunks.
            return_progress: Some(true),
            reasoning_format,
            reasoning_budget_tokens,
        };
        let body = request.to_json()?;

        let url = format!("{}/v1/chat/completions", base_url);
        let auth = api_key.as_deref().map(|k| format!("Bearer {}", k));
        let resp = http::send_with_transient_retries(
            || {
                let mut b = http_client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("Accept", "text/event-stream")
                    .body(body.clone());
                if let Some(ref key) = auth {
                    b = b.header("Authorization", key.clone());
                }
                async { b.send().await }
            },
            cancel_token,
        )
        .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(http::llm_error_or_http(status.as_u16(), &text));
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

    // ── Token counting ────────────────────────────────────────────────────────

    /// Count the exact token count of a chat-completion request using the
    /// llama.cpp `POST /v1/chat/completions/input_tokens` endpoint.
    ///
    /// This is a server-side count using the actual model tokenizer — far
    /// more accurate than the client-side char-count estimate
    /// (`message_char_count / chars_per_token`). Used by the trimming path
    /// to decide whether the context window is about to be exceeded, so the
    /// `exceed_context_size_error` retry is a backstop rather than the
    /// primary path.
    ///
    /// Returns `None` when the server doesn't support the endpoint (older
    /// llama.cpp builds) — the caller falls back to the char-count estimate.
    pub async fn count_tokens(
        &self,
        messages: &[Message],
        tools: Option<&[crate::tools::ToolDefinition]>,
    ) -> Option<usize> {
        let (reasoning_effort, chat_template_kwargs) =
            http::reasoning_wire(self.reasoning_effort());
        let (reasoning_format, reasoning_budget_tokens) = self.reasoning_budget();
        let request = http::ChatRequestRef {
            model: "local",
            messages,
            stream: false,
            tools,
            reasoning_effort: reasoning_effort.as_deref(),
            chat_template_kwargs,
            stream_options: None,
            return_progress: None,
            reasoning_format,
            reasoning_budget_tokens,
        };
        let body = request.to_json().ok()?;

        let mut builder = self
            .http_client
            .post(format!("{}/v1/chat/completions/input_tokens", self.url()))
            .header("Content-Type", "application/json")
            .body(body);
        if let Some(ref key) = self.settings.api_key() {
            builder = builder.header("Authorization", format!("Bearer {}", key));
        }

        let resp = builder.send().await.ok()?;
        if !resp.status().is_success() {
            // Endpoint not supported (404/501) or other error — fall back
            // to the char-count estimate at the call site.
            tracing::debug!(
                "count_tokens: server returned {}; falling back to estimate",
                resp.status()
            );
            return None;
        }

        let text = resp.text().await.ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
        // llama.cpp returns the count as a bare JSON number
        // (e.g. `1234`), not an object.
        let tokens = parsed.as_u64()? as usize;
        Some(tokens)
    }

    // ── Tool call helpers ─────────────────────────────────────────────────────

    /// Check for malformed tool calls in the conversation and return warnings.
    /// A tool call is considered malformed if its arguments are not valid JSON.
    pub fn check_tool_call_warnings(&self) -> Vec<(String, String)> {
        let conv = self.session.conversation().lock().unwrap();
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
