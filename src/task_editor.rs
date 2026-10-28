use gpui_component::{
    Disableable, Icon, IconName, Selectable, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Enter, Input, InputEvent, InputState, Textarea, TextareaState},
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    block_editor::BlockFields,
    model::{BlockDraft, Subtask, TaskDraft, TaskTiming, normalize_tags},
    theme::Palette,
    timer::duration_label,
};

const COLORS: [(&str, &str, u32); 5] = [
    ("lavender", "Lavender", 0x7770e8),
    ("blue", "Blue", 0x419ac9),
    ("green", "Green", 0x2c9f7b),
    ("amber", "Amber", 0xd99a20),
    ("rose", "Rose", 0xd56885),
];

actions!(task_editor, [ConfirmDelete]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", ConfirmDelete, Some("TaskDeleteConfirmation")),
        // Keep popover trigger shortcuts out of native editor controls.
        KeyBinding::new("space", NoAction, Some("TaskEditor")),
        KeyBinding::new("enter", NoAction, Some("TaskEditor")),
    ]);
}

pub enum TaskEditorEvent {
    Save(TaskDraft),
    Delete(i64),
    Cancel,
    ToggleTimer(i64),
}

pub struct TaskEditor {
    id: Option<i64>,
    title: Entity<InputState>,
    notes: Entity<TextareaState>,
    tags: Entity<InputState>,
    subtasks: Vec<ChecklistRow>,
    next_subtask_key: usize,
    blocks: Vec<BlockFields>,
    next_block_key: usize,
    selected_block: Option<usize>,
    show_schedule: bool,
    show_tags: bool,
    completed: bool,
    color: String,
    busy: bool,
    confirm_delete: bool,
    confirm_focus: FocusHandle,
    error: Option<String>,
    timing: Option<TaskTiming>,
    _input_events: Vec<Subscription>,
}

struct ChecklistRow {
    key: usize,
    title: Entity<InputState>,
    completed: bool,
    _changes: Subscription,
}

impl EventEmitter<TaskEditorEvent> for TaskEditor {}

