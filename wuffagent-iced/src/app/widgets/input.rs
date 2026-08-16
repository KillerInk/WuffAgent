use iced::widget::{button, row, text, text_input};
use iced::{Alignment, Element};

use crate::app::messages::Message;
use crate::app::state::AppState;
use iced::theme::Palette;

pub fn view(state: &AppState, _pal: Palette) -> Element<'static, Message> {
    let send_disabled = state.chat.input_text.is_empty() && state.chat.pending_image.is_none();

    let input = text_input("Type a message... (Ctrl+Enter to send)", &state.chat.input_text)
        .on_input(Message::InputChanged)
        .on_submit(Message::Send)
        .padding([8u16, 12u16])
        .size(14);

    let send_btn = if state.chat.is_generating {
        button(text("■ Stop"))
            .on_press(Message::Stop)
            .padding([8, 16])
    } else if send_disabled {
        button(text("Send"))
            .on_press(Message::Send)
            .padding([8, 16])
    } else {
        button(text("Send"))
            .on_press(Message::Send)
            .padding([8, 16])
    };

    row!()
        .push(input)
        .push(send_btn)
        .align_y(Alignment::Center)
        .spacing(8)
        .padding([8u16, 16u16])
        .into()
}
