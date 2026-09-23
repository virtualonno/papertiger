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
    /// Opaque continuation bound to the filters and authority snapshot.
    pub next_cursor: Option<String>,
    pub continuation_command: Option<String>,
    pub tasks: Vec<InventoryItem>,
}

const INVENTORY_CURSOR_PREFIX: &str = "inventory-v1";

fn filter_digest(status: Option<&str>, tag: Option<&str>) -> String {
    let scope = serde_json::json!([status, tag]).to_string();
    crate::sha256(scope.as_bytes())[..16].to_owned()
}

fn restart_command(status: Option<&str>, tag: Option<&str>, limit: usize) -> String {
    let mut command = String::from("papertiger list --all-plans");
    if let Some(status) = status {
        command.push_str(&format!(" --status {status}"));
    }
    if let Some(tag) = tag {
        command.push_str(&format!(" --tag {tag}"));
    }
    command.push_str(&format!(" --limit {limit} --json"));
    command
}

/// Decode `inventory-v1:<seq>:<filter-digest>:<event-head>` for these filters.
fn cursor_position(
    cursor: &str,
    status: Option<&str>,
    tag: Option<&str>,
    snapshot: &str,
    restart: &str,
) -> Result<i64> {
    let mut parts = cursor.splitn(4, ':');
    let (Some(INVENTORY_CURSOR_PREFIX), Some(seq), Some(filters), Some(cursor_snapshot)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("--after-cursor is not an inventory cursor; restart with `{restart}`");
    };
    let seq = seq
        .parse::<i64>()
        .ok()
        .filter(|seq| *seq > 0)
        .ok_or_else(|| anyhow::anyhow!("--after-cursor is malformed; restart with `{restart}`"))?;
    if filters != filter_digest(status, tag) {
        bail!(
            "--after-cursor belongs to different --status/--tag filters; repeat the filters that produced it or restart with `{restart}`"
        );
    }
    if cursor_snapshot != snapshot {
        bail!("inventory authority changed since this cursor; restart with `{restart}`");
    }
    Ok(seq)
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
    after_cursor: Option<&str>,
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
    let current = event_head(conn)?
        .map(|head| head.token)
        .unwrap_or_else(|| "empty".into());
    let restart = restart_command(status, tag, limit);
    let after_seq = after_cursor
        .map(|cursor| cursor_position(cursor, status, tag, &current, &restart))
        .transpose()?;
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
    let next_cursor = tasks.last().filter(|_| remaining > 0).map(|item| {
        format!(
            "{INVENTORY_CURSOR_PREFIX}:{}:{}:{current}",
            item.task.seq,
            filter_digest(status, tag)
        )
    });
    let continuation_command = next_cursor
        .as_ref()
        .map(|cursor| restart.replacen(" --json", &format!(" --after-cursor {cursor} --json"), 1));
    Ok(InventoryResponse {
        schema: "papertiger.task_inventory.v2",
        status: status.map(str::to_owned),
        tag: tag.map(str::to_owned),
        total: usize::try_from(total)?,
        remaining,
        next_cursor,
        continuation_command,
        tasks,
    })
}
