use std::collections::HashMap;

use crate::{MisePlannerProjection, MisePlannerProjectionSummary};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{begin_mutation, get_task, now, record_event_in_mutation};

pub(crate) const MISE_PROJECTION_SCHEMA_V4: &str = r#"
CREATE TABLE task_mise_projections (
  projection_sha256 TEXT PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  campaign_id TEXT NOT NULL,
  manifest_sha256 TEXT NOT NULL,
  candidate_id TEXT NOT NULL UNIQUE,
  nomination_id TEXT UNIQUE,
  disposition TEXT NOT NULL
    CHECK (disposition IN ('nominated','rejected','inconclusive','infrastructure_failed')),
  projection_json TEXT NOT NULL,
  recorded_by TEXT NOT NULL,
  recorded_at TEXT NOT NULL
);
CREATE INDEX idx_task_mise_projections_task ON task_mise_projections(task_id, recorded_at);
CREATE TRIGGER task_mise_projections_no_update BEFORE UPDATE ON task_mise_projections
BEGIN SELECT RAISE(ABORT, 'Mise projection history is immutable'); END;
CREATE TRIGGER task_mise_projections_no_delete BEFORE DELETE ON task_mise_projections
BEGIN SELECT RAISE(ABORT, 'Mise projection history is immutable'); END;
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MiseProjectionRecordOutcome {
    Recorded,
    Existing,
}

