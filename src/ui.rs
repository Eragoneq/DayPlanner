use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, TitleBar,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    checkbox::Checkbox,
    menu::{PopupMenu, PopupMenuItem},
    popover::Popover,
    scroll::Scrollbar,
    tooltip::Tooltip,
};
use gpui_kit::{self as gpui, prelude::FluentBuilder as _, *};

use crate::calendar::{CalendarDrag, DragMode, HOUR_HEIGHT, TIME_GUTTER};
use crate::clock::{ClockChanged, CurrentTimeLine, WallClock};
use crate::db::Database;
use crate::model::{BlockDraft, Change, DayBlock, PlannerData, Task, TaskDraft, local_time};
use crate::sync_panel::{SyncPanel, SyncRequested};
use crate::task_editor::{TaskEditor, TaskEditorEvent};
use crate::theme::{self, Palette};
use crate::timer::{StopTimer, TimerDisplay, duration_label};

const SIDEBAR_WIDTH: f32 = 260.;

actions!(dayplanner, [NewTask]);

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskFilter {
    Inbox,
    Today,
    Upcoming,
    Overdue,
    Completed,
}

impl TaskFilter {
    fn label(self) -> &'static str {
        match self {
            Self::Inbox => "Inbox",
            Self::Today => "Today",
            Self::Upcoming => "Upcoming",
            Self::Overdue => "Overdue",
            Self::Completed => "Completed",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Inbox => IconName::Inbox,
            Self::Today => IconName::Calendar,
            Self::Upcoming => IconName::ChevronRight,
            Self::Overdue => IconName::TriangleAlert,
            Self::Completed => IconName::CircleCheck,
        }
    }

    fn empty_message(self) -> &'static str {
        match self {
            Self::Inbox => "No unscheduled tasks.",
            Self::Today => "No tasks for today.",
            Self::Upcoming => "No upcoming tasks.",
            Self::Overdue => "No overdue tasks.",
            Self::Completed => "No completed tasks yet.",
        }
    }
}

pub struct DayPlanner {
    week_start: NaiveDate,
    filter: TaskFilter,
    selected_tag: Option<String>,
    sidebar_visible: bool,
    calendar_scroll: ScrollHandle,
    demo: bool,
    storage_status: StorageStatus,
    data: PlannerData,
    database: Option<Database>,
    editor: Option<EditorSession>,
    block_editor: Option<BlockEditorSession>,
    calendar_menu: Option<CalendarMenuSession>,
    interaction_focus: FocusHandle,
    drag_preview: Option<BlockDraft>,
    active_gesture: Option<CalendarDrag>,
    gesture_generation: u64,
    drag_position: Point<Pixels>,
    write_pending: bool,
    close_requested: Option<AnyWindowHandle>,
    operation_error: Option<String>,
    recovery_notice: Option<String>,
    timer_display: Entity<TimerDisplay>,
    _timer_events: Subscription,
    today: NaiveDate,
    wall_clock: Entity<WallClock>,
    current_time_line: Entity<CurrentTimeLine>,
    _clock_events: Subscription,
    sync_panel: Option<Entity<SyncPanel>>,
    sync_events: Option<Subscription>,
    sync_pending: bool,
    sync_feedback: Option<(String, bool)>,
}

struct EditorSession {
    target: Option<i64>,
    view: Entity<TaskEditor>,
    _events: Subscription,
}

struct BlockEditorSession {
    id: i64,
    date: NaiveDate,
    view: Entity<TaskEditor>,
    _events: Subscription,
}

struct CalendarMenuSession {
    view: Entity<PopupMenu>,
    position: Point<Pixels>,
    _dismiss: Subscription,
}

enum StorageStatus {
    Opening,
    Ready,
    Failed(String),
}

impl DayPlanner {
    pub fn new(demo: bool, cx: &mut Context<Self>) -> Self {
        let calendar_scroll = ScrollHandle::new();
        calendar_scroll.set_offset(point(px(0.), px(-8. * HOUR_HEIGHT)));
        let timer_display = cx.new(TimerDisplay::new);
        let timer_events = cx.subscribe(&timer_display, |this, _, _: &StopTimer, cx| {
            this.write_change(Change::StopTimer, cx)
        });
        let wall_clock = cx.new(WallClock::new);
        let current_time_line = cx.new(|_| CurrentTimeLine::new());
        let clock_events = cx.subscribe(&wall_clock, |this, _, event: &ClockChanged, cx| {
            this.current_time_line
                .update(cx, |line, cx| line.set_minute(event.minute, cx));
            if event.day_changed {
                this.today = event.date;
                cx.notify();
            }
        });
        let mut planner = Self {
            week_start: monday_of(Local::now().date_naive()),
            filter: if demo {
                TaskFilter::Inbox
            } else {
                TaskFilter::Today
            },
            selected_tag: None,
            sidebar_visible: true,
            calendar_scroll,
            demo,
            storage_status: StorageStatus::Opening,
            data: PlannerData::default(),
            database: None,
            editor: None,
            block_editor: None,
            calendar_menu: None,
            interaction_focus: cx.focus_handle(),
            drag_preview: None,
            active_gesture: None,
            gesture_generation: 0,
            drag_position: point(px(0.), px(0.)),
            write_pending: false,
            close_requested: None,
            operation_error: None,
            recovery_notice: None,
            timer_display,
            _timer_events: timer_events,
            today: Local::now().date_naive(),
            wall_clock,
            current_time_line,
            _clock_events: clock_events,
            sync_panel: None,
            sync_events: None,
            sync_pending: false,
            sync_feedback: None,
        };
        match Database::start(demo) {
            Ok((database, ready)) => {
                planner.database = Some(database);
                cx.spawn(async move |view, cx| {
                    while let Ok(result) = ready.recv().await {
                        if view.update(cx, |planner, cx| {
                            match result {
                            Ok(data) => {
                                if planner.operation_error.as_deref().is_some_and(|error| error.starts_with("Timer checkpoint failed:")) {
                                    planner.operation_error = None;
                                }
                                let opening = matches!(planner.storage_status, StorageStatus::Opening);
                                if opening && planner.demo
                                    && let Some(first) = data
                                        .blocks
                                        .first()
                                        .and_then(|block| local_time(block.starts_at))
                                    {
                                        planner.week_start = monday_of(first.date_naive());
                                }
                                if opening && data.recovered_sessions > 0 {
                                    planner.recovery_notice = Some("Interrupted timer stopped at its last saved checkpoint.".into());
                                    cx.spawn(async move |view, cx| {
                                        cx.background_executor().timer(std::time::Duration::from_secs(10)).await;
                                        let _ = view.update(cx, |this, cx| {this.recovery_notice = None; cx.notify();});
                                    }).detach();
                                }
                                planner.accept_data(data);
                                planner.storage_status = StorageStatus::Ready;
                            }
                            Err(error) => {
                                if matches!(planner.storage_status, StorageStatus::Opening) {
                                    planner.storage_status = StorageStatus::Failed(error);
                                } else { planner.operation_error = Some(error); }
                            }
                        }
                        planner.sync_tracking(cx);
                        cx.notify();
                        }).is_err() { break; }
                    }
                })
                .detach();
            }
            Err(error) => planner.storage_status = StorageStatus::Failed(format!("{error:#}")),
        }
        planner
    }

