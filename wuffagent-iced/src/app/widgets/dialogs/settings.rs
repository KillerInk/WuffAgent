use iced::widget::{button, column, row, text, text_input};
use iced::{Alignment, Element};

use crate::app::messages::Message;
use iced::theme::Palette;

pub fn view(config: &crate::config::Config, pal: Palette) -> Element<'static, Message> {
    let row = row!()
        .push(button(text("Save")).on_press(Message::SettingsSaved).padding([4u16, 8u16]))
        .push(button(text("Close")).on_press(Message::SettingsClosed).padding([4u16, 8u16]))
        .align_y(Alignment::Center)
        .spacing(8);
    column!()
        .push(text("Settings").size(16).color(pal.text))
        .push(text_input("Model path", &config.model_path)
            .on_input(|_| Message::SettingsSaved)
            .padding([4u16, 8u16]))
        .push(text_input("Server path", &config.server_path)
            .on_input(|_| Message::SettingsSaved)
            .padding([4u16, 8u16]))
        .push(text_input("Port", &config.port.to_string())
            .on_input(|_| Message::SettingsSaved)
            .padding([4u16, 8u16]))
        .push(row)
        .padding([16u16, 16u16])
        .width(400)
        .into()
}