impl MiseProjectionRecordOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Existing => "existing",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TaskMiseProjection {
    pub projection_sha256: String,
    pub task_seq: i64,
    pub projection: MisePlannerProjection,
    pub recorded_by: String,
    pub recorded_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TaskMiseProjectionSummary {
    pub projection_sha256: String,
    pub task_seq: i64,
    #[serde(flatten)]
    pub projection: MisePlannerProjectionSummary,
    pub recorded_by: String,
    pub recorded_at: String,
}

impl TaskMiseProjection {
    pub fn summary(&self) -> TaskMiseProjectionSummary {
        TaskMiseProjectionSummary {
            projection_sha256: self.projection_sha256.clone(),
            task_seq: self.task_seq,
            projection: self.projection.summary(),
            recorded_by: self.recorded_by.clone(),
            recorded_at: self.recorded_at.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiseProjectionDump {
    pub task_seq: i64,
    pub projection_sha256: String,
    pub projection: MisePlannerProjection,
    pub recorded_by: String,
    pub recorded_at: String,
}

pub fn parse_mise_planner_projection(bytes: &[u8]) -> Result<MisePlannerProjection> {
    let projection: MisePlannerProjection =
        serde_json::from_slice(bytes).context("parse Mise planner projection JSON")?;
    projection.validate()?;
    Ok(projection)
}

pub fn record_mise_projection(
    connection: &Connection,
    actor: &str,
    task_seq: i64,
    bytes: &[u8],
) -> Result<(MiseProjectionRecordOutcome, TaskMiseProjection)> {
    if actor.trim().is_empty() {
        bail!("recording a Mise projection requires a nonblank actor");
    }
    let projection = parse_mise_planner_projection(bytes)?;
    let projection_sha256 = projection.projection_sha256()?;
    let projection_json = serde_json::to_string(&projection)?;
    let transaction = begin_mutation(connection)?;
    let task = get_task(&transaction, task_seq)?;

    if let Some(existing_task_id) = transaction
        .query_row(
            "SELECT task_id FROM task_mise_projections WHERE projection_sha256=?1",
            params![projection_sha256],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    {
        if existing_task_id == task.task_id {
            let record = mise_projection(&transaction, &projection_sha256)?
                .context("existing Mise projection disappeared during replay")?;
            transaction.commit()?;
            return Ok((MiseProjectionRecordOutcome::Existing, record));
        }
        let existing_task_seq: i64 = transaction.query_row(
            "SELECT seq FROM tasks WHERE task_id=?1",
            params![existing_task_id],
            |row| row.get(0),
        )?;
        bail!(
            "candidate '{}' is already projected as {} on task #{}; inspect it with `papertiger mise show {}`",
            projection.candidate_id,
            projection_sha256,
            existing_task_seq,
            projection_sha256
        );
    }
    if let Some((existing_sha256, existing_task_id)) = transaction
        .query_row(
            "SELECT projection_sha256, task_id FROM task_mise_projections WHERE candidate_id=?1",
            params![projection.candidate_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?
    {
        let existing_task_seq: i64 = transaction.query_row(
            "SELECT seq FROM tasks WHERE task_id=?1",
            params![existing_task_id],
            |row| row.get(0),
        )?;
        bail!(
            "candidate '{}' is already projected as {} on task #{}; inspect it with `papertiger mise show {}`",
            projection.candidate_id,
            existing_sha256,
            existing_task_seq,
            existing_sha256
        );
    }

    let recorded_at = now();
    transaction.execute(
        "INSERT INTO task_mise_projections
         (projection_sha256, task_id, campaign_id, manifest_sha256, candidate_id,
          nomination_id, disposition, projection_json, recorded_by, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            projection_sha256,
            task.task_id,
            projection.campaign_id,
            projection.manifest_sha256,
            projection.candidate_id,
            projection.nomination_id,
            projection.disposition.as_str(),
            projection_json,
            actor,
            recorded_at,
        ],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "task",
        Some(task.task_id),
        "mise_projection",
        Some("recorded verified terminal Mise evidence; no task or gate state changed"),
        Some(&serde_json::json!({
            "projection_sha256": projection_sha256,
            "campaign_id": projection.campaign_id,
            "candidate_id": projection.candidate_id,
            "nomination_id": projection.nomination_id,
            "disposition": projection.disposition,
        })),
    )?;
    transaction.commit()?;
    let record = mise_projection(connection, &projection_sha256)?
        .context("recorded Mise projection disappeared after commit")?;
    Ok((MiseProjectionRecordOutcome::Recorded, record))
}

pub fn mise_projection(
    connection: &Connection,
    projection_sha256: &str,
) -> Result<Option<TaskMiseProjection>> {
    let row = connection
        .query_row(
            "SELECT p.projection_sha256, t.seq, p.campaign_id, p.manifest_sha256,
                    p.candidate_id, p.nomination_id, p.disposition, p.projection_json,
                    p.recorded_by, p.recorded_at
               FROM task_mise_projections p
               JOIN tasks t ON t.task_id=p.task_id
              WHERE p.projection_sha256=?1",
            params![projection_sha256],
            |row| {
                Ok(StoredProjectionRow {
                    projection_sha256: row.get(0)?,
                    task_seq: row.get(1)?,
                    campaign_id: row.get(2)?,
                    manifest_sha256: row.get(3)?,
                    candidate_id: row.get(4)?,
                    nomination_id: row.get(5)?,
                    disposition: row.get(6)?,
                    projection_json: row.get(7)?,
                    recorded_by: row.get(8)?,
                    recorded_at: row.get(9)?,
                })
            },
        )
        .optional()?;
    row.map(verify_stored_projection).transpose()
}

pub fn task_mise_projections(
    connection: &Connection,
    task_seq: i64,
) -> Result<Vec<TaskMiseProjection>> {
    get_task(connection, task_seq)?;
    let mut statement = connection.prepare(
        "SELECT projection_sha256 FROM task_mise_projections p
          JOIN tasks t ON t.task_id=p.task_id
         WHERE t.seq=?1 ORDER BY p.recorded_at, p.projection_sha256",
    )?;
    let ids = statement
        .query_map(params![task_seq], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.into_iter()
        .map(|id| {
            mise_projection(connection, &id)?
                .with_context(|| format!("Mise projection '{id}' disappeared during read"))
        })
        .collect()
}

pub fn task_mise_projection_summaries(
    connection: &Connection,
    task_seq: i64,
) -> Result<Vec<TaskMiseProjectionSummary>> {
    Ok(task_mise_projections(connection, task_seq)?
        .into_iter()
        .map(|record| record.summary())
        .collect())
}

#[derive(Debug)]
struct StoredProjectionRow {
    projection_sha256: String,
    task_seq: i64,
    campaign_id: String,
    manifest_sha256: String,
    candidate_id: String,
    nomination_id: Option<String>,
    disposition: String,
    projection_json: String,
    recorded_by: String,
    recorded_at: String,
}

fn verify_stored_projection(row: StoredProjectionRow) -> Result<TaskMiseProjection> {
    let projection = parse_mise_planner_projection(row.projection_json.as_bytes())?;
    let actual_sha256 = projection.projection_sha256()?;
    if actual_sha256 != row.projection_sha256
        || projection.campaign_id != row.campaign_id
        || projection.manifest_sha256 != row.manifest_sha256
        || projection.candidate_id != row.candidate_id
        || projection.nomination_id != row.nomination_id
        || projection.disposition.as_str() != row.disposition
    {
        bail!(
            "stored Mise projection '{}' columns disagree with its revalidated payload",
            row.projection_sha256
        );
    }
    if row.recorded_by.trim().is_empty() || row.recorded_at.trim().is_empty() {
        bail!(
            "stored Mise projection '{}' has blank recording provenance",
            row.projection_sha256
        );
    }
    chrono::DateTime::parse_from_rfc3339(&row.recorded_at).with_context(|| {
        format!(
            "stored Mise projection '{}' has invalid recorded_at",
            row.projection_sha256
        )
    })?;
    Ok(TaskMiseProjection {
        projection_sha256: row.projection_sha256,
        task_seq: row.task_seq,
        projection,
        recorded_by: row.recorded_by,
        recorded_at: row.recorded_at,
    })
}

pub(crate) fn export_mise_projections(
    connection: &Connection,
    selected_task_sequences: &std::collections::HashSet<i64>,
) -> Result<Vec<MiseProjectionDump>> {
    let mut statement = connection.prepare(
        "SELECT projection_sha256, t.seq FROM task_mise_projections p
          JOIN tasks t ON t.task_id=p.task_id ORDER BY p.recorded_at, projection_sha256",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .filter(|(_, task_seq)| selected_task_sequences.contains(task_seq))
        .map(|(projection_sha256, _)| {
            let record = mise_projection(connection, &projection_sha256)?.with_context(|| {
                format!("Mise projection '{projection_sha256}' disappeared during export")
            })?;
            Ok(MiseProjectionDump {
                task_seq: record.task_seq,
                projection_sha256: record.projection_sha256,
                projection: record.projection,
                recorded_by: record.recorded_by,
                recorded_at: record.recorded_at,
            })
        })
        .collect()
}

pub(crate) fn import_mise_projections(
    transaction: &Transaction<'_>,
    projections: &[MiseProjectionDump],
    imported_task_id_by_seq: &HashMap<i64, i64>,
) -> Result<()> {
    for dump in projections {
        let task_id = imported_task_id_by_seq
            .get(&dump.task_seq)
            .with_context(|| {
                format!(
                    "import Mise projection '{}' names task #{}, which is absent from the dump",
                    dump.projection_sha256, dump.task_seq
                )
            })?;
        dump.projection.validate()?;
        let actual_sha256 = dump.projection.projection_sha256()?;
        if actual_sha256 != dump.projection_sha256 {
            bail!(
                "import Mise projection '{}' payload hashes to '{}'",
                dump.projection_sha256,
                actual_sha256
            );
        }
        if dump.recorded_by.trim().is_empty() || dump.recorded_at.trim().is_empty() {
            bail!("import Mise projection has blank recording provenance");
        }
        chrono::DateTime::parse_from_rfc3339(&dump.recorded_at)
            .context("import Mise projection recorded_at is not RFC3339")?;
        transaction.execute(
            "INSERT INTO task_mise_projections
             (projection_sha256, task_id, campaign_id, manifest_sha256, candidate_id,
              nomination_id, disposition, projection_json, recorded_by, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                dump.projection_sha256,
                task_id,
                dump.projection.campaign_id,
                dump.projection.manifest_sha256,
                dump.projection.candidate_id,
                dump.projection.nomination_id,
                dump.projection.disposition.as_str(),
                serde_json::to_string(&dump.projection)?,
                dump.recorded_by,
                dump.recorded_at,
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn audit_mise_projections(connection: &Connection) -> Result<Vec<(String, String)>> {
    let mut statement = connection.prepare(
        "SELECT projection_sha256 FROM task_mise_projections ORDER BY projection_sha256",
    )?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids
        .into_iter()
        .filter_map(|id| match mise_projection(connection, &id) {
            Ok(Some(_)) => None,
            Ok(None) => Some((
                "missing_mise_projection".to_owned(),
                format!("Mise projection '{id}' disappeared during audit"),
            )),
            Err(error) => Some((
                "invalid_mise_projection".to_owned(),
                format!("Mise projection '{id}' failed reverification: {error:#}"),
            )),
        })
        .collect())
}
