use std::{
    fs::{self, File, OpenOptions},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use async_channel::{Receiver, Sender};
use chrono::{Datelike, Days, Local, NaiveDate, TimeZone};
use rusqlite::{Connection, params};

use crate::model::{Change, PlannerData, Subtask, Task, TimeBlock, normalize_tags};
use crate::timer::RunningTimer;

const SCHEMA_VERSION: i64 = 3;
type DataResult = Result<PlannerData, String>;

enum Command {
    Write(Change, Sender<DataResult>),
    Sync(
        crate::todoist::Job,
        Sender<Result<crate::todoist::Reply, String>>,
    ),
    Shutdown(Sender<()>),
}

/// Owns the database worker's lifetime. SQLite never runs on the UI thread.
pub struct Database {
    commands: mpsc::Sender<Command>,
}

#[derive(Clone)]
pub struct SyncStore {
    commands: mpsc::Sender<Command>,
}

impl SyncStore {
    pub fn request(&self, job: crate::todoist::Job) -> Result<crate::todoist::Reply> {
        let (tx, rx) = async_channel::bounded(1);
        self.commands
            .send(Command::Sync(job, tx))
            .map_err(|_| anyhow::anyhow!("The storage worker is unavailable."))?;
        rx.recv_blocking()
            .context("Storage stopped during sync")?
            .map_err(anyhow::Error::msg)
    }
}

impl Database {
    pub fn sync_store(&self) -> SyncStore {
        SyncStore {
            commands: self.commands.clone(),
        }
    }

    pub fn start(demo: bool) -> Result<(Self, Receiver<DataResult>)> {
        let (commands, commands_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = async_channel::unbounded();

        thread::Builder::new()
            .name("dayplanner-db".into())
            .spawn(move || match open(demo) {
                Ok((connection, data, lock)) => {
                    let path = data.database_path.clone();
                    if ready_tx.send_blocking(Ok(data)).is_ok() {
                        run(connection, path, commands_rx, ready_tx, lock);
                    }
                }
                Err(error) => {
                    let _ = ready_tx.send_blocking(Err(format!("{error:#}")));
                }
            })
            .context("Could not start the storage worker")?;

        Ok((Self { commands }, ready_rx))
    }

    pub fn write(&self, change: Change) -> Receiver<DataResult> {
        let (reply_tx, reply_rx) = async_channel::bounded(1);
        if self
            .commands
            .send(Command::Write(change, reply_tx.clone()))
            .is_err()
        {
            let _ = reply_tx.try_send(Err(
                "The storage worker is unavailable. Reopen the planner and try again.".into(),
            ));
        }
        reply_rx
    }

    /// Queued behind pending writes for the app's final cleanup.
    pub fn shutdown(&self) -> Receiver<()> {
        let (reply_tx, reply_rx) = async_channel::bounded(1);
        if self
            .commands
            .send(Command::Shutdown(reply_tx.clone()))
            .is_err()
        {
            let _ = reply_tx.try_send(());
        }
        reply_rx
    }
}

fn run(
    mut connection: Connection,
    path: PathBuf,
    commands: mpsc::Receiver<Command>,
    events: Sender<DataResult>,
    _lock: File,
) {
    let mut active = None;
    let mut revision = 0;
    loop {
        let command = match active {
            Some(timer) => commands.recv_timeout(RunningTimer::until_checkpoint(timer)),
            None => commands
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match command {
            Ok(Command::Sync(job, reply)) => {
                let result = sync_job(&mut connection, &path, job, &mut active, &mut revision)
                    .map_err(|error| format!("{error:#}"));
                let _ = reply.send_blocking(result);
            }
            Ok(Command::Write(change, reply)) => {
                let result = apply(&mut connection, &path, change, &mut active, &mut revision)
                    .map_err(|error| format!("{error:#}"));
                let _ = reply.send_blocking(result);
            }
            Ok(Command::Shutdown(reply)) => {
                if active.is_some() {
                    let _ = apply(
                        &mut connection,
                        &path,
                        Change::StopTimer,
                        &mut active,
                        &mut revision,
                    );
                }
                drop(connection);
                let _ = reply.send_blocking(());
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let result = apply(
                    &mut connection,
                    &path,
                    Change::CheckpointTimer,
                    &mut active,
                    &mut revision,
                );
                if result.is_err()
                    && let Some(timer) = &mut active
                {
                    timer.retry_later();
                }
                let _ = events.send_blocking(
                    result.map_err(|error| format!("Timer checkpoint failed: {error:#}")),
                );
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if active.is_some() {
                    let _ = apply(
                        &mut connection,
                        &path,
                        Change::StopTimer,
                        &mut active,
                        &mut revision,
                    );
                }
                return;
            }
        }
    }
}

fn apply(
    connection: &mut Connection,
    path: &std::path::Path,
    change: Change,
    active: &mut Option<RunningTimer>,
    revision: &mut u64,
) -> Result<PlannerData> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut next_timer = *active;
    let ending_task = match &change {
        Change::Save(draft) if draft.completed => draft.id,
        Change::SetCompleted {
            id,
            completed: true,
        }
        | Change::Delete(id) => Some(*id),
        _ => None,
    };
    if let Some(timer) = next_timer
        && ending_task == Some(timer.task_id)
    {
        save_session(&transaction, timer, true)?;
        next_timer = None;
    }
    match change {
        Change::Save(draft) => {
            let title = draft.title.trim();
            ensure!(!title.is_empty(), "Enter a task title.");
            let task_id = match draft.id {
                Some(id) => {
                    let changed = transaction.execute(
                        "UPDATE tasks SET title = ?1, notes = ?2, color = ?3, completed = ?4 WHERE id = ?5",
                        params![title, draft.notes, draft.color, draft.completed, id],
                    )?;
                    ensure!(
                        changed == 1,
                        "This task no longer exists. Reopen the planner to refresh."
                    );
                    id
                }
                None => {
                    transaction.execute(
                        "INSERT INTO tasks (title, notes, color, completed) VALUES (?1, ?2, ?3, ?4)",
                        params![title, draft.notes, draft.color, draft.completed],
                    )?;
                    transaction.last_insert_rowid()
                }
            };
            save_task_blocks(&transaction, task_id, &draft.blocks)?;
            transaction.execute("DELETE FROM task_tags WHERE task_id = ?1", [task_id])?;
            for tag in normalize_tags(&draft.tags.join(",")) {
                transaction.execute(
                    "INSERT INTO tags (name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
                    [&tag],
                )?;
                transaction.execute(
                    "INSERT INTO task_tags (task_id, tag_id) SELECT ?1, id FROM tags WHERE name = ?2",
                    params![task_id, tag],
                )?;
            }
            transaction.execute("DELETE FROM subtasks WHERE task_id = ?1", [task_id])?;
            for (position, item) in draft
                .subtasks
                .iter()
                .filter(|item| !item.title.trim().is_empty())
                .enumerate()
            {
                transaction.execute(
                    "INSERT INTO subtasks (task_id, position, title, completed) VALUES (?1, ?2, ?3, ?4)",
                    params![task_id, position as i64, item.title.trim(), item.completed],
                )?;
            }
            prune_tags(&transaction)?;
        }
        Change::SetCompleted { id, completed } => {
            let changed = transaction.execute(
                "UPDATE tasks SET completed = ?1 WHERE id = ?2",
                params![completed, id],
            )?;
            ensure!(
                changed == 1,
                "This task no longer exists. Reopen the planner to refresh."
            );
        }
        Change::Delete(id) => {
            crate::todoist::detach_deleted_task(&transaction, id)?;
            let changed = transaction.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
            ensure!(
                changed == 1,
                "This task no longer exists. Reopen the planner to refresh."
            );
            prune_tags(&transaction)?;
        }
        Change::SaveBlock(block) => {
            ensure!(
                block
                    .ends_at
                    .checked_sub(block.starts_at)
                    .is_some_and(|seconds| seconds >= 900),
                "A block must last at least 15 minutes."
            );
            match block.id {
                Some(id) => {
                    let changed = transaction.execute(
                        "UPDATE time_blocks SET starts_at = ?1, ends_at = ?2 WHERE id = ?3 AND task_id = ?4",
                        params![block.starts_at, block.ends_at, id, block.task_id],
                    )?;
                    ensure!(
                        changed == 1,
                        "This block no longer exists. Reopen the planner to refresh."
                    );
                }
                None => {
                    transaction.execute(
                        "INSERT INTO time_blocks (task_id, starts_at, ends_at) VALUES (?1, ?2, ?3)",
                        params![block.task_id, block.starts_at, block.ends_at],
                    )?;
                }
            }
        }
        Change::StartTimer(task_id) => {
            let completed: bool = transaction.query_row(
                "SELECT completed FROM tasks WHERE id = ?1",
                [task_id],
                |row| row.get(0),
            )?;
            ensure!(!completed, "Reopen this task before starting its timer.");
            if let Some(timer) = next_timer {
                ensure!(
                    timer.task_id != task_id,
                    "This task's timer is already running."
                );
                save_session(&transaction, timer, true)?;
            }
            let now = chrono::Utc::now().timestamp();
            transaction.execute("INSERT INTO time_sessions (task_id, started_at, last_saved_at) VALUES (?1, ?2, ?2)", params![task_id, now])?;
            next_timer = Some(RunningTimer::new(
                transaction.last_insert_rowid(),
                task_id,
                now,
            ));
        }
        Change::StopTimer => {
            if let Some(timer) = next_timer {
                save_session(&transaction, timer, true)?;
            }
            next_timer = None;
        }
        Change::CheckpointTimer => {
            if let Some(timer) = next_timer {
                next_timer = Some(save_session(&transaction, timer, false)?);
            }
        }
    }
    // Read the resulting snapshot in the same transaction: failures roll the
    // mutation back, so the UI never reports an unconfirmed change.
    let mut data = load_snapshot(&transaction, path.to_path_buf())?;
    transaction.commit()?;
    *active = next_timer;
    *revision += 1;
    data.active_timer = next_timer;
    data.revision = *revision;
    Ok(data)
}

fn sync_job(
    connection: &mut Connection,
    path: &std::path::Path,
    job: crate::todoist::Job,
    active: &mut Option<RunningTimer>,
    revision: &mut u64,
) -> Result<crate::todoist::Reply> {
    use crate::todoist::{self, Job, Reply};
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut next_timer = *active;
    let mut confirmed_error = None;
    let reply = match job {
        Job::State => Reply::State(todoist::state(&transaction)?),
        Job::Connect { account, tag } => {
            todoist::connect(&transaction, &account, &tag)?;
            Reply::Done
        }
        Job::Plan { items, resolution } => Reply::Plan(todoist::plan(
            &transaction,
            &items,
            resolution,
            &mut next_timer,
        )?),
        Job::Acknowledge(response) => {
            confirmed_error = todoist::acknowledge(&transaction, &response)?;
            Reply::Done
        }
        Job::Discard(uuids) => {
            todoist::discard(&transaction, &uuids)?;
            Reply::Done
        }
        Job::Finish => {
            transaction.execute(
                "UPDATE todoist_settings SET last_synced_at = ?1 WHERE id = 1",
                [chrono::Utc::now().timestamp()],
            )?;
            Reply::Done
        }
        Job::Snapshot => {
            let mut data = load_snapshot(&transaction, path.to_path_buf())?;
            data.active_timer = next_timer;
            data.revision = *revision + 1;
            Reply::Snapshot(data)
        }
    };
    transaction.commit()?;
    *active = next_timer;
    *revision += 1;
    // A definitive partial failure still commits the acknowledged commands.
    if let Some(error) = confirmed_error {
        bail!(error);
    }
    Ok(reply)
}

pub(crate) fn save_session(
    connection: &Connection,
    timer: RunningTimer,
    stop: bool,
) -> Result<RunningTimer> {
    let saved = timer.checkpoint();
    let changed = connection.execute(
        "UPDATE time_sessions SET elapsed_seconds = ?1, last_saved_at = ?2, ended_at = ?3 WHERE id = ?4 AND ended_at IS NULL",
        params![saved.persisted_seconds, saved.last_saved_at, stop.then_some(saved.last_saved_at), saved.session_id],
    )?;
    ensure!(
        changed == 1,
        "The active timer session no longer exists. Reopen the planner to refresh."
    );
    Ok(saved)
}

fn open(demo: bool) -> Result<(Connection, PlannerData, File)> {
    let path = database_path(demo)?;
    let parent = path
        .parent()
        .context("The planner data folder is missing")?;
    fs::create_dir_all(parent).with_context(|| format!("Could not create {}", parent.display()))?;

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    lock.try_lock().context("This planner data may already be open in another instance. Close that instance and try again.")?;

    let mut connection =
        Connection::open(&path).with_context(|| format!("Could not open {}", path.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    migrate(&mut connection, demo)
        .with_context(|| format!("Could not initialize {}", path.display()))?;
    let recovered = connection.execute(
        "UPDATE time_sessions SET ended_at = last_saved_at WHERE ended_at IS NULL",
        [],
    )?;
    let mut data = load(&mut connection, path.clone())
        .with_context(|| format!("Could not read {}", path.display()))?;
    data.recovered_sessions = recovered;
    Ok((connection, data, lock))
}

fn load(connection: &mut Connection, database_path: PathBuf) -> Result<PlannerData> {
    let transaction = connection.transaction()?;
    let data = load_snapshot(&transaction, database_path)?;
    transaction.commit()?;
    Ok(data)
}

fn load_snapshot(connection: &Connection, database_path: PathBuf) -> Result<PlannerData> {
    let mut tasks = {
        let mut statement = connection.prepare(
            "SELECT t.id, t.title, t.notes, t.completed, t.color,
                    COALESCE((SELECT SUM(s.elapsed_seconds) FROM time_sessions s WHERE s.task_id = t.id), 0)
             FROM tasks t ORDER BY t.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Task {
                id: row.get(0)?,
                title: row.get(1)?,
                notes: row.get(2)?,
                completed: row.get(3)?,
                color: row.get(4)?,
                tracked_seconds: row.get(5)?,
                tags: Vec::new(),
                subtasks: Vec::new(),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut tags = connection.prepare(
        "SELECT tt.task_id, t.name FROM task_tags tt JOIN tags t ON t.id = tt.tag_id ORDER BY t.name",
    )?;
    for row in tags.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, name) = row?;
        if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
            task.tags.push(name);
        }
    }
    let mut subtasks = connection
        .prepare("SELECT task_id, title, completed FROM subtasks ORDER BY task_id, position")?;
    for row in subtasks.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            Subtask {
                title: row.get(1)?,
                completed: row.get(2)?,
            },
        ))
    })? {
        let (id, item) = row?;
        if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
            task.subtasks.push(item);
        }
    }
    let blocks = {
        let mut statement = connection.prepare(
            "SELECT id, task_id, starts_at, ends_at FROM time_blocks ORDER BY starts_at, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(TimeBlock {
                id: row.get(0)?,
                task_id: row.get(1)?,
                starts_at: row.get(2)?,
                ends_at: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(PlannerData {
        database_path,
        tasks,
        blocks,
        active_timer: None,
        revision: 0,
        recovered_sessions: 0,
        todoist: crate::todoist::settings(connection)?,
    })
}

fn database_path(demo: bool) -> Result<PathBuf> {
    let data_dir = dirs::data_local_dir().context("Could not find your application-data folder")?;
    let app_dir = if cfg!(target_os = "windows") {
        "DayPlanner"
    } else {
        "dayplanner"
    };
    let file_name = if demo {
        "dayplanner-demo.sqlite3"
    } else {
        "dayplanner.sqlite3"
    };
    Ok(data_dir.join(app_dir).join(file_name))
}

fn migrate(connection: &mut Connection, demo: bool) -> Result<()> {
    // Take the write lock before reading the version so two launches cannot
    // both try to create the schema in the same fresh database.
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if !(0..=SCHEMA_VERSION).contains(&version) {
        bail!(
            "Database version {version} is not supported by this app (expected {SCHEMA_VERSION}). The database has been left unchanged."
        );
    }
    if version == 0 {
        transaction.execute_batch(include_str!("schema.sql"))?;
    }
    if version < 2 {
        transaction.execute_batch(include_str!("schema_v2.sql"))?;
    }
    if version < 3 {
        transaction.execute_batch(include_str!("schema_v3.sql"))?;
    }
    if version == 0 && demo {
        seed_demo(&transaction)?;
    }
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn seed_demo(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    for (id, title, notes, completed, color) in [
        (
            1,
            "Deep work",
            "Focus on one useful piece of work.",
            false,
            "lavender",
        ),
        (2, "Standup", "A short team check-in.", false, "blue"),
        (3, "Groceries", "An unscheduled task.", false, "amber"),
        (
            4,
            "Weekly review",
            "Review the week and clear loose ends.",
            true,
            "green",
        ),
    ] {
        transaction.execute(
            "INSERT INTO tasks (id, title, notes, completed, color) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, title, notes, completed, color],
        )?;
    }

    transaction.execute_batch(
        "INSERT INTO tags (id, name) VALUES (1, 'work'), (2, 'personal');
         INSERT INTO task_tags (task_id, tag_id) VALUES (1, 1), (2, 1), (3, 2), (4, 1);
         INSERT INTO subtasks (task_id, position, title, completed) VALUES
         (1, 0, 'Outline the work', 1), (1, 1, 'Finish the first draft', 0),
         (3, 0, 'Vegetables', 0), (3, 1, 'Coffee', 0);",
    )?;

    let today = Local::now().date_naive();
    let monday = today - Days::new(today.weekday().num_days_from_monday().into());
    for (id, task_id, day, start_hour, start_minute, end_hour, end_minute) in [
        (1, 1, 0, 9, 0, 10, 30),
        (2, 2, 0, 10, 0, 10, 30),
        (3, 1, 1, 14, 0, 15, 0),
        (4, 4, 5, 11, 0, 11, 30),
    ] {
        let date = monday + Days::new(day);
        transaction.execute(
            "INSERT INTO time_blocks (id, task_id, starts_at, ends_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                id,
                task_id,
                local_timestamp(date, start_hour, start_minute)?,
                local_timestamp(date, end_hour, end_minute)?,
            ],
        )?;
    }

    for (id, task_id, day, start_hour, start_minute, end_hour, end_minute) in [
        (1, 1, 0, 9, 0, 9, 25),
        (2, 1, 0, 9, 30, 9, 45),
        (3, 4, 5, 11, 0, 11, 30),
    ] {
        let date = monday + Days::new(day);
        let start = local_timestamp(date, start_hour, start_minute)?;
        let end = local_timestamp(date, end_hour, end_minute)?;
        transaction.execute(
            "INSERT INTO time_sessions (id, task_id, started_at, ended_at, elapsed_seconds, last_saved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?4)",
            params![id, task_id, start, end, end - start],
        )?;
    }

    Ok(())
}

fn local_timestamp(date: NaiveDate, hour: u32, minute: u32) -> Result<i64> {
    let time = date
        .and_hms_opt(hour, minute, 0)
        .context("Invalid demo time")?;
    Local
        .from_local_datetime(&time)
        .single()
        .map(|datetime| datetime.timestamp())
        .with_context(|| format!("Demo time {time} is ambiguous or unavailable in your timezone"))
}

fn prune_tags(connection: &Connection) -> Result<()> {
    connection.execute(
        "DELETE FROM tags WHERE NOT EXISTS (SELECT 1 FROM task_tags WHERE tag_id = tags.id)",
        [],
    )?;
    Ok(())
}

fn save_task_blocks(
    connection: &Connection,
    task_id: i64,
    blocks: &[crate::model::BlockDraft],
) -> Result<()> {
    let existing = connection
        .prepare("SELECT id FROM time_blocks WHERE task_id = ?1")?
        .query_map([task_id], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut kept = std::collections::HashSet::new();
    for block in blocks {
        ensure!(
            block
                .ends_at
                .checked_sub(block.starts_at)
                .is_some_and(|seconds| seconds >= 900),
            "A block must last at least 15 minutes."
        );
        ensure!(
            crate::model::local_time(block.starts_at).is_some()
                && crate::model::local_time(block.ends_at).is_some(),
            "The requested time is outside the supported range."
        );
        if let Some(id) = block.id {
            ensure!(
                existing.contains(&id) && kept.insert(id),
                "This planned block changed. Reopen the task to refresh."
            );
            connection.execute("UPDATE time_blocks SET starts_at = ?1, ends_at = ?2 WHERE id = ?3 AND task_id = ?4", params![block.starts_at, block.ends_at, id, task_id])?;
        } else {
            connection.execute(
                "INSERT INTO time_blocks (task_id, starts_at, ends_at) VALUES (?1, ?2, ?3)",
                params![task_id, block.starts_at, block.ends_at],
            )?;
        }
    }
    for id in existing.into_iter().filter(|id| !kept.contains(id)) {
        connection.execute(
            "DELETE FROM time_blocks WHERE id = ?1 AND task_id = ?2",
            params![id, task_id],
        )?;
    }
    Ok(())
}
