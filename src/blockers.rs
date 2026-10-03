//! External task blockers.

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    begin_mutation, default_open, get_task, now, record_event_in_mutation,
    validate_new_evidence_locator, validate_optional_sha256,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBlocker {
    pub name: String,
    pub condition: String,
    #[serde(default = "default_open")]
    pub status: String,
    pub evidence_locator: Option<String>,
    pub evidence_sha256: Option<String>,
    pub note: Option<String>,
    pub resolved_at: Option<String>,
}

pub fn task_blockers(conn: &Connection, task_id: i64) -> Result<Vec<TaskBlocker>> {
    let mut statement = conn.prepare(
        "SELECT name, condition, status, evidence_locator, evidence_sha256, note, resolved_at
           FROM task_blockers
          WHERE task_id=?1
          ORDER BY blocker_id",
    )?;
    Ok(statement
        .query_map(params![task_id], |row| {
            Ok(TaskBlocker {
                name: row.get(0)?,
                condition: row.get(1)?,
                status: row.get(2)?,
                evidence_locator: row.get(3)?,
                evidence_sha256: row.get(4)?,
                note: row.get(5)?,
                resolved_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn open_task_blocker_names(conn: &Connection, task_id: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT name FROM task_blockers
          WHERE task_id=?1 AND status='open'
          ORDER BY blocker_id",
    )?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn add_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    condition: &str,
) -> Result<()> {
    if name.trim().is_empty() || condition.trim().is_empty() {
        bail!(
            "adding a blocker requires a nonblank name and condition; run `papertiger blocker add <task> <name> --condition <text>`"
        );
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "#{seq} is {}; reopen it before adding blockers",
            task.status
        );
    }
    tx.execute(
        "INSERT INTO task_blockers (task_id, name, condition)
         VALUES (?1, ?2, ?3)",
        params![task.task_id, name.trim(), condition.trim()],
    )
    .with_context(|| {
        format!(
            "blocker '{}' already exists on #{seq}; choose a different blocker name",
            name.trim()
        )
    })?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "blocker_add",
        Some(condition.trim()),
        Some(&serde_json::json!({"seq": seq, "name": name.trim()})),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn resolve_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    evidence: &str,
    sha256: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    validate_new_evidence_locator(evidence)?;
    validate_optional_sha256(sha256)?;
    resolve_open_blocker(
        conn,
        actor,
        seq,
        name,
        BlockerResolution {
            status: "resolved",
            evidence: Some(evidence),
            sha256,
            note,
        },
    )
}

pub fn waive_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    why: &str,
) -> Result<()> {
    if why.trim().is_empty() {
        bail!("waiving a blocker requires a nonblank reason");
    }
    resolve_open_blocker(
        conn,
        actor,
        seq,
        name,
        BlockerResolution {
            status: "waived",
            evidence: None,
            sha256: None,
            note: Some(why),
        },
    )
}

pub fn reopen_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    why: &str,
) -> Result<()> {
    if why.trim().is_empty() {
        bail!("reopening a blocker requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "#{seq} is {}; reopen the task before reopening blockers",
            task.status
        );
    }
    let blocker: Option<(i64, String)> = tx
        .query_row(
            "SELECT blocker_id, status FROM task_blockers
              WHERE task_id=?1 AND name=?2",
            params![task.task_id, name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (blocker_id, status) = blocker.ok_or_else(|| anyhow!("no blocker '{name}' on #{seq}"))?;
    if status == "open" {
        bail!("blocker '{name}' on #{seq} is already open");
    }
    tx.execute(
        "UPDATE task_blockers
            SET status='open', evidence_locator=NULL, evidence_sha256=NULL,
                note=NULL, resolved_at=NULL
          WHERE blocker_id=?1",
        params![blocker_id],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "blocker_reopen",
        Some(why.trim()),
        Some(&serde_json::json!({
            "seq": seq,
            "name": name,
            "from": status,
        })),
    )?;
    tx.commit()?;
    Ok(())
}

struct BlockerResolution<'a> {
    status: &'a str,
    evidence: Option<&'a str>,
    sha256: Option<&'a str>,
    note: Option<&'a str>,
}

fn resolve_open_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    resolution: BlockerResolution<'_>,
) -> Result<()> {
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let blocker_id: i64 = tx
        .query_row(
            "SELECT blocker_id FROM task_blockers
              WHERE task_id=?1 AND name=?2 AND status='open'",
            params![task.task_id, name],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("no open blocker '{name}' on #{seq}"))?;
    tx.execute(
        "UPDATE task_blockers
            SET status=?1, evidence_locator=?2, evidence_sha256=?3,
                note=?4, resolved_at=?5
          WHERE blocker_id=?6",
        params![
            resolution.status,
            resolution.evidence,
            resolution.sha256,
            resolution.note,
            now(),
            blocker_id
        ],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        &format!("blocker_{}", resolution.status),
        resolution.note,
        Some(&serde_json::json!({
            "seq": seq,
            "name": name,
            "evidence": resolution.evidence,
        })),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn remove_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    why: &str,
) -> Result<()> {
    if why.trim().is_empty() {
        bail!("removing a blocker requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let blocker: Option<(i64, String)> = tx
        .query_row(
            "SELECT blocker_id, condition FROM task_blockers
              WHERE task_id=?1 AND name=?2 AND status='open'",
            params![task.task_id, name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (blocker_id, condition) =
        blocker.ok_or_else(|| anyhow!("no open blocker '{name}' on #{seq}"))?;
    tx.execute(
        "DELETE FROM task_blockers WHERE blocker_id=?1",
        params![blocker_id],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "blocker_remove",
        Some(why),
        Some(&serde_json::json!({
            "seq": seq,
            "name": name,
            "condition": condition,
        })),
    )?;
    tx.commit()?;
    Ok(())
}
