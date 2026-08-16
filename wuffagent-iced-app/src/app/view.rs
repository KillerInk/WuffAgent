use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Element, Length};

use super::backend::Backend;
use super::messages::Message;
use super::state::AppState;
use super::theme::palette;
use super::widgets::chat_area;
use super::widgets::input;
use super::widgets::status_bar;

/// Main view for the iced application.
pub fn view(state: &AppState, backend: &Backend) -> Element<'static, Message> {
    let _ = backend;
    let pal = palette(&state.theme_name);

    let header = view_header(state, pal);
    let chat = chat_area::view(state, pal);
    let input_area = input::view(state, pal);
    let status = status_bar::view(state, pal);

    container(
        column!()
            .push(header)
            .push(chat)
            .push(input_area)
            .push(status)
            .height(Length::Fill)
            .width(Length::Fill),
    )
    .into()
}

fn view_header(state: &AppState, pal: iced::theme::Palette) -> Element<'static, Message> {
    let title = text("WuffAgent")
        .size(20)
        .color(pal.text);

    let theme_btn = if state.theme_name == "dark" {
        button(text("☀ Light"))
            .on_press(Message::ThemeToggled)
            .padding([4u16, 8u16])
    } else {
        button(text("🌙 Dark"))
            .on_press(Message::ThemeToggled)
            .padding([4u16, 8u16])
    };

    let settings_btn = button(text("⚙ Settings"))
        .on_press(Message::SettingsClicked)
        .padding([4u16, 8u16]);

    let presets_btn = button(text("Presets"))
        .on_press(Message::PresetsClicked)
        .padding([4u16, 8u16]);

    let agent_btn = button(text("Agents"))
        .on_press(Message::AgentConfigClicked)
        .padding([4u16, 8u16]);

    row!()
        .push(title)
        .push(column!().push(text("")).spacing(0))
        .push(agent_btn)
        .push(presets_btn)
        .push(theme_btn)
        .push(settings_btn)
        .align_y(Alignment::Center)
        .padding([8u16, 16u16])
        .into()
}
