use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Element, Length};

use crate::app::messages::Message;
use crate::app::state::AppState;
use iced::theme::Palette;

pub fn view(state: &AppState, pal: Palette) -> Element<'static, Message> {
    let create_btn = button(text("+ New"))
        .on_press(Message::SessionCreated)
        .padding([4u16, 8u16]);

    let empty_placeholder = container(text("No sessions yet.")).width(Length::Fill);

    let mut session_col = column!()
        .push(empty_placeholder)
        .spacing(2);
    for s in &state.sessions.sessions {
        session_col = session_col.push(view_session(s, state, pal));
    }

    column!()
        .push(create_btn)
        .push(session_col)
        .padding([8u16, 8u16])
        .into()
}

fn view_session(s: &crate::sessions::model::Session, state: &AppState, pal: Palette) -> Element<'static, Message> {
    let selected = state.sessions.selected_id.as_deref() == Some(&s.id);
    let name = if selected {
        format!("[{}] {}", &s.id[..8], s.name)
    } else {
        format!("  {}", s.name)
    };
    let status_color = if selected { pal.primary } else { pal.text };
    let name_owned = name;
    let row = row![
        text(name_owned).size(12).color(status_color),
        column!().push(text("")).spacing(0),
        button(text("Del")).on_press(Message::SessionDeleted(s.id.clone())).padding([2u16, 4u16]),
    ]
    .align_y(Alignment::Center)
    .padding([4u16, 8u16]);
    container(row).into()
}