    pub fn prepare_quit(&self) -> Option<async_channel::Receiver<()>> {
        self.database.as_ref().map(Database::shutdown)
    }

    pub fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.write_pending {
            self.close_requested = Some(window.window_handle());
            cx.notify();
            false
        } else if self.data.active_timer.is_some() {
            self.close_requested = Some(window.window_handle());
            self.write_change(Change::StopTimer, cx);
            false
        } else {
            true
        }
    }

    pub fn new_task(
        &mut self,
        scheduled_start: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.write_pending
            || cx.has_active_drag()
            || !matches!(self.storage_status, StorageStatus::Ready)
        {
            return;
        }
        self.sidebar_visible = true;
        if scheduled_start.is_none()
            && let Some(editor) = self
                .editor
                .as_ref()
                .filter(|editor| editor.target.is_none())
        {
            window.focus(&editor.view.focus_handle(cx), cx);
            cx.notify();
            return;
        }
        self.open_task_editor(None, scheduled_start, window, cx);
    }

    fn open_editor(&mut self, target: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_task_editor(target, None, window, cx);
    }

    fn open_task_editor(
        &mut self,
        target: Option<i64>,
        scheduled_start: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.write_pending
            || cx.has_active_drag()
            || !matches!(self.storage_status, StorageStatus::Ready)
        {
            return;
        }
        self.sync_panel = None;
        self.sync_events = None;
        let mut draft = match target {
            Some(id) => match self.data.tasks.iter().find(|task| task.id == id) {
                Some(task) => TaskDraft::from(task),
                None => return,
            },
            None => TaskDraft {
                tags: self.selected_tag.iter().cloned().collect(),
                ..TaskDraft::default()
            },
        };
        draft.blocks = self
            .data
            .blocks
            .iter()
            .filter(|block| Some(block.task_id) == target)
            .map(BlockDraft::from)
            .collect();
        if let Some(start) = scheduled_start {
            draft.blocks.push(BlockDraft {
                id: None,
                task_id: 0,
                starts_at: start,
                ends_at: start + 1800,
            });
        }
        self.calendar_menu = None;
        self.block_editor = None;
        let (view, events) =
            self.task_editor_view(draft, None, scheduled_start.is_some(), window, cx);
        let focus = view.focus_handle(cx);
        window.focus(&focus, cx);
        self.editor = Some(EditorSession {
            target,
            view,
            _events: events,
        });
        self.operation_error = None;
        cx.notify();
    }

    fn task_editor_view(
        &self,
        draft: TaskDraft,
        selected_block: Option<i64>,
        show_schedule: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TaskEditor>, Subscription) {
        let timing = draft
            .id
            .and_then(|id| self.data.tasks.iter().find(|task| task.id == id))
            .map(|task| self.data.task_timing(task));
        let view =
            cx.new(|cx| TaskEditor::new(draft, timing, selected_block, show_schedule, window, cx));
        let events = cx.subscribe(&view, |this, _, event: &TaskEditorEvent, cx| match event {
            TaskEditorEvent::Save(draft) => this.write_change(Change::Save(draft.clone()), cx),
            TaskEditorEvent::Delete(id) => this.write_change(Change::Delete(*id), cx),
            TaskEditorEvent::ToggleTimer(id) => this.toggle_timer(*id, cx),
            TaskEditorEvent::Cancel => {
                if !this.write_pending {
                    this.editor = None;
                    this.block_editor = None;
                    cx.notify();
                }
            }
        });
        (view, events)
    }

    fn task_popover(
        &self,
        id: impl Into<ElementId>,
        target: Option<i64>,
        trigger: Button,
        fill: bool,
        cx: &mut Context<Self>,
    ) -> Popover {
        let editor = self
            .editor
            .as_ref()
            .filter(|editor| editor.target == target)
            .map(|editor| editor.view.clone());
        let focus = editor.as_ref().map(|view| view.focus_handle(cx));
        let mut trigger_style = div().flex_1().min_w_0();
        // Open task text on click, after GPUI has had a chance to start a drag.
        let trigger = if fill {
            trigger.on_click(
                cx.listener(move |this, _, window, cx| this.open_editor(target, window, cx)),
            )
        } else {
            trigger
        };
        Popover::new(id)
            .appearance(false)
            .arrow(true)
            .trigger(trigger)
            .mouse_button(if fill {
                MouseButton::Right
            } else {
                MouseButton::Left
            })
            .open(editor.is_some())
            .overlay_closable(!self.write_pending)
            .when(fill, |popover| {
                popover.trigger_style(trigger_style.style().clone())
            })
            .when_some(focus, |popover, focus| popover.track_focus(&focus))
            .on_open_change(cx.listener(move |this, open, window, cx| {
                if *open {
                    this.open_editor(target, window, cx);
                } else if !this.write_pending
                    && this
                        .editor
                        .as_ref()
                        .is_some_and(|editor| editor.target == target)
                {
                    this.editor = None;
                    cx.notify();
                } else {
                    cx.notify();
                }
            }))
            .content(move |_, _, _| div().children(editor.clone()))
    }

    fn write_change(&mut self, change: Change, cx: &mut Context<Self>) {
        if self.write_pending {
            return;
        }
        let Some(database) = &self.database else {
            return;
        };
        let creating = matches!(&change, Change::Save(draft) if draft.id.is_none());
        let close_editor = matches!(
            &change,
            Change::Save(_) | Change::Delete(_) | Change::SaveBlock(_)
        );
        let completing = matches!(&change, Change::SetCompleted { .. });
        let result = database.write(change);
        self.write_pending = true;
        self.operation_error = None;
        if let Some(editor) = &self.editor {
            editor.view.update(cx, |editor, cx| editor.saving(cx));
        }
        if let Some(editor) = &self.block_editor {
            editor.view.update(cx, |editor, cx| editor.saving(cx));
        }
        self.sync_tracking(cx);
        if completing {
            self.editor = None;
            self.block_editor = None;
        }
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = result.recv().await.unwrap_or_else(|_| {
                Err("The storage worker stopped before confirming your change.".into())
            });
            let _ = view.update(cx, |this, cx| {
                this.write_pending = false;
                this.drag_preview = None;
                match result {
                    Ok(data) => {
                        this.accept_data(data);
                        if close_editor {
                            this.editor = None;
                            this.block_editor = None;
                        }
                        if creating && let Some(task) = this.data.tasks.last() {
                            let group = this.group_for(task);
                            let matches_tag = this.matches_tag(task);
                            this.filter = group;
                            if !matches_tag {
                                this.selected_tag = None;
                            }
                        }
                        this.finish_close(cx);
                    }
                    Err(error) => {
                        this.close_requested = None;
                        if let Some(editor) = &this.editor {
                            editor
                                .view
                                .update(cx, |editor, cx| editor.failed(error, cx));
                        } else if let Some(editor) = &this.block_editor {
                            editor
                                .view
                                .update(cx, |editor, cx| editor.failed(error, cx));
                        } else {
                            this.operation_error = Some(error);
                        }
                    }
                }
                this.sync_tracking(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn open_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.write_pending
            || self.editor.is_some()
            || self.block_editor.is_some()
            || !matches!(self.storage_status, StorageStatus::Ready)
        {
            return;
        }
        let settings = self.data.todoist.clone();
        let demo = self.demo;
        let panel = cx.new(|cx| SyncPanel::new(&settings, demo, window, cx));
        self.sync_events = Some(cx.subscribe(&panel, |this, _, event: &SyncRequested, cx| {
            this.start_sync(
                crate::todoist::Request {
                    token: event.0.token.clone(),
                    tag: event.0.tag.clone(),
                    resolution: event.0.resolution,
                    demo: event.0.demo,
                },
                cx,
            );
        }));
        self.sync_panel = Some(panel);
        cx.notify();
    }

    fn start_sync(&mut self, request: crate::todoist::Request, cx: &mut Context<Self>) {
        if self.write_pending || self.editor.is_some() || self.block_editor.is_some() {
            return;
        }
        let Some(database) = &self.database else {
            return;
        };
        let result = crate::todoist::start(database.sync_store(), request);
        self.write_pending = true;
        self.sync_pending = true;
        self.sync_feedback = None;
        self.operation_error = None;
        if let Some(panel) = &self.sync_panel {
            panel.update(cx, |panel, cx| panel.pending(cx));
        }
        self.sync_tracking(cx);
        cx.notify();
        cx.spawn(async move |view, cx| {
            let mut outcome = result
                .recv()
                .await
                .unwrap_or_else(|_| crate::todoist::Outcome {
                    data: None,
                    message: "The sync worker stopped. Use Sync now to retry saved requests."
                        .into(),
                    failed: true,
                    conflicts: false,
                });
            let _ = view.update(cx, |this, cx| {
                this.write_pending = false;
                this.sync_pending = false;
                if let Some(data) = outcome.data.take() {
                    this.accept_data(data);
                }
                this.sync_feedback = Some((outcome.message.clone(), outcome.failed));
                if let Some(panel) = &this.sync_panel {
                    panel.update(cx, |panel, cx| {
                        panel.finished(&outcome, &this.data.todoist, cx)
                    });
                }
                this.sync_tracking(cx);
                this.finish_close(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn accept_data(&mut self, data: PlannerData) {
        if data.revision >= self.data.revision {
            if self
                .selected_tag
                .as_ref()
                .is_some_and(|tag| !data.tasks.iter().any(|task| task.tags.contains(tag)))
            {
                self.selected_tag = None;
            }
            self.data = data;
        }
    }

    fn sync_tracking(&mut self, cx: &mut Context<Self>) {
        let title = self
            .data
            .active_timer
            .and_then(|timer| self.data.tasks.iter().find(|task| task.id == timer.task_id))
            .map(|task| task.title.clone())
            .unwrap_or_else(|| "No active task".into());
        self.timer_display.update(cx, |display, cx| {
            display.set(self.data.active_timer, title, self.write_pending, cx)
        });
        if let Some(editor) = &self.editor {
            let timing = editor
                .target
                .and_then(|id| self.data.tasks.iter().find(|task| task.id == id))
                .map(|task| self.data.task_timing(task));
            editor.view.update(cx, |editor, cx| {
                editor.sync_timing(timing, self.write_pending, cx)
            });
        }
        if let Some(editor) = &self.block_editor {
            let timing = self
                .data
                .blocks
                .iter()
                .find(|block| block.id == editor.id)
                .and_then(|block| self.data.tasks.iter().find(|task| task.id == block.task_id))
                .map(|task| self.data.task_timing(task));
            editor.view.update(cx, |editor, cx| {
                editor.sync_timing(timing, self.write_pending, cx)
            });
        }
    }

    fn toggle_timer(&mut self, task_id: i64, cx: &mut Context<Self>) {
        let change = if self
            .data
            .active_timer
            .is_some_and(|timer| timer.task_id == task_id)
        {
            Change::StopTimer
        } else {
            Change::StartTimer(task_id)
        };
        self.write_change(change, cx);
    }

    fn finish_close(&mut self, cx: &mut Context<Self>) {
        if self.close_requested.is_none() {
            return;
        }
        if self.data.active_timer.is_some() {
            self.write_change(Change::StopTimer, cx);
        } else if let Some(window) = self.close_requested.take() {
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            });
        }
    }

    fn open_block_editor(
        &mut self,
        id: i64,
        date: NaiveDate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.write_pending || cx.has_active_drag() {
            return;
        }
        self.sync_panel = None;
        self.sync_events = None;
        let Some(block) = self.data.blocks.iter().find(|block| block.id == id) else {
            return;
        };
        let Some(task) = self.data.tasks.iter().find(|task| task.id == block.task_id) else {
            return;
        };
        let mut draft = TaskDraft::from(task);
        draft.blocks = self
            .data
            .blocks
            .iter()
            .filter(|block| block.task_id == task.id)
            .map(BlockDraft::from)
            .collect();
        self.calendar_menu = None;
        self.editor = None;
        let (view, events) = self.task_editor_view(draft, Some(id), false, window, cx);
        window.focus(&view.focus_handle(cx), cx);
        self.block_editor = Some(BlockEditorSession {
            id,
            date,
            view,
            _events: events,
        });
        self.operation_error = None;
        cx.notify();
    }

    fn drag_start(
        &self,
        cx: &Context<Self>,
    ) -> impl Fn(&CalendarDrag, Point<Pixels>, &mut Window, &mut App) -> Entity<Empty> + 'static
    {
        let view = cx.entity().downgrade();
        move |drag, offset, window, cx| {
            drag.grab_minutes.set(if drag.mode == DragMode::Move {
                drag.segment_offset + offset.y / px(HOUR_HEIGHT) * 60.
            } else {
                0.
            });
            let _ = view.update(cx, |this, cx| this.begin_drag(drag.clone(), window, cx));
            // The grid already previews the block; a cursor label would obscure it.
            cx.new(|_| Empty)
        }
    }

    fn begin_drag(&mut self, drag: CalendarDrag, window: &mut Window, cx: &mut Context<Self>) {
        self.editor = None;
        self.block_editor = None;
        self.operation_error = None;
        self.drag_position = window.mouse_position();
        self.active_gesture = Some(drag);
        self.gesture_generation = self.gesture_generation.wrapping_add(1);
        let generation = self.gesture_generation;
        window.focus(&self.interaction_focus, cx);
        cx.notify();
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                let keep_running = view
                    .update(cx, |this, cx| {
                        if this.gesture_generation != generation {
                            return false;
                        }
                        if !cx.has_active_drag() || this.active_gesture.is_none() {
                            let active = this.active_gesture.take().is_some();
                            let preview = if this.write_pending {
                                false
                            } else {
                                this.drag_preview.take().is_some()
                            };
                            if active || preview {
                                cx.notify();
                            }
                            return false;
                        }
                        let bounds = this.calendar_scroll.bounds();
                        let position = this.drag_position;
                        if bounds.contains(&position)
                            && position.x > bounds.left() + px(TIME_GUTTER)
                        {
                            let step = if position.y < bounds.top() + px(32.) {
                                8.
                            } else if position.y > bounds.bottom() - px(32.) {
                                -8.
                            } else {
                                0.
                            };
                            let mut offset = this.calendar_scroll.offset();
                            let previous = offset;
                            offset.y = (offset.y + px(step))
                                .clamp(-this.calendar_scroll.max_offset().y, px(0.));
                            if offset != previous {
                                this.calendar_scroll.set_offset(offset);
                                cx.notify();
                            }
                        }
                        if let Some(drag) = this.active_gesture.clone() {
                            this.update_preview(&drag, position, cx);
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    fn calendar_spot(&self, position: Point<Pixels>) -> Option<(NaiveDate, f32)> {
        let bounds = self.calendar_scroll.bounds();
        let x = position.x - bounds.left() - px(TIME_GUTTER);
        let width = bounds.size.width - px(TIME_GUTTER);
        if !bounds.contains(&position) || x < px(0.) || x >= width || width <= px(0.) {
            return None;
        }
        let day = (x / width * 7.).floor() as i64;
        let minute =
            (position.y - bounds.top() - self.calendar_scroll.offset().y) / px(HOUR_HEIGHT) * 60.;
        Some((
            self.week_start + Duration::days(day),
            minute.clamp(0., 1440.),
        ))
    }

    fn drag_proposal(
        &self,
        drag: &CalendarDrag,
        position: Point<Pixels>,
    ) -> Option<Result<BlockDraft, String>> {
        self.calendar_spot(position)
            .map(|(date, minute)| drag.proposal(date, minute))
    }

    fn new_task_at(
        &mut self,
        date: NaiveDate,
        minute: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let minute = ((minute / 15.).round() as i64 * 15).clamp(0, 1425);
        let time = date.and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(minute);
        match crate::calendar::local_timestamp(time) {
            Ok(start) => self.new_task(Some(start), window, cx),
            Err(error) => {
                self.operation_error = Some(error);
                cx.notify();
            }
        }
    }

    fn open_calendar_menu(
        &mut self,
        position: Point<Pixels>,
        spot: Option<(NaiveDate, f32)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.write_pending
            || cx.has_active_drag()
            || !matches!(self.storage_status, StorageStatus::Ready)
        {
            return;
        }
        self.calendar_menu = None;
        self.editor = None;
        self.block_editor = None;
        let planner = cx.entity().downgrade();
        let focus = self.interaction_focus.clone();
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            menu.action_context(focus).item(
                PopupMenuItem::new("New Task")
                    .icon(IconName::Plus)
                    .disabled(spot.is_none())
                    .on_click(move |_, window, cx| {
                        if let Some((date, minute)) = spot {
                            let planner = planner.clone();
                            window.defer(cx, move |window, cx| {
                                let _ = planner.update(cx, |planner, cx| {
                                    planner.new_task_at(date, minute, window, cx)
                                });
                            });
                        }
                    }),
            )
        });
        let dismiss = cx.subscribe(&menu, |this, source, _: &DismissEvent, cx| {
            // A dismissal from the previous menu must not close its replacement.
            if this
                .calendar_menu
                .as_ref()
                .is_some_and(|menu| menu.view.entity_id() == source.entity_id())
            {
                this.calendar_menu = None;
                cx.notify();
            }
        });
        window.focus(&menu.focus_handle(cx), cx);
        self.calendar_menu = Some(CalendarMenuSession {
            view: menu,
            position,
            _dismiss: dismiss,
        });
        cx.notify();
    }

    fn update_preview(
        &mut self,
        drag: &CalendarDrag,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.drag_position = position;
        let preview = self.drag_proposal(drag, position).and_then(Result::ok);
        if preview != self.drag_preview {
            self.drag_preview = preview;
            cx.notify();
        }
    }

    fn finish_drag(&mut self, drag: &CalendarDrag, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.drag_proposal(drag, window.mouse_position());
        self.active_gesture = None;
        self.drag_preview = None;
        if !self.write_pending {
            match result {
                Some(Ok(block)) if block != drag.original => {
                    // Keep the drop position visible until the committed snapshot arrives.
                    self.drag_preview = Some(block);
                    self.write_change(Change::SaveBlock(block), cx)
                }
                Some(Err(error)) => self.operation_error = Some(error),
                _ => {}
            }
        }
        cx.notify();
    }

    fn release_drag(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left
            && cx.has_active_drag()
            && let Some(drag) = self.active_gesture.clone()
        {
            cx.stop_active_drag(window);
            self.finish_drag(&drag, window, cx);
            // Do not turn the release into a click on a task or block underneath.
            cx.stop_propagation();
        }
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        let (message, error) = match &self.storage_status {
            StorageStatus::Failed(message) => (format!("Could not open planner: {message}"), true),
            StorageStatus::Opening => ("Opening planner…".into(), false),
            StorageStatus::Ready => {
                if let Some(error) = &self.operation_error {
                    (format!("Could not save: {error}"), true)
                } else if self.close_requested.is_some() {
                    ("Saving before closing…".into(), false)
                } else if self.sync_pending {
                    ("Syncing Todoist…".into(), false)
                } else if self.write_pending {
                    ("Saving…".into(), false)
                } else if let Some(notice) = &self.recovery_notice {
                    (notice.clone(), false)
                } else if let Some((message, failed)) = &self.sync_feedback {
                    (message.clone(), *failed)
                } else {
                    ("Ready".into(), false)
                }
            }
        };
        let details = message.clone();
        div()
            .id("planner-status")
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(28.))
            .flex_shrink_0()
            .px(px(12.))
            .border_t_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.surface))
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(rgb(if error { colors.error } else { colors.muted }))
            .child(div().flex_1())
            .child(
                div()
                    .id("status-message")
                    .min_w_0()
                    .text_ellipsis()
                    .child(message)
                    .when(!cx.has_active_drag(), |text| {
                        text.tooltip(move |window, cx| {
                            Tooltip::new(details.clone()).build(window, cx)
                        })
                    }),
            )
            .when(
                self.operation_error.is_some() || self.sync_feedback.is_some(),
                |bar| {
                    bar.child(
                        Button::new("dismiss-save-error")
                            .ghost()
                            .small()
                            .h(px(20.))
                            .label("Dismiss")
                            .accessibility_label("Dismiss status message")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.operation_error = None;
                                this.sync_feedback = None;
                                cx.notify();
                            })),
                    )
                },
            )
    }

    fn app_title(&self) -> &'static str {
        if self.demo {
            "DayPlanner · Demo"
        } else {
            "DayPlanner"
        }
    }

    fn empty_message(&self) -> &'static str {
        match self.storage_status {
            StorageStatus::Opening => "Opening planner…",
            StorageStatus::Failed(_) => "Your planner could not be opened.",
            StorageStatus::Ready if self.selected_tag.is_some() => {
                "No tasks in this group with that tag."
            }
            StorageStatus::Ready => self.filter.empty_message(),
        }
    }

    fn group_for(&self, task: &Task) -> TaskFilter {
        if task.completed {
            return TaskFilter::Completed;
        }
        let mut scheduled = false;
        let mut future = false;
        for block in self
            .data
            .blocks
            .iter()
            .filter(|block| block.task_id == task.id)
        {
            scheduled = true;
            if block.touches_date(self.today) {
                return TaskFilter::Today;
            }
            future |=
                local_time(block.starts_at).is_some_and(|start| start.date_naive() > self.today);
        }
        if future {
            TaskFilter::Upcoming
        } else if scheduled {
            TaskFilter::Overdue
        } else {
            TaskFilter::Inbox
        }
    }

    fn matches_tag(&self, task: &Task) -> bool {
        self.selected_tag
            .as_ref()
            .is_none_or(|tag| task.tags.contains(tag))
    }

    fn matches_filter(&self, task: &Task, filter: TaskFilter) -> bool {
        self.group_for(task) == filter && self.matches_tag(task)
    }

    fn task_count(&self, filter: TaskFilter) -> usize {
        self.data
            .tasks
            .iter()
            .filter(|task| self.matches_filter(task, filter))
            .count()
    }

    fn task_row(&self, task: &Task, cx: &mut Context<Self>) -> AnyElement {
        let colors = Palette::current(cx);
        let task_id = task.id;
        let editing = self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.target == Some(task_id));
        let text_button = ButtonCustomVariant::new(cx).foreground(rgb(colors.ink).into());
        let mut summary = self
            .data
            .blocks
            .iter()
            .find(|block| block.task_id == task.id)
            .and_then(|block| local_time(block.starts_at))
            .map(|start| start.format("%a %-d %b, %H:%M").to_string())
            .unwrap_or_else(|| "Unscheduled".into());
        let tracked_seconds = self.data.task_timing(task).tracked_seconds;
        if tracked_seconds > 0 {
            summary.push_str(&format!(" · {} tracked", duration_label(tracked_seconds)));
        }
        let mut metadata = task
            .tags
            .iter()
            .map(|tag| format!("#{tag}"))
            .collect::<Vec<_>>();
        if !task.subtasks.is_empty() {
            metadata.push(format!(
                "{}/{} sub-tasks",
                task.subtasks.iter().filter(|item| item.completed).count(),
                task.subtasks.len()
            ));
        }
        let metadata = metadata.join(" · ");
        div()
            .id(("task", task.id as usize))
            .flex()
            .items_start()
            .gap_2()
            .p(px(8.))
            .mb(px(4.))
            .w_full()
            .min_w_0()
            .rounded(px(6.))
            .when(editing, |row| row.bg(rgb(colors.selection)))
            .when(!self.write_pending, |row| {
                row.hover(|style| {
                    style.bg(rgb(if editing {
                        colors.selection
                    } else {
                        colors.hover
                    }))
                })
                .on_drag(
                    CalendarDrag::new(
                        BlockDraft {
                            id: None,
                            task_id,
                            starts_at: 0,
                            ends_at: 1800,
                        },
                        DragMode::Create,
                        0.,
                    ),
                    self.drag_start(cx),
                )
            })
            .child(
                div()
                    .id(("task-checkbox", task_id as usize))
                    .flex_shrink_0()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Checkbox::new(("complete-task", task_id as usize))
                            .small()
                            .mt(px(3.))
                            .checked(task.completed)
                            .disabled(self.write_pending)
                            .accessibility_label(format!(
                                "{} {}",
                                if task.completed { "Reopen" } else { "Complete" },
                                task.title
                            ))
                            .on_change(cx.listener(move |this, completed, _, cx| {
                                this.write_change(
                                    Change::SetCompleted {
                                        id: task_id,
                                        completed: *completed,
                                    },
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                self.task_popover(
                    ("edit-task", task_id as usize),
                    Some(task_id),
                    Button::new(("edit-task-trigger", task_id as usize))
                        .custom(text_button)
                        .w_full()
                        .min_w_0()
                        .h_auto()
                        .p_0()
                        .font_weight(FontWeight::NORMAL)
                        .disabled(self.write_pending)
                        .accessibility_label(format!("Edit {}", task.title))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .w_full()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .line_height(px(18.))
                                        .text_ellipsis()
                                        .when(task.completed, |title| title.line_through())
                                        .child(task.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .line_height(px(16.))
                                        .text_color(rgb(colors.muted))
                                        .text_ellipsis()
                                        .child(summary),
                                )
                                .when(!metadata.is_empty(), |text| {
                                    text.child(
                                        div()
                                            .text_size(px(11.))
                                            .line_height(px(14.))
                                            .text_color(rgb(colors.muted))
                                            .text_ellipsis()
                                            .child(metadata),
                                    )
                                }),
                        ),
                    true,
                    cx,
                ),
            )
            .into_any_element()
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        let selected = self.filter;
        let count = self.task_count(selected);
        let path = self.data.database_path.clone();
        div()
            .flex()
            .flex_col()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .bg(rgb(colors.sidebar))
            .border_r_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(64.))
                    .px(px(20.))
                    .flex_shrink_0()
                    .child(
                        Icon::new(IconName::Calendar)
                            .size_4()
                            .text_color(rgb(colors.accent)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .min_w_0()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.app_title()),
                            )
                            .child(self.wall_clock.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px(px(12.))
                    .pt(px(12.))
                    .pb(px(20.))
                    .children(
                        [
                            TaskFilter::Inbox,
                            TaskFilter::Today,
                            TaskFilter::Upcoming,
                            TaskFilter::Overdue,
                            TaskFilter::Completed,
                        ]
                        .into_iter()
                        .map(|filter| {
                            let active = selected == filter;
                            let variant = ButtonCustomVariant::new(cx)
                                .color(if active {
                                    rgb(colors.selection).into()
                                } else {
                                    gpui::transparent_black()
                                })
                                .foreground(rgb(colors.ink).into())
                                .hover(
                                    rgb(if active {
                                        colors.selected_hover
                                    } else {
                                        colors.hover
                                    })
                                    .into(),
                                )
                                .active(rgb(colors.selected_hover).into());
                            Button::new(filter.label())
                                .custom(variant)
                                .disabled(self.write_pending)
                                .selected(active)
                                .accessibility_label(filter.label())
                                .w_full()
                                .h(px(38.))
                                .px(px(12.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .w_full()
                                        .child(Icon::new(filter.icon()).size_4().text_color(rgb(
                                            match filter {
                                                TaskFilter::Inbox => colors.accent,
                                                TaskFilter::Today => colors.today_icon,
                                                TaskFilter::Upcoming => colors.today_icon,
                                                TaskFilter::Overdue => colors.error,
                                                TaskFilter::Completed => colors.completed_icon,
                                            },
                                        )))
                                        .child(filter.label())
                                        .child(div().flex_1())
                                        .child(
                                            div()
                                                .text_size(px(12.))
                                                .text_color(rgb(colors.muted))
                                                .child(self.task_count(filter).to_string()),
                                        ),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.filter = filter;
                                    this.editor = None;
                                    cx.notify();
                                }))
                        }),
                    ),
            )
            .child(self.tag_filters(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .mx(px(20.))
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .pt(px(20.))
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(selected.label()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(rgb(colors.muted))
                                            .child(count.to_string()),
                                    )
                                    .child(
                                        self.task_popover(
                                            "new-task",
                                            None,
                                            Button::new("add-task")
                                                .ghost()
                                                .small()
                                                .icon(IconName::Plus)
                                                .accessibility_label("Add task")
                                                .tooltip("Add task")
                                                .disabled(
                                                    self.write_pending
                                                        || !matches!(
                                                            self.storage_status,
                                                            StorageStatus::Ready
                                                        ),
                                                ),
                                            false,
                                            cx,
                                        ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("task-list")
                            .flex_1()
                            .min_h_0()
                            .mt(px(8.))
                            .overflow_y_scroll()
                            .when(count == 0, |list| {
                                list.child(
                                    div()
                                        .mt(px(6.))
                                        .text_size(px(13.))
                                        .text_color(rgb(colors.muted))
                                        .child(self.empty_message()),
                                )
                            })
                            .children(
                                self.data
                                    .tasks
                                    .iter()
                                    .filter(|task| self.matches_filter(task, selected))
                                    .map(|task| self.task_row(task, cx)),
                            ),
                    ),
            )
            .when(!path.as_os_str().is_empty(), |sidebar| {
                sidebar.child(
                    div().flex_shrink_0().px(px(16.)).py(px(12.)).child(
                        Button::new("open-data-file")
                            .ghost()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label("Open data file")
                            .tooltip(path.display().to_string())
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    ),
                )
            })
    }

    fn timer_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        let theme_label = if colors.dark {
            "Switch to light mode"
        } else {
            "Switch to dark mode"
        };
        let toggle_label = if self.sidebar_visible {
            "Hide sidebar"
        } else {
            "Show sidebar"
        };
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .h(px(64.))
            .px(px(16.))
            .flex_shrink_0()
            .bg(rgb(colors.surface))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                Button::new("toggle-sidebar")
                    .ghost()
                    .small()
                    .disabled(self.write_pending)
                    .icon(if self.sidebar_visible {
                        IconName::PanelLeftClose
                    } else {
                        IconName::PanelLeftOpen
                    })
                    .accessibility_label(toggle_label)
                    .tooltip(toggle_label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_visible = !this.sidebar_visible;
                        this.editor = None;
                        cx.notify();
                    })),
            )
            .child(self.timer_display.clone())
            .child(div().flex_1())
            .child({
                let panel = self.sync_panel.clone();
                Popover::new("todoist-sync-popover")
                    .appearance(false)
                    .arrow(true)
                    .open(panel.is_some())
                    .overlay_closable(!self.write_pending)
                    .trigger(
                        Button::new("todoist-sync")
                            .ghost()
                            .small()
                            .label("Sync")
                            .tooltip("Todoist sync settings")
                            .disabled(
                                self.write_pending
                                    || self.editor.is_some()
                                    || self.block_editor.is_some()
                                    || !matches!(self.storage_status, StorageStatus::Ready),
                            ),
                    )
                    .on_open_change(cx.listener(|this, open, window, cx| {
                        if *open && this.sync_panel.is_none() {
                            this.open_sync(window, cx);
                        } else if !*open && !this.write_pending {
                            this.sync_panel = None;
                            this.sync_events = None;
                            cx.notify();
                        }
                    }))
                    .content(move |_, _, _| div().children(panel.clone()))
            })
            .child(
                Button::new("toggle-dark-mode")
                    .ghost()
                    .small()
                    .selected(colors.dark)
                    .icon(if colors.dark {
                        IconName::Sun
                    } else {
                        IconName::Moon
                    })
                    .accessibility_label(theme_label)
                    .tooltip(theme_label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Err(error) = theme::toggle(cx) {
                            this.operation_error = Some(format!(
                                "Theme changed, but your preference could not be saved: {error}"
                            ));
                        }
                        cx.notify();
                    })),
            )
    }

    fn tag_filters(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        let tags: std::collections::BTreeSet<_> = self
            .data
            .tasks
            .iter()
            .flat_map(|task| task.tags.iter().cloned())
            .collect();
        div().when(!tags.is_empty(), |section| {
            section
                .flex()
                .flex_col()
                .flex_shrink_0()
                .mx(px(20.))
                .pb(px(12.))
                .gap_1()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(colors.muted))
                        .child("Tags"),
                )
                .child(
                    div()
                        .id("tag-filters")
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .max_h(px(80.))
                        .overflow_y_scroll()
                        .child(
                            Button::new("all-tags")
                                .ghost()
                                .small()
                                .label("All")
                                .selected(self.selected_tag.is_none())
                                .disabled(self.write_pending)
                                .accessibility_label("Show all tags")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.selected_tag = None;
                                    this.editor = None;
                                    this.block_editor = None;
                                    cx.notify();
                                })),
                        )
                        .children(tags.into_iter().map(|tag| {
                            Button::new(format!("tag-filter-{tag}"))
                                .ghost()
                                .small()
                                .max_w(px(210.))
                                .label(format!("#{tag}"))
                                .selected(self.selected_tag.as_ref() == Some(&tag))
                                .disabled(self.write_pending)
                                .tooltip(format!("Filter tasks and calendar by #{tag}"))
                                .accessibility_label(format!("Filter by tag {tag}"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.selected_tag = if this.selected_tag.as_ref() == Some(&tag)
                                    {
                                        None
                                    } else {
                                        Some(tag.clone())
                                    };
                                    this.editor = None;
                                    this.block_editor = None;
                                    cx.notify();
                                }))
                        })),
                )
        })
    }

    fn week_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(46.))
            .px(px(14.))
            .flex_shrink_0()
            .bg(rgb(colors.chrome))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("previous-week")
                            .ghost()
                            .small()
                            .disabled(self.write_pending)
                            .icon(IconName::ChevronLeft)
                            .accessibility_label("Previous week")
                            .tooltip("Previous week")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.week_start -= Duration::weeks(1);
                                this.block_editor = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .min_w(px(186.))
                            .px(px(10.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(week_label(self.week_start)),
                    )
                    .child(
                        Button::new("next-week")
                            .ghost()
                            .small()
                            .disabled(self.write_pending)
                            .icon(IconName::ChevronRight)
                            .accessibility_label("Next week")
                            .tooltip("Next week")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.week_start += Duration::weeks(1);
                                this.block_editor = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(colors.muted))
                            .child("7 days"),
                    )
                    .child(
                        Button::new("this-week")
                            .small()
                            .disabled(self.write_pending)
                            .label("Today")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.week_start = monday_of(Local::now().date_naive());
                                this.block_editor = None;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn day_headers(&self, today: NaiveDate, cx: &App) -> impl IntoElement {
        let colors = Palette::current(cx);
        div()
            .flex()
            .h(px(64.))
            .flex_shrink_0()
            .bg(rgb(colors.chrome))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .w(px(TIME_GUTTER))
                    .h_full()
                    .flex_shrink_0()
                    .pl(px(16.))
                    .text_size(px(12.))
                    .text_color(rgb(colors.muted))
                    .child(self.week_start.format("%b").to_string())
                    .child(format!("W{}", self.week_start.iso_week().week())),
            )
            .children((0..7).map(|day| {
                let date = self.week_start + Duration::days(day);
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .border_l_1()
                    .border_color(rgb(colors.border))
                    .when(date == today, |header| header.bg(rgb(colors.today_header)))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(colors.muted))
                            .child(date.format("%a").to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(date.day().to_string()),
                    )
            }))
    }

    fn calendar_block(
        &self,
        item: &DayBlock<'_>,
        date: NaiveDate,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = Palette::current(cx);
        let id = item.block.id;
        let editor = self
            .block_editor
            .as_ref()
            .filter(|editor| editor.id == id && editor.date == date)
            .map(|editor| editor.view.clone());
        let focus = editor.as_ref().map(|view| view.focus_handle(cx));
        let mut trigger_style = div().size_full();
        let original = BlockDraft::from(item.block);
        let start = local_time(item.block.starts_at).expect("Rendered block has a local start");
        let end = local_time(item.block.ends_at).expect("Rendered block has a local end");
        let segment_offset = (date - start.date_naive()).num_days() as f32 * 1440.
            + item.start_minute
            - (start.hour() * 60 + start.minute()) as f32;
        let moving = self
            .active_gesture
            .as_ref()
            .is_some_and(|drag| drag.original.id == Some(id));
        calendar_block(item, !cx.has_active_drag() && !self.write_pending, colors)
            .when(moving, |block| block.opacity(0.4))
            .when(!self.write_pending, |block| {
                block.on_drag(
                    CalendarDrag::new(original, DragMode::Move, segment_offset),
                    self.drag_start(cx),
                )
            })
            .child(
                div().absolute().inset_0().px(px(2.)).child(
                    Popover::new(("block-editor", id as usize))
                        .appearance(false)
                        .arrow(true)
                        // Left clicks use the button handler; reserve right clicks for the calendar menu.
                        .mouse_button(MouseButton::Middle)
                        .trigger_style(trigger_style.style().clone())
                        .trigger(
                            Button::new(("edit-block", id as usize))
                                .custom(ButtonCustomVariant::new(cx))
                                .size_full()
                                .p_0()
                                .disabled(self.write_pending)
                                .accessibility_label(format!(
                                    "Edit planned block for {}",
                                    item.task.title
                                ))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_block_editor(id, date, window, cx)
                                })),
                        )
                        .open(editor.is_some())
                        .overlay_closable(!self.write_pending)
                        .when_some(focus, |popover, focus| popover.track_focus(&focus))
                        .on_open_change(cx.listener(move |this, open, window, cx| {
                            if *open {
                                this.open_block_editor(id, date, window, cx);
                            } else if !this.write_pending
                                && this
                                    .block_editor
                                    .as_ref()
                                    .is_some_and(|editor| editor.id == id && editor.date == date)
                            {
                                this.block_editor = None;
                            }
                            cx.notify();
                        }))
                        .content(move |_, _, _| div().children(editor.clone())),
                ),
            )
            .when(!self.write_pending, |block| {
                let show_start = start.date_naive() == date;
                let show_end = end.date_naive() == date
                    || (end.date_naive() == date + Duration::days(1)
                        && end.time() == chrono::NaiveTime::MIN);
                block.children(
                    [(DragMode::Start, show_start), (DragMode::End, show_end)]
                        .into_iter()
                        .filter(|(_, show)| *show)
                        .map(|(mode, _)| {
                            div()
                                .id(if mode == DragMode::Start {
                                    "resize-start"
                                } else {
                                    "resize-end"
                                })
                                .absolute()
                                .left(px(4.))
                                .right(px(4.))
                                .h(px(5.))
                                .when(mode == DragMode::Start, |handle| handle.top_0())
                                .when(mode == DragMode::End, |handle| handle.bottom_0())
                                .occlude()
                                .cursor_row_resize()
                                .hover(|handle| handle.bg(rgb(colors.accent).opacity(0.25)))
                                .on_click(|_, _, cx| cx.stop_propagation())
                                .on_drag(CalendarDrag::new(original, mode, 0.), self.drag_start(cx))
                        }),
                )
            })
            .into_any_element()
    }

    fn calendar(&self, today: NaiveDate, cx: &mut Context<Self>) -> AnyElement {
        let colors = Palette::current(cx);
        let preview_block = self.drag_preview.map(|block| crate::model::TimeBlock {
            id: block.id.unwrap_or(-1),
            task_id: block.task_id,
            starts_at: block.starts_at,
            ends_at: block.ends_at,
        });
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .id("calendar-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.calendar_scroll)
                    .child(
                        div()
                            .flex()
                            .w_full()
                            .h(px(24. * HOUR_HEIGHT))
                            .flex_shrink_0()
                            .child(
                                div()
                                    .relative()
                                    .w(px(TIME_GUTTER))
                                    .h_full()
                                    .flex_shrink_0()
                                    .children((0..24).map(|hour| {
                                        div()
                                            .absolute()
                                            .top(px(hour as f32 * HOUR_HEIGHT + 4.))
                                            .right(px(10.))
                                            .text_size(px(11.))
                                            .text_color(rgb(colors.muted))
                                            .child(format!("{hour:02}:00"))
                                    })),
                            )
                            .children((0..7).map(|day| {
                                let date = self.week_start + Duration::days(day);
                                div()
                                    .id(("calendar-day", day as usize))
                                    .flex()
                                    .flex_col()
                                    .relative()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .border_l_1()
                                    .border_color(rgb(colors.border))
                                    .when(date == today, |column| {
                                        column.bg(rgb(colors.today_column))
                                    })
                                    .children((0..24).map(|hour| {
                                        div()
                                            .h(px(HOUR_HEIGHT))
                                            .w_full()
                                            .flex_shrink_0()
                                            .when(hour > 0, |row| {
                                                row.border_t_1().border_color(rgb(colors.grid))
                                            })
                                    }))
                                    .children(
                                        self.data
                                            .day_blocks(
                                                date,
                                                preview_block.as_ref(),
                                                self.selected_tag.as_deref(),
                                            )
                                            .iter()
                                            .map(|item| {
                                                if preview_block
                                                    .as_ref()
                                                    .is_some_and(|block| block.id == item.block.id)
                                                {
                                                    calendar_block(item, false, colors)
                                                        .border_1()
                                                        .border_color(rgb(colors.accent))
                                                        .opacity(0.65)
                                                        .into_any_element()
                                                } else {
                                                    self.calendar_block(item, date, cx)
                                                }
                                            }),
                                    )
                                    .when(date == today, |column| {
                                        column.child(self.current_time_line.clone())
                                    })
                            })),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            let position = event.position;
                            let spot = this.calendar_spot(position);
                            let planner = cx.entity().downgrade();
                            // Finish outside-click dismissal before replacing the menu.
                            // Keep its target tied to the press, not the later cursor position.
                            window.defer(cx, move |window, cx| {
                                let _ = planner.update(cx, |planner, cx| {
                                    planner.open_calendar_menu(position, spot, window, cx)
                                });
                            });
                            cx.stop_propagation();
                        }),
                    ),
            )
            .child(Scrollbar::vertical(&self.calendar_scroll))
            .into_any_element()
    }
}

