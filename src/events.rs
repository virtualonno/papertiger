//! Free-standing notes and event recording inside a mutation.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, Transaction, params};

use crate::{begin_mutation, get_task, mutation, now, validate_meaning_source};

pub fn add_note(
    conn: &Connection,
    actor: &str,
    task_seq: Option<i64>,
    text: &str,
    meaning_source: Option<&str>,
) -> Result<()> {
    let meaning_source = validate_meaning_source(meaning_source)?;
    let tx = begin_mutation(conn)?;
    let (entity, entity_id) = match task_seq {
        Some(seq) => ("task", Some(get_task(&tx, seq)?.task_id)),
        None => ("plan", None),
    };
    let payload = meaning_source.map(|source| serde_json::json!({"meaning_source": source}));
    record_event_in_mutation(
        &tx,
        actor,
        entity,
        entity_id,
        "note",
        Some(text),
        payload.as_ref(),
    )?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn record_event_in_mutation(
    tx: &Transaction<'_>,
    actor: &str,
    entity: &str,
    entity_id: Option<i64>,
    kind: &str,
    why: Option<&str>,
    payload: Option<&serde_json::Value>,
) -> Result<()> {
    let mut payload = payload.cloned();
    let (model, reasoning_effort) = mutation::current_attribution(tx)?;
    if let Some(model) = model {
        let object = payload.get_or_insert_with(|| serde_json::json!({}));
        object
            .as_object_mut()
            .context("event payload must be an object to record model attribution")?
            .insert("model".into(), model.into());
    }
    if let Some(effort) = reasoning_effort {
        payload
            .get_or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .context("event payload must be an object to record reasoning effort")?
            .insert("reasoning_effort".into(), effort.into());
    }
    mutation::payload_model(payload.as_ref())?;
    mutation::payload_reasoning_effort(payload.as_ref())?;
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
    mutation::capture_event(tx, tx.last_insert_rowid())?;
    Ok(())
}