impl TaskEditor {
    pub fn new(
        draft: TaskDraft,
        timing: Option<TaskTiming>,
        selected_block: Option<i64>,
        show_schedule: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Task title")
                .default_value(draft.title)
        });
        let notes = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Add notes…")
                .submit_on_enter(true)
                .auto_grow(2, 5)
                .default_value(draft.notes)
        });
        let tags = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("work, personal")
                .default_value(draft.tags.join(", "))
        });
        let mut events = vec![
            cx.subscribe(&title, |this, _, event: &InputEvent, cx| {
                this.on_input(event, cx)
            }),
            cx.subscribe(&notes, |this, _, event: &InputEvent, cx| {
                this.on_input(event, cx)
            }),
            cx.subscribe(&tags, |this, _, event: &InputEvent, cx| {
                this.on_input(event, cx)
            }),
        ];
        let subtasks: Vec<_> = draft
            .subtasks
            .into_iter()
            .enumerate()
            .map(|(key, item)| Self::checklist_row(key, item, window, cx))
            .collect();
        let blocks: Vec<_> = draft
            .blocks
            .into_iter()
            .enumerate()
            .map(|(key, block)| BlockFields::new(key, block, window, cx))
            .collect();
        for block in &blocks {
            for input in block.inputs() {
                events.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    this.on_input(event, cx)
                }));
            }
        }
        let selected_block = blocks
            .iter()
            .find(|block| selected_block.is_some() && block.original.id == selected_block)
            .or_else(|| blocks.first())
            .map(|block| block.key);
        Self {
            id: draft.id,
            title,
            notes,
            tags,
            next_subtask_key: subtasks.len(),
            subtasks,
            next_block_key: blocks.len(),
            blocks,
            selected_block,
            show_schedule,
            show_tags: false,
            completed: draft.completed,
            color: draft.color,
            busy: false,
            confirm_delete: false,
            confirm_focus: cx.focus_handle(),
            error: None,
            timing,
            _input_events: events,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.confirm_delete {
            return;
        }
        let title = self.title.read(cx).value().trim().to_owned();
        if title.is_empty() {
            self.error = Some("Enter a task title.".into());
            cx.notify();
            return;
        }
        let mut blocks = Vec::new();
        for fields in &self.blocks {
            match fields.draft(cx) {
                Ok(block) => blocks.push(block),
                Err(error) => {
                    self.error = Some(error);
                    self.show_schedule = true;
                    self.selected_block = Some(fields.key);
                    cx.notify();
                    return;
                }
            }
        }
        cx.emit(TaskEditorEvent::Save(TaskDraft {
            id: self.id,
            title,
            notes: self.notes.read(cx).value().to_string(),
            color: self.color.clone(),
            completed: self.completed,
            blocks,
            tags: normalize_tags(&self.tags.read(cx).value()),
            subtasks: self
                .subtasks
                .iter()
                .filter_map(|item| {
                    let title = item.title.read(cx).value().trim().to_owned();
                    (!title.is_empty()).then_some(Subtask {
                        title,
                        completed: item.completed,
                    })
                })
                .collect(),
        }));
    }

    fn checklist_row(
        key: usize,
        item: Subtask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ChecklistRow {
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Sub-task")
                .default_value(item.title)
        });
        let changes = cx.subscribe(&title, |this, _, event: &InputEvent, cx| {
            this.on_input(event, cx)
        });
        ChecklistRow {
            key,
            title,
            completed: item.completed,
            _changes: changes,
        }
    }

    fn add_subtask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let item = Self::checklist_row(
            self.next_subtask_key,
            Subtask {
                title: String::new(),
                completed: false,
            },
            window,
            cx,
        );
        self.next_subtask_key += 1;
        window.focus(&item.title.focus_handle(cx), cx);
        self.subtasks.push(item);
        self.error = None;
        cx.notify();
    }

    fn add_block(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let starts_at = (chrono::Local::now().timestamp().div_euclid(900) + 1) * 900;
        let fields = BlockFields::new(
            self.next_block_key,
            BlockDraft {
                id: None,
                task_id: self.id.unwrap_or(0),
                starts_at,
                ends_at: starts_at + 1800,
            },
            window,
            cx,
        );
        for input in fields.inputs() {
            self._input_events
                .push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    this.on_input(event, cx)
                }));
        }
        self.next_block_key += 1;
        self.selected_block = Some(fields.key);
        window.focus(&fields.start_date.focus_handle(cx), cx);
        self.blocks.push(fields);
        self.error = None;
        cx.notify();
    }

    fn on_input(&mut self, event: &InputEvent, cx: &mut Context<Self>) {
        if matches!(event, InputEvent::Change) {
            self.error = None;
            cx.notify();
        }
    }

    pub fn saving(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        cx.notify();
    }

    pub fn failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = Some(message);
        cx.notify();
    }

    pub fn sync_timing(&mut self, timing: Option<TaskTiming>, busy: bool, cx: &mut Context<Self>) {
        self.timing = timing;
        self.busy = busy;
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> Div {
        let disabled = self.busy || self.confirm_delete;
        let colors = Palette::current(cx);
        div()
            .flex()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .child(
                Checkbox::new("task-completed")
                    .small()
                    .checked(self.completed)
                    .disabled(disabled)
                    .accessibility_label("Task completed, save to apply")
                    .on_change(cx.listener(|this, completed, _, cx| {
                        this.completed = *completed;
                        cx.notify();
                    })),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.title)
                        .small()
                        .h(px(24.))
                        .appearance(false)
                        .p_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .aria_label("Task title")
                        .disabled(disabled)
                        .when(self.completed, |input| {
                            input.line_through().text_color(rgb(colors.muted))
                        }),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .flex_shrink_0()
                    .children(COLORS.into_iter().map(|(color, label, fill)| {
                        Button::new(color)
                            .ghost()
                            .size(px(18.))
                            .p_0()
                            .rounded_full()
                            .selected(self.color == color)
                            .disabled(disabled)
                            .tooltip(label)
                            .accessibility_label(format!("{label} task color"))
                            .child(div().size(px(12.)).rounded_full().bg(rgb(fill)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.color = color.into();
                                cx.notify();
                            }))
                    })),
            )
    }

    fn checklist_item(&self, item: &ChecklistRow, cx: &mut Context<Self>) -> Stateful<Div> {
        let key = item.key;
        let colors = Palette::current(cx);
        let disabled = self.busy || self.confirm_delete;
        div()
            .id(("subtask-row", key))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .child(
                Checkbox::new(("subtask-complete", key))
                    .small()
                    .checked(item.completed)
                    .disabled(disabled)
                    .accessibility_label(if item.completed {
                        "Reopen sub-task"
                    } else {
                        "Complete sub-task"
                    })
                    .on_change(cx.listener(move |this, completed, _, cx| {
                        if let Some(item) = this.subtasks.iter_mut().find(|item| item.key == key) {
                            item.completed = *completed;
                            cx.notify();
                        }
                    })),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&item.title)
                        .small()
                        .h(px(24.))
                        .appearance(false)
                        .p_0()
                        .aria_label("Sub-task title")
                        .disabled(disabled)
                        .when(item.completed, |input| {
                            input.line_through().text_color(rgb(colors.muted))
                        }),
                ),
            )
            .child(
                Button::new(("remove-subtask", key))
                    .ghost()
                    .size(px(22.))
                    .icon(IconName::Minus)
                    .accessibility_label("Remove sub-task")
                    .tooltip("Remove sub-task")
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.subtasks.retain(|item| item.key != key);
                        cx.notify();
                    })),
            )
    }

    fn schedule(&self, cx: &mut Context<Self>) -> Div {
        let colors = Palette::current(cx);
        let disabled = self.busy || self.confirm_delete;
        let selected = self
            .blocks
            .iter()
            .find(|block| Some(block.key) == self.selected_block);
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_2()
            .border_t_1()
            .border_color(rgb(colors.border))
            .pt_2()
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(colors.muted))
                    .child("Planned blocks"),
            )
            .children(self.blocks.iter().map(|block| {
                let key = block.key;
                Button::new(("select-block", key))
                    .ghost()
                    .small()
                    .w_full()
                    .label(block.label(cx))
                    .selected(self.selected_block == Some(key))
                    .disabled(disabled)
                    .accessibility_label(format!("Edit planned block {}", block.label(cx)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_block = Some(key);
                        cx.notify();
                    }))
            }))
            .when_some(selected, |panel, block| {
                panel.child(block.element(disabled, cx))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        Button::new("add-planned-block")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add block")
                            .disabled(disabled)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_block(window, cx)),
                            ),
                    )
                    .when_some(self.selected_block, |row, key| {
                        row.child(
                            Button::new("remove-planned-block")
                                .ghost()
                                .small()
                                .label("Remove block")
                                .text_color(rgb(colors.error))
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.blocks.retain(|block| block.key != key);
                                    this.selected_block =
                                        this.blocks.first().map(|block| block.key);
                                    cx.notify();
                                })),
                        )
                    }),
            )
    }

    fn body(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let colors = Palette::current(cx);
        let disabled = self.busy || self.confirm_delete;
        div()
            .id("task-editor-body")
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(340.))
            .min_h_0()
            .overflow_y_scroll()
            .child(
                Textarea::new(&self.notes)
                    .appearance(false)
                    .p_0()
                    .h(px(56.))
                    .flex_shrink_0()
                    .aria_label("Task notes, Shift+Enter for a new line")
                    .disabled(disabled),
            )
            .child(
                div().flex().flex_col().flex_shrink_0().gap_1().children(
                    self.subtasks
                        .iter()
                        .map(|item| self.checklist_item(item, cx)),
                ),
            )
            .when(self.show_tags, |body| {
                body.child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_shrink_0()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(colors.muted))
                                .child("Tags · comma separated"),
                        )
                        .child(
                            Input::new(&self.tags)
                                .small()
                                .aria_label("Task tags, separated by commas")
                                .disabled(disabled),
                        ),
                )
            })
            .when(self.show_schedule, |body| body.child(self.schedule(cx)))
            .when_some(self.timing, |body, timing| {
                body.child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.))
                        .text_color(rgb(colors.muted))
                        .child(format!(
                            "{} planned · {} tracked",
                            duration_label(timing.planned_seconds),
                            duration_label(timing.tracked_seconds)
                        )),
                )
            })
    }

    fn feedback(&self, cx: &mut Context<Self>) -> Div {
        let colors = Palette::current(cx);
        div().flex().flex_col().flex_shrink_0().gap_2()
            .when_some(self.error.clone(), |body, error| {
                body.child(div().text_size(px(12.)).text_color(rgb(colors.error)).child(error))
            })
            .when(self.confirm_delete, |body| {
                body.child(
                    div().id("delete-confirmation")
                        .track_focus(&self.confirm_focus)
                        .key_context("TaskDeleteConfirmation")
                        .text_size(px(12.))
                        .text_color(rgb(colors.error))
                        .child("Delete this task and all its planned and tracked time? Press Enter to confirm.")
                        .on_action(cx.listener(|this, _: &ConfirmDelete, _, cx| {
                            if !this.busy && this.confirm_delete && let Some(id) = this.id {
                                cx.emit(TaskEditorEvent::Delete(id));
                            }
                        })),
                )
            })
    }

    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let colors = Palette::current(cx);
        let disabled = self.busy || self.confirm_delete;
        let tags = normalize_tags(&self.tags.read(cx).value());
        let tag_label = match tags.first() {
            Some(tag) if tags.len() > 1 => format!("#{tag} +{}", tags.len() - 1),
            Some(tag) => format!("#{tag}"),
            None => "Tags".into(),
        };
        let running = self.timing.is_some_and(|timing| timing.running);
        div()
            .flex()
            .items_center()
            .gap_1()
            .flex_shrink_0()
            .border_t_1()
            .border_color(rgb(colors.border))
            .pt_2()
            .child(
                Button::new("edit-tags")
                    .ghost()
                    .small()
                    .max_w(px(82.))
                    .label(tag_label)
                    .selected(self.show_tags)
                    .disabled(disabled)
                    .tooltip("Edit tags")
                    .accessibility_label("Edit task tags")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_tags = !this.show_tags;
                        if this.show_tags {
                            window.focus(&this.tags.focus_handle(cx), cx);
                        }
                        cx.notify();
                    })),
            )
            .child(
                Button::new("edit-schedule")
                    .ghost()
                    .size(px(26.))
                    .icon(IconName::Calendar)
                    .selected(self.show_schedule)
                    .disabled(disabled)
                    .tooltip(format!("Edit planned blocks ({})", self.blocks.len()))
                    .accessibility_label("Edit planned blocks")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_schedule = !this.show_schedule;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("add-subtask")
                    .ghost()
                    .size(px(26.))
                    .icon(IconName::Plus)
                    .disabled(disabled)
                    .tooltip("Add sub-task")
                    .accessibility_label("Add sub-task")
                    .on_click(cx.listener(|this, _, window, cx| this.add_subtask(window, cx))),
            )
            .when_some(self.id, |row, id| {
                row.child(
                    Button::new("task-timer")
                        .ghost()
                        .size(px(26.))
                        .icon(if running {
                            IconName::Pause
                        } else {
                            IconName::Play
                        })
                        .disabled(
                            disabled
                                || (!running
                                    && (self.completed
                                        || self.timing.is_some_and(|timing| timing.completed))),
                        )
                        .tooltip(if running {
                            "Stop timer"
                        } else if self.completed
                            || self.timing.is_some_and(|timing| timing.completed)
                        {
                            "Save to reopen this task before starting the timer"
                        } else {
                            "Start timer"
                        })
                        .accessibility_label(if running {
                            "Stop task timer"
                        } else {
                            "Start task timer"
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(TaskEditorEvent::ToggleTimer(id))
                        })),
                )
            })
            .when(self.id.is_some() && !self.confirm_delete, |row| {
                row.child(
                    Button::new("delete-task")
                        .ghost()
                        .size(px(26.))
                        .icon(Icon::new(gpui_kit::assets::IconName::Trash))
                        .disabled(self.busy)
                        .tooltip("Delete task")
                        .accessibility_label("Delete task")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_delete = true;
                            this.error = None;
                            window.focus(&this.confirm_focus, cx);
                            cx.notify();
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("cancel-edit")
                    .ghost()
                    .small()
                    .px(px(6.))
                    .label(if self.confirm_delete {
                        "Keep"
                    } else {
                        "Cancel"
                    })
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.confirm_delete {
                            this.confirm_delete = false;
                            window.focus(&this.title.focus_handle(cx), cx);
                            cx.notify();
                        } else {
                            cx.emit(TaskEditorEvent::Cancel);
                        }
                    })),
            )
            .child(if self.confirm_delete {
                Button::new("confirm-delete-task")
                    .danger()
                    .small()
                    .px(px(8.))
                    .label(if self.busy { "Deleting…" } else { "Delete" })
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(id) = this.id {
                            cx.emit(TaskEditorEvent::Delete(id));
                        }
                    }))
            } else {
                Button::new("save-task")
                    .primary()
                    .small()
                    .px(px(8.))
                    .label(if self.busy { "Saving…" } else { "Save" })
                    .tooltip("Save changes (Enter)")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx)))
            })
    }
}

