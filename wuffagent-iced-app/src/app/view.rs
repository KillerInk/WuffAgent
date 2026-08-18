use iced::widget::{button, column, container, row, text, scrollable};
use iced::{Alignment, Element, Length};

use super::backend::Backend;
use super::messages::Message;
use super::state::{AppState, Dialog};
use super::theme::palette;
use super::widgets::chat_area;
use super::widgets::input;
use super::widgets::status_bar;

/// Main view for the iced application.
pub fn view<'a>(state: &'a AppState, backend: &'a Backend) -> Element<'a, Message> {
    let _ = backend;
    let pal = palette(&state.theme_name);

    let header = view_header(state, pal);
    let chat = chat_area::view(state, pal);
    let input_area = input::view(state, pal);
    let status = status_bar::view(state, pal);

    let content = column!()
        .push(header)
        .push(chat)
        .push(input_area)
        .push(status);

    let element: Element<'_, Message> = match &state.dialog {
        Some(Dialog::Settings) => {
            let dialog_el = if let Some(ref sd) = state.settings_dialog {
                sd.view(pal)
            } else {
                column!().into()
            };
            let stack = column!()
                .push(content)
                .push(dialog_el);
            container(scrollable(stack))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        Some(Dialog::Presets) => {
            let dialog_el = if let Some(ref pd) = state.presets_dialog {
                pd.view(pal)
            } else {
                column!().into()
            };
            let stack = column!()
                .push(content)
                .push(dialog_el);
            container(scrollable(stack))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        Some(Dialog::AgentConfig) => {
            let dialog_el = if let Some(ref ad) = state.agent_config_dialog {
                ad.view(pal)
            } else {
                column!().into()
            };
            let stack = column!()
                .push(content)
                .push(dialog_el);
            container(scrollable(stack))
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
        None => {
            container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        }
    };

    element
}

fn view_header(_state: &AppState, pal: iced::theme::Palette) -> Element<'static, Message> {
    let title = text("WuffAgent")
        .size(16)
        .color(pal.text)
        .font(iced::font::Font::DEFAULT);

    let settings_btn = button(text("Settings"))
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
        .push(iced::widget::text(""))
        .push(settings_btn)
        .push(presets_btn)
        .push(agent_btn)
        .align_y(Alignment::Center)
        .padding([8u16, 16u16])
        .into()
}
