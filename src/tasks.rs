//! Task rows: the stored shape, validation, lookup, listing and task references.

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    MAX_TAG_CHARS, MAX_TASK_TITLE_CHARS, MEANING_SOURCES, TASK_KINDS, TaskPickup, priority_recovery,
};

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    #[serde(skip_serializing)]
    pub task_id: i64,
    pub seq: i64,
    #[serde(skip_serializing)]
    pub plan_id: i64,
    #[serde(skip_serializing)]
    pub parent_id: Option<i64>,
    #[serde(skip_serializing)]
    pub replacement_task_id: Option<i64>,
    pub title: String,
    pub intent: String,
    pub intent_source: Option<String>,
    pub kind: String,
    pub result: Option<String>,
    pub result_source: Option<String>,
    pub status: String,
    pub priority: i64,
    pub pickup: Option<TaskPickup>,
}

pub(crate) fn task_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        task_id: r.get(0)?,
        seq: r.get(1)?,
        plan_id: r.get(2)?,
        parent_id: r.get(3)?,
        replacement_task_id: r.get(4)?,
        title: r.get(5)?,
        intent: r.get(6)?,
        intent_source: r.get(7)?,
        kind: r.get(8)?,
        result: r.get(9)?,
        result_source: r.get(10)?,
        status: r.get(11)?,
        priority: priority_recovery::read_priority(r, 1, 12)?,
        pickup: match (
            r.get::<_, Option<String>>(13)?,
            r.get::<_, Option<String>>(14)?,
        ) {
            (None, None) => None,
            (session, Some(at)) => Some(TaskPickup { session, at }),
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
    })
}

pub(crate) const TASK_COLS: &str = "task_id, seq, plan_id, parent_id, replacement_task_id, title, intent, intent_source, kind, result, result_source, status, priority, pickup_session, pickup_at";

pub(crate) fn validate_task_kind(kind: &str) -> Result<()> {
    if !TASK_KINDS.contains(&kind) {
        bail!(
            "unknown task kind '{kind}' (expected {})",
            TASK_KINDS.join("|")
        );
    }
    Ok(())
}

pub fn validate_meaning_source(source: Option<&str>) -> Result<Option<&str>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let source = source.trim();
    if !MEANING_SOURCES.contains(&source) {
        bail!(
            "unknown meaning source '{source}' (expected {})",
            MEANING_SOURCES.join("|")
        );
    }
    Ok(Some(source))
}

pub(crate) fn meaning_source_requires_text(text: Option<&str>, source: Option<&str>) -> bool {
    source.is_none() || text.is_some_and(|text| !text.trim().is_empty())
}

pub(crate) fn validate_task_title(title: &str) -> Result<&str> {
    let title = require_nonblank("task title", title)?;
    let characters = title.chars().count();
    if characters > MAX_TASK_TITLE_CHARS {
        bail!(
            "task title has {characters} characters; shorten it to at most {MAX_TASK_TITLE_CHARS} characters"
        );
    }
    Ok(title)
}

pub(crate) fn validate_tag(tag: &str) -> Result<&str> {
    let tag = require_nonblank("tag", tag)?;
    let characters = tag.chars().count();
    if characters > MAX_TAG_CHARS {
        bail!("tag has {characters} characters; shorten it to at most {MAX_TAG_CHARS} characters");
    }
    Ok(tag)
}

pub(crate) fn require_nonblank<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} must not be blank");
    }
    Ok(value)
}

pub fn get_task(conn: &Connection, seq: i64) -> Result<Task> {
    conn.query_row(
        &format!("SELECT {TASK_COLS} FROM tasks WHERE seq=?1"),
        params![seq],
        task_from_row,
    )
    .optional()?
    .ok_or_else(|| anyhow!("no task #{seq}"))
}

pub(crate) fn task_tags_by_id(conn: &Connection, task_id: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare("SELECT tag FROM task_tags WHERE task_id=?1 ORDER BY tag")?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Query tasks through the canonical task projection. Optional filters are
/// applied without placeholder renumbering or a caller-owned row mapping.
pub fn list_tasks(
    conn: &Connection,
    plan_id: i64,
    status: Option<&str>,
    tag: Option<&str>,
) -> Result<Vec<Task>> {
    list_tasks_ordered(conn, plan_id, status, tag, false)
}

pub fn list_tasks_by_activity(
    conn: &Connection,
    plan_id: i64,
    status: Option<&str>,
    tag: Option<&str>,
) -> Result<Vec<Task>> {
    list_tasks_ordered(conn, plan_id, status, tag, true)
}

fn list_tasks_ordered(
    conn: &Connection,
    plan_id: i64,
    status: Option<&str>,
    tag: Option<&str>,
    by_activity: bool,
) -> Result<Vec<Task>> {
    let order = if by_activity {
        "ORDER BY COALESCE((SELECT MAX(event_id) FROM canonical_events
                            WHERE entity_seq=tasks.seq
                              AND entity IN ('task','dep','gate')), 0) DESC, seq"
    } else {
        "ORDER BY seq"
    };
    let (sql, parameters): (String, Vec<rusqlite::types::Value>) = match (status, tag) {
        (Some(status), Some(tag)) => (
            format!(
                "SELECT {TASK_COLS} FROM tasks WHERE plan_id=?1 AND status=?2
                 AND task_id IN (SELECT task_id FROM task_tags WHERE tag=?3) {order}"
            ),
            vec![
                plan_id.into(),
                status.to_owned().into(),
                tag.to_owned().into(),
            ],
        ),
        (Some(status), None) => (
            format!("SELECT {TASK_COLS} FROM tasks WHERE plan_id=?1 AND status=?2 {order}"),
            vec![plan_id.into(), status.to_owned().into()],
        ),
        (None, Some(tag)) => (
            format!(
                "SELECT {TASK_COLS} FROM tasks WHERE plan_id=?1
                 AND task_id IN (SELECT task_id FROM task_tags WHERE tag=?2) {order}"
            ),
            vec![plan_id.into(), tag.to_owned().into()],
        ),
        (None, None) => (
            format!("SELECT {TASK_COLS} FROM tasks WHERE plan_id=?1 {order}"),
            vec![plan_id.into()],
        ),
    };
    let mut statement = conn.prepare(&sql)?;
    Ok(statement
        .query_map(rusqlite::params_from_iter(parameters), task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Non-container tasks in one durable status, used by the compact status view.
pub fn leaf_tasks_with_status(conn: &Connection, plan_id: i64, status: &str) -> Result<Vec<Task>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks task WHERE plan_id=?1 AND status=?2
         AND NOT EXISTS (SELECT 1 FROM tasks child WHERE child.parent_id=task.task_id)
         ORDER BY seq"
    ))?;
    Ok(statement
        .query_map(params![plan_id, status], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn parse_task_ref(task_ref: &str) -> Result<i64> {
    let digits = task_ref.strip_prefix('#').unwrap_or(task_ref);
    let invalid = || {
        anyhow!(
            "bad task reference '{task_ref}' (expected task.seq as N or #N, with N a canonical positive ASCII decimal)"
        )
    };
    if digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    digits.parse::<i64>().map_err(|_| invalid())
}
