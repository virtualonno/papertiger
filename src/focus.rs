//! Work selection: focus ranking and the ready queue.

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{
    PlanIdentity, StatusProjection, TASK_COLS, Task, TaskPickup, TaskSummary, get_plan, open_deps,
    open_gates, pickup, seq_handle, task_ancestry, task_from_row, validate_session,
};

/// Handles of done dependencies whose results are unreviewed or disputed.
fn unreviewed_dependencies(conn: &Connection, task: &Task) -> Result<Vec<String>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks
          WHERE status='done' AND task_id IN (SELECT depends_on FROM deps WHERE task_id=?1)
          ORDER BY seq"
    ))?;
    let dependencies = statement
        .query_map(params![task.task_id], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut handles = Vec::new();
    for dependency in dependencies {
        if let Some(result) = crate::task_review(conn, &dependency)?.result
            && result.state.needs_review()
            && !result.is_legacy()
        {
            handles.push(dependency.handle);
        }
    }
    Ok(handles)
}

pub(crate) fn entry_blockers(conn: &Connection, task: &Task) -> Result<Vec<String>> {
    let mut blockers = open_deps(conn, task.task_id)?
        .into_iter()
        .map(|dependency| Ok(format!("dep:{}", seq_handle(conn, dependency)?)))
        .collect::<Result<Vec<_>>>()?;
    let mut external = conn.prepare(
        "SELECT name FROM gates
          WHERE task_id=?1 AND kind='external' AND status='open' ORDER BY gate_id",
    )?;
    blockers.extend(
        external
            .query_map(params![task.task_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|name| format!("external:{name}")),
    );
    // A disputed result holds the work that builds on it, and the parent
    // that would otherwise close over it, until it is confirmed or revised.
    let mut statement = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks
          WHERE status='done'
            AND (task_id IN (SELECT depends_on FROM deps WHERE task_id=?1) OR parent_id=?1)
          ORDER BY seq"
    ))?;
    let related = statement
        .query_map(params![task.task_id], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for related in related {
        if crate::review::result_disputed(conn, &related)? {
            blockers.push(format!("disputed-result:{}", related.handle));
        }
    }
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
    /// Whether the pickup's owner process still runs, observed now.
    pub owner_state: Option<crate::OwnerState>,
    pub readiness: String,
    pub blockers: Vec<String>,
    pub open_gate_count: usize,
    pub immediate_unlock_count: usize,
    pub unfinished_downstream_count: usize,
    /// Children of a parent whose children are all closed; the remaining
    /// work is closing the parent itself. Zero for leaf tasks.
    pub closed_child_count: usize,
    /// Ancestor handles from the root down to the direct parent.
    pub ancestry: Vec<String>,
    /// Review state of the task's filing.
    pub review: crate::ClaimReview,
    /// Done dependencies whose results still need someone else's check.
    /// Results recorded before authorship are not listed.
    pub unreviewed_dependencies: Vec<String>,
    /// The task's newest note when no status, definition, dependency or gate
    /// change followed it: it may hold or defer the task in prose that this
    /// ranking cannot see.
    pub note_since_change: Option<crate::EventRecord>,
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
                "readiness, priority (now, next, later), immediate unlocks descending, unfinished downstream descending, task.seq ascending",
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
    unreviewed: bool,
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
                ancestry: task_ancestry(conn, &task)?,
                review: crate::task_review(conn, &task)?.filing,
                unreviewed_dependencies: unreviewed_dependencies(conn, &task)?,
                note_since_change: crate::note_since_structural_change(conn, task.seq)?,
                task: TaskSummary::from(&task),
                owner_state: task.pickup.as_ref().map(crate::owner_state),
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
    if unreviewed {
        entries.retain(|entry| entry.review.state.needs_review());
    }
    entries.sort_by(|left, right| {
        focus_readiness_rank(&left.readiness)
            .cmp(&focus_readiness_rank(&right.readiness))
            .then_with(|| left.task.priority.cmp(&right.task.priority))
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
    let kind_filter = format!(
        "{}{}",
        kind.map(|kind| format!(" --kind {kind}"))
            .unwrap_or_default(),
        if unreviewed { " --unreviewed" } else { "" }
    );
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
            "readiness, priority (now, next, later), immediate unlocks descending, unfinished downstream descending, task.seq ascending",
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
                  FROM gates external
                 WHERE external.task_id=dependent.task_id
                   AND external.kind='external'
                   AND external.status='open'
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

/// Ready tasks (proposed, no open deps), by priority level then seq.
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
         ORDER BY CASE priority WHEN 'now' THEN 0 WHEN 'next' THEN 1 ELSE 2 END, seq ASC"
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