impl Render for DayPlanner {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        let today = self.today;
        div()
            .id("planner")
            .track_focus(&self.interaction_focus)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && cx.has_active_drag() {
                    cx.stop_active_drag(window);
                    this.active_gesture = None;
                    this.drag_preview = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<CalendarDrag>, _, cx| {
                    let drag = event.drag(cx).clone();
                    this.update_preview(&drag, event.event.position, cx);
                }),
            )
            .capture_any_mouse_up(cx.listener(Self::release_drag))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::release_drag))
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .font_family(cx.theme().font_family.clone())
            .text_size(px(14.))
            .text_color(rgb(colors.ink))
            .bg(rgb(colors.canvas))
            .when(cfg!(target_os = "windows"), |root| {
                root.child(
                    TitleBar::new()
                        .on_close_window(cx.listener(|this, _, window, cx| {
                            if this.should_close(window, cx) {
                                window.remove_window();
                            }
                        }))
                        .bg(rgb(colors.surface))
                        .text_color(rgb(colors.ink))
                        .border_color(rgb(colors.border))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .h_full()
                                .text_size(px(12.))
                                .child(self.app_title()),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when(self.sidebar_visible, |body| body.child(self.sidebar(cx)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.timer_bar(cx))
                            .child(self.week_toolbar(cx))
                            .child(self.day_headers(today, cx))
                            .child(self.calendar(today, cx)),
                    ),
            )
            .child(self.status_bar(cx))
            .when_some(self.calendar_menu.as_ref(), |root, menu| {
                root.child(
                    deferred(
                        anchored()
                            .position(menu.position)
                            .snap_to_window_with_margin(px(8.))
                            .child(menu.view.clone()),
                    )
                    .with_priority(gpui::base::POPUP_PRIORITY),
                )
            })
    }
}