impl Focusable for TaskEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.title.focus_handle(cx)
    }
}

impl Render for TaskEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Palette::current(cx);
        let accent = COLORS
            .iter()
            .find(|(color, _, _)| *color == self.color)
            .map(|(_, _, fill)| *fill)
            .unwrap_or(colors.accent);
        div()
            .id("task-editor-form")
            .key_context("TaskEditor")
            .relative()
            .flex()
            .flex_col()
            .gap(px(10.))
            .w(px(380.))
            .max_h(px(520.))
            .p(px(14.))
            .pl(px(18.))
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.surface))
            .text_color(rgb(colors.ink))
            .text_size(px(14.))
            .overflow_hidden()
            // Consume propagated input Enter before the popover's toggle binding.
            .on_action(cx.listener(|this, action: &Enter, _, cx| {
                if !action.secondary && !action.shift {
                    this.submit(cx);
                }
            }))
            .child(
                div()
                    .absolute()
                    // GPUI clips descendants rectangularly. Keep the rail clear
                    // of the card's 12 px corners instead of relying on clipping.
                    .left(px(1.))
                    .top(px(12.))
                    .bottom(px(12.))
                    .w(px(4.))
                    .rounded_full()
                    .bg(rgb(accent)),
            )
            .child(self.header(cx))
            .child(self.body(cx))
            .when(self.error.is_some() || self.confirm_delete, |card| {
                card.child(self.feedback(cx))
            })
            .child(self.footer(cx))
    }
}
