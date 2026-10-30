//! Full task context reads: related tasks, gates, history and open dependencies.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    CommitAssociation, EventCursor, EventRecord, ExternalReference, Plan, TASK_COLS, Task,
    TaskActivity, TaskMiseProjectionSummary, TaskSummary, commit_associations, event_log,
    external_references, get_plan, get_task, immediate_unlock_count, latest_task_note,
    task_activity, task_from_row, task_handle, task_mise_projection_summaries, task_tags_by_id,
    unfinished_downstream_count,
};

#[derive(Debug, Clone, Serialize)]
pub struct GateRecord {
    pub name: String,
    pub kind: String,
    pub requirement: String,
    pub status: String,
    pub evidence_locator: Option<String>,
    pub evidence_sha256: Option<String>,
    pub note: Option<String>,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskContext {
    pub schema: String,
    pub plan: Plan,
    pub task: Task,
    pub tags: Vec<String>,
    /// Ancestor handles from the root down to the direct parent.
    pub ancestry: Vec<String>,
    /// Review state of the task's filing and, once done, its result.
    pub review: crate::TaskReview,
    /// Whether the pickup's owner process still runs, observed now.
    pub owner_state: Option<crate::OwnerState>,
    pub parent: Option<TaskSummary>,
    pub replacement: Option<TaskSummary>,
    pub dependencies: Vec<TaskSummary>,
    pub dependents: Vec<TaskSummary>,
    pub children: Vec<TaskSummary>,
    pub gates: Vec<GateRecord>,
    pub commit_associations: Vec<CommitAssociation>,
    pub external_references: Vec<ExternalReference>,
    pub activity: TaskActivity,
    pub mise_projections: Vec<TaskMiseProjectionSummary>,
    pub immediate_unlock_count: usize,
    pub unfinished_downstream_count: usize,
    pub recent_events: Vec<EventRecord>,
    pub recent_events_truncated: bool,
    pub older_events_cursor: Option<EventCursor>,
    /// The task's newest note in full, even when it is older than
    /// `recent_events`: usually the last session's handoff.
    pub latest_note: Option<EventRecord>,
}

/// Handles of a task's ancestors from the root down to its direct parent;
/// empty for a root task.
pub fn task_ancestry(conn: &Connection, task: &Task) -> Result<Vec<String>> {
    let mut ancestry = Vec::new();
    let mut seen = std::collections::HashSet::from([task.task_id]);
    let mut next = task.parent_id;
    while let Some(task_id) = next {
        if !seen.insert(task_id) {
            bail!(
                "{} sits in a parent cycle; run `papertiger audit` and restore a verified export",
                task.handle
            );
        }
        let (seq, kind, parent_id): (i64, String, Option<i64>) = conn.query_row(
            "SELECT seq, kind, parent_id FROM tasks WHERE task_id=?1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        ancestry.push(task_handle(seq, &kind));
        next = parent_id;
    }
    ancestry.reverse();
    Ok(ancestry)
}

pub fn task_context(conn: &Connection, seq: i64) -> Result<TaskContext> {
    task_context_with_history(conn, seq, true)
}

/// Full current context with explicitly omitted history, on its own schema.
pub fn task_current(conn: &Connection, seq: i64) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(task_context_with_history(conn, seq, false)?)?;
    let object = value.as_object_mut().expect("task context object");
    object.remove("recent_events");
    object.remove("recent_events_truncated");
    object.remove("older_events_cursor");
    object.remove("latest_note");
    object.insert("schema".into(), "papertiger.task_current.v4".into());
    object.insert(
        "history_command".into(),
        format!("papertiger log --task {seq} --json").into(),
    );
    Ok(value)
}

fn task_context_with_history(
    conn: &Connection,
    seq: i64,
    include_history: bool,
) -> Result<TaskContext> {
    let task = get_task(conn, seq)?;
    let plan = get_plan(conn, task.plan_id)?;
    let parent = task
        .parent_id
        .map(|parent_id| {
            conn.query_row(
                &format!("SELECT {TASK_COLS} FROM tasks WHERE task_id=?1"),
                params![parent_id],
                task_from_row,
            )
            .optional()
            .context("query task parent")
        })
        .transpose()?
        .flatten();
    let replacement = task
        .replacement_task_id
        .map(|replacement_task_id| {
            conn.query_row(
                &format!("SELECT {TASK_COLS} FROM tasks WHERE task_id=?1"),
                params![replacement_task_id],
                task_from_row,
            )
            .optional()
            .context("query task replacement")
        })
        .transpose()?
        .flatten();

    let tags = task_tags_by_id(conn, task.task_id)?;

    let dependencies = query_related_tasks(
        conn,
        &format!(
            "SELECT {TASK_COLS} FROM tasks
              WHERE task_id IN (SELECT depends_on FROM deps WHERE task_id=?1)
              ORDER BY seq"
        ),
        task.task_id,
    )?;
    let dependents = query_related_tasks(
        conn,
        &format!(
            "SELECT {TASK_COLS} FROM tasks
              WHERE task_id IN (SELECT task_id FROM deps WHERE depends_on=?1)
              ORDER BY seq"
        ),
        task.task_id,
    )?;
    let children = query_related_tasks(
        conn,
        &format!("SELECT {TASK_COLS} FROM tasks WHERE parent_id=?1 ORDER BY seq"),
        task.task_id,
    )?;

    let mut statement = conn.prepare(
        "SELECT name, kind, requirement, status, evidence_locator,
                evidence_sha256, note, resolved_at
           FROM gates
          WHERE task_id=?1
          ORDER BY gate_id",
    )?;
    let gates = statement
        .query_map(params![task.task_id], |row| {
            Ok(GateRecord {
                name: row.get(0)?,
                kind: row.get(1)?,
                requirement: row.get(2)?,
                status: row.get(3)?,
                evidence_locator: row.get(4)?,
                evidence_sha256: row.get(5)?,
                note: row.get(6)?,
                resolved_at: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    const RECENT_EVENT_LIMIT: usize = 12;
    let (recent_events, recent_events_truncated, older_events_cursor, latest_note) =
        if include_history {
            let log = event_log(conn, Some(task.seq), RECENT_EVENT_LIMIT, None, None)?;
            (
                log.events,
                log.truncated,
                log.continuation,
                latest_task_note(conn, task.seq)?,
            )
        } else {
            (Vec::new(), false, None, None)
        };

    let ancestry = task_ancestry(conn, &task)?;
    let review = crate::task_review(conn, &task)?;
    let owner_state = task.pickup.as_ref().map(crate::owner_state);
    Ok(TaskContext {
        schema: "papertiger.task_context.v9".into(),
        plan,
        tags,
        ancestry,
        review,
        owner_state,
        parent: parent.as_ref().map(TaskSummary::from),
        replacement: replacement.as_ref().map(TaskSummary::from),
        dependencies: dependencies.iter().map(TaskSummary::from).collect(),
        dependents: dependents.iter().map(TaskSummary::from).collect(),
        children: children.iter().map(TaskSummary::from).collect(),
        gates,
        commit_associations: commit_associations(conn, task.seq)?,
        external_references: external_references(conn, task.seq)?,
        activity: task_activity(conn, task.seq)?,
        mise_projections: task_mise_projection_summaries(conn, task.seq)?,
        immediate_unlock_count: immediate_unlock_count(conn, task.task_id)?,
        unfinished_downstream_count: unfinished_downstream_count(conn, task.task_id)?,
        recent_events,
        recent_events_truncated,
        older_events_cursor,
        latest_note,
        task,
    })
}

fn query_related_tasks(conn: &Connection, sql: &str, task_id: i64) -> Result<Vec<Task>> {
    let mut statement = conn.prepare(sql)?;
    Ok(statement
        .query_map(params![task_id], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Blocking dependencies of a task: every dependency that is not done.
/// Retired and rejected prerequisites remain blockers until the stale edge is
/// explicitly removed as a course correction.
pub fn open_deps(conn: &Connection, task_id: i64) -> Result<Vec<i64>> {
    let mut st = conn.prepare(
        "SELECT t.seq FROM deps d JOIN tasks t ON t.task_id = d.depends_on
         WHERE d.task_id=?1 AND t.status <> 'done' ORDER BY t.seq",
    )?;
    Ok(st
        .query_map(params![task_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
