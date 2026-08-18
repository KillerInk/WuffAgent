use std::sync::Arc;
use iced::Task;

use super::messages::Message;
use super::state::{AppState, Dialog};
use super::backend::Backend;

/// Update function for the iced application.
pub fn update(message: Message, state: &mut AppState, backend: &Backend) -> Task<Message> {
    match message {
        Message::FakeEvent => {
            tracing::debug!("Fake event received (spike test)");
            Task::none()
        }
        Message::AppEvent(event) => {
            tracing::debug!("App event: {:?}", event);
            handle_app_event(event, state, backend)
        }
        Message::InputChanged(text) => {
            state.chat.input_text = text;
            Task::none()
        }
        Message::Send => {
            handle_send(state, backend)
        }
        Message::Stop => {
            handle_stop(state, backend)
        }
        Message::Scrolled(offset) => {
            handle_scrolled(offset, state);
            Task::none()
        }
        Message::ThemeToggled => {
            handle_theme_toggle(state);
            Task::none()
        }
        Message::JumpToBottom => {
            state.scroll.at_bottom = true;
            state.scroll.follow_bottom = true;
            Task::none()
        }
        Message::ToggleFollowBottom => {
            state.scroll.follow_bottom = !state.scroll.follow_bottom;
            Task::none()
        }
        Message::SettingsClicked => {
            let cfg = state.config.lock().unwrap().clone();
            state.dialog = Some(Dialog::Settings);
            state.settings_dialog = Some(super::widgets::dialogs::settings::SettingsDialog::new(&cfg));
            Task::none()
        }
        Message::SettingsSaved => {
            if let Some(ref sd) = state.settings_dialog {
                sd.save(&state.config);
                // Save presets too
                if let Some(path) = crate::config::get_presets_path().parent() {
                    if let Err(e) = state.presets.save(&path.join("presets.json")) {
                        tracing::warn!("Failed to save presets: {}", e);
                    }
                }
            }
            state.dialog = None;
            state.settings_dialog = None;
            Task::none()
        }
        Message::SettingsClosed => {
            state.dialog = None;
            state.settings_dialog = None;
            Task::none()
        }
        Message::PresetsClicked => {
            state.dialog = Some(Dialog::Presets);
            state.presets_dialog = Some(super::widgets::dialogs::presets::PresetsDialog::new(state.presets.clone()));
            Task::none()
        }
        Message::PresetsClosed => {
            state.dialog = None;
            state.presets_dialog = None;
            Task::none()
        }
        Message::AgentConfigClicked => {
            state.dialog = Some(Dialog::AgentConfig);
            state.agent_config_dialog = Some(super::widgets::dialogs::agent_config::AgentConfigDialog::new(&Arc::new(backend.clone())));
            Task::none()
        }
        Message::AgentConfigClosed => {
            state.dialog = None;
            state.agent_config_dialog = None;
            Task::none()
        }
        Message::SessionSelected(id) => {
            handle_session_select(id, state, backend)
        }
        Message::SessionCreated => {
            handle_session_create(state, backend)
        }
        Message::SessionRenamed(id, name) => {
            handle_session_rename(id, name, state, backend)
        }
        Message::SessionDeleted(id) => {
            handle_session_delete(id, state, backend)
        }
        Message::PipelineCancelled => {
            handle_pipeline_cancel(state, backend)
        }
        Message::ErrorToast(msg) => {
            state.error_toast = Some(msg);
            Task::none()
        }
        Message::DismissErrorToast => {
            state.error_toast = None;
            Task::none()
        }
        Message::MessageEditEntered(index) => {
            state.chat.editing_message_index = Some(index);
            if let Some(msg) = state.chat.messages.get(index) {
                state.chat.editing_message_content = msg.content.clone();
            }
            Task::none()
        }
        Message::MessageEditChanged(text) => {
            state.chat.editing_message_content = text;
            Task::none()
        }
        Message::MessageEditCommitted(index) => {
            let content = state.chat.editing_message_content.clone();
            handle_message_edit_commit(index, &content, state, backend)
        }
        Message::MessageEditCancelled => {
            state.chat.editing_message_index = None;
            state.chat.editing_message_content.clear();
            Task::none()
        }
        Message::MessageDeleted(index) => {
            handle_message_delete(index, state, backend)
        }
        Message::ImageAttached(path) => {
            state.chat.pending_image = path;
            Task::none()
        }
        // ─── Settings dialog ─────────────────────────────────────────────────────
        Message::SettingsServerPath(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.server_path = v;
            }
            Task::none()
        }
        Message::SettingsModelPath(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.model_path = v;
            }
            Task::none()
        }
        Message::SettingsPort(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.port = v.parse().unwrap_or(sd.port);
            }
            Task::none()
        }
        Message::SettingsGpuLayers(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.n_gpu_layers = v.parse().unwrap_or(sd.n_gpu_layers);
            }
            Task::none()
        }
        Message::SettingsNCtx(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.n_ctx = v.parse().unwrap_or(sd.n_ctx);
            }
            Task::none()
        }
        Message::SettingsThreads(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.threads = v.parse().unwrap_or(sd.threads);
            }
            Task::none()
        }
        Message::SettingsSystemPrompt(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.system_prompt = v;
            }
            Task::none()
        }
        Message::SettingsStreaming(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.streaming = v;
            }
            Task::none()
        }
        Message::SettingsTheme(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.theme = v.clone();
                state.theme_name = v;
            }
            Task::none()
        }
        Message::SettingsConnectionType(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.connection_type = v;
            }
            Task::none()
        }
        Message::SettingsRemoteUrl(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.remote_url = v;
            }
            Task::none()
        }
        Message::SettingsRemoteApiKey(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.remote_api_key = v;
            }
            Task::none()
        }
        Message::SettingsEncryptionEnabled(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.encryption_enabled = v;
            }
            Task::none()
        }
        Message::SettingsEncryptionPassword(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.encryption_password = v;
            }
            Task::none()
        }
        Message::SettingsMaxMessages(v) => {
            if let Some(ref mut sd) = state.settings_dialog {
                sd.max_messages = v.parse().unwrap_or(sd.max_messages);
            }
            Task::none()
        }
        Message::SettingsShowPresets => {
            state.dialog = Some(Dialog::Presets);
            state.settings_dialog = None;
            state.presets_dialog = Some(super::widgets::dialogs::presets::PresetsDialog::new(state.presets.clone()));
            Task::none()
        }
        // ─── Presets dialog ──────────────────────────────────────────────────────
        Message::PresetsSelect(i) => {
            if let Some(ref mut pd) = state.presets_dialog {
                if pd.store.presets.len() > i {
                    pd.selected_index = if pd.selected_index == Some(i) { None } else { Some(i) };
                }
            }
            Task::none()
        }
        Message::PresetsLoad(i) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.load(i, &state.config);
            }
            Task::none()
        }
        Message::PresetsDelete(i) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.delete(i);
            }
            Task::none()
        }
        Message::PresetsNew => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.show_add_form = true;
                pd.selected_index = None;
                pd.clear_new_form();
            }
            Task::none()
        }
        Message::PresetsNewName(v) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.new_name = v;
            }
            Task::none()
        }
        Message::PresetsNewType(t) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.new_type = t;
            }
            Task::none()
        }
        Message::PresetsNewServerPath(v) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.new_server_path = v;
            }
            Task::none()
        }
        Message::PresetsNewModelPath(v) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.new_model_path = v;
            }
            Task::none()
        }
        Message::PresetsNewPort(v) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.new_port = v.parse().unwrap_or(pd.new_port);
            }
            Task::none()
        }
        Message::PresetsNewGpuLayers(v) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.new_n_gpu_layers = v.parse().unwrap_or(pd.new_n_gpu_layers);
            }
            Task::none()
        }
        Message::PresetsNewNCtx(v) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.new_n_ctx = v.parse().unwrap_or(pd.new_n_ctx);
            }
            Task::none()
        }
        Message::PresetsNewThreads(v) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.new_threads = v.parse().unwrap_or(pd.new_threads);
            }
            Task::none()
        }
        Message::PresetsNewRemoteUrl(v) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.new_remote_url = v;
            }
            Task::none()
        }
        Message::PresetsNewRemoteApiKey(v) => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.new_remote_api_key = v;
            }
            Task::none()
        }
        Message::PresetsAdd => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.add();
            }
            Task::none()
        }
        Message::PresetsCancelAdd => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.show_add_form = false;
            }
            Task::none()
        }
        Message::PresetsSaveStore => {
            if let Some(ref mut pd) = state.presets_dialog {
                pd.save_store();
                state.presets = pd.store.clone();
            }
            Task::none()
        }
        // ─── Agent config dialog ─────────────────────────────────────────────────
        Message::AgentConfigSelect(i) => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.select_agent(i);
            }
            Task::none()
        }
        Message::AgentConfigEdit(i) => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.edit_agent(i);
            }
            Task::none()
        }
        Message::AgentConfigNew => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.new_agent();
            }
            Task::none()
        }
        Message::AgentConfigName(v) => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.name = v;
            }
            Task::none()
        }
        Message::AgentConfigDescription(v) => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.description = v;
            }
            Task::none()
        }
        Message::AgentConfigPriority(v) => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.priority = v.parse().unwrap_or(ad.priority);
            }
            Task::none()
        }
        Message::AgentConfigMaxConcurrent(v) => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.max_concurrent = v.parse().unwrap_or(ad.max_concurrent);
            }
            Task::none()
        }
        Message::AgentConfigEnabled(v) => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.enabled = v;
            }
            Task::none()
        }
        Message::AgentConfigSystemPrompt(v) => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.system_prompt = v;
            }
            Task::none()
        }
        Message::AgentConfigSelectAllTools => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                for cb in &mut ad.tool_checkboxes {
                    *cb = true;
                }
                ad.sync_tools_from_checkboxes();
            }
            Task::none()
        }
        Message::AgentConfigClearTools => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                for cb in &mut ad.tool_checkboxes {
                    *cb = false;
                }
                ad.sync_tools_from_checkboxes();
            }
            Task::none()
        }
        Message::AgentConfigToolToggle(i, v) => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                if i < ad.tool_checkboxes.len() {
                    ad.tool_checkboxes[i] = v;
                }
            }
            Task::none()
        }
        Message::AgentConfigSave => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.save(&Arc::new(backend.clone()));
            }
            Task::none()
        }
        Message::AgentConfigCancel => {
            state.dialog = None;
            state.agent_config_dialog = None;
            Task::none()
        }
        Message::AgentConfigDelete => {
            if let Some(ref mut ad) = state.agent_config_dialog {
                ad.delete_agent(&Arc::new(backend.clone()));
            }
            Task::none()
        }
    }
}