fn monday_of(date: NaiveDate) -> NaiveDate {
    date - Duration::days(date.weekday().num_days_from_monday().into())
}

fn calendar_block(item: &DayBlock<'_>, show_tooltip: bool, colors: Palette) -> Stateful<Div> {
    let (background, foreground, border) =
        colors.task_colors(&item.task.color, item.task.completed);
    let time_label = match (
        local_time(item.block.starts_at),
        local_time(item.block.ends_at),
    ) {
        (Some(start), Some(end)) => format!(
            "{} – {} ({})",
            start.format("%H:%M"),
            end.format("%H:%M"),
            duration_label(item.block.ends_at - item.block.starts_at)
        ),
        _ => String::new(),
    };
    let dates = match (
        local_time(item.block.starts_at),
        local_time(item.block.ends_at),
    ) {
        (Some(start), Some(end)) => format!(
            "{} – {}",
            start.format("%a %-d %b, %H:%M"),
            end.format("%a %-d %b, %H:%M")
        ),
        _ => time_label.clone(),
    };
    let details = format!(
        "{}\n{} ({})\n{}",
        item.task.title,
        dates,
        duration_label(item.block.ends_at - item.block.starts_at),
        item.task.notes
    );
    div()
        .id(("block", item.block.id as usize))
        .absolute()
        .left(relative(item.lane as f32 / item.lane_count as f32))
        .w(relative(1. / item.lane_count as f32))
        .top(px(item.start_minute / 60. * HOUR_HEIGHT + 1.))
        .h(px(((item.end_minute - item.start_minute) / 60.
            * HOUR_HEIGHT
            - 2.)
            .max(1.)))
        .px(px(2.))
        .child(
            div()
                .size_full()
                .overflow_hidden()
                .rounded(px(3.))
                .border_1()
                .border_color(rgb(border))
                .bg(rgb(background))
                .text_color(rgb(foreground))
                .px(px(3.))
                .py(px(2.))
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(14.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_ellipsis()
                        .when(item.task.completed, |title| title.line_through())
                        .child(item.task.title.clone()),
                )
                .child(
                    div()
                        .text_size(px(10.))
                        .line_height(px(12.))
                        .text_ellipsis()
                        .child(time_label),
                ),
        )
        .when(show_tooltip, |block| {
            block.tooltip(move |window, cx| Tooltip::new(details.clone()).build(window, cx))
        })
}

fn week_label(start: NaiveDate) -> String {
    let end = start + Duration::days(6);
    if start.year() != end.year() {
        format!(
            "{} – {}",
            start.format("%b %-d, %Y"),
            end.format("%b %-d, %Y")
        )
    } else if start.month() != end.month() {
        format!("{} – {}", start.format("%b %-d"), end.format("%b %-d, %Y"))
    } else {
        format!("{} – {}, {}", start.format("%b %-d"), end.day(), end.year())
    }
}
