use iced::widget::{button, column, row, text};
use iced::Element;

use crate::app::messages::Message;
use crate::app::state::{AppState, PipelineTaskStatus};
use iced::theme::Palette;

pub fn view(state: &AppState, pal: Palette) -> Element<'static, Message> {
    let mut pipeline_col = column!().spacing(2);
    for t in &state.panels.pipeline_tasks {
        pipeline_col = pipeline_col.push(view_task(t, pal));
    }

    let mut entries_col = column!().spacing(2);
    for e in &state.panels.chain_entries {
        entries_col = entries_col.push(view_chain_entry(e, pal));
    }

    let cancel_btn = if state.panels.pipeline_active {
        Some(
            button(text("Cancel"))
                .on_press(Message::PipelineCancelled)
                .padding([4u16, 8u16]),
        )
    } else {
        None
    };

    let cancel_row = match cancel_btn {
        Some(btn) => row!()
            .push(text("Agent Pipeline").size(14).color(pal.text))
            .push(column!().push(text("")).spacing(0))
            .push(btn),
        None => row!()
            .push(text("Agent Pipeline").size(14).color(pal.text))
            .push(column!().push(text("")).spacing(0)),
    };

    column!()
        .push(cancel_row)
        .push(pipeline_col)
        .push(text("Chain").size(12).color(pal.text))
        .push(entries_col)
        .padding([8u16, 8u16])
        .into()
}

fn view_task(t: &crate::app::state::PipelineTaskEntry, pal: Palette) -> Element<'static, Message> {
    let status_color = match t.status {
        PipelineTaskStatus::Pending => iced::Color::from_rgb(0.5, 0.5, 0.5),
        PipelineTaskStatus::Running => pal.warning,
        PipelineTaskStatus::Completed => pal.success,
        PipelineTaskStatus::Failed => pal.danger,
    };
    let desc = t.description.clone();
    let status_str = format!("{:?}", t.status);
    row!()
        .push(text(desc).size(12).color(pal.text))
        .push(column!().push(text("")).spacing(0))
        .push(text(status_str).size(10).color(status_color))
        .padding([2u16, 8u16])
        .into()
}

fn view_chain_entry(e: &crate::sessions::model::AgentChainEntry, pal: Palette) -> Element<'static, Message> {
    let status_color = match e.status {
        crate::sessions::model::AgentChainEntryStatus::Running => pal.warning,
        crate::sessions::model::AgentChainEntryStatus::Completed => pal.success,
        crate::sessions::model::AgentChainEntryStatus::Failed => pal.danger,
        crate::sessions::model::AgentChainEntryStatus::Pending => iced::Color::from_rgb(0.5, 0.5, 0.5),
    };
    let name = e.agent_name.clone();
    row!()
        .push(text(name).size(12).color(pal.text))
        .push(column!().push(text("")).spacing(0))
        .push(text(format!("depth={}", e.depth)).size(10).color(status_color))
        .padding([2u16, 8u16])
        .into()
}
