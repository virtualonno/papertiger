//! Explicit recovery of text priorities written outside the typed planner API.
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, types::Type};

use crate::{AuditFinding, TaskEdit, get_task, now, record_event_in_mutation};

fn correction(seq: i64, storage: &str) -> String {
    if storage == "text" {
        format!(
            "task #{seq} priority is stored as text, not an integer; preserve a copy with `papertiger backup --output <new-path>`, then run `papertiger edit {seq} --priority <integer> --why <reason>` with no other edits"
        )
    } else {
        format!(
            "task #{seq} priority has invalid SQLite type {storage}; run `papertiger audit` and restore a verified authority"
        )
    }
}

pub(crate) fn read_priority(
    row: &Row<'_>,
    seq_index: usize,
    index: usize,
) -> rusqlite::Result<i64> {
    row.get(index).map_err(|error| {
        let Ok(value) = row.get_ref(index) else {
            return error;
        };
        let Ok(seq) = row.get::<_, i64>(seq_index) else {
            return error;
        };
        let storage = match value.data_type() {
            Type::Text => "text",
            Type::Real => "real",
            Type::Blob => "blob",
            Type::Null => "null",
            Type::Integer => return error,
        };
        rusqlite::Error::FromSqlConversionFailure(
            index,
            value.data_type(),
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                correction(seq, storage),
            )),
        )
    })
}

pub(crate) fn audit_priorities(conn: &Connection) -> Result<Vec<AuditFinding>> {
    let mut statement = conn.prepare(
        "SELECT seq, typeof(priority) FROM tasks WHERE typeof(priority) != 'integer' ORDER BY seq",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (seq, storage) = row?;
        Ok(AuditFinding {
            kind: "invalid_task_priority".into(),
            detail: correction(seq, &storage),
        })
    })
    .collect()
}

/// Runs inside edit's transaction. Recovery is deliberately separate from a
/// valid integer-to-integer definition revision; the original text is evidence.
pub(crate) fn repair_text_priority(
    conn: &Transaction<'_>,
    actor: &str,
    seq: i64,
    edit: &TaskEdit<'_>,
    why: &str,
) -> Result<bool> {
    let Some(priority) = edit.priority else {
        return Ok(false);
    };
    let before: Option<String> = conn
        .query_row(
            "SELECT priority FROM tasks WHERE seq=?1 AND typeof(priority)='text'",
            [seq],
            |row| row.get(0),
        )
        .optional()?;
    let Some(before) = before else {
        return Ok(false);
    };
    if edit.title.is_some()
        || edit.intent.is_some()
        || edit.intent_source.is_some()
        || edit.parent.is_some()
        || edit.kind.is_some()
    {
        bail!(
            "priority recovery cannot be combined with other edits; run `papertiger edit {seq} --priority {priority} --why <reason>` first"
        );
    }
    conn.execute(
        "UPDATE tasks SET priority=?1, updated_at=?2 WHERE seq=?3",
        params![priority, now(), seq],
    )?;
    // Any remaining unreadable task field refuses and rolls back the repair.
    let task = get_task(conn, seq)?;
    record_event_in_mutation(
        conn,
        actor,
        "task",
        Some(task.task_id),
        "repair_priority",
        Some(why),
        Some(&serde_json::json!({
            "seq": seq, "before": {"storage_type": "text", "value": before}, "after": priority,
        })),
    )?;
    Ok(true)
}