fn handle_app_event(event: crate::types::AppEvent, state: &mut AppState, _backend: &Backend) -> Task<Message> {
    match event {
        crate::types::AppEvent::StreamChunk { content } => {
            state.chat.streaming = true;
            state.chat.current_response.push_str(&content);
            if state.scroll.follow_bottom {
                state.scroll.at_bottom = true;
            }
            Task::none()
        }
        crate::types::AppEvent::StreamComplete { content: _, usage } => {
            state.chat.streaming = false;
            state.chat.is_generating = false;
            state.chat.current_response.clear();
            state.chat.current_thinking.clear();
            if let Some(u) = usage {
                state.chat.token_count = u.total_tokens;
            }
            Task::none()
        }
        crate::types::AppEvent::StreamError { error } => {
            state.chat.streaming = false;
            state.chat.is_generating = false;
            state.chat.status = crate::types::AppStatus::Error(error.clone());
            Task::none()
        }
        crate::types::AppEvent::MessageResult { content: _, usage } => {
            state.chat.is_generating = false;
            if let Some(u) = usage {
                state.chat.token_count = u.total_tokens;
            }
            Task::none()
        }
        crate::types::AppEvent::MessageError { error } => {
            state.chat.is_generating = false;
            state.chat.status = crate::types::AppStatus::Error(error.clone());
            Task::none()
        }
        crate::types::AppEvent::StreamThinkingChunk { content } => {
            state.chat.current_thinking.push_str(&content);
            Task::none()
        }
        crate::types::AppEvent::StreamThinkingComplete { content } => {
            state.chat.current_thinking = content;
            Task::none()
        }
        crate::types::AppEvent::AgentChainStarted { agent_name, depth } => {
            state.panels.chain_active = true;
            state.panels.chain_entries.push(
                crate::sessions::model::AgentChainEntry {
                    agent_name,
                    request: String::new(),
                    result: String::new(),
                    depth,
                    tool_calls: Vec::new(),
                    completed_at: chrono::Utc::now(),
                    error: None,
                    status: crate::sessions::model::AgentChainEntryStatus::Running,
                }
            );
            Task::none()
        }
        crate::types::AppEvent::AgentChainCompleted { agent_name, result, depth } => {
            if let Some(entry) = state.panels.chain_entries.iter_mut().find(|e| e.agent_name == agent_name && e.depth == depth) {
                entry.result = result;
                entry.status = crate::sessions::model::AgentChainEntryStatus::Completed;
            }
            Task::none()
        }
        crate::types::AppEvent::AgentChainError { agent_name, error, depth } => {
            if let Some(entry) = state.panels.chain_entries.iter_mut().find(|e| e.agent_name == agent_name && e.depth == depth) {
                entry.result = format!("Error: {}", error);
                entry.status = crate::sessions::model::AgentChainEntryStatus::Failed;
            }
            Task::none()
        }
        crate::types::AppEvent::AgentChainComplete { response: _, entries } => {
            state.panels.chain_entries = entries;
            state.panels.chain_active = false;
            Task::none()
        }
        crate::types::AppEvent::NCtxUpdated { n_ctx } => {
            state.remote_n_ctx = n_ctx;
            Task::none()
        }
        crate::types::AppEvent::AgentTaskStarted { task_id, task_description, agent_type } => {
            state.panels.pipeline_active = true;
            state.panels.pipeline_tasks.push(
                crate::app::state::PipelineTaskEntry {
                    id: task_id,
                    description: task_description,
                    status: crate::app::state::PipelineTaskStatus::Running,
                    agent_type,
                }
            );
            Task::none()
        }
        crate::types::AppEvent::AgentTaskCompleted { task_id, status, duration_ms: _ } => {
            if let Some(task) = state.panels.pipeline_tasks.iter_mut().find(|t| t.id == task_id) {
                task.status = match status.as_str() {
                    "completed" => crate::app::state::PipelineTaskStatus::Completed,
                    "failed" => crate::app::state::PipelineTaskStatus::Failed,
                    _ => crate::app::state::PipelineTaskStatus::Pending,
                };
            }
            Task::none()
        }
        crate::types::AppEvent::AgentPipelineComplete { result_count: _, final_output: _ } => {
            state.panels.pipeline_active = false;
            for task in &mut state.panels.pipeline_tasks {
                if task.status == crate::app::state::PipelineTaskStatus::Running {
                    task.status = crate::app::state::PipelineTaskStatus::Completed;
                }
            }
            Task::none()
        }
        crate::types::AppEvent::AgentPipelineError { error: _ } => {
            state.panels.pipeline_active = false;
            Task::none()
        }
        crate::types::AppEvent::AgentPipelineCancelled => {
            state.panels.pipeline_active = false;
            for task in &mut state.panels.pipeline_tasks {
                task.status = crate::app::state::PipelineTaskStatus::Failed;
            }
            Task::none()
        }
        _ => Task::none(),
    }
}

