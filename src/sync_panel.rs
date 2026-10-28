use gpui_component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
    input::{Input, InputState},
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    theme::Palette,
    todoist::{Request, Resolution, Settings},
};

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("space", NoAction, Some("SyncSettings")),
        KeyBinding::new("enter", NoAction, Some("SyncSettings")),
    ]);
}

pub struct SyncPanel {
    tag: Entity<InputState>,
    token: Entity<InputState>,
    connected: bool,
    demo: bool,
    busy: bool,
    message: String,
    failed: bool,
    conflicts: bool,
}

pub struct SyncRequested(pub Request);
impl EventEmitter<SyncRequested> for SyncPanel {}

impl SyncPanel {
    pub fn new(
        settings: &Settings,
        demo: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            tag: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(settings.tag.clone())
                    .placeholder("dayplanner")
            }),
            token: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("Paste a token, or use the saved token")
            }),
            connected: settings.account_id.is_some(),
            demo,
            busy: false,
            message: settings
                .last_synced_at
                .and_then(crate::model::local_time)
                .map(|time| format!("Last synced {}", time.format("%d/%m/%Y %H:%M")))
                .unwrap_or_else(|| "Not synced yet.".into()),
            failed: false,
            conflicts: false,
        }
    }

    pub fn pending(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.failed = false;
        self.conflicts = false;
        self.message = "Syncing Todoist…".into();
        cx.notify();
    }

    pub fn finished(
        &mut self,
        outcome: &crate::todoist::Outcome,
        settings: &Settings,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        self.connected = settings.account_id.is_some();
        self.message = outcome.message.clone();
        self.failed = outcome.failed;
        self.conflicts = outcome.conflicts;
        cx.notify();
    }

    fn submit(&self, resolution: Resolution, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        cx.emit(SyncRequested(Request {
            token: self.token.read(cx).value().to_string(),
            tag: self.tag.read(cx).value().to_string(),
            resolution,
            demo: self.demo,
        }));
    }
}

impl Render for SyncPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        div().id("sync-settings").key_context("SyncSettings")
            .w(px(380.)).max_h(px(550.)).overflow_y_scroll().p(px(16.)).flex().flex_col().gap_3()
            .bg(rgb(colors.surface)).text_color(rgb(colors.ink)).border_1().border_color(rgb(colors.border)).rounded(px(12.))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(if self.demo { "Todoist · Demo database" } else { "Todoist" }))
            .child(div().text_size(px(12.)).text_color(rgb(colors.muted)).child("Sync tasks with this tag and the same label in Todoist. Titles, notes, and completion sync both ways."))
            .child(div().flex().flex_col().gap_1()
                .child(div().text_size(px(12.)).child("Selection tag"))
                .child(Input::new(&self.tag).small().disabled(self.busy).aria_label("Todoist selection tag"))
                .when(self.connected, |row| row.child(div().text_size(px(11.)).text_color(rgb(colors.muted)).child("Changing the tag disconnects the previous selection."))))
            .child(div().flex().flex_col().gap_1()
                .child(div().text_size(px(12.)).child("Personal API token"))
                .child(Input::new(&self.token).small().disabled(self.busy).aria_label("Todoist personal API token"))
                .child(div().text_size(px(11.)).text_color(rgb(colors.muted)).child("Todoist Settings → Integrations → Developer. Stored in your OS credential store; leave blank to reuse it.")))
            .child(div().text_size(px(12.)).text_color(rgb(colors.muted)).child("Blocks, time tracking, colors, other tags, and checklists stay local. Removing the selection tag or deleting a task disconnects it; it does not delete the other copy. Recurring and nested Todoist tasks are skipped."))
            .child(div().id("sync-result").max_h(px(100.)).overflow_y_scroll().text_size(px(12.)).text_color(rgb(if self.failed { colors.error } else { colors.muted })).child(self.message.clone()))
            .when(self.conflicts, |panel| panel.child(div().flex().flex_col().gap_1()
                .child(div().text_size(px(12.)).text_color(rgb(colors.muted)).child("Apply only to conflicting fields:"))
                .child(div().flex().gap_2()
                    .child(Button::new("keep-local-sync-edits").small().label("Keep DayPlanner edits").disabled(self.busy).on_click(cx.listener(|this, _, _, cx| this.submit(Resolution::Local, cx))))
                    .child(Button::new("keep-remote-sync-edits").small().label("Use Todoist edits").disabled(self.busy).on_click(cx.listener(|this, _, _, cx| this.submit(Resolution::Remote, cx)))))))
            .child(div().flex().items_center().justify_between()
                .child(Button::new("todoist-api-help").ghost().small().label("API help").on_click(|_, _, cx| cx.open_url("https://developer.todoist.com/api/v1/")))
                .child(Button::new("sync-todoist-now").primary().small().label(if self.busy { "Syncing…" } else { "Sync now" }).disabled(self.busy).on_click(cx.listener(|this, _, _, cx| this.submit(Resolution::Ask, cx)))))
    }
}
