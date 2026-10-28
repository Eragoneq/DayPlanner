//! Manual, tag-scoped Todoist sync. SQLite owns the durable baseline and request
//! UUIDs; HTTP and credential access run on a separate, short-lived worker.
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{db::SyncStore, model::PlannerData, timer::RunningTimer};

const ENDPOINT: &str = "https://api.todoist.com/api/v1/sync";
const MAX_COMMANDS: usize = 80;

#[derive(Clone)]
pub struct Settings {
    pub account_id: Option<String>,
    pub tag: String,
    pub last_synced_at: Option<i64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            account_id: None,
            tag: "dayplanner".into(),
            last_synced_at: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Ask,
    Local,
    Remote,
}

pub struct Request {
    pub token: String,
    pub tag: String,
    pub resolution: Resolution,
    pub demo: bool,
}

pub struct Outcome {
    pub data: Option<PlannerData>,
    pub message: String,
    pub failed: bool,
    pub conflicts: bool,
}

pub enum Job {
    State,
    Connect {
        account: String,
        tag: String,
    },
    Plan {
        items: Vec<RemoteTask>,
        resolution: Resolution,
    },
    Acknowledge(SyncResponse),
    Discard(Vec<String>),
    Finish,
    Snapshot,
}

pub enum Reply {
    State(State),
    Plan(Plan),
    Done,
    Snapshot(PlannerData),
}

pub struct State {
    settings: Settings,
    pending: Vec<Operation>,
    linked_ids: Vec<String>,
    baselines: HashMap<String, Fields>,
    history_since: i64,
    full_history: bool,
}

#[derive(Default)]
pub struct Plan {
    operations: Vec<Operation>,
    imported: usize,
    updated: usize,
    detached: usize,
    skipped: usize,
    conflicts: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Fields {
    title: String,
    notes: String,
    completed: bool,
}

#[derive(Clone, Deserialize)]
pub struct RemoteTask {
    id: String,
    #[serde(rename = "content", default)]
    title: String,
    #[serde(rename = "description", default)]
    notes: String,
    #[serde(default, deserialize_with = "flag")]
    checked: bool,
    #[serde(default, deserialize_with = "flag")]
    is_deleted: bool,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    due: Option<Value>,
}

fn flag<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<bool, D::Error> {
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Bool(value) => Ok(value),
        Value::Number(value) if value == 0.into() => Ok(false),
        Value::Number(value) if value == 1.into() => Ok(true),
        _ => Err(serde::de::Error::custom("Expected a boolean or 0/1")),
    }
}

