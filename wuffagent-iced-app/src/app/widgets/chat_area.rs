use iced::widget::{column, container, scrollable, text};
use iced::{Element, Length};

use super::message::view_message;
use crate::app::messages::Message;
use crate::app::state::AppState;
use iced::theme::Palette;

pub fn view(state: &AppState, pal: Palette) -> Element<'static, Message> {
    let messages: Vec<Element<Message>> = state
        .chat
        .messages
        .iter()
        .enumerate()
        .map(|(i, msg)| view_message(msg, i, state, pal))
        .collect();

    let spacer1: Element<Message> = container(text("")).padding([10u16, 0u16]).into();
    let spacer2: Element<Message> = container(text("")).padding([10u16, 0u16]).into();

    let chat_list = scrollable(
        column!()
            .push(spacer1)
            .extend(messages)
            .push(spacer2)
            .spacing(4),
    )
    .on_scroll(|_offset| Message::Scrolled(0.0))
    .height(Length::Fill);

    // Jump-to-bottom button
    let jump_btn: Element<Message> = if state.scroll.jump_button_opacity > 0.0 && !state.scroll.at_bottom {
        container(
            iced::widget::button(text("↓"))
                .on_press(Message::JumpToBottom)
                .padding([4u16, 8u16]),
        )
        .into()
    } else {
        container(text("")).into()
    };

    column!()
        .push(chat_list)
        .push(jump_btn)
        .height(Length::Fill)
        .into()
}
