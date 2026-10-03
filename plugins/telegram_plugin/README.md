# telegram_plugin — talk to WuffAgent from Telegram

A WuffAgent **plugin** (cdylib, same install model as `hello_plugin`) that
lets you drive WuffAgent sessions from a Telegram chat. The bot runs as a
thread **inside the running WuffAgent process** — there is no extra
executable. Because injected messages run in the app's real pipeline, the
agent has the app's full tool registry (builtins, MCP tools, other plugins).

## What it does

- You send a message → it is injected into your chat's **current session**
  and a full agent turn runs (LLM + tools).
- The agent's reply is streamed back to Telegram (chunked at ≤4096 chars,
  plain text).
- Each Telegram chat keeps its **own session** (persisted in
  `~/.wuffagent/telegram-state.json`), and you can:
  - **create** sessions from Telegram (`/new`) — they appear in the GUI's
    sessions panel immediately;
  - **join** any existing session — including ones created or renamed in the
    GUI — with `/use <name|id>`;
  - **switch** freely between them (`/sessions`, `/current`).
- The agent itself gets a `telegram` tool (`status | start | stop | send |
  new_session | list_sessions | use_session`) so it can organize work or
  message you proactively.

## Setup

1. **Create a bot** with [@BotFather](https://t.me/BotFather) → copy the
   token (`123456:ABC...`).
2. **Get your chat id** — message the bot once, then look at
   `~/.wuffagent/telegram.log` (it logs every ignored chat id), or use a
   service like @userinfobot.
3. **Write the config** — `~/.wuffagent/telegram.json`:

   ```json
   {
     "token": "123456:ABC...",
     "allow_chat_ids": [123456789],
     "default_session": "Telegram",
     "chunk_chars": 4096,
     "api_base": null,
     "poll_timeout_secs": 50
   }
   ```

4. **Build + install**:

   ```sh
   cargo build -p telegram_plugin
   # copy the DLL into the plugins dir (Windows shown):
   copy target\debug\telegram_plugin.dll %USERPROFILE%\.wuffagent\plugins\
   ```

   Then in WuffAgent call the `reload_plugins` tool (or restart the app).
   If the config file parses at load time, the bot **auto-starts** —
   install → reload → talk, no further interaction.

## Commands (from Telegram)

| Command | Effect |
|---|---|
| *(free text)* | Runs a turn in this chat's current session (auto-creates the `default_session` on first use). While a turn is running, one further message is queued; a second one gets "still working". |
| `/new [name]` | Creates a new session (default name `Telegram <HH:mm>`) and switches this chat to it. |
| `/sessions` | Lists all sessions (id + name, names truncated); `*` marks this chat's current one. |
| `/use <name\|id>` | Joins an existing session — exact id, else case-insensitive name (first match wins; names are not unique). Unknown → replies with the list. |
| `/current` | Shows this chat's current session. |
| `/help` | Command list. |

Slash commands are always processed (they don't wait on the LLM).

## The `telegram` tool (agent-facing)

| action | params | result |
|---|---|---|
| `status` | — | running, allowlist, last error, chats; token shown as prefix only (`1234…`) |
| `start` / `stop` | — | poller lifecycle |
| `send` | `chat_id`, `text` | proactive bot→user message |
| `new_session` | `name` | creates + returns the id (does not switch any chat) |
| `list_sessions` | — | `[{id, name}]` |
| `use_session` | `query` (id or name fragment) | sets the calling chat's current session; outside a telegram-originated turn it just resolves + reports |

## Config reference (`~/.wuffagent/telegram.json`)

| key | required | meaning |
|---|---|---|
| `token` | yes | BotFather token. Stays in this local file (same trust level as the LLM API key in the app config). |
| `allow_chat_ids` | yes | **The security boundary** — a bot token is full access to the bot; every other chat id is ignored. |
| `default_session` | no (default `Telegram`) | session name each chat starts in (auto-created on first free text). |
| `chunk_chars` | no (default 4096) | outgoing message chunk size (Telegram's hard limit). |
| `api_base` | no (default `https://api.telegram.org`) | full Bot API base URL — a test override (the fake-Telegram integration test uses it). |
| `poll_timeout_secs` | no (default 50) | `getUpdates` long-poll hold (server-side, max 50). |

Files the plugin owns: `telegram.json` (config), `telegram-state.json`
(per-chat session pointers, atomic temp+rename), `telegram.log`
(append-only, small rotation cap).

## Shared-session semantics (documented, not solved)

Joining a session you are also driving in the GUI means both sides append to
the same conversation. The existing abort-on-new-send pipeline semantics
apply: a new send replaces the in-flight run. There is no per-side locking in
the MVP — don't expect interleaved turns from bot and GUI to both complete.

## Troubleshooting

- **409 Conflict** — two pollers with the same token (e.g. two WuffAgent
  builds, dev + release). The bot backs off 30 s and keeps retrying; visible
  in `status`/`telegram.log`. Stop the other instance.
- **401 Unauthorized** — bad token: the bot stops and `status` shows the
  error. Re-check `telegram.json`.
- **Server down / no replies** — check `telegram.log`; the poller retries
  automatically. `status` shows `last_error` and `last_poll_secs_ago`.
- **Session vanished mid-conversation** — deleted in the GUI: the bot replies
  "no longer exists", clears the chat's pointer, and the next free text
  re-creates the default.
- **Nothing happens, no log entries** — your chat id is not in
  `allow_chat_ids`.
- **`status` says `host_api: missing`** — the plugin was loaded by something
  that doesn't provide the host API (or a very old app build). The bot runs
  send-only: `send`/`status` work, injects fail with a clear error.

## Dual-linking note (why the log file exists)

Like `hello_plugin`, this cdylib statically links its **own copy** of
`wuffagent-core` and its dependencies. The only thing handed across the
boundary is the host-API vtable pointer (layout-compatible: both copies
compile from the same core source). Consequence: `tracing!` inside the
plugin goes to the plugin's own (unset) global dispatcher and is **silently
dropped** — so the plugin logs via `eprintln!` (the app console) plus the
append-only `~/.wuffagent/telegram.log`. Plugins are never unloaded (they
live until process exit); `stop` just halts the poller.

## Tests

```sh
cargo test -p telegram_plugin
```

- Unit: chunk boundaries/CRLF/multibyte, allowlist filter, command parsing,
  config+state parse/round-trip/corrupt recovery, token prefix, start
  preconditions.
- Integration (`tests/fake_telegram_e2e.rs`): a fake Telegram server on a
  local `TcpListener` (via `api_base`) drives the real poller/worker end to
  end against a stub host vtable — free-text inject, event-callback reply,
  `/new` create+switch, `/sessions` listing, `/use` switch, state-file
  persistence.
