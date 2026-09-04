//! Connection-local recording of the exact committed events produced by a command.
//!
//! Capture happens inside each mutation transaction. Neither concurrent writers
//! nor a subsequent task edit can change the command's result snapshot.

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{EventRecord, Plan, TaskSummary, get_plan, get_task};

pub fn validate_model(model: &str) -> Result<()> {
    if model.is_empty()
        || model.len() > 160
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
    {
        bail!(
            "model must be a nonblank identifier of at most 160 ASCII letters, digits, '.', '_', ':', '/', or '-'; supply --model <model-id> or omit unknown attribution"
        );
    }
    Ok(())
}

pub(crate) fn payload_model(payload: Option<&serde_json::Value>) -> Result<Option<String>> {
    match payload.and_then(|value| value.get("model")) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(model)) => {
            validate_model(model)?;
            Ok(Some(model.clone()))
        }
        Some(_) => bail!(
            "event model must be a string identifier or null; run `papertiger audit` and restore a verified export"
        ),
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MutationEvent {
    pub event: EventRecord,
    /// Task state at this event, not a later post-commit read.
    pub task: Option<TaskSummary>,
    pub plan: Option<Plan>,
}

#[derive(Debug, Serialize)]
pub struct MutationReceipt {
    pub schema: &'static str,
    pub changed: bool,
    pub events: Vec<MutationEvent>,
}

pub struct MutationRecorder<'a> {
    conn: &'a Connection,
}

impl<'a> MutationRecorder<'a> {
    /// Attribute and capture subsequent public API mutations on this connection.
    /// Drop the recorder before starting another recording scope.
    /// Dropping the scope rolls back any transaction still open within it.
    pub fn new(conn: &'a Connection, model: Option<&str>) -> Result<Self> {
        if let Some(model) = model {
            validate_model(model)?;
        }
        if active(conn)? {
            bail!(
                "a mutation recorder is already active on this connection; finish and drop it before starting another"
            );
        }
        let tx = crate::begin_mutation(conn)?;
        tx.execute_batch(
            "CREATE TEMP TABLE papertiger_command_context(model TEXT);
             CREATE TEMP TABLE papertiger_command_events(event_id INTEGER PRIMARY KEY, snapshot TEXT NOT NULL);",
        )?;
        tx.execute(
            "INSERT INTO temp.papertiger_command_context VALUES (?1)",
            params![model],
        )?;
        tx.commit()?;
        Ok(Self { conn })
    }

    pub fn receipt(&self) -> Result<MutationReceipt> {
        if !self.conn.is_autocommit() {
            bail!(
                "mutation receipt requires committed transactions; commit or roll back the pending mutation first"
            );
        }
        let mut statement = self
            .conn
            .prepare("SELECT snapshot FROM temp.papertiger_command_events ORDER BY event_id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let events = rows
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<Vec<_>>>()?;
        Ok(MutationReceipt {
            schema: "papertiger.mutation.v1",
            changed: !events.is_empty(),
            events,
        })
    }
}

impl Drop for MutationRecorder<'_> {
    fn drop(&mut self) {
        // A later rollback must not resurrect a dropped model context. The
        // scope starts outside a transaction, so any open one began inside it.
        if !self.conn.is_autocommit()
            && let Err(error) = self.conn.execute_batch("ROLLBACK")
        {
            eprintln!(
                "could not roll back an unfinished mutation recording scope: {error}; close this connection before further mutations"
            );
            return;
        }
        // Connection-local scratch only; failure cannot mint durable evidence.
        if let Err(error) = self.conn.execute_batch("DROP TABLE temp.papertiger_command_events; DROP TABLE temp.papertiger_command_context;") {
            eprintln!("could not remove connection-local mutation scratch: {error}; close this connection before another recording scope");
        }
    }
}

fn active(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row("SELECT 1 FROM sqlite_temp_master WHERE type='table' AND name='papertiger_command_context'", [], |_| Ok(())).optional()?.is_some())
}

pub(crate) fn current_model(conn: &Connection) -> Result<Option<String>> {
    if !active(conn)? {
        return Ok(None);
    }
    Ok(conn.query_row(
        "SELECT model FROM temp.papertiger_command_context",
        [],
        |row| row.get(0),
    )?)
}

pub(crate) fn capture_event(conn: &Connection, event_id: i64) -> Result<()> {
    if !active(conn)? {
        return Ok(());
    }
    let event = crate::read_model::event_by_id(conn, event_id)?;
    let task = event.task_seq.map(|seq| get_task(conn, seq)).transpose()?;
    let plan = if let Some(task) = &task {
        Some(get_plan(conn, task.plan_id)?)
    } else if let Some(slug) = &event.plan {
        let id = conn.query_row(
            "SELECT plan_id FROM plans WHERE slug=?1",
            params![slug],
            |row| row.get(0),
        )?;
        Some(get_plan(conn, id)?)
    } else {
        None
    };
    let snapshot = MutationEvent {
        event,
        task: task.as_ref().map(TaskSummary::from),
        plan,
    };
    conn.execute(
        "INSERT INTO temp.papertiger_command_events VALUES (?1, ?2)",
        params![event_id, serde_json::to_string(&snapshot)?],
    )?;
    Ok(())
}
