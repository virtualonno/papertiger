//! Free-standing notes and event recording inside a mutation.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, Transaction, params};

use crate::{begin_mutation, get_task, mutation, now};

/// Record a note and return its event id, which `note:<event-id>` evidence cites.
pub fn add_note(conn: &Connection, actor: &str, task_seq: Option<i64>, text: &str) -> Result<i64> {
    let tx = begin_mutation(conn)?;
    let (entity, entity_id) = match task_seq {
        Some(seq) => ("task", Some(get_task(&tx, seq)?.task_id)),
        None => ("plan", None),
    };
    let event_id =
        record_event_in_mutation(&tx, actor, entity, entity_id, "note", Some(text), None)?;
    tx.commit()?;
    Ok(event_id)
}

pub(crate) fn record_event_in_mutation(
    tx: &Transaction<'_>,
    actor: &str,
    entity: &str,
    entity_id: Option<i64>,
    kind: &str,
    why: Option<&str>,
    payload: Option<&serde_json::Value>,
) -> Result<i64> {
    let mut payload = payload.cloned();
    let object = payload
        .get_or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("event payload must be an object to record its authorship")?;
    for (key, value) in crate::authorship::current_authorship(tx)? {
        // A pickup event names its session already; the author's session is the same one.
        object.entry(key).or_insert(value);
    }
    crate::authorship::validate_payload_authorship(payload.as_ref())?;
    let (entity_plan, entity_seq, gate_name): (Option<String>, Option<i64>, Option<String>) =
        match (entity, entity_id) {
            ("plan", Some(plan_id)) => (
                Some(tx.query_row(
                    "SELECT slug FROM plans WHERE plan_id=?1",
                    params![plan_id],
                    |row| row.get(0),
                )?),
                None,
                None,
            ),
            ("task" | "dep", Some(task_id)) => {
                let (seq, slug) = tx.query_row(
                    "SELECT t.seq, p.slug FROM tasks t JOIN plans p ON p.plan_id=t.plan_id
                 WHERE t.task_id=?1",
                    params![task_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                (Some(slug), Some(seq), None)
            }
            ("gate", Some(gate_id)) => {
                let (seq, slug, name) = tx.query_row(
                    "SELECT t.seq, p.slug, g.name FROM gates g
                 JOIN tasks t ON t.task_id=g.task_id JOIN plans p ON p.plan_id=t.plan_id
                 WHERE g.gate_id=?1",
                    params![gate_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                (Some(slug), Some(seq), Some(name))
            }
            ("plan", None) => (None, None, None),
            _ => bail!("event '{entity}' requires a durable entity identity"),
        };
    tx.execute(
        "INSERT INTO events
         (at, actor, entity, entity_id, entity_plan, entity_seq, gate_name, kind, why, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            now(),
            actor,
            entity,
            entity_id,
            entity_plan,
            entity_seq,
            gate_name,
            kind,
            why,
            payload.as_ref().map(|p| p.to_string())
        ],
    )?;
    let event_id = tx.last_insert_rowid();
    mutation::capture_event(tx, event_id)?;
    Ok(event_id)
}
