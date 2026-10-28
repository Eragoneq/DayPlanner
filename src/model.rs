use std::path::PathBuf;

use chrono::{DateTime, Local, NaiveDate, Timelike};

#[derive(Default)]
pub struct PlannerData {
    pub database_path: PathBuf,
    pub tasks: Vec<Task>,
    pub blocks: Vec<TimeBlock>,
    pub active_timer: Option<crate::timer::RunningTimer>,
    pub revision: u64,
    pub recovered_sessions: usize,
    pub todoist: crate::todoist::Settings,
}

pub struct Task {
    pub id: i64,
    pub title: String,
    pub notes: String,
    pub completed: bool,
    pub color: String,
    pub tracked_seconds: i64,
    pub tags: Vec<String>,
    pub subtasks: Vec<Subtask>,
}

#[derive(Clone)]
pub struct Subtask {
    pub title: String,
    pub completed: bool,
}

#[derive(Clone)]
pub struct TaskDraft {
    pub id: Option<i64>,
    pub title: String,
    pub notes: String,
    pub color: String,
    pub completed: bool,
    pub blocks: Vec<BlockDraft>,
    pub tags: Vec<String>,
    pub subtasks: Vec<Subtask>,
}

impl Default for TaskDraft {
    fn default() -> Self {
        Self {
            id: None,
            title: String::new(),
            notes: String::new(),
            color: "lavender".into(),
            completed: false,
            blocks: Vec::new(),
            tags: Vec::new(),
            subtasks: Vec::new(),
        }
    }
}

impl From<&Task> for TaskDraft {
    fn from(task: &Task) -> Self {
        Self {
            id: Some(task.id),
            title: task.title.clone(),
            notes: task.notes.clone(),
            color: task.color.clone(),
            completed: task.completed,
            blocks: Vec::new(),
            tags: task.tags.clone(),
            subtasks: task.subtasks.clone(),
        }
    }
}

#[derive(Clone)]
pub enum Change {
    Save(TaskDraft),
    SetCompleted { id: i64, completed: bool },
    Delete(i64),
    SaveBlock(BlockDraft),
    StartTimer(i64),
    StopTimer,
    CheckpointTimer,
}

#[derive(Clone, Copy)]
pub struct TaskTiming {
    pub planned_seconds: i64,
    pub tracked_seconds: i64,
    pub running: bool,
    pub completed: bool,
}

#[derive(Clone)]
pub struct TimeBlock {
    pub id: i64,
    pub task_id: i64,
    pub starts_at: i64,
    pub ends_at: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BlockDraft {
    pub id: Option<i64>,
    pub task_id: i64,
    pub starts_at: i64,
    pub ends_at: i64,
}

impl From<&TimeBlock> for BlockDraft {
    fn from(block: &TimeBlock) -> Self {
        Self {
            id: Some(block.id),
            task_id: block.task_id,
            starts_at: block.starts_at,
            ends_at: block.ends_at,
        }
    }
}

pub struct DayBlock<'a> {
    pub block: &'a TimeBlock,
    pub task: &'a Task,
    pub start_minute: f32,
    pub end_minute: f32,
    pub lane: usize,
    pub lane_count: usize,
}

impl TimeBlock {
    pub(crate) fn minutes_on(&self, date: NaiveDate) -> Option<(f32, f32)> {
        let start = local_time(self.starts_at)?;
        let end = local_time(self.ends_at)?;
        if start.date_naive() > date || end.date_naive() < date {
            return None;
        }
        let start_minute = if start.date_naive() < date {
            0.
        } else {
            start.num_seconds_from_midnight() as f32 / 60.
        };
        let end_minute = if end.date_naive() > date {
            1440.
        } else {
            end.num_seconds_from_midnight() as f32 / 60.
        };
        if end.date_naive() == date && end_minute == 0. {
            return None;
        }
        // A repeated local hour can make the wall-clock end precede its start.
        let end_minute = if end_minute <= start_minute {
            (start_minute + (self.ends_at - self.starts_at) as f32 / 60.).min(1440.)
        } else {
            end_minute
        };
        (end_minute > start_minute).then_some((start_minute, end_minute))
    }

    pub fn touches_date(&self, date: NaiveDate) -> bool {
        self.minutes_on(date).is_some()
    }
}

impl PlannerData {
    pub fn task_timing(&self, task: &Task) -> TaskTiming {
        let live_seconds = self
            .active_timer
            .filter(|timer| timer.task_id == task.id)
            .map(|timer| {
                timer
                    .elapsed_seconds()
                    .saturating_sub(timer.persisted_seconds)
            })
            .unwrap_or(0);
        TaskTiming {
            planned_seconds: self
                .blocks
                .iter()
                .filter(|block| block.task_id == task.id)
                .map(|block| block.ends_at - block.starts_at)
                .sum(),
            tracked_seconds: task.tracked_seconds + live_seconds,
            running: self
                .active_timer
                .is_some_and(|timer| timer.task_id == task.id),
            completed: task.completed,
        }
    }
    pub fn day_blocks<'a>(
        &'a self,
        date: NaiveDate,
        preview: Option<&'a TimeBlock>,
        tag: Option<&str>,
    ) -> Vec<DayBlock<'a>> {
        let mut blocks: Vec<_> = self
            .blocks
            .iter()
            .filter(|block| preview.is_none_or(|preview| block.id != preview.id))
            .chain(preview)
            .filter_map(|block| {
                let (start_minute, end_minute) = block.minutes_on(date)?;
                let task = self.tasks.iter().find(|task| task.id == block.task_id)?;
                if tag.is_some_and(|tag| !task.tags.iter().any(|name| name == tag)) {
                    return None;
                }
                Some(DayBlock {
                    block,
                    task,
                    start_minute,
                    end_minute,
                    lane: 0,
                    lane_count: 1,
                })
            })
            .collect();
        blocks.sort_by(|a, b| {
            a.start_minute
                .total_cmp(&b.start_minute)
                .then(a.block.id.cmp(&b.block.id))
        });

        let mut group_start = 0;
        while group_start < blocks.len() {
            let mut group_end = group_start + 1;
            let mut latest_end = blocks[group_start].end_minute;
            while group_end < blocks.len() && blocks[group_end].start_minute < latest_end {
                latest_end = latest_end.max(blocks[group_end].end_minute);
                group_end += 1;
            }

            let group = &mut blocks[group_start..group_end];
            let mut lane_ends = Vec::new();
            for block in group.iter_mut() {
                let lane = lane_ends
                    .iter()
                    .position(|end| *end <= block.start_minute)
                    .unwrap_or(lane_ends.len());
                if lane == lane_ends.len() {
                    lane_ends.push(block.end_minute);
                } else {
                    lane_ends[lane] = block.end_minute;
                }
                block.lane = lane;
            }
            for block in group {
                block.lane_count = lane_ends.len();
            }
            group_start = group_end;
        }
        blocks
    }
}

pub fn local_time(timestamp: i64) -> Option<DateTime<Local>> {
    DateTime::from_timestamp(timestamp, 0).map(|time| time.with_timezone(&Local))
}

pub fn normalize_tags(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|tag| tag.trim().trim_start_matches('#').trim().to_lowercase())
        .filter(|tag| !tag.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}