fn handle_send(state: &mut AppState, backend: &Backend) -> Task<Message> {
    if state.chat.input_text.is_empty() {
        return Task::none();
    }
    let text = state.chat.input_text.clone();
    let image = state.chat.pending_image.take();
    state.chat.input_text.clear();
    state.chat.is_generating = true;
    state.chat.status = crate::types::AppStatus::Generating;

    let client = backend.client.clone();
    let sender = backend.event_sender.clone();
    let config = backend.config.clone();
    backend.spawn(async move {
        // Placeholder: actual message sending will be wired in Phase 2
        let _ = (client, sender, text, image, config);
    });
    Task::none()
}

fn handle_stop(state: &mut AppState, backend: &Backend) -> Task<Message> {
    let _ = backend;
    state.chat.is_generating = false;
    state.chat.streaming = false;
    state.chat.current_response.clear();
    state.chat.current_thinking.clear();
    Task::none()
}

fn handle_scrolled(offset: f32, state: &mut AppState) {
    let _ = offset;
    state.scroll.at_bottom = false;
}

fn handle_theme_toggle(state: &mut AppState) {
    state.theme_name = if state.theme_name == "dark" {
        "light".to_string()
    } else {
        "dark".to_string()
    };
}

fn handle_session_select(id: String, state: &mut AppState, backend: &Backend) -> Task<Message> {
    state.sessions.selected_id = Some(id.clone());
    let client = backend.client.clone();
    let sessions_dir;
    {
        let cfg = backend.config.lock().unwrap();
        sessions_dir = cfg.sessions_dir.clone();
    }
    backend.spawn(async move {
        if let Ok(mut cl) = client.lock() {
            let _ = cl.set_session(Some(id), sessions_dir);
            let _ = cl.load_session();
        }
    });
    Task::none()
}

