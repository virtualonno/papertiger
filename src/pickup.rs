//! Advisory pickup history. A pickup is neither a lock nor proof of liveness.

use anyhow::{Result, bail};
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{Task, now, record_event_in_mutation};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskPickup {
    pub session: Option<String>,
    pub at: String,
}

// The nullable pair is populated only by pickup, never by guessing from an
// old event's actor. Old in-progress work retains unknown pickup context.
pub(crate) const SCHEMA: &str = r#"
ALTER TABLE tasks ADD COLUMN pickup_session TEXT;
ALTER TABLE tasks ADD COLUMN pickup_at TEXT
  CHECK ((pickup_session IS NULL AND pickup_at IS NULL) OR
         (pickup_at IS NOT NULL AND status = 'in_progress'));
"#;

pub fn validate_session(session: &str) -> Result<()> {
    if session.is_empty()
        || session.len() > 128
        || !session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        || !session.as_bytes()[0].is_ascii_alphanumeric()
    {
        bail!(
            "session identity must be 1..128 ASCII letters, digits, '.', '_', ':', or '-', starting with a letter or digit; supply --session <unique-session-id> or PAPERTIGER_SESSION"
        );
    }
    Ok(())
}

pub(crate) fn validate_pickup(status: &str, pickup: Option<&TaskPickup>) -> Result<()> {
    if let Some(pickup) = pickup {
        if let Some(session) = &pickup.session {
            validate_session(session)?;
        }
        if status != "in_progress" {
            bail!(
                "pickup context requires in_progress status; import a consistent Papertiger export"
            );
        }
        chrono::DateTime::parse_from_rfc3339(&pickup.at).map_err(|_| {
            anyhow::anyhow!(
                "pickup.at must be an RFC 3339 timestamp; import an intact Papertiger export"
            )
        })?;
    }
    Ok(())
}

/// Called under the same transaction as start/resume. Repeating start from the
/// same identified session is eventless; anonymous calls cannot prove identity.
pub(crate) fn record_pickup(
    tx: &Transaction<'_>,
    actor: &str,
    task: &Task,
    session: Option<&str>,
    why: Option<&str>,
) -> Result<()> {
    if let Some(session) = session {
        validate_session(session)?;
    }
    if session.is_some() && task.pickup.as_ref().and_then(|p| p.session.as_deref()) == session {
        return Ok(());
    }
    let pickup = TaskPickup {
        session: session.map(str::to_owned),
        at: now(),
    };
    tx.execute(
        "UPDATE tasks SET pickup_session=?1, pickup_at=?2, updated_at=?2 WHERE task_id=?3",
        params![pickup.session, pickup.at, task.task_id],
    )?;
    record_event_in_mutation(
        tx,
        actor,
        "task",
        Some(task.task_id),
        "pickup",
        why,
        Some(&serde_json::json!({"seq": task.seq, "before": task.pickup, "after": pickup})),
    )?;
    Ok(())
}

pub(crate) fn readiness(task: &Task, session: Option<&str>, unblocked: bool) -> &'static str {
    match task.status.as_str() {
        "in_progress" => match task.pickup.as_ref().and_then(|p| p.session.as_deref()) {
            Some(last) if Some(last) == session => "mine",
            Some(_) => "picked_up_elsewhere",
            None => "in_progress",
        },
        "proposed" if unblocked => "ready",
        "proposed" => "blocked",
        _ => "unknown",
    }
}