impl RemoteTask {
    fn fields(&self) -> Fields {
        Fields {
            title: self.title.clone(),
            notes: self.notes.clone(),
            completed: self.checked,
        }
    }
    fn selected(&self, tag: &str) -> bool {
        self.labels.iter().any(|label| label.to_lowercase() == tag)
    }
    fn supported(&self, parents: &HashSet<&str>) -> bool {
        self.parent_id.is_none()
            && !parents.contains(self.id.as_str())
            && !self
                .due
                .as_ref()
                .and_then(|due| due.get("is_recurring"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
    }
}

#[derive(Deserialize)]
struct User {
    id: String,
}

#[derive(Deserialize)]
pub struct SyncResponse {
    #[serde(default)]
    sync_token: Option<String>,
    #[serde(default)]
    user: Option<User>,
    #[serde(default)]
    items: Vec<RemoteTask>,
    #[serde(default)]
    sync_status: HashMap<String, Value>,
    #[serde(default)]
    temp_id_mapping: HashMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Operation {
    // Cleared in the task-deletion transaction; the request UUID/payload stay
    // intact so a previously submitted create can still be acknowledged.
    task_id: Option<i64>,
    remote_id: String,
    command: Value,
    effect: Effect,
    #[serde(default)]
    expected: Option<Fields>,
}

#[derive(Clone, Serialize, Deserialize)]
enum Effect {
    Create(Fields),
    Details {
        title: Option<String>,
        notes: Option<String>,
        enable: bool,
    },
    Completion(bool),
}

struct LocalTask {
    id: i64,
    fields: Fields,
}
struct Link {
    remote_id: String,
    task_id: Option<i64>,
    baseline: Fields,
    enabled: bool,
}

pub fn settings(connection: &Connection) -> Result<Settings> {
    Ok(connection.query_row(
        "SELECT account_id, tag, last_synced_at FROM todoist_settings WHERE id = 1",
        [],
        |row| {
            Ok(Settings {
                account_id: row.get(0)?,
                tag: row.get(1)?,
                last_synced_at: row.get(2)?,
            })
        },
    )?)
}

pub fn state(connection: &Connection) -> Result<State> {
    let settings = settings(connection)?;
    let pending = pending(connection)?;
    let mut query = connection.prepare("SELECT l.remote_id FROM todoist_links l JOIN task_tags tt ON tt.task_id = l.task_id JOIN tags t ON t.id = tt.tag_id WHERE t.name = ?1")?;
    let linked_ids = query
        .query_map([&settings.tag], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let baselines = links(connection)?
        .into_iter()
        .map(|link| (link.remote_id, link.baseline))
        .collect();
    let connected_at: Option<i64> = connection.query_row(
        "SELECT connected_at FROM todoist_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    // Global sync time can advance while a link is disconnected. Reconnection
    // must search that whole gap, even when the saved baseline was incomplete.
    let full_history: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM todoist_links l JOIN tasks t ON t.id = l.task_id JOIN task_tags tt ON tt.task_id = t.id JOIN tags tag ON tag.id = tt.tag_id WHERE tag.name = ?1 AND (l.enabled = 0 OR (json_extract(l.baseline, '$.completed') = 1 AND (t.title != json_extract(l.baseline, '$.title') OR t.notes != json_extract(l.baseline, '$.notes') OR t.completed != 1))))", [&settings.tag], |row| row.get(0))?;
    let history_since = settings
        .last_synced_at
        .filter(|_| !full_history)
        .or(connected_at)
        .unwrap_or_else(|| chrono::Utc::now().timestamp())
        .saturating_sub(86400);
    Ok(State {
        settings,
        pending,
        linked_ids,
        baselines,
        history_since,
        full_history,
    })
}

pub fn connect(connection: &Connection, account: &str, tag: &str) -> Result<()> {
    let current = settings(connection)?;
    ensure!(
        current.account_id.as_deref().is_none_or(|id| id == account),
        "This database is linked to another Todoist account. Its tasks have been left unchanged."
    );
    if current.tag != tag {
        ensure!(
            state(connection)?.pending.is_empty(),
            "Confirm the saved request with tag #{} before changing the selection tag.",
            current.tag
        );
        connection.execute("UPDATE todoist_links SET enabled = 0", [])?;
    }
    connection.execute(
        "UPDATE todoist_settings SET account_id = ?1, tag = ?2, connected_at = COALESCE(connected_at, unixepoch()) WHERE id = 1",
        params![account, tag],
    )?;
    Ok(())
}

fn links(connection: &Connection) -> Result<Vec<Link>> {
    let mut query =
        connection.prepare("SELECT remote_id, task_id, baseline, enabled FROM todoist_links")?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, bool>(3)?,
        ))
    })?;
    rows.map(|row| {
        let (remote_id, task_id, baseline, enabled) = row?;
        Ok(Link {
            remote_id,
            task_id,
            baseline: serde_json::from_str(&baseline)?,
            enabled,
        })
    })
    .collect()
}

fn merge<T: Clone + PartialEq>(
    base: &T,
    local: &T,
    remote: &T,
    resolution: Resolution,
    field: &'static str,
    conflicts: &mut Vec<&'static str>,
) -> T {
    if local == base || local == remote {
        return remote.clone();
    }
    if remote == base {
        return local.clone();
    }
    match resolution {
        Resolution::Local => local.clone(),
        Resolution::Remote => remote.clone(),
        Resolution::Ask => {
            conflicts.push(field);
            local.clone()
        }
    }
}

