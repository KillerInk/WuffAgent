use iced::widget::{button, column, text};
use iced::Element;

use crate::app::messages::Message;
use iced::theme::Palette;
use crate::config::PresetStore;

pub fn view(store: &PresetStore, pal: Palette) -> Element<'static, Message> {
    let mut presets_col = column!().spacing(2);
    for p in &store.presets {
        let name = match p {
            crate::config::Preset::Local(l) => l.name.clone(),
            crate::config::Preset::Remote(r) => r.name.clone(),
        };
        presets_col = presets_col.push(text(name).size(12));
    }

    let content = column!()
        .push(text("Presets").size(16).color(pal.text))
        .push(presets_col)
        .push(button(text("Close")).on_press(Message::PresetsClosed).padding([4u16, 8u16]));
    content.padding([16u16, 16u16]).width(300).into()
}
