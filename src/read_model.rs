//! Stable, mutation-free projections for history-aware planner reads.

use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::{
    Plan, SCHEMA_VERSION, TASK_STATUSES, Task, get_plan, get_task, leaf_tasks_with_status,
    list_tasks, list_tasks_by_activity, portable_absolute, ready_tasks, resolve_plan,
    task_tags_by_id, validate_sha256,
};

const EVENT_CURSOR_PREFIX: &str = "event-v1";
const MAX_EVENT_PAGE: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct EventCursor {
    pub event_id: i64,
    pub token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventRecord {
    pub event_id: i64,
    pub at: String,
    pub actor: String,
    pub entity: String,
    pub plan: Option<String>,
    pub task_seq: Option<i64>,
    pub gate: Option<String>,
    pub kind: String,
    pub why: Option<String>,
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventLog {
    pub schema: String,
    pub task_seq: Option<i64>,
    pub direction: String,
    pub head: Option<EventCursor>,
    pub floor_event_id: Option<i64>,
    pub events: Vec<EventRecord>,
    pub truncated: bool,
    pub continuation: Option<EventCursor>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ActivityEvent {
    pub event_id: i64,
    pub at: String,
    pub actor: String,
    pub entity: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TaskActivity {
    pub created_event: Option<ActivityEvent>,
    pub last_event: Option<ActivityEvent>,
    pub status_event: Option<ActivityEvent>,
    pub started_event: Option<ActivityEvent>,
    pub completed_event: Option<ActivityEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskSummary {
    pub seq: i64,
    pub title: String,
    pub status: String,
    pub kind: String,
    pub priority: i64,
}

impl From<&Task> for TaskSummary {
    fn from(task: &Task) -> Self {
        Self {
            seq: task.seq,
            title: task.title.clone(),
            status: task.status.clone(),
            kind: task.kind.clone(),
            priority: task.priority,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthorityInfo {
    pub papertiger_version: String,
    pub requested_path: String,
    pub resolved_path: String,
    pub schema_version: i64,
    pub event_head: Option<EventCursor>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TaskCounts {
    pub proposed: i64,
    pub in_progress: i64,
    pub done: i64,
    pub retired: i64,
    pub rejected: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusTask {
    pub task: TaskSummary,
    pub activity: TaskActivity,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusReadyTask {
    pub task: TaskSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanStatus {
    pub plan: Plan,
    pub counts: TaskCounts,
    pub in_progress: Vec<StatusTask>,
    pub ready: Vec<StatusReadyTask>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    pub schema: String,
    pub authority: AuthorityInfo,
    pub active_plans: Vec<PlanStatus>,
    pub recent_notes: Vec<EventRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskListItem {
    pub task: TaskSummary,
    pub tags: Vec<String>,
    pub last_event: Option<ActivityEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskListResponse {
    pub schema: String,
    pub plan: Plan,
    pub status: Option<String>,
    pub tag: Option<String>,
    pub sort: String,
    pub tasks: Vec<TaskListItem>,
}

#[derive(Debug)]
struct StoredEvent {
    event_id: i64,
    at: String,
    actor: String,
    entity: String,
    entity_id: Option<i64>,
    entity_plan: Option<String>,
    entity_seq: Option<i64>,
    gate_name: Option<String>,
    kind: String,
    why: Option<String>,
    payload: Option<String>,
}

impl StoredEvent {
    fn public(self) -> Result<EventRecord> {
        Ok(EventRecord {
            event_id: self.event_id,
            at: self.at,
            actor: self.actor,
            entity: self.entity,
            plan: self.entity_plan,
            task_seq: self.entity_seq,
            gate: self.gate_name,
            kind: self.kind,
            why: self.why,
            payload: self
                .payload
                .map(|raw| serde_json::from_str(&raw).context("invalid stored event payload"))
                .transpose()?,
        })
    }

    fn activity(&self) -> ActivityEvent {
        ActivityEvent {
            event_id: self.event_id,
            at: self.at.clone(),
            actor: self.actor.clone(),
            entity: self.entity.clone(),
            kind: self.kind.clone(),
        }
    }
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvent> {
    Ok(StoredEvent {
        event_id: row.get(0)?,
        at: row.get(1)?,
        actor: row.get(2)?,
        entity: row.get(3)?,
        entity_id: row.get(4)?,
        entity_plan: row.get(5)?,
        entity_seq: row.get(6)?,
        gate_name: row.get(7)?,
        kind: row.get(8)?,
        why: row.get(9)?,
        payload: row.get(10)?,
    })
}

fn hash_event(hasher: &mut Sha256, event: &StoredEvent) -> Result<()> {
    let bytes = serde_json::to_vec(&(
        event.event_id,
        &event.at,
        &event.actor,
        &event.entity,
        event.entity_id,
        &event.entity_plan,
        event.entity_seq,
        &event.gate_name,
        &event.kind,
        &event.why,
        &event.payload,
    ))?;
    hasher.update(u64::try_from(bytes.len())?.to_le_bytes());
    hasher.update(bytes);
    Ok(())
}

pub fn event_cursor(conn: &Connection, event_id: i64) -> Result<EventCursor> {
    if event_id <= 0 {
        bail!("event cursor requires a positive event ID");
    }
    let mut statement = conn.prepare(
        "SELECT event_id, at, actor, entity, entity_id, entity_plan, entity_seq,
                gate_name, kind, why, payload
           FROM events
          WHERE event_id<=?1
          ORDER BY event_id",
    )?;
    let rows = statement.query_map(params![event_id], event_from_row)?;
    let mut hasher = Sha256::new();
    let mut last = None;
    for row in rows {
        let event = row?;
        last = Some(event.event_id);
        hash_event(&mut hasher, &event)?;
    }
    if last != Some(event_id) {
        bail!(
            "event {event_id} is absent from this authority; discard the cursor and rerun `papertiger log --json` on the intended authority"
        );
    }
    let digest = format!("{:x}", hasher.finalize());
    Ok(EventCursor {
        event_id,
        token: format!("{EVENT_CURSOR_PREFIX}:{event_id}:{digest}"),
    })
}

pub fn event_head(conn: &Connection) -> Result<Option<EventCursor>> {
    let event_id = conn.query_row("SELECT MAX(event_id) FROM events", [], |row| {
        row.get::<_, Option<i64>>(0)
    })?;
    event_id
        .map(|event_id| event_cursor(conn, event_id))
        .transpose()
}

fn validate_event_cursor(conn: &Connection, token: &str) -> Result<EventCursor> {
    let mut parts = token.split(':');
    let prefix = parts.next();
    let event_id = parts.next();
    let digest = parts.next();
    if prefix != Some(EVENT_CURSOR_PREFIX)
        || event_id.is_none()
        || digest.is_none()
        || parts.next().is_some()
    {
        bail!(
            "invalid event cursor; pass the complete event-v1 cursor emitted by `papertiger log --json` or `papertiger status --json`"
        );
    }
    let event_id_text = event_id.unwrap_or_default();
    if event_id_text.starts_with('0')
        || event_id_text.is_empty()
        || !event_id_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!(
            "invalid event cursor; pass the complete event-v1 cursor emitted by `papertiger log --json` or `papertiger status --json`"
        );
    }
    let event_id = event_id_text.parse::<i64>().map_err(|_| {
        anyhow::anyhow!(
            "invalid event cursor; pass the complete event-v1 cursor emitted by `papertiger log --json` or `papertiger status --json`"
        )
    })?;
    let digest = digest.unwrap_or_default();
    validate_sha256(digest, "event cursor digest")?;
    let actual = event_cursor(conn, event_id)?;
    if actual.token != token {
        bail!(
            "event cursor does not belong to this history; discard it and rerun `papertiger log --json` on the intended authority"
        );
    }
    Ok(actual)
}

pub fn event_log(
    conn: &Connection,
    task_seq: Option<i64>,
    limit: usize,
    before_cursor: Option<&str>,
    after_cursor: Option<&str>,
) -> Result<EventLog> {
    if !(1..=MAX_EVENT_PAGE).contains(&limit) {
        bail!("event log --limit must be between 1 and {MAX_EVENT_PAGE}");
    }
    if before_cursor.is_some() && after_cursor.is_some() {
        bail!("event log accepts only one of --before-cursor or --after-cursor");
    }
    if let Some(seq) = task_seq {
        get_task(conn, seq)?;
    }
    let before = before_cursor
        .map(|cursor| validate_event_cursor(conn, cursor))
        .transpose()?;
    let after = after_cursor
        .map(|cursor| validate_event_cursor(conn, cursor))
        .transpose()?;
    let ascending = after.is_some();
    let direction = if before.is_some() {
        "before"
    } else if ascending {
        "after"
    } else {
        "latest"
    };
    let order = if ascending { "ASC" } else { "DESC" };
    let sql = format!(
        "SELECT event_id, at, actor, entity, entity_id, entity_plan, entity_seq,
                gate_name, kind, why, payload
           FROM events
          WHERE (?1 IS NULL OR (entity_seq=?1 AND entity IN ('task','dep','gate')))
            AND (?2 IS NULL OR event_id<?2)
            AND (?3 IS NULL OR event_id>?3)
          ORDER BY event_id {order}
          LIMIT ?4"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(
            params![
                task_seq,
                before.as_ref().map(|cursor| cursor.event_id),
                after.as_ref().map(|cursor| cursor.event_id),
                i64::try_from(limit + 1)?,
            ],
            event_from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let truncated = rows.len() > limit;
    let rows = rows.into_iter().take(limit).collect::<Vec<_>>();
    let last_event_id = rows.last().map(|event| event.event_id);
    let events = rows
        .into_iter()
        .map(StoredEvent::public)
        .collect::<Result<Vec<_>>>()?;
    let head = event_head(conn)?;
    let continuation = if truncated {
        last_event_id
            .map(|event_id| event_cursor(conn, event_id))
            .transpose()?
    } else if ascending {
        head.clone()
    } else {
        None
    };
    let floor_event_id = conn.query_row("SELECT MIN(event_id) FROM events", [], |row| {
        row.get::<_, Option<i64>>(0)
    })?;
    Ok(EventLog {
        schema: "papertiger.event_log.v1".into(),
        task_seq,
        direction: direction.into(),
        head,
        floor_event_id,
        events,
        truncated,
        continuation,
    })
}

pub fn task_activity(conn: &Connection, seq: i64) -> Result<TaskActivity> {
    let task = get_task(conn, seq)?;
    let mut statement = conn.prepare(
        "SELECT event_id, at, actor, entity, entity_id, entity_plan, entity_seq,
                gate_name, kind, why, payload
           FROM events
          WHERE entity_seq=?1 AND entity IN ('task','dep','gate')
          ORDER BY event_id",
    )?;
    let rows = statement
        .query_map(params![seq], event_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut created_event = None;
    let mut last_event = None;
    let mut status_event = None;
    let mut latest_started_event = None;
    let mut latest_completed_event = None;
    for event in rows {
        let activity = event.activity();
        last_event = Some(activity.clone());
        if event.entity == "task" && event.kind == "create" && created_event.is_none() {
            created_event = Some(activity.clone());
        }
        if event.entity == "task" && event.kind == "status" {
            status_event = Some(activity.clone());
            let status = event
                .payload
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|value| {
                    value
                        .get("to")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                });
            match status.as_deref() {
                Some("in_progress") => latest_started_event = Some(activity),
                Some("done") => latest_completed_event = Some(activity),
                _ => {}
            }
        }
    }
    Ok(TaskActivity {
        created_event,
        last_event,
        status_event,
        started_event: (task.status == "in_progress")
            .then_some(latest_started_event)
            .flatten(),
        completed_event: (task.status == "done")
            .then_some(latest_completed_event)
            .flatten(),
    })
}

pub fn authority_info(conn: &Connection, requested_path: &str) -> Result<AuthorityInfo> {
    let resolved = std::fs::canonicalize(Path::new(requested_path))
        .with_context(|| format!("resolve existing Papertiger authority path {requested_path}"))?;
    Ok(AuthorityInfo {
        papertiger_version: env!("CARGO_PKG_VERSION").into(),
        requested_path: requested_path.into(),
        resolved_path: portable_absolute(&resolved)?,
        schema_version: SCHEMA_VERSION,
        event_head: event_head(conn)?,
    })
}

pub fn status_response(conn: &Connection, requested_path: &str) -> Result<StatusResponse> {
    let mut statement =
        conn.prepare("SELECT plan_id FROM plans WHERE status='active' ORDER BY plan_id")?;
    let plan_ids = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    let mut active_plans = Vec::with_capacity(plan_ids.len());
    for plan_id in plan_ids {
        let plan = get_plan(conn, plan_id)?;
        let mut counts = TaskCounts::default();
        let mut count_statement =
            conn.prepare("SELECT status, COUNT(*) FROM tasks WHERE plan_id=?1 GROUP BY status")?;
        let rows = count_statement
            .query_map(params![plan_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (status, count) in rows {
            match status.as_str() {
                "proposed" => counts.proposed = count,
                "in_progress" => counts.in_progress = count,
                "done" => counts.done = count,
                "retired" => counts.retired = count,
                "rejected" => counts.rejected = count,
                _ => bail!(
                    "plan {} contains noncanonical task status {status:?}; run `papertiger audit`",
                    plan.slug
                ),
            }
        }
        let in_progress = leaf_tasks_with_status(conn, plan_id, "in_progress")?
            .into_iter()
            .map(|task| {
                Ok(StatusTask {
                    activity: task_activity(conn, task.seq)?,
                    task: TaskSummary::from(&task),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let ready = ready_tasks(conn, plan_id, 5, false)?
            .into_iter()
            .map(|entry| StatusReadyTask {
                task: TaskSummary::from(&entry.task),
            })
            .collect();
        active_plans.push(PlanStatus {
            plan,
            counts,
            in_progress,
            ready,
        });
    }
    let mut note_statement = conn.prepare(
        "SELECT event_id, at, actor, entity, entity_id, entity_plan, entity_seq,
                gate_name, kind, why, payload
           FROM events
          WHERE kind='note'
          ORDER BY event_id DESC
          LIMIT 3",
    )?;
    let recent_notes = note_statement
        .query_map([], event_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(StoredEvent::public)
        .collect::<Result<Vec<_>>>()?;
    Ok(StatusResponse {
        schema: "papertiger.status.v1".into(),
        authority: authority_info(conn, requested_path)?,
        active_plans,
        recent_notes,
    })
}

pub fn task_list_response(
    conn: &Connection,
    plan: Option<&str>,
    status: Option<&str>,
    tag: Option<&str>,
    sort: &str,
) -> Result<TaskListResponse> {
    if let Some(status) = status
        && !TASK_STATUSES.contains(&status)
    {
        bail!(
            "unknown task status '{status}' (expected {})",
            TASK_STATUSES.join("|")
        );
    }
    let (plan_id, _) = resolve_plan(conn, plan)?;
    let tasks = match sort {
        "seq" => list_tasks(conn, plan_id, status, tag)?,
        "activity" => list_tasks_by_activity(conn, plan_id, status, tag)?,
        _ => bail!("unknown list sort '{sort}' (expected seq|activity)"),
    };
    let tasks = tasks
        .into_iter()
        .map(|task| {
            Ok(TaskListItem {
                tags: task_tags_by_id(conn, task.task_id)?,
                last_event: task_activity(conn, task.seq)?.last_event,
                task: TaskSummary::from(&task),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TaskListResponse {
        schema: "papertiger.task_list.v1".into(),
        plan: get_plan(conn, plan_id)?,
        status: status.map(str::to_owned),
        tag: tag.map(str::to_owned),
        sort: sort.into(),
        tasks,
    })
}
