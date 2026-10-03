//! A bounded read of one task and its whole subtree for resuming work cold:
//! what it is for, how far it got, what to do next, what is open and owed,
//! what was decided, and what was said. It replaces hand-written handoff
//! documents; it changes nothing.

use std::collections::HashSet;

use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{
    ClaimReview, EventRecord, OwnerState, TASK_COLS, Task, TaskPickup, TaskSummary, focus,
    get_task, owner_state, task_ancestry, task_from_row, task_review,
};

/// Rows per section; each section reports how many it omitted.
pub const BRIEF_SECTION_LIMIT: usize = 10;

#[derive(Debug, Clone, Default, Serialize)]
pub struct SubtreeProgress {
    pub tasks: usize,
    pub done: usize,
    pub retired: usize,
    pub rejected: usize,
    pub open: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefOpenTask {
    pub task: TaskSummary,
    pub ancestry: Vec<String>,
    pub pickup: Option<TaskPickup>,
    pub owner_state: Option<OwnerState>,
    pub review: ClaimReview,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefObligation {
    pub task: String,
    pub gate: String,
    pub kind: String,
    pub requirement: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefOutcome {
    pub task: TaskSummary,
    pub result: String,
    /// Review of the result; absent when its completion is not in history.
    pub review: Option<ClaimReview>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefSection<T> {
    pub entries: Vec<T>,
    pub omitted: usize,
}

impl<T> BriefSection<T> {
    fn bounded(mut entries: Vec<T>) -> Self {
        let omitted = entries.len().saturating_sub(BRIEF_SECTION_LIMIT);
        entries.truncate(BRIEF_SECTION_LIMIT);
        Self { entries, omitted }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Brief {
    pub schema: &'static str,
    pub task: TaskSummary,
    pub intent: String,
    pub ancestry: Vec<String>,
    pub review: ClaimReview,
    /// The task and all its descendants.
    pub progress: SubtreeProgress,
    /// Ready leaves in the subtree, in focus order.
    pub next: BriefSection<TaskSummary>,
    /// Proposed and in-progress descendants, by sequence.
    pub open: BriefSection<BriefOpenTask>,
    /// Open gates in the subtree, external conditions included.
    pub obligations: BriefSection<BriefObligation>,
    /// Results of done tasks in the subtree, latest completion first.
    pub outcomes: BriefSection<BriefOutcome>,
    /// Notes on tasks in the subtree, latest first.
    pub notes: BriefSection<EventRecord>,
}

/// The task and every descendant, by sequence.
fn subtree(conn: &Connection, root: &Task) -> Result<Vec<Task>> {
    let mut statement = conn.prepare(&format!(
        "WITH RECURSIVE subtree(task_id) AS (
           SELECT ?1
           UNION ALL
           SELECT child.task_id FROM tasks child JOIN subtree ON child.parent_id=subtree.task_id
         )
         SELECT {TASK_COLS} FROM tasks WHERE task_id IN (SELECT task_id FROM subtree) ORDER BY seq"
    ))?;
    Ok(statement
        .query_map(params![root.task_id], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn brief(conn: &Connection, seq: i64) -> Result<Brief> {
    let root = get_task(conn, seq)?;
    let tasks = subtree(conn, &root)?;
    let members: HashSet<i64> = tasks.iter().map(|task| task.seq).collect();
    let mut progress = SubtreeProgress {
        tasks: tasks.len(),
        ..SubtreeProgress::default()
    };
    let mut open = Vec::new();
    let mut obligations = Vec::new();
    let mut outcomes = Vec::new();
    for task in &tasks {
        match task.status.as_str() {
            "done" => progress.done += 1,
            "retired" => progress.retired += 1,
            "rejected" => progress.rejected += 1,
            _ if task.task_id == root.task_id => progress.open += 1,
            _ => {
                progress.open += 1;
                open.push(BriefOpenTask {
                    task: TaskSummary::from(task),
                    ancestry: task_ancestry(conn, task)?,
                    owner_state: task.pickup.as_ref().map(owner_state),
                    pickup: task.pickup.clone(),
                    review: task_review(conn, task)?.filing,
                });
            }
        }
        let mut gates = conn.prepare(
            "SELECT name, kind, requirement FROM gates
              WHERE task_id=?1 AND status='open' ORDER BY gate_id",
        )?;
        for gate in gates.query_map(params![task.task_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (gate, kind, requirement) = gate?;
            obligations.push(BriefObligation {
                task: task.handle.clone(),
                gate,
                kind,
                requirement,
            });
        }
        if task.status == "done"
            && let Some(result) = task
                .result
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty())
        {
            let completed: Option<i64> = conn.query_row(
                "SELECT MAX(event_id) FROM canonical_events
                  WHERE entity='task' AND entity_seq=?1 AND kind='status'",
                params![task.seq],
                |row| row.get(0),
            )?;
            outcomes.push((
                completed.unwrap_or_default(),
                BriefOutcome {
                    task: TaskSummary::from(task),
                    result: result.to_owned(),
                    review: task_review(conn, task)?.result,
                },
            ));
        }
    }
    outcomes.sort_by_key(|(event_id, _)| std::cmp::Reverse(*event_id));
    let next = focus(conn, root.plan_id, usize::MAX, false, None, false, None)?
        .projection
        .entries
        .into_iter()
        .filter(|entry| entry.readiness == "ready" && members.contains(&entry.task.seq))
        .map(|entry| entry.task)
        .collect();
    let sequences = tasks
        .iter()
        .map(|task| task.seq.to_string())
        .collect::<Vec<_>>();
    let mut note_ids = conn.prepare(&format!(
        "SELECT event_id FROM canonical_events
          WHERE kind='note' AND entity='task' AND entity_seq IN ({})
          ORDER BY event_id DESC LIMIT {}",
        sequences.join(","),
        BRIEF_SECTION_LIMIT + 1
    ))?;
    let note_ids = note_ids
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let note_total: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM canonical_events
              WHERE kind='note' AND entity='task' AND entity_seq IN ({})",
            sequences.join(",")
        ),
        [],
        |row| row.get(0),
    )?;
    let notes = note_ids
        .into_iter()
        .map(|event_id| crate::read_model::event_by_id(conn, event_id))
        .collect::<Result<Vec<_>>>()?;
    Ok(Brief {
        schema: "papertiger.brief.v1",
        task: TaskSummary::from(&root),
        intent: root.intent.clone(),
        ancestry: task_ancestry(conn, &root)?,
        review: task_review(conn, &root)?.filing,
        progress,
        next: BriefSection::bounded(next),
        open: BriefSection::bounded(open),
        obligations: BriefSection::bounded(obligations),
        outcomes: BriefSection::bounded(outcomes.into_iter().map(|(_, outcome)| outcome).collect()),
        notes: BriefSection {
            omitted: usize::try_from(note_total)?
                .saturating_sub(notes.len().min(BRIEF_SECTION_LIMIT)),
            entries: notes.into_iter().take(BRIEF_SECTION_LIMIT).collect(),
        },
    })
}
