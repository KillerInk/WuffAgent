use iced::widget::container;
use iced::Element;

use crate::app::messages::Message;
use crate::app::state::AppState;
use iced::theme::Palette;

pub fn view(state: &AppState, pal: Palette) -> Element<'static, Message> {
    let status_text = match &state.chat.status {
        crate::types::AppStatus::Stopped | crate::types::AppStatus::Connecting | crate::types::AppStatus::Ready => {
            "Ready".to_string()
        }
        crate::types::AppStatus::Generating => "Generating...".to_string(),
        crate::types::AppStatus::Error(msg) => format!("Error: {}", msg),
    };
    let status_color = match &state.chat.status {
        crate::types::AppStatus::Stopped | crate::types::AppStatus::Connecting | crate::types::AppStatus::Ready => {
            pal.text
        }
        crate::types::AppStatus::Generating => pal.text,
        crate::types::AppStatus::Error(_) => pal.danger,
    };

    let context_pct = (state.chat.context_used * 100.0) as u32;
    let status = format!(
        "{}  Tokens: {}  Context: {}%  n_ctx: {}  Theme: {}",
        status_text,
        state.chat.token_count,
        context_pct,
        state.remote_n_ctx,
        state.theme_name
    );

    container(iced::widget::text(status).size(11).color(status_color)).into()
}
