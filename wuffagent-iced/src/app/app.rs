use iced::Subscription;
use iced::Task;
use iced::Element;

use super::backend::Backend;
use super::messages::Message;
use super::state::AppState;
use super::subscription::event_subscription;

/// Boot function: creates initial state from backend flags.
pub fn boot(backend: std::sync::Arc<Backend>) -> (AppState, Task<Message>) {
    let presets = crate::config::PresetStore::default();
    let state = AppState::new(backend.config.clone(), presets, backend);
    (state, Task::none())
}

/// Update function for the iced application.
pub fn update(state: &mut AppState, message: Message) -> Task<Message> {
    let backend = state.backend.clone();
    super::update::update(message, state, &backend)
}

/// View function for the iced application.
pub fn view<'a>(state: &'a AppState) -> Element<'a, Message> {
    super::view::view(state, &state.backend)
}

/// Subscription function.
pub fn subscription() -> Subscription<Message> {
    event_subscription()
}
