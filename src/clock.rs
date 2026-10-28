use chrono::{DateTime, Local, NaiveDate, Timelike};
use gpui_kit::*;

use crate::calendar::HOUR_HEIGHT;
use crate::theme::Palette;

pub struct ClockChanged {
    pub date: NaiveDate,
    pub minute: u32,
    pub day_changed: bool,
}

pub struct WallClock {
    now: DateTime<Local>,
}

impl EventEmitter<ClockChanged> for WallClock {}

impl WallClock {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |view, cx| {
            loop {
                let delay = 1000 - Local::now().timestamp_subsec_millis();
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(u64::from(delay)))
                    .await;
                if view
                    .update(cx, |this, cx| {
                        let now = Local::now();
                        let day_changed = now.date_naive() != this.now.date_naive();
                        let minute = now.hour() * 60 + now.minute();
                        if day_changed || minute != this.now.hour() * 60 + this.now.minute() {
                            cx.emit(ClockChanged {
                                date: now.date_naive(),
                                minute,
                                day_changed,
                            });
                        }
                        this.now = now;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self { now: Local::now() }
    }
}

impl Render for WallClock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        div()
            .flex()
            .items_center()
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(rgb(colors.muted))
            .font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
            .child(self.now.format("%d/%m/%Y | %H").to_string())
            .child(
                div()
                    .opacity(if self.now.second().is_multiple_of(2) {
                        1.
                    } else {
                        0.
                    })
                    .child(":"),
            )
            .child(self.now.format("%M").to_string())
    }
}

pub struct CurrentTimeLine {
    minute: u32,
}

impl CurrentTimeLine {
    pub fn new() -> Self {
        let now = Local::now();
        Self {
            minute: now.hour() * 60 + now.minute(),
        }
    }

    pub fn set_minute(&mut self, minute: u32, cx: &mut Context<Self>) {
        self.minute = minute;
        cx.notify();
    }
}

impl Render for CurrentTimeLine {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        div()
            .absolute()
            .left_0()
            .w_full()
            .top(px(self.minute as f32 / 60. * HOUR_HEIGHT))
            .h(px(1.))
            .bg(rgb(colors.current_time))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(-2.))
                    .size(px(5.))
                    .rounded_full()
                    .bg(rgb(colors.current_time)),
            )
    }
}
