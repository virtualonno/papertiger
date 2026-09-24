//! Explicit quarantine preserves untrusted legacy history without inventing it.
//!
//! Original rows remain immutable. Ordinary history uses `canonical_events`;
//! each excluded row is represented by an ordinary, exportable recovery event
//! containing every original field (including the exact raw payload string).
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

const INSPECTION_SCHEMA: &str = "papertiger.history_inspection.v2";
const QUARANTINE_SCHEMA: &str = "papertiger.history_quarantine.v2";
/// Envelope id written by releases before 0.18; stored events are immutable,
/// so readers keep accepting it.
const QUARANTINE_SCHEMA_V1: &str = "papertiger.history-quarantine.v1";

fn quarantine_schema(value: &serde_json::Value) -> bool {
    matches!(
        value.as_str(),
        Some(QUARANTINE_SCHEMA | QUARANTINE_SCHEMA_V1)
    )
}

pub(crate) const VIEW: &str = "CREATE VIEW canonical_events AS SELECT events.* FROM events WHERE NOT EXISTS (SELECT 1 FROM event_quarantines q WHERE q.event_id=events.event_id)";
pub(crate) const IMMUTABILITY: &str = r#"
CREATE TRIGGER event_quarantines_append_only_update BEFORE UPDATE ON event_quarantines
BEGIN SELECT RAISE(ABORT, 'papertiger history quarantine is append-only; inspect with papertiger history inspect <event-id>'); END;
CREATE TRIGGER event_quarantines_append_only_delete BEFORE DELETE ON event_quarantines
BEGIN SELECT RAISE(ABORT, 'papertiger history quarantine is append-only; inspect with papertiger history inspect <event-id>'); END;
"#;
const TABLE: &str = "
CREATE TABLE event_quarantines (
  event_id INTEGER PRIMARY KEY REFERENCES events(event_id),
  recovery_event_id INTEGER NOT NULL UNIQUE REFERENCES events(event_id),
  CHECK (event_id < recovery_event_id)
);
";

