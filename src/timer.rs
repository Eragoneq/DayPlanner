use std::time::{Duration, Instant};

use gpui_component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;

use crate::theme::Palette;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RunningTimer {
    pub session_id: i64,
    pub task_id: i64,
    pub started_at: i64,
    pub last_saved_at: i64,
    pub persisted_seconds: i64,
    started: Instant,
    checkpointed: Instant,
}

impl RunningTimer {
    pub fn new(session_id: i64, task_id: i64, started_at: i64) -> Self {
        let now = Instant::now();
        Self {
            session_id,
            task_id,
            started_at,
            last_saved_at: started_at,
            persisted_seconds: 0,
            started: now,
            checkpointed: now,
        }
    }

    pub fn elapsed_seconds(self) -> i64 {
        self.started.elapsed().as_secs().min(i64::MAX as u64) as i64
    }

    pub fn until_checkpoint(self) -> Duration {
        Duration::from_secs(30).saturating_sub(self.checkpointed.elapsed())
    }

    pub fn checkpoint(self) -> Self {
        Self {
            persisted_seconds: self.elapsed_seconds(),
            last_saved_at: chrono::Utc::now().timestamp().max(self.last_saved_at),
            checkpointed: Instant::now(),
            ..self
        }
    }

    pub fn retry_later(&mut self) {
        self.checkpointed = Instant::now();
    }
}

pub struct StopTimer;

pub struct TimerDisplay {
    active: Option<RunningTimer>,
    title: String,
    busy: bool,
}

impl EventEmitter<StopTimer> for TimerDisplay {}

impl TimerDisplay {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if view
                    .update(cx, |this, cx| {
                        if this.active.is_some() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            active: None,
            title: "No active task".into(),
            busy: false,
        }
    }

    pub fn set(
        &mut self,
        active: Option<RunningTimer>,
        title: String,
        busy: bool,
        cx: &mut Context<Self>,
    ) {
        if self.active != active || self.title != title || self.busy != busy {
            self.active = active;
            self.title = title;
            self.busy = busy;
            cx.notify();
        }
    }
}

impl Render for TimerDisplay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        let elapsed = self.active.map(RunningTimer::elapsed_seconds).unwrap_or(0);
        div()
            .flex()
            .items_center()
            .gap(px(14.))
            .h(px(48.))
            .min_w_0()
            .px(px(14.))
            .rounded(px(12.))
            .bg(rgb(colors.timer))
            .border_1()
            .border_color(rgb(colors.timer_border))
            .child(
                div()
                    .w(px(118.))
                    .flex_shrink_0()
                    .text_size(px(26.))
                    .font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!(
                        "{:02}:{:02}:{:02}",
                        elapsed / 3600,
                        elapsed / 60 % 60,
                        elapsed % 60
                    )),
            )
            .child(
                div()
                    .w(px(1.))
                    .h(px(24.))
                    .flex_shrink_0()
                    .bg(rgb(colors.timer_separator)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(rgb(
                        if self.active.is_some() {
                            colors.accent
                        } else {
                            colors.timer_idle
                        },
                    )))
                    .child(
                        div()
                            .w(px(180.))
                            .min_w_0()
                            .text_size(px(13.))
                            .text_color(rgb(colors.muted))
                            .text_ellipsis()
                            .child(self.title.clone()),
                    ),
            )
            .child(
                Button::new("stop-active-timer")
                    .ghost()
                    .small()
                    .label("Stop")
                    .disabled(self.busy || self.active.is_none())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(StopTimer))),
            )
    }
}

pub fn duration_label(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds % 3600 < 60 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    }
}
