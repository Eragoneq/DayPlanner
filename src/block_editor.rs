use chrono::NaiveDateTime;
use gpui_component::{
    Sizable,
    input::{Input, InputState},
};
use gpui_kit::*;

use crate::{
    calendar::local_timestamp,
    model::{BlockDraft, local_time},
    theme::Palette,
};

/// Scheduling fields shared by the task editor, regardless of where it opens.
pub struct BlockFields {
    pub key: usize,
    pub original: BlockDraft,
    pub start_date: Entity<InputState>,
    pub start_time: Entity<InputState>,
    pub end_date: Entity<InputState>,
    pub end_time: Entity<InputState>,
}

impl BlockFields {
    pub fn new(key: usize, block: BlockDraft, window: &mut Window, cx: &mut App) -> Self {
        let start = local_time(block.starts_at);
        let end = local_time(block.ends_at);
        let mut field =
            |value: String| cx.new(|cx| InputState::new(window, cx).default_value(value));
        Self {
            key,
            original: block,
            start_date: field(
                start
                    .map(|time| time.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
            ),
            start_time: field(
                start
                    .map(|time| time.format("%H:%M").to_string())
                    .unwrap_or_default(),
            ),
            end_date: field(
                end.map(|time| time.format("%Y-%m-%d").to_string())
                    .unwrap_or_default(),
            ),
            end_time: field(
                end.map(|time| time.format("%H:%M").to_string())
                    .unwrap_or_default(),
            ),
        }
    }

    pub fn inputs(&self) -> [&Entity<InputState>; 4] {
        [
            &self.start_date,
            &self.start_time,
            &self.end_date,
            &self.end_time,
        ]
    }

    pub fn draft(&self, cx: &App) -> Result<BlockDraft, String> {
        // Preserve each untouched endpoint independently, including its known
        // repeated-hour occurrence and precision hidden by the minute fields.
        let parse = |date: &Entity<InputState>, time: &Entity<InputState>, original: i64| {
            let date = date.read(cx).value();
            let time = time.read(cx).value();
            if let Some(saved) = local_time(original)
                && date.trim() == saved.format("%Y-%m-%d").to_string()
                && time.trim() == saved.format("%H:%M").to_string()
            {
                return Ok(original);
            }
            let value = format!("{} {}", date.trim(), time.trim());
            let wall_time = NaiveDateTime::parse_from_str(&value, "%Y-%m-%d %H:%M")
                .map_err(|_| "Use dates as YYYY-MM-DD and times as HH:MM (24-hour).".to_string())?;
            local_timestamp(wall_time)
        };
        let starts_at = parse(&self.start_date, &self.start_time, self.original.starts_at)?;
        let ends_at = parse(&self.end_date, &self.end_time, self.original.ends_at)?;
        if ends_at - starts_at < 900 {
            return Err("End must be at least 15 minutes after start.".into());
        }
        Ok(BlockDraft {
            starts_at,
            ends_at,
            ..self.original
        })
    }

    pub fn label(&self, cx: &App) -> String {
        format!(
            "{}  {} – {}",
            self.start_date.read(cx).value(),
            self.start_time.read(cx).value(),
            self.end_time.read(cx).value()
        )
    }

    pub fn element(&self, busy: bool, cx: &App) -> Div {
        let colors = Palette::current(cx);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(
                [
                    ("Start", &self.start_date, &self.start_time),
                    ("End", &self.end_date, &self.end_time),
                ]
                .into_iter()
                .map(|(label, date, time)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(34.))
                                .text_size(px(12.))
                                .text_color(rgb(colors.muted))
                                .child(label),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                Input::new(date)
                                    .small()
                                    .aria_label(format!("{label} date, YYYY-MM-DD"))
                                    .disabled(busy),
                            ),
                        )
                        .child(
                            div().w(px(76.)).child(
                                Input::new(time)
                                    .small()
                                    .aria_label(format!("{label} time, HH:MM"))
                                    .disabled(busy),
                            ),
                        )
                }),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(colors.muted))
                    .child("Local time · YYYY-MM-DD / HH:MM"),
            )
    }
}
