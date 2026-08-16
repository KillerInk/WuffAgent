use iced::widget::{button, column, text};
use iced::Element;

use crate::app::messages::Message;
use iced::theme::Palette;

pub fn view(pal: Palette) -> Element<'static, Message> {
    let content = column!()
        .push(text("Agent Config").size(16).color(pal.text))
        .push(text("Agent configuration panel — to be implemented").size(12))
        .push(button(text("Close")).on_press(Message::AgentConfigClosed).padding([4u16, 8u16]));
    content.padding([16u16, 16u16]).width(400).into()
}