pub fn plan(
    connection: &Connection,
    items: &[RemoteTask],
    resolution: Resolution,
    active: &mut Option<RunningTimer>,
) -> Result<Plan> {
    let state = state(connection)?;
    ensure!(
        state.pending.is_empty(),
        "An earlier request must be confirmed before planning another sync."
    );
    let tag = state.settings.tag;
    let links = links(connection)?;
    let mut query = connection.prepare("SELECT t.id, t.title, t.notes, t.completed FROM tasks t JOIN task_tags tt ON tt.task_id = t.id JOIN tags tag ON tag.id = tt.tag_id WHERE tag.name = ?1 ORDER BY t.id")?;
    let locals = query
        .query_map([&tag], |row| {
            Ok(LocalTask {
                id: row.get(0)?,
                fields: Fields {
                    title: row.get(1)?,
                    notes: row.get(2)?,
                    completed: row.get(3)?,
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let by_id: HashMap<_, _> = items.iter().map(|item| (item.id.as_str(), item)).collect();
    let parents: HashSet<_> = items
        .iter()
        .filter_map(|item| item.parent_id.as_deref())
        .collect();
    let mut result = Plan::default();
    let mut updates = Vec::new();
    let mut detach = Vec::new();
    let mut new_locals = Vec::new();
    for link in &links {
        let Some(local) = locals.iter().find(|local| Some(local.id) == link.task_id) else {
            if link.enabled {
                detach.push((link, false));
            }
            continue;
        };
        let remote = by_id
            .get(link.remote_id.as_str())
            .context("A linked Todoist task was not returned. Sync again to refresh.")?;
        if remote.is_deleted || (link.enabled && !remote.selected(&tag)) {
            detach.push((link, true));
            continue;
        }
        if !remote.supported(&parents) {
            result.skipped += 1;
            continue;
        }
        let remote_fields = remote.fields();
        let mut fields = Vec::new();
        let merged = Fields {
            title: merge(
                &link.baseline.title,
                &local.fields.title,
                &remote_fields.title,
                resolution,
                "title",
                &mut fields,
            ),
            notes: merge(
                &link.baseline.notes,
                &local.fields.notes,
                &remote_fields.notes,
                resolution,
                "notes",
                &mut fields,
            ),
            completed: merge(
                &link.baseline.completed,
                &local.fields.completed,
                &remote_fields.completed,
                resolution,
                "completion",
                &mut fields,
            ),
        };
        if !fields.is_empty() {
            result
                .conflicts
                .push(format!("{}: {}", local.fields.title, fields.join(", ")));
        }
        updates.push((local, link, *remote, remote_fields, merged));
    }
    if !result.conflicts.is_empty() {
        return Ok(result);
    }

    for (link, remove_tag) in detach {
        connection.execute(
            "UPDATE todoist_links SET enabled = 0 WHERE remote_id = ?1",
            [&link.remote_id],
        )?;
        if remove_tag {
            connection.execute("DELETE FROM task_tags WHERE task_id = ?1 AND tag_id IN (SELECT id FROM tags WHERE name = ?2)", params![link.task_id, tag])?;
        }
        result.detached += 1;
    }
    for (local, link, remote, baseline, merged) in updates {
        if local.fields != merged {
            update_local(connection, local.id, &merged, active)?;
            result.updated += 1;
        }
        connection.execute(
            "UPDATE todoist_links SET baseline = ?1 WHERE remote_id = ?2",
            params![serde_json::to_string(&baseline)?, link.remote_id],
        )?;
        let title = (merged.title != baseline.title).then(|| merged.title.clone());
        let notes = (merged.notes != baseline.notes).then(|| merged.notes.clone());
        let enable = !link.enabled;
        if title.is_some() || notes.is_some() || enable {
            let mut args = json!({"id": link.remote_id});
            if let Some(title) = &title {
                args["content"] = json!(title);
            }
            if let Some(notes) = &notes {
                args["description"] = json!(notes);
            }
            if enable {
                let mut labels = remote.labels.clone();
                if !remote.selected(&tag) {
                    labels.push(tag.clone());
                }
                args["labels"] = json!(labels);
            }
            let mut operation = operation(
                local.id,
                &link.remote_id,
                "item_update",
                args,
                Effect::Details {
                    title,
                    notes,
                    enable,
                },
            );
            operation.expected = Some(baseline.clone());
            result.operations.push(operation);
        }
        if merged.completed != baseline.completed {
            let mut operation = completion(local.id, &link.remote_id, merged.completed);
            operation.expected = Some(baseline);
            result.operations.push(operation);
        }
    }
    for local in &locals {
        if !links.iter().any(|link| link.task_id == Some(local.id)) {
            new_locals.push(local);
        }
    }
    for remote in items {
        if remote.is_deleted
            || remote.checked
            || !remote.selected(&tag)
            || links.iter().any(|link| link.remote_id == remote.id)
        {
            continue;
        }
        if !remote.supported(&parents) {
            result.skipped += 1;
            continue;
        }
        ensure!(
            !remote.title.trim().is_empty(),
            "A tagged Todoist task has no title."
        );
        connection.execute(
            "INSERT INTO tasks (title, notes, completed, color) VALUES (?1, ?2, ?3, 'lavender')",
            params![remote.title, remote.notes, remote.checked],
        )?;
        let id = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO tags (name) VALUES (?1) ON CONFLICT(name) DO NOTHING",
            [&tag],
        )?;
        connection.execute(
            "INSERT INTO task_tags (task_id, tag_id) SELECT ?1, id FROM tags WHERE name = ?2",
            params![id, tag],
        )?;
        connection.execute(
            "INSERT INTO todoist_links (remote_id, task_id, baseline) VALUES (?1, ?2, ?3)",
            params![remote.id, id, serde_json::to_string(&remote.fields())?],
        )?;
        result.imported += 1;
    }
    for local in new_locals {
        let temp_id = Uuid::new_v4().to_string();
        let mut fields = local.fields.clone();
        fields.completed = false;
        let mut create = operation(
            local.id,
            &temp_id,
            "item_add",
            json!({"content": fields.title, "description": fields.notes, "labels": [&tag]}),
            Effect::Create(fields),
        );
        create.command["temp_id"] = json!(temp_id);
        result.operations.push(create);
        if local.fields.completed {
            result.operations.push(completion(local.id, &temp_id, true));
        }
    }
    // Keep create/complete pairs together and leave excess work for Sync now.
    if result.operations.len() > MAX_COMMANDS {
        let mut end = MAX_COMMANDS;
        if matches!(result.operations[end - 1].effect, Effect::Create(_))
            && matches!(result.operations[end].effect, Effect::Completion(_))
        {
            end -= 1;
        }
        result.operations.truncate(end);
    }
    save_pending(connection, &result.operations)?;
    Ok(result)
}

fn update_local(
    connection: &Connection,
    id: i64,
    fields: &Fields,
    active: &mut Option<RunningTimer>,
) -> Result<()> {
    ensure!(
        !fields.title.trim().is_empty(),
        "A tagged Todoist task has no title."
    );
    if fields.completed
        && let Some(timer) = active.filter(|timer| timer.task_id == id)
    {
        crate::db::save_session(connection, timer, true)?;
        *active = None;
    }
    connection.execute(
        "UPDATE tasks SET title = ?1, notes = ?2, completed = ?3 WHERE id = ?4",
        params![fields.title, fields.notes, fields.completed, id],
    )?;
    Ok(())
}

fn operation(task_id: i64, remote_id: &str, kind: &str, args: Value, effect: Effect) -> Operation {
    Operation {
        task_id: Some(task_id),
        remote_id: remote_id.into(),
        command: json!({"type": kind, "uuid": Uuid::new_v4().to_string(), "args": args}),
        effect,
        expected: None,
    }
}

fn completion(task_id: i64, remote_id: &str, completed: bool) -> Operation {
    operation(
        task_id,
        remote_id,
        if completed {
            "item_complete"
        } else {
            "item_uncomplete"
        },
        json!({"id": remote_id}),
        Effect::Completion(completed),
    )
}

fn pending(connection: &Connection) -> Result<Vec<Operation>> {
    let value: Option<String> = connection.query_row(
        "SELECT pending_batch FROM todoist_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    Ok(value
        .map(|value| serde_json::from_str(&value))
        .transpose()?
        .unwrap_or_default())
}

fn save_pending(connection: &Connection, operations: &[Operation]) -> Result<()> {
    let value = (!operations.is_empty())
        .then(|| serde_json::to_string(operations))
        .transpose()?;
    connection.execute(
        "UPDATE todoist_settings SET pending_batch = ?1 WHERE id = 1",
        [value],
    )?;
    Ok(())
}

pub fn discard(connection: &Connection, uuids: &[String]) -> Result<()> {
    let mut pending = pending(connection)?;
    pending.retain(|operation| !uuids.iter().any(|uuid| operation.command["uuid"] == *uuid));
    save_pending(connection, &pending)
}

/// Called before deleting the task, in the same transaction. Do not let a
/// reused SQLite integer ID attach an old remote create to a different task.
pub fn detach_deleted_task(connection: &Connection, task_id: i64) -> Result<()> {
    let mut pending = pending(connection)?;
    let mut changed = false;
    for operation in &mut pending {
        if operation.task_id == Some(task_id) {
            operation.task_id = None;
            changed = true;
        }
    }
    if changed {
        save_pending(connection, &pending)?;
    }
    Ok(())
}

// Do not overwrite edits made while an uncertain request was offline.
// Creates retain their original UUID so replay can recover the assigned ID.
fn pending_conflict(operation: &Operation, remote: &RemoteTask) -> bool {
    let Some(base) = &operation.expected else {
        return false;
    };
    match &operation.effect {
        Effect::Details { title, notes, .. } => {
            title
                .as_ref()
                .is_some_and(|title| remote.title != base.title && remote.title != *title)
                || notes
                    .as_ref()
                    .is_some_and(|notes| remote.notes != base.notes && remote.notes != *notes)
        }
        Effect::Completion(completed) => {
            remote.checked != base.completed && remote.checked != *completed
        }
        Effect::Create(_) => false,
    }
}

pub fn acknowledge(connection: &Connection, response: &SyncResponse) -> Result<Option<String>> {
    let pending = pending(connection)?;
    // An incomplete reply is uncertain: keep every original UUID for replay.
    for operation in &pending {
        let uuid = operation.command["uuid"]
            .as_str()
            .context("Invalid saved request UUID")?;
        ensure!(
            response.sync_status.contains_key(uuid),
            "Todoist did not confirm every command. Sync again to retry the saved request."
        );
        if matches!(operation.effect, Effect::Create(_)) && response.sync_status[uuid] == "ok" {
            ensure!(
                response.temp_id_mapping.contains_key(&operation.remote_id),
                "Todoist did not return the new task ID. The request is saved for retry."
            );
        }
    }
    let mut failures = 0;
    for operation in &pending {
        let uuid = operation.command["uuid"].as_str().unwrap();
        if response.sync_status[uuid] != "ok" {
            failures += 1;
            continue;
        }
        let remote_id = response
            .temp_id_mapping
            .get(&operation.remote_id)
            .unwrap_or(&operation.remote_id);
        match &operation.effect {
            Effect::Create(fields) => {
                // Also recover batches saved by an older build where the task
                // was deleted without clearing its reference in pending JSON.
                let task_id: Option<i64> = connection
                    .query_row(
                        "SELECT id FROM tasks WHERE id = ?1",
                        [operation.task_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let enabled: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM task_tags tt JOIN tags t ON t.id = tt.tag_id JOIN todoist_settings s ON s.id = 1 AND s.tag = t.name WHERE tt.task_id = ?1)",
                    [task_id], |row| row.get(0),
                )?;
                connection.execute(
                    "INSERT INTO todoist_links (remote_id, task_id, baseline, enabled) VALUES (?1, ?2, ?3, ?4)",
                    params![remote_id, task_id, serde_json::to_string(fields)?, enabled],
                )?;
            }
            effect => {
                let baseline: Option<String> = connection
                    .query_row(
                        "SELECT baseline FROM todoist_links WHERE remote_id = ?1",
                        [remote_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(baseline) = baseline else {
                    continue;
                };
                let mut baseline: Fields = serde_json::from_str(&baseline)?;
                match effect {
                    Effect::Details {
                        title,
                        notes,
                        enable,
                    } => {
                        if let Some(value) = title {
                            baseline.title = value.clone();
                        }
                        if let Some(value) = notes {
                            baseline.notes = value.clone();
                        }
                        if *enable {
                            connection.execute(
                                "UPDATE todoist_links SET enabled = 1 WHERE remote_id = ?1",
                                [remote_id],
                            )?;
                        }
                    }
                    Effect::Completion(completed) => baseline.completed = *completed,
                    Effect::Create(_) => unreachable!(),
                }
                connection.execute(
                    "UPDATE todoist_links SET baseline = ?1 WHERE remote_id = ?2",
                    params![serde_json::to_string(&baseline)?, remote_id],
                )?;
            }
        }
    }
    save_pending(connection, &[])?;
    Ok((failures > 0).then(|| format!("Todoist rejected {failures} change(s). Confirmed changes were saved; remaining local edits are retained. Check task limits/permissions, then Sync now again.")))
}

fn request(client: &Client, token: &str, operations: &[Operation]) -> Result<SyncResponse> {
    let mut response = request_once(client, token, operations, "*")?;
    // Full syncs can be generated from an older account snapshot. Apply the
    // following incremental response before comparing or writing task fields.
    if operations.is_empty()
        && let Some(cursor) = response.sync_token.clone()
    {
        let delta = request_once(client, token, &[], &cursor)?;
        if let Some(user) = delta.user {
            ensure!(
                response
                    .user
                    .as_ref()
                    .is_some_and(|previous| previous.id == user.id),
                "Todoist account changed during refresh."
            );
        }
        for item in delta.items {
            if let Some(previous) = response
                .items
                .iter_mut()
                .find(|previous| previous.id == item.id)
            {
                *previous = item;
            } else {
                response.items.push(item);
            }
        }
    }
    Ok(response)
}

fn request_once(
    client: &Client,
    token: &str,
    operations: &[Operation],
    sync_token: &str,
) -> Result<SyncResponse> {
    let commands: Vec<_> = operations
        .iter()
        .map(|operation| &operation.command)
        .collect();
    let response = client
        .post(ENDPOINT)
        .bearer_auth(token)
        .form(&[
            ("sync_token", sync_token.to_string()),
            ("resource_types", "[\"items\",\"user\"]".to_string()),
            ("commands", serde_json::to_string(&commands)?),
        ])
        .send()
        .context("Could not reach Todoist. Local changes are saved; use Sync now to retry.")?;
    let status = response.status();
    ensure!(
        status.is_success(),
        "Todoist returned HTTP {}. {}",
        status.as_u16(),
        match status.as_u16() {
            401 | 403 => "Check your API token and account permissions.",
            429 => "Request limit reached; wait before trying again.",
            _ => "Local edits and any uncertain request are retained for retry.",
        }
    );
    let mut body = Vec::new();
    response
        .take(20 * 1024 * 1024 + 1)
        .read_to_end(&mut body)
        .context("Todoist's response was interrupted. Sync again to retry.")?;
    ensure!(
        body.len() <= 20 * 1024 * 1024,
        "Todoist's response is too large."
    );
    serde_json::from_slice(&body).context("Could not read Todoist's response. Local data is unchanged; uncertain requests remain saved.")
}

fn get_missing(client: &Client, token: &str, id: &str) -> Result<Option<RemoteTask>> {
    let mut url = reqwest::Url::parse("https://api.todoist.com/api/v1/tasks/")?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Invalid Todoist URL"))?
        .pop_if_empty()
        .push(id);
    let response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .context("Could not refresh a linked Todoist task")?;
    if matches!(response.status().as_u16(), 404 | 410) {
        // Completed tasks also return 404 here; check completion history.
        return Ok(None);
    }
    ensure!(
        response.status().is_success(),
        "Could not refresh a linked Todoist task (HTTP {}).",
        response.status().as_u16()
    );
    let task: RemoteTask = response
        .json()
        .context("Could not read a linked Todoist task")?;
    ensure!(task.id == id, "Todoist returned an unexpected task ID.");
    Ok(Some(task))
}

#[derive(Deserialize)]
struct CompletedPage {
    items: Vec<RemoteTask>,
    next_cursor: Option<String>,
}

fn refresh_missing(
    client: &Client,
    token: &str,
    response: &mut SyncResponse,
    state: &State,
    deadline: Instant,
) -> Result<()> {
    let mut missing = HashSet::new();
    for id in &state.linked_ids {
        ensure!(
            Instant::now() < deadline,
            "Sync took too long. Local edits and pending requests are retained; try again."
        );
        if response.items.iter().any(|item| item.id == *id) {
            continue;
        }
        match get_missing(client, token, id)? {
            Some(task) => response.items.push(task),
            None => {
                missing.insert(id.clone());
            }
        }
    }
    // The active-task endpoint omits completed tasks. Completion history is
    // paginated and permits up to three months per request; use 80-day windows.
    let now = chrono::Utc::now().timestamp() + 60;
    let mut since = state.history_since.min(now);
    while !missing.is_empty() && since < now {
        let until = (since + 80 * 86400).min(now);
        let format = |value| {
            chrono::DateTime::from_timestamp(value, 0)
                .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                .context("Invalid completion history date")
        };
        let since_text = format(since)?;
        let until_text = format(until)?;
        let mut cursor: Option<String> = None;
        loop {
            ensure!(
                Instant::now() < deadline,
                "Sync took too long while checking completed tasks. Local edits and pending requests are retained; try again."
            );
            let mut query = vec![
                ("since", since_text.clone()),
                ("until", until_text.clone()),
                ("limit", "200".into()),
            ];
            if let Some(value) = &cursor {
                query.push(("cursor", value.clone()));
            }
            let result = client
                .get("https://api.todoist.com/api/v1/tasks/completed/by_completion_date")
                .bearer_auth(token)
                .query(&query)
                .send()
                .context("Could not check Todoist completion history")?;
            ensure!(
                result.status().is_success(),
                "Could not check Todoist completion history (HTTP {}). Local tasks were retained.",
                result.status().as_u16()
            );
            let page: CompletedPage = result
                .json()
                .context("Could not read Todoist completion history")?;
            for mut task in page.items {
                if missing.remove(&task.id) {
                    task.checked = true;
                    response.items.push(task);
                }
            }
            if page.next_cursor.is_none() || missing.is_empty() {
                break;
            }
            ensure!(
                page.next_cursor != cursor,
                "Todoist repeated a completion history page."
            );
            cursor = page.next_cursor;
        }
        since = until;
    }
    for id in missing {
        let base = state
            .baselines
            .get(&id)
            .context("A linked task has no sync baseline")?;
        // Do not resurrect or delete known completed tasks when their completion
        // falls outside the current history window. Reopening is explicit.
        response.items.push(RemoteTask {
            id,
            title: base.title.clone(),
            notes: base.notes.clone(),
            checked: base.completed,
            is_deleted: !base.completed || state.full_history,
            labels: vec![state.settings.tag.clone()],
            parent_id: None,
            due: None,
        });
    }
    Ok(())
}

pub fn start(store: SyncStore, request: Request) -> async_channel::Receiver<Outcome> {
    let (tx, rx) = async_channel::bounded(1);
    let error_tx = tx.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("dayplanner-todoist".into())
        .spawn(move || {
            let result = run(&store, request);
            let data = match store.request(Job::Snapshot) {
                Ok(Reply::Snapshot(data)) => Some(data),
                _ => None,
            };
            let outcome = match result {
                Ok((message, conflicts)) => Outcome {
                    data,
                    message,
                    failed: conflicts,
                    conflicts,
                },
                Err(error) => Outcome {
                    data,
                    message: format!("{error:#}"),
                    failed: true,
                    conflicts: false,
                },
            };
            let _ = tx.send_blocking(outcome);
        })
    {
        let _ = error_tx.try_send(Outcome {
            data: None,
            message: format!("Could not start sync: {error}"),
            failed: true,
            conflicts: false,
        });
    }
    rx
}

fn read_state(store: &SyncStore) -> Result<State> {
    match store.request(Job::State)? {
        Reply::State(state) => Ok(state),
        _ => bail!("Unexpected sync state"),
    }
}

fn run(store: &SyncStore, options: Request) -> Result<(String, bool)> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let tag = options
        .tag
        .trim()
        .trim_start_matches('#')
        .trim()
        .to_lowercase();
    ensure!(
        !tag.is_empty() && !tag.contains([',', ' ', '\t', '\n', '\r']),
        "Use one tag without commas or spaces, for example dayplanner."
    );
    let entry = keyring::Entry::new(
        "dev.dayplanner.todoist",
        if options.demo { "demo" } else { "normal" },
    )
    .context("Could not access the OS credential store")?;
    let supplied = !options.token.trim().is_empty();
    let token = if supplied {
        options.token.trim().to_string()
    } else {
        entry.get_password().context("No saved Todoist token is available. Paste your API token; on Fedora, unlock your Secret Service keyring.")?
    };
    let client = Client::builder()
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("DayPlanner/0.1")
        .build()?;
    // Validate the account before replaying any persisted write commands.
    let mut response = request(&client, &token, &[])?;
    let account = response
        .user
        .as_ref()
        .context("Todoist did not identify the account")?
        .id
        .clone();
    store.request(Job::Connect {
        account: account.clone(),
        tag,
    })?;
    if supplied {
        entry.set_password(&token).context("The token is valid, but could not be saved to your OS credential store. No task changes were sent.")?;
    }
    let mut state = read_state(store)?;
    refresh_missing(&client, &token, &mut response, &state, deadline)?;
    if !state.pending.is_empty() {
        let mut discard = Vec::new();
        let mut conflicts = Vec::new();
        let parents: HashSet<_> = response
            .items
            .iter()
            .filter_map(|item| item.parent_id.as_deref())
            .collect();
        for operation in &state.pending {
            if operation.expected.is_none() {
                continue;
            }
            let remote = response
                .items
                .iter()
                .find(|item| item.id == operation.remote_id);
            let uuid = operation.command["uuid"]
                .as_str()
                .context("Invalid saved request UUID")?
                .to_string();
            if !state.linked_ids.contains(&operation.remote_id)
                || remote.is_some_and(|item| {
                    item.is_deleted
                        || !item.supported(&parents)
                        || (!item.selected(&state.settings.tag)
                            && !matches!(operation.effect, Effect::Details { enable: true, .. }))
                })
            {
                discard.push(uuid);
            } else if let Some(remote) = remote
                && pending_conflict(operation, remote)
            {
                conflicts.push(remote.title.clone());
                if options.resolution == Resolution::Remote {
                    discard.push(uuid);
                }
            }
        }
        if !conflicts.is_empty() && options.resolution == Resolution::Ask {
            return Ok((
                format!(
                    "Conflicting edits since the interrupted request: {}. Choose which edits to keep.",
                    conflicts
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                true,
            ));
        }
        if !discard.is_empty() {
            store.request(Job::Discard(discard))?;
            state = read_state(store)?;
        }
    }
    if !state.pending.is_empty() {
        let replay = request(&client, &token, &state.pending)?;
        ensure!(
            replay.user.as_ref().is_some_and(|user| user.id == account),
            "Todoist account changed during sync."
        );
        store.request(Job::Acknowledge(replay))?;
        response = request(&client, &token, &[])?;
        state = read_state(store)?;
    }
    ensure!(
        response
            .user
            .as_ref()
            .is_some_and(|user| user.id == account),
        "Todoist account changed during sync."
    );
    refresh_missing(&client, &token, &mut response, &state, deadline)?;
    let plan = match store.request(Job::Plan {
        items: response.items,
        resolution: options.resolution,
    })? {
        Reply::Plan(plan) => plan,
        _ => bail!("Unexpected sync plan"),
    };
    if !plan.conflicts.is_empty() {
        return Ok((
            format!(
                "Conflicting edits: {}{}. Choose which edits to keep, or make the fields match in both apps.",
                plan.conflicts
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; "),
                if plan.conflicts.len() > 3 {
                    format!("; and {} more", plan.conflicts.len() - 3)
                } else {
                    String::new()
                }
            ),
            true,
        ));
    }
    let sent = plan.operations.len();
    if sent > 0 {
        let response = request(&client, &token, &plan.operations)?;
        ensure!(
            response
                .user
                .as_ref()
                .is_some_and(|user| user.id == account),
            "Todoist account changed during sync."
        );
        store.request(Job::Acknowledge(response))?;
    }
    store.request(Job::Finish)?;
    Ok((
        format!(
            "Todoist synced: {} imported, {} updated, {sent} changes sent, {} disconnected.{}{}",
            plan.imported,
            plan.updated,
            plan.detached,
            if plan.skipped > 0 {
                format!(" {} recurring/nested tasks skipped.", plan.skipped)
            } else {
                String::new()
            },
            if sent >= MAX_COMMANDS - 1 {
                " More changes may remain; use Sync now again."
            } else {
                ""
            }
        ),
        false,
    ))
}
