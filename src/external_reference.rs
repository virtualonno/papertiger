//! Inward reference archaeology. Locators never import external lifecycle state.

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::{
    begin_mutation, get_task, now, record_event_in_mutation, require_nonblank,
    validate_evidence_locator, validate_optional_sha256,
};

pub const REFERENCE_KINDS: [&str; 6] = ["pull_request", "issue", "review", "adr", "input", "other"];
pub(crate) const REFERENCE_SCHEMA: &str = "
CREATE TABLE external_references (
    reference_id INTEGER PRIMARY KEY,
    task_id INTEGER NOT NULL REFERENCES tasks(task_id),
    kind TEXT NOT NULL CHECK(kind IN ('pull_request','issue','review','adr','input','other')),
    locator TEXT NOT NULL,
    sha256 TEXT,
    note TEXT,
    recorded_at TEXT NOT NULL,
    UNIQUE(task_id, kind, locator)
);
CREATE INDEX idx_external_references_locator ON external_references(locator, task_id);";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalReference {
    pub kind: String,
    pub locator: String,
    pub sha256: Option<String>,
    pub note: Option<String>,
    pub recorded_at: String,
}

impl ExternalReference {
    pub(crate) fn validate(&self) -> Result<()> {
        validate_identity(&self.kind, &self.locator)?;
        validate_optional_sha256(self.sha256.as_deref())?;
        if self
            .note
            .as_deref()
            .is_some_and(|note| note.trim().is_empty())
        {
            bail!("reference note must be nonblank; omit --note for no annotation");
        }
        let at = chrono::DateTime::parse_from_rfc3339(&self.recorded_at)?;
        if at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true) != self.recorded_at {
            bail!(
                "reference recorded_at must be canonical UTC RFC3339 with milliseconds; restore a verified export"
            );
        }
        Ok(())
    }
}

fn validate_identity(kind: &str, locator: &str) -> Result<()> {
    if !REFERENCE_KINDS.contains(&kind) {
        bail!(
            "unknown reference kind '{kind}'; supply --kind {}",
            REFERENCE_KINDS.join("|")
        );
    }
    if locator.len() > 4096
        || locator.trim() != locator
        || locator.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        bail!(
            "reference locator must be an exact scheme-qualified locator of at most 4096 bytes without whitespace; percent-encode spaces in the supplied locator"
        );
    }
    validate_evidence_locator(locator)
}

pub fn add_external_reference(
    conn: &Connection,
    actor: &str,
    seq: i64,
    reference: &ExternalReference,
) -> Result<()> {
    reference.validate()?;
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    insert_reference(&tx, task.task_id, reference)?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "reference_add",
        reference.note.as_deref(),
        Some(&serde_json::json!({"seq":seq,"reference":reference})),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn new_external_reference(
    kind: &str,
    locator: &str,
    sha256: Option<&str>,
    note: Option<&str>,
) -> Result<ExternalReference> {
    let reference = ExternalReference {
        kind: kind.into(),
        locator: locator.into(),
        sha256: sha256.map(str::to_owned),
        note: note.map(str::to_owned),
        recorded_at: now(),
    };
    reference.validate()?;
    Ok(reference)
}

pub(crate) fn insert_reference(
    conn: &Connection,
    task_id: i64,
    reference: &ExternalReference,
) -> Result<()> {
    reference.validate()?;
    let count = conn.execute("INSERT INTO external_references(task_id,kind,locator,sha256,note,recorded_at) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(task_id,kind,locator) DO NOTHING",params![task_id,reference.kind,reference.locator,reference.sha256,reference.note,reference.recorded_at])?;
    if count == 0 {
        bail!(
            "reference {} '{}' already exists on this task; inspect `papertiger reference list <task>` and use `reference remove <task> <locator> --kind <kind> --why <reason>` before rebinding",
            reference.kind,
            reference.locator
        );
    }
    Ok(())
}

pub fn remove_external_reference(
    conn: &Connection,
    actor: &str,
    seq: i64,
    kind: &str,
    locator: &str,
    why: &str,
) -> Result<()> {
    validate_identity(kind, locator)?;
    let why = require_nonblank("reference removal --why", why)?;
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let reference = external_references(&tx,seq)?.into_iter().find(|item| item.kind == kind && item.locator == locator).ok_or_else(|| anyhow::anyhow!("reference {kind} '{locator}' is absent from #{seq}; inspect `papertiger reference list {seq}`"))?;
    tx.execute(
        "DELETE FROM external_references WHERE task_id=?1 AND kind=?2 AND locator=?3",
        params![task.task_id, kind, locator],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "reference_remove",
        Some(why),
        Some(&serde_json::json!({"seq":seq,"reference":reference})),
    )?;
    tx.commit()?;
    Ok(())
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExternalReference> {
    Ok(ExternalReference {
        kind: row.get(0)?,
        locator: row.get(1)?,
        sha256: row.get(2)?,
        note: row.get(3)?,
        recorded_at: row.get(4)?,
    })
}

pub fn external_references(conn: &Connection, seq: i64) -> Result<Vec<ExternalReference>> {
    let task = get_task(conn, seq)?;
    let mut statement = conn.prepare("SELECT kind,locator,sha256,note,recorded_at FROM external_references WHERE task_id=?1 ORDER BY kind,locator")?;
    let references = statement
        .query_map(params![task.task_id], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for reference in &references {
        reference.validate()?;
    }
    Ok(references)
}

#[derive(Debug, Serialize)]
pub struct ReferenceMatch {
    pub task_seq: i64,
    pub task_title: String,
    pub reference: ExternalReference,
}

pub fn find_external_references(conn: &Connection, locator: &str) -> Result<Vec<ReferenceMatch>> {
    validate_identity("other", locator)?;
    let mut statement = conn.prepare("SELECT r.kind,r.locator,r.sha256,r.note,r.recorded_at,t.seq,t.title FROM external_references r JOIN tasks t ON t.task_id=r.task_id WHERE r.locator=?1 ORDER BY t.seq,r.kind")?;
    let matches = statement
        .query_map(params![locator], |row| {
            Ok(ReferenceMatch {
                reference: from_row(row)?,
                task_seq: row.get(5)?,
                task_title: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for item in &matches {
        item.reference.validate()?;
    }
    Ok(matches)
}
