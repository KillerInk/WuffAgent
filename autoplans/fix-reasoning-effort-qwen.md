# Fix: reasoning effort can't be turned off/on (Qwen3.x)

**Status:** implemented
**Reported:** "reasoning effort doesn't work as it should and can't be turned off/on"
against the official Qwen3.8 usage (thinking ON by default, `xhigh` by default,
supported levels `xhigh`/`medium`/`low`, off via
`chat_template_kwargs: {enable_thinking: false}`).

## Root causes

### RC1 — wire: "Off" is invisible to the server
`ReasoningEffort::Off` maps to `as_wire_value() == None`, so the field is
**omitted** from the request body (`client/http.rs` `ChatRequest`,
`#[serde(skip_serializing_if)]`). For Qwen3.8 the server default is
`enable_thinking: true` + `reasoning_effort: xhigh`, and the documented
`reasoning_effort` levels contain **no off level** — so omitting the field does
not disable thinking. Qwen's official way to switch thinking off is
`chat_template_kwargs: {"enable_thinking": false}`. WuffAgent never sends
`chat_template_kwargs`.

→ **Symptom: "can't turn off"** (Off → model still thinks at xhigh).

### RC2 — precedence: agent profile overrides the live UI toggle
Chat path: `AgentEngine::execute_with_tools` copies the profile's
`reasoning_effort` into `chat_config`
(`wuffagent-core/src/agents/engine.rs:179`), and `Agent::new`
(`agents/agent/mod.rs:98`) forces it onto a client clone whenever it is
non-`Off`. So for profiles that set a non-Off effort (e.g.
`agents/researcher.json`: `"high"`), the input-bar dropdown is **ignored
entirely** — every request goes out at the profile level.

→ **Symptom: "can't turn on/off"** while a non-Off profile is selected
(UI Low/Medium/Off all become the profile's `xhigh`).

(The dropdown → UI state → `ChatPipeline::new(self.reasoning_effort)` wiring is
intact: the pipeline is rebuilt per message and applies the value to the
cloned engine client via `with_reasoning_effort`.)

## Fix

Wire semantics (single source of truth in `types/reasoning.rs`):

| UI level | `reasoning_effort` | `chat_template_kwargs.enable_thinking` |
|----------|--------------------|----------------------------------------|
| Off      | *(omitted)*        | `false` — explicitly disables thinking |
| Low      | `"low"`            | `true` — explicitly enables thinking   |
| Medium   | `"medium"`         | `true`                                  |
| High     | `"xhigh"`          | `true`                                  |

Sending `enable_thinking` both ways makes the toggle robust on backends whose
default is the opposite of what the user asked for (Qwen3: default on;
llama.cpp Qwen3: default off). Backends that don't know the field (llama.cpp
server, OpenAI, OpenRouter) ignore or pass it through; precedent: we already
send the llama.cpp-only `return_progress` field on every request.

Precedence: on the interactive chat path the **live UI value wins** over the
agent profile (the profile effort remains in effect for autonomous runs and as
handoff-target configuration, which is by design — a handoff switches to the
target agent's setup).

## Changes

1. `wuffagent-core/src/types/reasoning.rs` — add `ReasoningEffort::enable_thinking()`.
2. `wuffagent-core/src/client/http.rs` — `ChatTemplateKwargs` struct,
   `ChatRequest.chat_template_kwargs` (skip-when-None), `reasoning_wire(effort)`
   helper; `build_request` uses it.
3. `wuffagent-core/src/client/chat.rs` — the 3 other `ChatRequest` construction
   sites (complete_messages ×2, stream_with_messages_arc) use the helper.
4. `wuffagent-core/src/agents/engine.rs` — `execute_with_tools` no longer copies
   the profile effort into `chat_config` (stays `Off` = inherit the client,
   which the pipeline set from the UI).
5. Docs/tests — update stale "Off = omitted" comments (ui/state.rs, http.rs),
   extend `client/tests.rs` request-body assertions for all 4 levels, unit-test
   the new mapping.

## Verified

- `cargo build` clean; `cargo test -p wuffagent-core` green
  (incl. new wire-shape tests + existing per-agent effort tests).

## Follow-ups (not in scope)

- `High` → `"xhigh"` is Qwen-flavored; OpenAI-style backends that only accept
  `minimal/low/medium/high` would 400. Could become backend-aware later.
- Per-agent `reasoning_effort` in the agent editor now only shapes autonomous /
  handoff-target runs; the dropdown is the live session control. Consider
  documenting that in the agent-config dialog tooltip.