fn handle_session_create(state: &mut AppState, backend: &Backend) -> Task<Message> {
    let sessions_dir;
    {
        let cfg = backend.config.lock().unwrap();
        sessions_dir = cfg.sessions_dir.clone();
    }
    let session = crate::sessions::create_session(&sessions_dir, "Untitled");
    let sid = session.id.clone();
    let client = backend.client.clone();
    let config = backend.config.clone();
    backend.spawn(async move {
        if let Ok(mut cl) = client.lock() {
            let _ = cl.set_session(Some(sid), sessions_dir);
            let _ = cl.load_session();
        }
        if let Ok(cfg) = config.lock() {
            let _ = cfg.save();
        }
    });
    state.sessions.selected_id = Some(session.id);
    Task::none()
}

fn handle_session_rename(id: String, name: String, _state: &mut AppState, backend: &Backend) -> Task<Message> {
    let sessions_dir;
    {
        let cfg = backend.config.lock().unwrap();
        sessions_dir = cfg.sessions_dir.clone();
    }
    let config = backend.config.clone();
    backend.spawn(async move {
        if let Some(mut session) = crate::sessions::load_session(&sessions_dir, &id) {
            session.name = name;
            let _ = crate::sessions::save_session(&sessions_dir, &session);
        }
        if let Ok(cfg) = config.lock() {
            if let Some(sid) = &cfg.session_id {
                if sid == &id {
                    let _ = cfg.save();
                }
            }
        }
    });
    Task::none()
}

