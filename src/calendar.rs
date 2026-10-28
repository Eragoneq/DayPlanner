use std::{cell::Cell, rc::Rc};

use chrono::{Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike};

use crate::model::{BlockDraft, local_time};

pub const HOUR_HEIGHT: f32 = 80.;
pub const TIME_GUTTER: f32 = 64.;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DragMode {
    Create,
    Move,
    Start,
    End,
}

#[derive(Clone)]
pub struct CalendarDrag {
    pub original: BlockDraft,
    pub mode: DragMode,
    // Shared with GPUI's stored drag payload; filled when its threshold is crossed.
    pub grab_minutes: Rc<Cell<f32>>,
    pub segment_offset: f32,
}

impl CalendarDrag {
    pub fn new(original: BlockDraft, mode: DragMode, segment_offset: f32) -> Self {
        Self {
            original,
            mode,
            grab_minutes: Rc::new(Cell::new(0.)),
            segment_offset,
        }
    }

    pub fn proposal(&self, date: NaiveDate, minute: f32) -> Result<BlockDraft, String> {
        let minute = if self.mode == DragMode::Move {
            minute - self.grab_minutes.get()
        } else {
            minute
        };
        let minute = (minute / 15.).round() as i64 * 15;
        let minute = if self.mode == DragMode::Create {
            minute.clamp(0, 1425)
        } else {
            minute
        };
        let wall_time = date
            .and_hms_opt(0, 0, 0)
            .and_then(|time| time.checked_add_signed(Duration::minutes(minute)))
            .ok_or("The requested date is outside the supported range.")?;
        let timestamp = local_timestamp(wall_time)?;
        let mut block = self.original;
        match self.mode {
            DragMode::Create => {
                block.starts_at = timestamp;
                block.ends_at = timestamp + 1800;
            }
            DragMode::Move => {
                block.starts_at = timestamp;
                block.ends_at = timestamp + (self.original.ends_at - self.original.starts_at);
            }
            DragMode::Start => {
                block.starts_at = if timestamp > block.ends_at - 900 {
                    snap_limit(block.ends_at - 900, false)?
                } else {
                    timestamp
                };
            }
            DragMode::End => {
                block.ends_at = if timestamp < block.starts_at + 900 {
                    snap_limit(block.starts_at + 900, true)?
                } else {
                    timestamp
                };
            }
        }
        if local_time(block.starts_at).is_none() || local_time(block.ends_at).is_none() {
            return Err("The requested date is outside the supported range.".into());
        }
        Ok(block)
    }
}

fn snap_limit(timestamp: i64, up: bool) -> Result<i64, String> {
    let local =
        local_time(timestamp).ok_or("The requested date is outside the supported range.")?;
    let seconds = i64::from(local.num_seconds_from_midnight());
    let quarters = if up {
        (seconds + 899) / 900
    } else {
        seconds / 900
    };
    local_timestamp(
        local.date_naive().and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(quarters * 15),
    )
}

pub fn local_timestamp(wall_time: NaiveDateTime) -> Result<i64, String> {
    match Local.from_local_datetime(&wall_time) {
        LocalResult::Single(time) => Ok(time.timestamp()),
        LocalResult::None => {
            Err("This time is skipped by daylight saving. Choose another time.".into())
        }
        LocalResult::Ambiguous(_, _) => {
            Err("This time occurs twice due to daylight saving. Choose another time.".into())
        }
    }
}
