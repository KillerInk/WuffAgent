use iced::widget::{container, row, text};
use iced::theme::Palette;
use iced::Element;

use super::super::messages::Message;
use super::super::state::AppState;

pub fn view_message(msg: &crate::types::ChatMessage, _index: usize, state: &AppState, pal: Palette) -> Element<'static, Message> {
    let _ = state;
    let is_user = msg.role == "user";
    let text_color = if is_user { iced::Color::WHITE } else { pal.text };
    let _bg_color = if is_user { pal.primary } else { pal.background };
    let initials = if is_user { "U" } else { "A" };
    let content_str = msg.content.clone();
    let timestamp_str = msg.timestamp.clone();
    let _role_str = msg.role.clone();

    let avatar = container(
        text(initials)
            .size(10)
            .color(iced::Color::WHITE)
    )
    .width(24)
    .height(24)
    .align_x(iced::Alignment::Center)
    .align_y(iced::Alignment::Center);

    let content = text(content_str).size(14).color(text_color);

    let bubble = row!()
        .push(avatar)
        .push(container(content).padding([8u16, 12u16]))
        .align_y(iced::Alignment::Start);

    let timestamp = text(timestamp_str).size(10).color(pal.text);

    let is_user_clone = is_user;
    let row = if is_user_clone {
        row![bubble, timestamp].align_y(iced::Alignment::Start)
    } else {
        row![timestamp, bubble].align_y(iced::Alignment::Start)
    };

    container(row)
        .padding([4u16, 8u16])
        .into()
}
