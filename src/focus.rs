//! Work selection: focus ranking and the ready queue.

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{
    PlanIdentity, StatusProjection, TASK_COLS, Task, TaskPickup, TaskSummary, get_plan, open_deps,
    open_gates, open_task_blocker_names, pickup, task_from_row, validate_session,
};

pub(crate) fn entry_blockers(conn: &Connection, task: &Task) -> Result<Vec<String>> {
    let mut blockers = open_deps(conn, task.task_id)?
        .into_iter()
        .map(|dependency| format!("dep:#{dependency}"))
        .collect::<Vec<_>>();
    blockers.extend(
        open_task_blocker_names(conn, task.task_id)?
            .into_iter()
            .map(|name| format!("blocker:{name}")),
    );
    Ok(blockers)
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadyEntry {
    pub task: Task,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FocusEntry {
    pub task: TaskSummary,
    pub pickup: Option<TaskPickup>,
    pub readiness: String,
    pub blockers: Vec<String>,
    pub open_gate_count: usize,
    pub immediate_unlock_count: usize,
    pub unfinished_downstream_count: usize,
    /// Children of a parent whose children are all closed; the remaining
    /// work is closing the parent itself. Zero for leaf tasks.
    pub closed_child_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FocusResponse {
    pub schema: String,
    pub selection_state: String,
    pub plan: Option<PlanIdentity>,
    #[serde(flatten)]
    pub projection: StatusProjection<FocusEntry>,
}

impl FocusResponse {
    pub fn no_active_plan() -> Self {
        Self {
            schema: "papertiger.focus.v8".into(),
            selection_state: "no_active_plan".into(),
            plan: None,
            projection: StatusProjection::new(
                "actionable leaf tasks in the selected active plan",
                "readiness, priority descending, immediate unlocks descending, unfinished downstream descending, task.seq ascending",
                0,
                Vec::new(),
                None,
            ),
        }
    }
}

/// Actionable leaf tasks ordered by active work, explicit priority, and the
/// amount of unfinished dependency graph they can unblock. Proposed blocked
/// work is opt-in; active work that became blocked is always surfaced.
pub fn focus(
    conn: &Connection,
    plan_id: i64,
    limit: usize,
    include_blocked: bool,
    kind: Option<&str>,
    session: Option<&str>,
) -> Result<FocusResponse> {
    crate::validate_kind_filter(kind)?;
    if let Some(session) = session {
        validate_session(session)?;
    }
    if limit == 0 {
        bail!("focus --limit must be at least 1");
    }
    let plan = get_plan(conn, plan_id)?;
    let mut statement = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks task
          WHERE task.plan_id=?1
            AND task.status IN ('proposed','in_progress')
            AND NOT EXISTS (
                SELECT 1 FROM tasks child
                 WHERE child.parent_id=task.task_id
                   AND child.status IN ('proposed','in_progress')
            )"
    ))?;
    let tasks = statement
        .query_map(params![plan_id], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut entries = tasks
        .into_iter()
        .map(|task| {
            let blockers = entry_blockers(conn, &task)?;
            let readiness = pickup::readiness(&task, session, blockers.is_empty());
            Ok(FocusEntry {
                open_gate_count: open_gates(conn, task.task_id)?.len(),
                immediate_unlock_count: immediate_unlock_count(conn, task.task_id)?,
                unfinished_downstream_count: unfinished_downstream_count(conn, task.task_id)?,
                closed_child_count: child_count(conn, task.task_id)?,
                task: TaskSummary::from(&task),
                pickup: task.pickup,
                readiness: readiness.to_string(),
                blockers,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if !include_blocked {
        entries.retain(|entry| entry.readiness != "blocked");
    }
    if let Some(kind) = kind {
        entries.retain(|entry| entry.task.kind == kind);
    }
    entries.sort_by(|left, right| {
        focus_readiness_rank(&left.readiness)
            .cmp(&focus_readiness_rank(&right.readiness))
            .then_with(|| right.task.priority.cmp(&left.task.priority))
            .then_with(|| {
                right
                    .immediate_unlock_count
                    .cmp(&left.immediate_unlock_count)
            })
            .then_with(|| {
                right
                    .unfinished_downstream_count
                    .cmp(&left.unfinished_downstream_count)
            })
            .then_with(|| left.task.seq.cmp(&right.task.seq))
    });
    let eligible_count = entries.len();
    entries.truncate(limit);
    let blocked = if include_blocked {
        " --include-blocked"
    } else {
        ""
    };
    let kind_filter = kind
        .map(|kind| format!(" --kind {kind}"))
        .unwrap_or_default();
    let continuation_command = format!(
        "papertiger focus --plan {} --limit {eligible_count}{blocked}{kind_filter}{} --json",
        plan.slug,
        session
            .map(|s| format!(" --session {s}"))
            .unwrap_or_default()
    );
    Ok(FocusResponse {
        schema: "papertiger.focus.v8".into(),
        selection_state: "resolved".into(),
        plan: Some(PlanIdentity::from(&plan)),
        projection: StatusProjection::new(
            if include_blocked {
                "active and proposed actionable leaf tasks and parents whose children are all closed, including blocked proposed work"
            } else {
                "active and unblocked proposed actionable leaf tasks and parents whose children are all closed"
            },
            "readiness, priority descending, immediate unlocks descending, unfinished downstream descending, task.seq ascending",
            eligible_count,
            entries,
            Some(continuation_command),
        ),
    })
}

fn focus_readiness_rank(readiness: &str) -> u8 {
    match readiness {
        "mine" => 0,
        "in_progress" => 1,
        "ready" => 2,
        "picked_up_elsewhere" => 3,
        "blocked" => 4,
        _ => 5,
    }
}

pub(crate) fn immediate_unlock_count(conn: &Connection, task_id: i64) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*)
           FROM deps dependency
           JOIN tasks dependent ON dependent.task_id=dependency.task_id
          WHERE dependency.depends_on=?1
            AND dependent.status='proposed'
            AND NOT EXISTS (
                SELECT 1 FROM tasks child WHERE child.parent_id=dependent.task_id
            )
            AND NOT EXISTS (
                SELECT 1
                  FROM deps other_dependency
                  JOIN tasks prerequisite
                    ON prerequisite.task_id=other_dependency.depends_on
                 WHERE other_dependency.task_id=dependent.task_id
                   AND other_dependency.depends_on<>?1
                   AND prerequisite.status<>'done'
            )
            AND NOT EXISTS (
                SELECT 1
                  FROM task_blockers blocker
                 WHERE blocker.task_id=dependent.task_id
                   AND blocker.status='open'
            )",
        params![task_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

pub(crate) fn unfinished_downstream_count(conn: &Connection, task_id: i64) -> Result<usize> {
    let count: i64 = conn.query_row(
        "WITH RECURSIVE downstream(task_id) AS (
             SELECT dependency.task_id
               FROM deps dependency
              WHERE dependency.depends_on=?1
             UNION
             SELECT dependency.task_id
               FROM deps dependency
               JOIN downstream parent
                 ON dependency.depends_on=parent.task_id
         )
         SELECT COUNT(DISTINCT downstream.task_id)
           FROM downstream
           JOIN tasks task ON task.task_id=downstream.task_id
          WHERE task.status NOT IN ('done','retired','rejected')",
        params![task_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

fn child_count(conn: &Connection, task_id: i64) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE parent_id=?1",
        params![task_id],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count)?)
}

/// Ready tasks (proposed, no open deps), priority desc then seq asc.
/// With `include_blocked`, blocked proposed tasks follow with named blockers.
pub fn ready_tasks(
    conn: &Connection,
    plan_id: i64,
    limit: usize,
    include_blocked: bool,
) -> Result<Vec<ReadyEntry>> {
    // Open children are the actionable units of a parent, so a parent enters
    // the ready queue only once every child is closed and closing it remains.
    let mut st = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks t WHERE plan_id=?1 AND status='proposed'
         AND NOT EXISTS (SELECT 1 FROM tasks c WHERE c.parent_id = t.task_id
                          AND c.status IN ('proposed','in_progress'))
         ORDER BY priority DESC, seq ASC"
    ))?;
    let tasks: Vec<Task> = st
        .query_map(params![plan_id], task_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    for task in tasks {
        let blockers = entry_blockers(conn, &task)?;
        if blockers.is_empty() {
            ready.push(ReadyEntry {
                task,
                blockers: vec![],
            });
        } else if include_blocked {
            blocked.push(ReadyEntry { task, blockers });
        }
    }
    ready.truncate(limit);
    let remaining = limit.saturating_sub(ready.len());
    ready.extend(blocked.into_iter().take(remaining));
    Ok(ready)
}