fn handle_session_delete(id: String, _state: &mut AppState, backend: &Backend) -> Task<Message> {
    let sessions_dir;
    {
        let cfg = backend.config.lock().unwrap();
        sessions_dir = cfg.sessions_dir.clone();
    }
    let client = backend.client.clone();
    let config = backend.config.clone();
    backend.spawn(async move {
        let _ = crate::sessions::delete_session(&sessions_dir, &id);
        let session = crate::sessions::create_session(&sessions_dir, "Untitled");
        if let Ok(mut cl) = client.lock() {
            let _ = cl.set_session(Some(session.id.clone()), sessions_dir.clone());
            let _ = cl.load_session();
        }
        if let Ok(mut cfg) = config.lock() {
            cfg.session_id = Some(session.id.clone());
            let _ = cfg.save();
        }
    });
    Task::none()
}

fn handle_pipeline_cancel(state: &mut AppState, backend: &Backend) -> Task<Message> {
    let _ = backend;
    state.panels.pipeline_active = false;
    state.panels.pipeline_tasks.clear();
    Task::none()
}

fn handle_message_edit_commit(index: usize, content: &str, state: &mut AppState, backend: &Backend) -> Task<Message> {
    if let Some(msg) = state.chat.messages.get_mut(index) {
        msg.content = content.to_string();
    }
    state.chat.editing_message_index = None;
    state.chat.editing_message_content.clear();
    let client = backend.client.clone();
    backend.spawn(async move {
        if let Ok(cl) = client.lock() {
            let _ = cl.save_session();
        }
    });
    Task::none()
}

fn handle_message_delete(index: usize, state: &mut AppState, backend: &Backend) -> Task<Message> {
    state.chat.messages.remove(index);
    state.chat.editing_message_index = None;
    state.chat.editing_message_content.clear();
    let client = backend.client.clone();
    backend.spawn(async move {
        if let Ok(cl) = client.lock() {
            let _ = cl.save_session();
        }
    });
    Task::none()
}