pub(crate) fn install_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(TABLE)?;
    conn.execute_batch(VIEW)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawEvent {
    pub event_id: i64,
    pub at: String,
    pub actor: String,
    pub entity: String,
    pub entity_id: Option<i64>,
    pub entity_plan: Option<String>,
    pub entity_seq: Option<i64>,
    pub gate_name: Option<String>,
    pub kind: String,
    pub why: Option<String>,
    /// Stored text, deliberately not parsed or normalized as JSON.
    pub payload: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HistoryInspection {
    pub schema: &'static str,
    pub original: RawEvent,
    pub sha256: String,
    pub problems: Vec<String>,
    pub recovery_event_id: Option<i64>,
}

pub fn inspect(conn: &Connection, event_id: i64) -> Result<HistoryInspection> {
    let original = conn.query_row(
        "SELECT event_id,at,actor,entity,entity_id,entity_plan,entity_seq,gate_name,kind,why,payload FROM events WHERE event_id=?1",
        [event_id], |r| Ok(RawEvent {
            event_id: r.get(0)?, at: r.get(1)?, actor: r.get(2)?, entity: r.get(3)?, entity_id: r.get(4)?,
            entity_plan: r.get(5)?, entity_seq: r.get(6)?, gate_name: r.get(7)?, kind: r.get(8)?, why: r.get(9)?, payload: r.get(10)?,
        }),
    ).optional()?.with_context(|| format!("event {event_id} does not exist; use `papertiger log --json` to select a stored event"))?;
    let sha256 = crate::digest::sha256(&serde_json::to_vec(&original)?);
    let mut problems = Vec::new();
    if chrono::DateTime::parse_from_rfc3339(&original.at).is_err() {
        problems
            .push("timestamp has no trustworthy RFC3339 instant; no timezone is inferred".into());
    }
    if let Some(raw) = &original.payload
        && serde_json::from_str::<serde_json::Value>(raw).is_err()
    {
        problems.push("payload is not JSON; original text is retained verbatim".into());
    }
    if original.actor.trim().is_empty() || original.kind.trim().is_empty() {
        problems.push("actor or event kind is blank".into());
    }
    if !["plan", "task", "dep", "gate"].contains(&original.entity.as_str()) {
        problems.push("unknown event entity".into());
    }
    if matches!(original.entity.as_str(), "task" | "dep" | "gate") {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE seq=?1)",
            [original.entity_seq],
            |r| r.get(0),
        )?;
        if !exists
            || original.entity_plan.is_none()
            || (original.entity == "gate" && original.gate_name.is_none())
        {
            problems.push(
                "stable task/plan/gate reference is missing; no association is inferred".into(),
            );
        }
    }
    if let Some(plan) = &original.entity_plan {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM plans WHERE slug=?1)",
            [plan],
            |r| r.get(0),
        )?;
        if !exists {
            problems.push(
                "stored plan slug is absent; no plan is inferred from a numeric ID or current task"
                    .into(),
            );
        }
    }
    let recovery_event_id = conn
        .query_row(
            "SELECT recovery_event_id FROM event_quarantines WHERE event_id=?1",
            [event_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(HistoryInspection {
        schema: INSPECTION_SCHEMA,
        original,
        sha256,
        problems,
        recovery_event_id,
    })
}

pub(crate) fn validate_envelope(value: Option<&serde_json::Value>) -> Result<()> {
    let value =
        value.context("quarantine_event lacks its evidence envelope; restore a verified export")?;
    let original: RawEvent = serde_json::from_value(value["original"].clone())
        .context("quarantine_event original fields are incomplete; restore a verified export")?;
    let digest = crate::digest::sha256(&serde_json::to_vec(&original)?);
    if !quarantine_schema(&value["schema"]) || value["source_event_sha256"] != digest {
        bail!("quarantine_event evidence digest or schema is invalid; restore a verified export");
    }
    Ok(())
}

pub(crate) fn audit_structure(conn: &Connection) -> Result<Vec<crate::AuditFinding>> {
    let mut stmt = conn.prepare(
        "SELECT event_id FROM canonical_events e WHERE trim(actor)='' OR trim(kind)=''
         OR entity NOT IN ('plan','task','dep','gate')
         OR (entity IN ('task','dep','gate') AND
             (entity_plan IS NULL OR entity_seq IS NULL OR NOT EXISTS(SELECT 1 FROM tasks t WHERE t.seq=e.entity_seq)))
         OR (entity='gate' AND gate_name IS NULL)
         OR (entity_plan IS NOT NULL AND NOT EXISTS(SELECT 1 FROM plans p WHERE p.slug=e.entity_plan)) ORDER BY event_id",
    )?;
    let mut findings = Vec::new();
    for id in stmt.query_map([], |r| r.get::<_, i64>(0))? {
        let id = id?;
        findings.push(crate::AuditFinding {
            kind: "invalid_event_identity".into(),
            detail: format!("event {id} has an invalid actor, kind, entity or stable reference; run `papertiger history inspect {id}` and preserve a backup before recovery"),
        });
    }
    Ok(findings)
}

pub fn quarantine(
    conn: &Connection,
    actor: &str,
    event_id: i64,
    expected_sha256: &str,
    why: &str,
) -> Result<i64> {
    crate::digest::validate_sha256(expected_sha256, "--expect-sha256")?;
    if actor.trim().is_empty() || why.trim().is_empty() {
        bail!("history quarantine requires a nonblank actor and --why <reason>");
    }
    let tx = crate::begin_mutation(conn)?;
    let inspected = inspect(&tx, event_id)?;
    if inspected.sha256 != expected_sha256 {
        bail!(
            "event {event_id} changed since inspection; run `papertiger history inspect {event_id}` and review the exact record again"
        );
    }
    if let Some(recovery) = inspected.recovery_event_id {
        bail!(
            "event {event_id} is already quarantined by recovery event {recovery}; inspect it with `papertiger history inspect {event_id}`"
        );
    }
    if inspected.problems.is_empty() {
        bail!(
            "event {event_id} has no supported structural defect; record disagreement with `papertiger note --text <explanation>` instead of quarantining valid history"
        );
    }
    let payload = serde_json::json!({
        "schema": QUARANTINE_SCHEMA,
        "disposition": "untrusted historical evidence; no timestamp, task association, or task state is inferred",
        "source_event_sha256": inspected.sha256,
        "original": inspected.original,
        "problems": inspected.problems,
    });
    crate::record_event_in_mutation(
        &tx,
        actor,
        "plan",
        None,
        "quarantine_event",
        Some(why),
        Some(&payload),
    )?;
    let recovery = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO event_quarantines (event_id,recovery_event_id) VALUES (?1,?2)",
        params![event_id, recovery],
    )?;
    tx.commit()?;
    Ok(recovery)
}

/// A mapping may exclude a row only while its complete evidence envelope agrees.
pub(crate) fn validate(conn: &Connection) -> Result<()> {
    let view: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='view' AND name='canonical_events'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if view.as_deref() != Some(VIEW) {
        bail!(
            "Papertiger canonical history view is missing or altered, so history reads cannot be trusted; stop direct SQLite access, then run `papertiger repair-guards --why <reason>` to reinstall it with an audited record of what changed"
        );
    }
    let mut envelopes =
        conn.prepare("SELECT payload FROM canonical_events WHERE kind='quarantine_event'")?;
    for raw in envelopes.query_map([], |r| r.get::<_, Option<String>>(0))? {
        let parsed = raw?
            .map(|s| serde_json::from_str::<serde_json::Value>(&s))
            .transpose()?;
        validate_envelope(parsed.as_ref())?;
    }
    let mut stmt = conn.prepare("SELECT q.event_id,q.recovery_event_id,e.kind,e.payload FROM event_quarantines q LEFT JOIN events e ON e.event_id=q.recovery_event_id")?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })? {
        let (original_id, recovery_id, kind, payload) = row?;
        let inspected = inspect(conn, original_id)?;
        let value = payload
            .as_deref()
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()?;
        let valid = value.as_ref().is_some_and(|v| {
            quarantine_schema(&v["schema"])
                && v["source_event_sha256"] == inspected.sha256
                && serde_json::from_value::<RawEvent>(v["original"].clone())
                    .ok()
                    .as_ref()
                    == Some(&inspected.original)
        });
        if kind.as_deref() != Some("quarantine_event")
            || !valid
            || original_id >= recovery_id
            || inspected.problems.is_empty()
        {
            bail!(
                "event {original_id} quarantine evidence does not match recovery event {recovery_id}; preserve with `papertiger backup --output <new-path>` and restore a verified authority"
            );
        }
    }
    Ok(())
}
