# Session System Bug Fixes

> **Goal:** Fix session persistence and UI bugs that prevent the model from remembering conversations and cause loading/saving issues.

---

## Root Cause Analysis

### Bug 1: Session ID Not Updated on Switch (Critical)

**Location:** `src/ui/window.rs:291-301`

**Problem:** When the user clicks a different session in the sidebar, `switch_session()` is called, but it calls `cl.load_session()` which uses `self.session_id`. However, `self.session_id` is **never updated** when switching sessions. The `ChatClient.session_id` remains the initial session ID.

**Impact:** Clicking any session always loads the same (initial) session. The model never sees different conversations.

**Fix:** Update `client.session_id` before calling `load_session()`.

```rust
fn switch_session(&mut self, session_id: &str) {
    let mut cl = self.client.lock().unwrap();
    cl.set_session(Some(session_id.to_string()), cl.session_dir.clone());
    if let Some(session) = cl.load_session() {
        self.chat_display = session.messages.iter().map(|m| ChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
        }).collect();
        cl.set_system_prompt(&session.system_prompt);
    }
    drop(cl);
}
```

---

### Bug 2: chat_display Not Initialized from Session on Startup (Critical)

**Location:** `src/ui/window.rs:78-119`

**Problem:** `ChatApp::new()` initializes `chat_display` from `config.chat_history`, but the actual conversation is stored in `client.conversation` (loaded from the session file). These are two separate stores that are never synced.

**Impact:** On startup, the UI shows an empty chat (or stale config history) instead of the session's messages.

**Fix:** After initializing `sessions_panel`, load the current session and populate `chat_display` from it.

```rust
// In ChatApp::new(), after creating sessions_panel:
if let Some(ref mut panel) = self.sessions_panel {
    // Load the initial session into the client
    let mut cl = self.client.lock().unwrap();
    if let Some(session) = cl.load_session() {
        self.chat_display = session.messages.iter().map(|m| ChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
        }).collect();
    }
}
```

Also remove the config-based initialization of `chat_display` in `new()` or make it a fallback.

---

### Bug 3: Session Not Saved After Messages (High)

**Location:** `src/ui/window.rs:407-422`

**Problem:** `save_session()` is only called in `eframe::App::save()` which runs on app close. If the app crashes or is force-closed, session data is lost.

**Impact:** Lost conversations on crash/force-close.

**Fix:** Save session after each message is added and on session switch.

Add to `add_message()`:
```rust
pub(super) fn add_message(&mut self, role: &str, content: &str) {
    self.chat_display.push(ChatMessage {
        role: role.to_string(),
        content: content.to_string(),
    });
    // Truncate if too many messages
    if self.chat_display.len() > self.max_display_messages {
        self.chat_display.drain(..self.chat_display.len() - self.max_display_messages);
    }
    // Save session after adding message
    if let Err(e) = self.client.lock().unwrap().save_session() {
        eprintln!("Failed to save session: {}", e);
    }
}
```

---

### Bug 4: Dead clear_action Code (Low)

**Location:** `src/ui/sessions_panel.rs:16, 63-65, 144-146`

**Problem:** The `clear_action` field exists and is set when the "Clear" button is clicked (line 63-65), and checked in `window.rs` (line 137-142). However, the check at line 144-146 in `sessions_panel.rs` is dead code because the action is already processed by `window.rs`.

**Fix:** Remove the dead code at lines 144-146 in `sessions_panel.rs`:
```rust
// REMOVE THIS:
if self.clear_action {
    self.clear_action = false;
}
```

The clear action flow is:
1. User clicks "Clear" button → `self.clear_action = true`
2. `draw()` returns, panel returns `selected_id` (not the clear action)
3. `window.rs` checks `panel.clear_action` and calls `cl.clear_session_messages()`
4. `window.rs` sets `panel.clear_action = false`

This is actually correct - the dead code is just redundant. Remove it to avoid confusion.

---

### Bug 5: No Session Save on Switch (Medium)

**Location:** `src/ui/window.rs:291-301`

**Problem:** When switching sessions, the old session is not saved before loading the new one.

**Fix:** Save the current session before switching:

```rust
fn switch_session(&mut self, session_id: &str) {
    // Save current session before switching
    if let Err(e) = self.client.lock().unwrap().save_session() {
        eprintln!("Failed to save session before switch: {}", e);
    }
    
    let mut cl = self.client.lock().unwrap();
    cl.set_session(Some(session_id.to_string()), cl.session_dir.clone());
    if let Some(session) = cl.load_session() {
        self.chat_display = session.messages.iter().map(|m| ChatMessage {
            role: m.role.clone(),
            content: m.content.clone(),
        }).collect();
        cl.set_system_prompt(&session.system_prompt);
    }
    drop(cl);
}
```

---

## Additional Observations

### Dual Message Stores

The system has three message stores that should be kept in sync:
1. `client.conversation` (`Arc<Mutex<Vec<Message>>>`) - the source of truth
2. `config.chat_history` (`Vec<ChatMessage>`) - legacy persistence in config
3. `chat_display` (`Vec<ChatMessage>`) - UI display state

**Recommendation:** Keep `chat_history` in config as a fallback for quick restarts, but the primary persistence is via session files. The config `chat_history` should be updated when switching sessions or on app close.

### Session Save Timing

Current save points:
- App close (via `eframe::App::save()`)

Proposed additional save points:
- After each message is added
- After session switch
- After clear action

---

## Implementation Plan

### Step 1: Fix `switch_session()` in `src/ui/window.rs`
- Update `session_id` before loading
- Save current session before switching

### Step 2: Fix `ChatApp::new()` initialization
- Load session into `chat_display` after creating `sessions_panel`

### Step 3: Add session save in `add_message()`
- Save after each message addition

### Step 4: Remove dead code in `sessions_panel.rs`
- Remove the redundant `clear_action` check at the end of `draw()`

### Step 5: Run tests
- Verify session creation, loading, and saving
- Verify session switching works correctly

---

## Files to Modify

1. `src/ui/window.rs` - Fix `switch_session()`, `ChatApp::new()`, `add_message()`
2. `src/ui/sessions_panel.rs` - Remove dead code
3. `tests/sessions_test.rs` - Add tests for session switching

---

## Testing Checklist

- [ ] Start app, send messages, close app, restart app - messages should persist
- [ ] Create two sessions, switch between them - each should show its own messages
- [ ] Rename a session - name should persist after restart
- [ ] Delete a session - should disappear from list
- [ ] Clear a session - messages should be cleared
- [ ] App crash simulation - session should be saved (via periodic saves)
