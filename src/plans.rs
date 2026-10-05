//! Plans: lookup, selection, creation, orientation edits and status.

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{begin_mutation, given_reason, now, record_event_in_mutation, require_nonblank};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    #[serde(skip)]
    pub plan_id: i64,
    pub slug: String,
    pub title: String,
    pub intent: String,
    pub status: String,
}

pub fn get_plan(conn: &Connection, plan_id: i64) -> Result<Plan> {
    conn.query_row(
        "SELECT plan_id, slug, title, intent, status FROM plans WHERE plan_id=?1",
        params![plan_id],
        |row| {
            Ok(Plan {
                plan_id: row.get(0)?,
                slug: row.get(1)?,
                title: row.get(2)?,
                intent: row.get(3)?,
                status: row.get(4)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| anyhow!("no plan id {plan_id}"))
}

pub(crate) fn plan_status(conn: &Connection, plan_id: i64) -> Result<String> {
    conn.query_row(
        "SELECT status FROM plans WHERE plan_id=?1",
        params![plan_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| anyhow!("no plan id {plan_id}"))
}

/// Every active plan, oldest first.
pub fn active_plans(conn: &Connection) -> Result<Vec<(i64, String)>> {
    let mut statement =
        conn.prepare("SELECT plan_id, slug FROM plans WHERE status='active' ORDER BY plan_id")?;
    Ok(statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn active_plan(conn: &Connection) -> Result<Option<(i64, String)>> {
    let rows = active_plans(conn)?;
    match rows.len() {
        0 => Ok(None),
        1 => Ok(rows.into_iter().next()),
        _ => bail!(
            "multiple active plans ({}); pass --plan",
            rows.iter()
                .map(|(_, slug)| slug.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Resolve the plan to operate on: explicit slug, else the single active plan.
pub fn resolve_plan(conn: &Connection, slug: Option<&str>) -> Result<(i64, String)> {
    if let Some(slug) = slug {
        if let Some(found) = conn
            .query_row(
                "SELECT plan_id, slug FROM plans WHERE slug=?1",
                params![slug],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        {
            return Ok(found);
        }
        let known = conn
            .prepare("SELECT slug FROM plans ORDER BY slug")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if known.is_empty() {
            bail!(
                "no plan '{slug}': this authority has no plans; create one with `plan add <slug> \"Title\"`"
            );
        }
        bail!(
            "no plan '{slug}'; this authority's plans are: {}",
            known.join(", ")
        );
    }
    active_plan(conn)?.ok_or_else(|| anyhow!("no active plan; create one with `plan add`"))
}

pub fn add_plan(
    conn: &Connection,
    actor: &str,
    slug: &str,
    title: &str,
    intent: &str,
) -> Result<i64> {
    let slug = require_nonblank("plan slug", slug)?;
    let title = require_nonblank("plan title", title)?;
    let tx = begin_mutation(conn)?;
    let t = now();
    tx.execute(
        "INSERT INTO plans (slug, title, intent, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'active', ?4, ?4)",
        params![slug, title, intent, t],
    )?;
    let id = tx.last_insert_rowid();
    record_event_in_mutation(
        &tx,
        actor,
        "plan",
        Some(id),
        "create",
        None,
        Some(&serde_json::json!({"slug": slug})),
    )?;
    tx.commit()?;
    Ok(id)
}

pub fn edit_plan(
    conn: &Connection,
    actor: &str,
    slug: &str,
    title: Option<&str>,
    intent: Option<&str>,
    why: Option<&str>,
) -> Result<Vec<&'static str>> {
    let why = given_reason(why);
    if title.is_none() && intent.is_none() {
        bail!("plan edit requires at least one of --title or --intent");
    }
    let tx = begin_mutation(conn)?;
    let (plan_id, _) = resolve_plan(&tx, Some(slug))?;
    let mut changed = Vec::new();
    if let Some(new) = title {
        let new = require_nonblank("plan title", new)?;
        tx.execute(
            "UPDATE plans SET title=?1, updated_at=?2 WHERE plan_id=?3",
            params![new, now(), plan_id],
        )?;
        changed.push("title");
    }
    if let Some(new) = intent {
        tx.execute(
            "UPDATE plans SET intent=?1, updated_at=?2 WHERE plan_id=?3",
            params![new, now(), plan_id],
        )?;
        changed.push("intent");
    }
    record_event_in_mutation(
        &tx,
        actor,
        "plan",
        Some(plan_id),
        "edit",
        why,
        Some(&serde_json::json!({"slug": slug, "fields": changed})),
    )?;
    tx.commit()?;
    Ok(changed)
}

pub fn set_plan_status(
    conn: &Connection,
    actor: &str,
    slug: &str,
    status: &str,
    why: Option<&str>,
) -> Result<()> {
    if !["active", "paused", "done", "retired"].contains(&status) {
        bail!("unknown plan status '{status}'");
    }
    let why = given_reason(why);
    let tx = begin_mutation(conn)?;
    let (plan_id, _) = resolve_plan(&tx, Some(slug))?;
    if status == "done" {
        let live_tasks: i64 = tx.query_row(
            "SELECT COUNT(*) FROM tasks
              WHERE plan_id=?1 AND status IN ('proposed','in_progress')",
            params![plan_id],
            |row| row.get(0),
        )?;
        if live_tasks > 0 {
            bail!(
                "plan '{slug}' has {live_tasks} live task(s); finish or disposition them before marking the plan done"
            );
        }
    }
    tx.execute(
        "UPDATE plans SET status=?1, updated_at=?2 WHERE plan_id=?3",
        params![status, now(), plan_id],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "plan",
        Some(plan_id),
        "status",
        why,
        Some(&serde_json::json!({"to": status})),
    )?;
    tx.commit()?;
    Ok(())
}
