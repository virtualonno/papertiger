//! Explicit whole-authority inventory, separate from active-plan readiness.

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{Plan, PlanIdentity, TASK_STATUSES, TaskSummary, event_head, get_plan, resolve_plan};

#[derive(Debug, Serialize)]
pub struct InventoryItem {
    pub plan: PlanIdentity,
    pub task: TaskSummary,
}

#[derive(Debug, Serialize)]
pub struct InventoryResponse {
    pub schema: &'static str,
    pub status: Option<String>,
    pub tag: Option<String>,
    pub total: usize,
    pub remaining: usize,
    pub snapshot: String,
    pub next_after_seq: Option<i64>,
    pub tasks: Vec<InventoryItem>,
}

pub fn plan_inventory(conn: &Connection, slug: Option<&str>) -> Result<Vec<Plan>> {
    if let Some(slug) = slug {
        return Ok(vec![get_plan(conn, resolve_plan(conn, Some(slug))?.0)?]);
    }
    let mut statement = conn.prepare("SELECT plan_id FROM plans ORDER BY plan_id")?;
    let ids = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    ids.into_iter().map(|id| get_plan(conn, id)).collect()
}

pub fn task_inventory(
    conn: &Connection,
    status: Option<&str>,
    tag: Option<&str>,
    limit: usize,
    after_seq: Option<i64>,
    snapshot: Option<&str>,
) -> Result<InventoryResponse> {
    if !(1..=500).contains(&limit) {
        bail!("inventory --limit must be between 1 and 500");
    }
    if let Some(status) = status
        && status != "unfinished"
        && !TASK_STATUSES.contains(&status)
    {
        bail!(
            "unknown inventory status '{status}'; use unfinished or {}",
            TASK_STATUSES.join("|")
        );
    }
    if after_seq.is_some_and(|seq| seq < 0) {
        bail!("--after-seq must be nonnegative; use the previous next_after_seq");
    }
    if after_seq.is_some() && snapshot.is_none() {
        bail!("--after-seq requires --snapshot from the previous inventory page");
    }
    let current = event_head(conn)?
        .map(|head| head.token)
        .unwrap_or_else(|| "empty".into());
    if snapshot.is_some_and(|snapshot| snapshot != current) {
        bail!(
            "inventory authority changed; restart list --all-plans without --after-seq or --snapshot"
        );
    }
    // Fetch only compact fields; task bodies and historical payloads never enter this projection.
    let predicate = "(?1 IS NULL OR t.status=?1 OR (?1='unfinished' AND t.status IN ('proposed','in_progress'))) AND (?2 IS NULL OR EXISTS (SELECT 1 FROM task_tags g WHERE g.task_id=t.task_id AND g.tag=?2))";
    let total: i64 = conn.query_row(
        &format!("SELECT count(*) FROM tasks t WHERE {predicate}"),
        params![status, tag],
        |row| row.get(0),
    )?;
    let remaining_before: i64 = conn.query_row(
        &format!("SELECT count(*) FROM tasks t WHERE {predicate} AND t.seq>?3"),
        params![status, tag, after_seq.unwrap_or(0)],
        |row| row.get(0),
    )?;
    let mut statement = conn.prepare(&format!("SELECT p.slug,p.status,t.seq,t.title,t.status,t.kind,t.priority FROM tasks t JOIN plans p ON p.plan_id=t.plan_id WHERE {predicate} AND t.seq>?3 ORDER BY t.seq LIMIT ?4"))?;
    let tasks = statement
        .query_map(
            params![status, tag, after_seq.unwrap_or(0), i64::try_from(limit)?],
            |row| {
                Ok(InventoryItem {
                    plan: PlanIdentity {
                        slug: row.get(0)?,
                        status: row.get(1)?,
                    },
                    task: TaskSummary {
                        seq: row.get(2)?,
                        title: row.get(3)?,
                        status: row.get(4)?,
                        kind: row.get(5)?,
                        priority: crate::priority_recovery::read_priority(row, 2, 6)?,
                    },
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let remaining = usize::try_from(remaining_before)? - tasks.len();
    Ok(InventoryResponse {
        schema: "papertiger.task_inventory.v1",
        status: status.map(str::to_owned),
        tag: tag.map(str::to_owned),
        total: usize::try_from(total)?,
        remaining,
        snapshot: current,
        next_after_seq: if remaining > 0 {
            tasks.last().map(|item| item.task.seq)
        } else {
            None
        },
        tasks,
    })
}
