//! Durable cooperative requests. The supervisor owns termination and settlement;
//! a request alone never asserts process absence or refunds launched work.

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::store::{begin_mutation, now, record_event_in_mutation};
use crate::validation::validate_nonblank;

pub(crate) const CANCELLATION_SCHEMA_V9: &str = r#"
CREATE TABLE cancellation_requests (
  trial_id TEXT UNIQUE REFERENCES trials(trial_id),
  execution_id TEXT UNIQUE REFERENCES paired_runs(execution_id),
  actor TEXT NOT NULL CHECK (length(trim(actor)) > 0),
  reason TEXT NOT NULL CHECK (length(trim(reason)) > 0),
  requested_at TEXT NOT NULL,
  CHECK ((trial_id IS NOT NULL) != (execution_id IS NOT NULL))
);
CREATE TRIGGER cancellation_requests_no_update BEFORE UPDATE ON cancellation_requests
BEGIN SELECT RAISE(ABORT, 'cancellation request is immutable'); END;
CREATE TRIGGER cancellation_requests_no_delete BEFORE DELETE ON cancellation_requests
BEGIN SELECT RAISE(ABORT, 'cancellation request is immutable'); END;
CREATE TRIGGER cancellation_request_launched_guard BEFORE INSERT ON cancellation_requests
WHEN (NEW.trial_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM trials WHERE trial_id=NEW.trial_id AND status='launched'))
  OR (NEW.execution_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM paired_runs WHERE execution_id=NEW.execution_id AND status='launched'))
BEGIN SELECT RAISE(ABORT, 'cancellation requires a launched execution; inspect trial show or paired show-run'); END;
CREATE TRIGGER trial_cancellation_success_guard BEFORE UPDATE OF status ON trials
WHEN NEW.status='succeeded' AND EXISTS (
  SELECT 1 FROM cancellation_requests WHERE trial_id=NEW.trial_id)
BEGIN SELECT RAISE(ABORT, 'cancellation requested; supervisor must settle the trial without qualification'); END;
CREATE TRIGGER paired_cancellation_success_guard BEFORE UPDATE OF status ON paired_runs
WHEN NEW.status='succeeded' AND EXISTS (
  SELECT 1 FROM cancellation_requests WHERE execution_id=NEW.execution_id)
BEGIN SELECT RAISE(ABORT, 'cancellation requested; supervisor must settle the paired run without qualification'); END;
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationTarget {
    Trial,
    PairedRun,
}

impl CancellationTarget {
    fn column(self) -> &'static str {
        match self {
            Self::Trial => "trial_id",
            Self::PairedRun => "execution_id",
        }
    }

    fn entity(self) -> &'static str {
        match self {
            Self::Trial => "trial",
            Self::PairedRun => "paired_run",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancellationRequest {
    pub target: CancellationTarget,
    pub execution_id: String,
    pub actor: String,
    pub reason: String,
    pub requested_at: String,
}

#[derive(Debug)]
pub(crate) struct CancellationPending;

impl std::fmt::Display for CancellationPending {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("authority-recorded cancellation requested; settle without qualification")
    }
}

impl std::error::Error for CancellationPending {}

pub(crate) fn ensure_not_cancelled(
    connection: &Connection,
    target: CancellationTarget,
    execution_id: &str,
) -> Result<()> {
    if cancellation_request(connection, target, execution_id)?.is_some() {
        return Err(CancellationPending.into());
    }
    Ok(())
}

pub fn cancellation_request(
    connection: &Connection,
    target: CancellationTarget,
    execution_id: &str,
) -> Result<Option<CancellationRequest>> {
    Ok(connection
        .query_row(
            &format!(
                "SELECT actor, reason, requested_at FROM cancellation_requests WHERE {}=?1",
                target.column()
            ),
            params![execution_id],
            |row| {
                Ok(CancellationRequest {
                    target,
                    execution_id: execution_id.to_owned(),
                    actor: row.get(0)?,
                    reason: row.get(1)?,
                    requested_at: row.get(2)?,
                })
            },
        )
        .optional()?)
}

/// Record one immutable request for a launched deterministic trial or paired
/// execution. Exact replay returns the original request, including its author.
/// Completion and cancellation serialize through the same authority transaction.
pub fn request_cancellation(
    connection: &Connection,
    actor: &str,
    target: CancellationTarget,
    execution_id: &str,
    reason: &str,
) -> Result<CancellationRequest> {
    validate_nonblank("actor", actor)?;
    validate_nonblank("execution_id", execution_id)?;
    validate_nonblank("cancellation reason (--reason)", reason)?;
    let transaction = begin_mutation(connection)?;
    if let Some(existing) = cancellation_request(&transaction, target, execution_id)? {
        if existing.reason != reason {
            bail!(
                "execution '{execution_id}' already has a cancellation request; replay its exact --reason or inspect its recorded request"
            );
        }
        return Ok(existing);
    }
    let request = CancellationRequest {
        target,
        execution_id: execution_id.to_owned(),
        actor: actor.to_owned(),
        reason: reason.to_owned(),
        requested_at: now(),
    };
    transaction.execute(
        &format!("INSERT INTO cancellation_requests ({}, actor, reason, requested_at) VALUES (?1, ?2, ?3, ?4)", target.column()),
        params![execution_id, actor, reason, request.requested_at],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        target.entity(),
        execution_id,
        "cancellation-requested",
        Some(reason),
        Some(&serde_json::to_value(&request)?),
    )?;
    transaction.commit()?;
    Ok(request)
}
