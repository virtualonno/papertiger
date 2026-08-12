use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{
    Connection, ErrorCode, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

mod digest;
pub use digest::{sha256, sha256_bytes, validate_sha256};
mod atomic_file;
pub use atomic_file::{atomic_create_file, atomic_replace_file};
mod export_file;
pub use export_file::{ExportFileReceipt, write_export_file};
mod path_identity;
pub use path_identity::portable_absolute;
mod read_model;
pub use read_model::{
    ActivityEvent, AuthorityInfo, EventCursor, EventLog, EventRecord, PlanStatus, StatusReadyTask,
    StatusResponse, StatusTask, TaskActivity, TaskCounts, TaskListItem, TaskListResponse,
    TaskSummary, authority_info, event_cursor, event_head, event_log, status_response,
    task_activity, task_list_response,
};
mod search;
pub use search::{SearchExcerpt, SearchHit, SearchResponse, search_tasks};
mod commit_association;
pub use commit_association::{
    CommitAssociation, CommitAssociationMatch, add_commit_association, commit_associations,
    find_commit_associations, remove_commit_association,
};
mod mise_projection;
mod mise_projection_contract;
pub use mise_projection::{
    MiseProjectionDump, MiseProjectionRecordOutcome, TaskMiseProjection, TaskMiseProjectionSummary,
    mise_projection, parse_mise_planner_projection, record_mise_projection,
    task_mise_projection_summaries, task_mise_projections,
};
pub use mise_projection_contract::{
    MISE_PLANNER_PROJECTION_SCHEMA_V1, MiseBudgetProjection, MiseMutationProjection,
    MisePlannerProjection, MisePlannerProjectionSummary, MiseProjectionDisposition,
    MiseSourceProjection,
};

pub const SCHEMA_VERSION: i64 = 6;
const SQLITE_LOCK_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

pub const TASK_STATUSES: [&str; 5] = ["proposed", "in_progress", "done", "retired", "rejected"];
pub const TASK_KINDS: [&str; 3] = ["work", "probe", "decision"];
pub const GATE_KINDS: [&str; 8] = [
    "test",
    "benchmark",
    "review",
    "capture",
    "fixture",
    "build",
    "doc",
    "other",
];
pub fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn configure_connection(conn: Connection) -> Result<Connection> {
    conn.busy_timeout(SQLITE_LOCK_GRACE)
        .context("configure papertiger SQLite lock grace")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// Replace any SQLite lock error in an anyhow context chain with Papertiger's
/// single operator-facing retry refusal. SQLite performs the bounded wait;
/// Papertiger never replays the command.
pub fn normalize_sqlite_lock_error(error: anyhow::Error) -> anyhow::Error {
    let locked = error.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code)
            .is_some_and(|code| matches!(code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked))
    });
    if locked {
        anyhow!(
            "papertiger SQLite lock admission refused after a {}ms grace: the authority is still locked; retry the command after the current database operation finishes",
            SQLITE_LOCK_GRACE.as_millis()
        )
    } else {
        error
    }
}

pub fn open_for_init(path: &str) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("open {path} for initialization"))?;
    configure_connection(conn)
}

pub fn open_existing(path: &str) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("open existing papertiger database {path}"))?;
    let conn = configure_connection(conn)?;
    validate_existing(&conn, path)?;
    Ok(conn)
}

/// Open an initialized Papertiger authority without granting mutation access.
/// Evidence consumers such as papertiger-mise use this to verify an exact
/// promotion gate while preserving Papertiger's independent ownership.
pub fn open_existing_read_only(path: &str) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open existing papertiger database read-only {path}"))?;
    let conn = configure_connection(conn)?;
    validate_existing(&conn, path)?;
    Ok(conn)
}

fn validate_existing(conn: &Connection, path: &str) -> Result<()> {
    let has_meta: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !has_meta {
        bail!(
            "{path} is not an initialized papertiger database; run `papertiger --db {path} init`"
        );
    }
    let version = schema_version(conn)?;
    if version != SCHEMA_VERSION {
        bail!(
            "{path} uses papertiger schema v{version}; run `papertiger --db {path} init` explicitly to upgrade to v{SCHEMA_VERSION}"
        );
    }
    Ok(())
}

fn schema_version(conn: &Connection) -> Result<i64> {
    let raw: String = conn.query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    raw.parse()
        .map_err(|_| anyhow!("corrupt schema_version '{raw}'"))
}

/// Reserve SQLite's writer slot within the connection-wide lock grace. The
/// command itself is never replayed. Dropping the returned transaction rolls
/// the logical mutation back and releases the reservation.
pub fn begin_mutation(conn: &Connection) -> Result<Transaction<'_>> {
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .context("begin papertiger mutation")
}

pub fn init(conn: &Connection) -> Result<()> {
    let has_meta: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if has_meta {
        migrate(conn, schema_version(conn)?)?;
        return Ok(());
    }
    let initial_page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let tx = begin_mutation(conn)?;
    let foreign_object: Option<(String, String)> = tx
        .query_row(
            "SELECT type, name FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name
             LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((object_type, name)) = foreign_object {
        bail!(
            "refusing to initialize nonempty database without papertiger metadata: found {object_type} '{name}'"
        );
    }
    if initial_page_count != 0 {
        bail!(
            "refusing to initialize nonempty database without papertiger metadata: found {initial_page_count} allocated page(s)"
        );
    }
    tx.execute_batch(
        r#"
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE plans (
  plan_id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL UNIQUE,
  title TEXT NOT NULL,
  intent TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','paused','done','retired')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE tasks (
  task_id INTEGER PRIMARY KEY,
  seq INTEGER NOT NULL UNIQUE,
  plan_id INTEGER NOT NULL REFERENCES plans(plan_id),
  parent_id INTEGER REFERENCES tasks(task_id),
  replacement_task_id INTEGER REFERENCES tasks(task_id),
  title TEXT NOT NULL,
  intent TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL DEFAULT 'work' CHECK (kind IN ('work','probe','decision')),
  result TEXT,
  status TEXT NOT NULL DEFAULT 'proposed'
    CHECK (status IN ('proposed','in_progress','done','retired','rejected')),
  priority INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE task_tags (
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  tag TEXT NOT NULL,
  UNIQUE (task_id, tag)
);
CREATE TABLE deps (
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  depends_on INTEGER NOT NULL REFERENCES tasks(task_id),
  UNIQUE (task_id, depends_on),
  CHECK (task_id <> depends_on)
);
CREATE TABLE gates (
  gate_id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  name TEXT NOT NULL,
  kind TEXT NOT NULL
    CHECK (kind IN ('test','benchmark','review','capture','fixture','build','doc','other')),
  requirement TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','closed','waived')),
  evidence_locator TEXT,
  evidence_sha256 TEXT,
  note TEXT,
  closed_at TEXT,
  UNIQUE (task_id, name)
);
CREATE TABLE task_blockers (
  blocker_id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  name TEXT NOT NULL,
  reason TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','waived')),
  evidence_locator TEXT,
  evidence_sha256 TEXT,
  note TEXT,
  resolved_at TEXT,
  UNIQUE (task_id, name)
);
CREATE TABLE events (
  event_id INTEGER PRIMARY KEY,
  at TEXT NOT NULL,
  actor TEXT NOT NULL,
  entity TEXT NOT NULL,
  entity_id INTEGER,
  entity_plan TEXT,
  entity_seq INTEGER,
  gate_name TEXT,
  kind TEXT NOT NULL,
  why TEXT,
  payload TEXT
);
CREATE TABLE commit_associations (
  commit_association_id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  repository TEXT NOT NULL,
  commit_oid TEXT NOT NULL,
  note TEXT,
  recorded_at TEXT NOT NULL,
  UNIQUE (task_id, repository, commit_oid)
);
CREATE INDEX idx_tasks_plan ON tasks(plan_id);
CREATE INDEX idx_events_entity ON events(entity, entity_id);
CREATE INDEX idx_events_entity_seq ON events(entity_seq, event_id);
CREATE INDEX idx_commit_associations_lookup ON commit_associations(repository, commit_oid);
"#,
    )?;
    tx.execute_batch(mise_projection::MISE_PROJECTION_SCHEMA_V4)?;
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

fn migrate(conn: &Connection, from: i64) -> Result<()> {
    if from > SCHEMA_VERSION {
        bail!(
            "database schema v{from} is newer than this papertiger (v{SCHEMA_VERSION}); upgrade the tool"
        );
    }
    if from == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = begin_mutation(conn)?;
    let mut version = from;
    if version == 1 {
        tx.execute_batch(
            r#"
ALTER TABLE tasks
  ADD COLUMN kind TEXT NOT NULL DEFAULT 'work'
  CHECK (kind IN ('work','probe','decision'));
ALTER TABLE tasks ADD COLUMN result TEXT;
CREATE TABLE task_blockers (
  blocker_id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  name TEXT NOT NULL,
  reason TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','waived')),
  evidence_locator TEXT,
  evidence_sha256 TEXT,
  note TEXT,
  resolved_at TEXT,
  UNIQUE (task_id, name)
);
"#,
        )?;
        version = 2;
    }
    if version == 2 {
        tx.execute_batch(
            r#"
ALTER TABLE events ADD COLUMN entity_plan TEXT;
ALTER TABLE events ADD COLUMN entity_seq INTEGER;
ALTER TABLE events ADD COLUMN gate_name TEXT;

UPDATE events
   SET entity_plan=(SELECT p.slug FROM plans p WHERE p.plan_id=events.entity_id)
 WHERE entity='plan' AND entity_id IS NOT NULL;

UPDATE events
   SET entity_seq=(SELECT t.seq FROM tasks t WHERE t.task_id=events.entity_id),
       entity_plan=(SELECT p.slug FROM tasks t JOIN plans p ON p.plan_id=t.plan_id
                     WHERE t.task_id=events.entity_id)
 WHERE entity IN ('task','dep') AND entity_id IS NOT NULL;

UPDATE events
   SET entity_seq=(SELECT t.seq FROM gates g JOIN tasks t ON t.task_id=g.task_id
                    WHERE g.gate_id=events.entity_id),
       entity_plan=(SELECT p.slug FROM gates g JOIN tasks t ON t.task_id=g.task_id
                     JOIN plans p ON p.plan_id=t.plan_id WHERE g.gate_id=events.entity_id),
       gate_name=(SELECT g.name FROM gates g WHERE g.gate_id=events.entity_id)
 WHERE entity='gate' AND entity_id IS NOT NULL;

UPDATE events
   SET entity_seq=CAST(json_extract(payload, '$.task') AS INTEGER),
       gate_name=json_extract(payload, '$.name')
 WHERE entity='gate' AND entity_seq IS NULL AND json_valid(payload)
   AND json_type(payload, '$.task')='integer' AND json_type(payload, '$.name')='text';

UPDATE events
   SET entity_plan=(SELECT p.slug FROM tasks t JOIN plans p ON p.plan_id=t.plan_id
                     WHERE t.seq=events.entity_seq)
 WHERE entity IN ('task','dep','gate') AND entity_plan IS NULL AND entity_seq IS NOT NULL;

CREATE INDEX idx_events_entity_seq ON events(entity_seq, event_id);
"#,
        )?;
        version = 3;
    }
    if version == 3 {
        tx.execute_batch(mise_projection::MISE_PROJECTION_SCHEMA_V4)?;
        version = 4;
    }
    if version == 4 {
        tx.execute_batch(
            r#"
ALTER TABLE tasks DROP COLUMN alias;
CREATE TABLE commit_associations (
  commit_association_id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  repository TEXT NOT NULL,
  commit_oid TEXT NOT NULL,
  note TEXT,
  recorded_at TEXT NOT NULL,
  UNIQUE (task_id, repository, commit_oid)
);
CREATE INDEX idx_commit_associations_lookup ON commit_associations(repository, commit_oid);
"#,
        )?;
        version = 5;
    }
    if version == 5 {
        tx.execute_batch(
            r#"
ALTER TABLE tasks
  ADD COLUMN replacement_task_id INTEGER REFERENCES tasks(task_id);
"#,
        )?;
        version = 6;
    }
    if version != SCHEMA_VERSION {
        bail!("no papertiger migration path from schema v{from} to v{SCHEMA_VERSION}");
    }
    tx.execute(
        "UPDATE meta SET value=?1 WHERE key='schema_version'",
        params![SCHEMA_VERSION.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn add_note(conn: &Connection, actor: &str, task_seq: Option<i64>, text: &str) -> Result<()> {
    let tx = begin_mutation(conn)?;
    let (entity, entity_id) = match task_seq {
        Some(seq) => ("task", Some(get_task(&tx, seq)?.task_id)),
        None => ("plan", None),
    };
    record_event_in_mutation(&tx, actor, entity, entity_id, "note", Some(text), None)?;
    tx.commit()?;
    Ok(())
}

fn record_event_in_mutation(
    tx: &Transaction<'_>,
    actor: &str,
    entity: &str,
    entity_id: Option<i64>,
    kind: &str,
    why: Option<&str>,
    payload: Option<&serde_json::Value>,
) -> Result<()> {
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
            payload.map(|p| p.to_string())
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    #[serde(skip_serializing)]
    pub task_id: i64,
    pub seq: i64,
    #[serde(skip_serializing)]
    pub plan_id: i64,
    #[serde(skip_serializing)]
    pub parent_id: Option<i64>,
    #[serde(skip_serializing)]
    pub replacement_task_id: Option<i64>,
    pub title: String,
    pub intent: String,
    pub kind: String,
    pub result: Option<String>,
    pub status: String,
    pub priority: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    #[serde(skip_serializing)]
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

fn plan_status(conn: &Connection, plan_id: i64) -> Result<String> {
    conn.query_row(
        "SELECT status FROM plans WHERE plan_id=?1",
        params![plan_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| anyhow!("no plan id {plan_id}"))
}

pub(crate) fn task_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        task_id: r.get(0)?,
        seq: r.get(1)?,
        plan_id: r.get(2)?,
        parent_id: r.get(3)?,
        replacement_task_id: r.get(4)?,
        title: r.get(5)?,
        intent: r.get(6)?,
        kind: r.get(7)?,
        result: r.get(8)?,
        status: r.get(9)?,
        priority: r.get(10)?,
    })
}

pub(crate) const TASK_COLS: &str = "task_id, seq, plan_id, parent_id, replacement_task_id, title, intent, kind, result, status, priority";

fn validate_task_kind(kind: &str) -> Result<()> {
    if !TASK_KINDS.contains(&kind) {
        bail!(
            "unknown task kind '{kind}' (expected {})",
            TASK_KINDS.join("|")
        );
    }
    Ok(())
}

fn require_nonblank<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} must not be blank");
    }
    Ok(value)
}

pub fn get_task(conn: &Connection, seq: i64) -> Result<Task> {
    conn.query_row(
        &format!("SELECT {TASK_COLS} FROM tasks WHERE seq=?1"),
        params![seq],
        task_from_row,
    )
    .optional()?
    .ok_or_else(|| anyhow!("no task #{seq}"))
}

pub(crate) fn task_tags_by_id(conn: &Connection, task_id: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare("SELECT tag FROM task_tags WHERE task_id=?1 ORDER BY tag")?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Query tasks through the canonical task projection. Optional filters are
/// applied without placeholder renumbering or a caller-owned row mapping.
pub fn list_tasks(
    conn: &Connection,
    plan_id: i64,
    status: Option<&str>,
    tag: Option<&str>,
) -> Result<Vec<Task>> {
    list_tasks_ordered(conn, plan_id, status, tag, false)
}

pub fn list_tasks_by_activity(
    conn: &Connection,
    plan_id: i64,
    status: Option<&str>,
    tag: Option<&str>,
) -> Result<Vec<Task>> {
    list_tasks_ordered(conn, plan_id, status, tag, true)
}

fn list_tasks_ordered(
    conn: &Connection,
    plan_id: i64,
    status: Option<&str>,
    tag: Option<&str>,
    by_activity: bool,
) -> Result<Vec<Task>> {
    let order = if by_activity {
        "ORDER BY COALESCE((SELECT MAX(event_id) FROM events
                            WHERE entity_seq=tasks.seq
                              AND entity IN ('task','dep','gate')), 0) DESC, seq"
    } else {
        "ORDER BY seq"
    };
    let (sql, parameters): (String, Vec<rusqlite::types::Value>) = match (status, tag) {
        (Some(status), Some(tag)) => (
            format!(
                "SELECT {TASK_COLS} FROM tasks WHERE plan_id=?1 AND status=?2
                 AND task_id IN (SELECT task_id FROM task_tags WHERE tag=?3) {order}"
            ),
            vec![
                plan_id.into(),
                status.to_owned().into(),
                tag.to_owned().into(),
            ],
        ),
        (Some(status), None) => (
            format!("SELECT {TASK_COLS} FROM tasks WHERE plan_id=?1 AND status=?2 {order}"),
            vec![plan_id.into(), status.to_owned().into()],
        ),
        (None, Some(tag)) => (
            format!(
                "SELECT {TASK_COLS} FROM tasks WHERE plan_id=?1
                 AND task_id IN (SELECT task_id FROM task_tags WHERE tag=?2) {order}"
            ),
            vec![plan_id.into(), tag.to_owned().into()],
        ),
        (None, None) => (
            format!("SELECT {TASK_COLS} FROM tasks WHERE plan_id=?1 {order}"),
            vec![plan_id.into()],
        ),
    };
    let mut statement = conn.prepare(&sql)?;
    Ok(statement
        .query_map(rusqlite::params_from_iter(parameters), task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Non-container tasks in one durable status, used by the compact status view.
pub fn leaf_tasks_with_status(conn: &Connection, plan_id: i64, status: &str) -> Result<Vec<Task>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks task WHERE plan_id=?1 AND status=?2
         AND NOT EXISTS (SELECT 1 FROM tasks child WHERE child.parent_id=task.task_id)
         ORDER BY seq"
    ))?;
    Ok(statement
        .query_map(params![plan_id, status], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn parse_task_ref(task_ref: &str) -> Result<i64> {
    let digits = task_ref.strip_prefix('#').unwrap_or(task_ref);
    let invalid = || {
        anyhow!(
            "bad task reference '{task_ref}' (expected task.seq as N or #N, with N a canonical positive ASCII decimal)"
        )
    };
    if digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    digits.parse::<i64>().map_err(|_| invalid())
}

pub fn active_plan(conn: &Connection) -> Result<Option<(i64, String)>> {
    let mut statement =
        conn.prepare("SELECT plan_id, slug FROM plans WHERE status='active' ORDER BY plan_id")?;
    let rows: Vec<(i64, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
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
        return conn
            .query_row(
                "SELECT plan_id, slug FROM plans WHERE slug=?1",
                params![slug],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| anyhow!("no plan '{slug}'"));
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
    why: &str,
) -> Result<Vec<&'static str>> {
    if why.trim().is_empty() {
        bail!("editing a plan requires a nonblank reason");
    }
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
        Some(why),
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
    why: &str,
) -> Result<()> {
    if !["active", "paused", "done", "retired"].contains(&status) {
        bail!("unknown plan status '{status}'");
    }
    if why.trim().is_empty() {
        bail!("changing plan status requires a nonblank reason");
    }
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
        Some(why),
        Some(&serde_json::json!({"to": status})),
    )?;
    tx.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn add_task(
    conn: &Connection,
    actor: &str,
    plan_id: i64,
    title: &str,
    intent: &str,
    parent: Option<i64>,
    deps: &[i64],
    tags: &[String],
    priority: i64,
    why: Option<&str>,
) -> Result<i64> {
    add_task_with_kind(
        conn, actor, plan_id, title, intent, "work", parent, deps, tags, priority, why,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn add_task_with_kind(
    conn: &Connection,
    actor: &str,
    plan_id: i64,
    title: &str,
    intent: &str,
    kind: &str,
    parent: Option<i64>,
    deps: &[i64],
    tags: &[String],
    priority: i64,
    why: Option<&str>,
) -> Result<i64> {
    let tx = begin_mutation(conn)?;
    let seq = add_task_in_mutation(
        &tx, actor, plan_id, title, intent, kind, parent, deps, tags, priority, why,
    )?;
    tx.commit()?;
    Ok(seq)
}

#[allow(clippy::too_many_arguments)]
pub fn add_task_for_plan(
    conn: &Connection,
    actor: &str,
    plan: Option<&str>,
    title: &str,
    intent: &str,
    kind: &str,
    parent: Option<i64>,
    deps: &[i64],
    tags: &[String],
    priority: i64,
    why: Option<&str>,
) -> Result<(i64, String)> {
    let tx = begin_mutation(conn)?;
    let (plan_id, slug) = resolve_plan(&tx, plan)?;
    let seq = add_task_in_mutation(
        &tx, actor, plan_id, title, intent, kind, parent, deps, tags, priority, why,
    )?;
    tx.commit()?;
    Ok((seq, slug))
}

#[allow(clippy::too_many_arguments)]
fn add_task_in_mutation(
    tx: &Transaction<'_>,
    actor: &str,
    plan_id: i64,
    title: &str,
    intent: &str,
    kind: &str,
    parent: Option<i64>,
    deps: &[i64],
    tags: &[String],
    priority: i64,
    why: Option<&str>,
) -> Result<i64> {
    validate_task_kind(kind)?;
    let title = require_nonblank("task title", title)?;
    let status = plan_status(tx, plan_id)?;
    if matches!(status.as_str(), "done" | "retired") {
        bail!("plan is {status}; reactivate it before adding tasks");
    }
    let parent_id = match parent {
        Some(p) => {
            let parent = get_task(tx, p)?;
            if parent.plan_id != plan_id {
                bail!("parent #{p} belongs to a different plan");
            }
            if matches!(parent.status.as_str(), "done" | "retired" | "rejected") {
                bail!(
                    "parent #{p} is {}; reopen it before adding live children",
                    parent.status
                );
            }
            Some(parent.task_id)
        }
        None => None,
    };
    let seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM tasks", [], |r| r.get(0))?;
    let t = now();
    tx.execute(
        "INSERT INTO tasks
         (seq, plan_id, parent_id, title, intent, kind, status, priority, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'proposed', ?7, ?8, ?8)",
        params![seq, plan_id, parent_id, title, intent, kind, priority, t],
    )?;
    let id = tx.last_insert_rowid();
    record_event_in_mutation(
        tx,
        actor,
        "task",
        Some(id),
        "create",
        why,
        Some(&serde_json::json!({"seq": seq, "title": title, "kind": kind})),
    )?;
    for d in deps {
        add_dep_inner(tx, actor, seq, *d, true, why)?;
    }
    for tag in tags {
        tx.execute(
            "INSERT OR IGNORE INTO task_tags (task_id, tag) VALUES (?1, ?2)",
            params![id, tag],
        )?;
    }
    Ok(seq)
}

pub struct TaskEdit<'a> {
    pub title: Option<&'a str>,
    pub intent: Option<&'a str>,
    pub parent: Option<Option<i64>>,
    pub kind: Option<&'a str>,
    pub priority: Option<i64>,
}

pub fn edit_task(
    conn: &Connection,
    actor: &str,
    seq: i64,
    edit: TaskEdit<'_>,
    why: &str,
) -> Result<Vec<&'static str>> {
    if why.trim().is_empty() {
        bail!("editing a task requires a nonblank reason");
    }
    if edit.title.is_none()
        && edit.intent.is_none()
        && edit.parent.is_none()
        && edit.kind.is_none()
        && edit.priority.is_none()
    {
        bail!(
            "task edit requires at least one of --title, --intent, --parent, --kind, or --priority"
        );
    }
    if let Some(kind) = edit.kind {
        validate_task_kind(kind)?;
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let mut changed = Vec::new();
    if let Some(new) = edit.title {
        let new = require_nonblank("task title", new)?;
        tx.execute(
            "UPDATE tasks SET title=?1, updated_at=?2 WHERE task_id=?3",
            params![new, now(), task.task_id],
        )?;
        changed.push("title");
    }
    if let Some(new) = edit.intent {
        tx.execute(
            "UPDATE tasks SET intent=?1, updated_at=?2 WHERE task_id=?3",
            params![new, now(), task.task_id],
        )?;
        changed.push("intent");
    }
    if let Some(new) = edit.kind {
        tx.execute(
            "UPDATE tasks SET kind=?1, updated_at=?2 WHERE task_id=?3",
            params![new, now(), task.task_id],
        )?;
        changed.push("kind");
    }
    if let Some(parent) = edit.parent {
        let parent_id = match parent {
            Some(parent_seq) => {
                let new_parent = get_task(&tx, parent_seq)?;
                if new_parent.task_id == task.task_id {
                    bail!("#{seq} cannot be its own parent");
                }
                if new_parent.plan_id != task.plan_id {
                    bail!("#{seq} and parent #{parent_seq} belong to different plans");
                }
                if matches!(task.status.as_str(), "proposed" | "in_progress")
                    && matches!(new_parent.status.as_str(), "done" | "retired" | "rejected")
                {
                    bail!(
                        "parent #{parent_seq} is {}; reopen it before assigning live child #{seq}",
                        new_parent.status
                    );
                }
                Some(new_parent.task_id)
            }
            None => None,
        };
        tx.execute(
            "UPDATE tasks SET parent_id=?1, updated_at=?2 WHERE task_id=?3",
            params![parent_id, now(), task.task_id],
        )?;
        if let Some(cycle) = find_cycle(
            &tx,
            "SELECT task_id, parent_id FROM tasks WHERE parent_id IS NOT NULL",
        )? {
            bail!(
                "parent change would create cycle {}",
                cycle
                    .iter()
                    .map(|task| format!("#{task}"))
                    .collect::<Vec<_>>()
                    .join(" -> ")
            );
        }
        changed.push("parent");
    }
    if let Some(new) = edit.priority {
        tx.execute(
            "UPDATE tasks SET priority=?1, updated_at=?2 WHERE task_id=?3",
            params![new, now(), task.task_id],
        )?;
        changed.push("priority");
    }
    let edited = get_task(&tx, seq)?;
    if edited.status == "done"
        && matches!(edited.kind.as_str(), "probe" | "decision")
        && edited
            .result
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        bail!(
            "completed {} task #{seq} requires a durable result; reopen it before changing kind",
            edited.kind
        );
    }
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "edit",
        Some(why),
        Some(&serde_json::json!({"seq": task.seq, "fields": changed})),
    )?;
    tx.commit()?;
    Ok(changed)
}

pub fn add_tag(conn: &Connection, actor: &str, seq: i64, tag: &str, why: &str) -> Result<()> {
    let tag = tag.trim();
    if tag.is_empty() {
        bail!("adding a tag requires a nonblank tag");
    }
    if why.trim().is_empty() {
        bail!("adding a tag requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let changed = tx.execute(
        "INSERT OR IGNORE INTO task_tags (task_id, tag) VALUES (?1, ?2)",
        params![task.task_id, tag],
    )?;
    if changed == 0 {
        bail!("#{seq} already has tag '{tag}'");
    }
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "tag_add",
        Some(why),
        Some(&serde_json::json!({"seq": seq, "tag": tag})),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn remove_tag(conn: &Connection, actor: &str, seq: i64, tag: &str, why: &str) -> Result<()> {
    let tag = tag.trim();
    if tag.is_empty() {
        bail!("removing a tag requires a nonblank tag");
    }
    if why.trim().is_empty() {
        bail!("removing a tag requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let changed = tx.execute(
        "DELETE FROM task_tags WHERE task_id=?1 AND tag=?2",
        params![task.task_id, tag],
    )?;
    if changed == 0 {
        bail!("#{seq} does not have tag '{tag}'");
    }
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "tag_remove",
        Some(why),
        Some(&serde_json::json!({"seq": seq, "tag": tag})),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn add_dep(conn: &Connection, actor: &str, seq: i64, on_seq: i64, why: &str) -> Result<()> {
    if why.trim().is_empty() {
        bail!("adding a dependency requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    add_dep_inner(&tx, actor, seq, on_seq, true, Some(why))?;
    tx.commit()?;
    Ok(())
}

fn add_dep_inner(
    tx: &Transaction<'_>,
    actor: &str,
    seq: i64,
    on_seq: i64,
    event: bool,
    why: Option<&str>,
) -> Result<()> {
    let task = get_task(tx, seq)?;
    let on = get_task(tx, on_seq)?;
    if task.task_id == on.task_id {
        bail!("#{seq} cannot depend on itself");
    }
    if task.plan_id != on.plan_id {
        bail!("#{seq} and dependency #{on_seq} belong to different plans");
    }
    if event && matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "#{seq} is {}; reopen it before adding dependencies",
            task.status
        );
    }
    if event && matches!(on.status.as_str(), "retired" | "rejected") {
        bail!(
            "dependency #{on_seq} is {}; reopen it or choose a viable prerequisite",
            on.status
        );
    }
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM deps WHERE task_id=?1 AND depends_on=?2)",
        params![task.task_id, on.task_id],
        |r| r.get(0),
    )?;
    if exists {
        bail!("#{seq} already depends on #{on_seq}");
    }
    // Reject cycles: is `task` reachable from `on` via deps?
    let mut stack = vec![on.task_id];
    let mut seen = HashSet::new();
    while let Some(cur) = stack.pop() {
        if cur == task.task_id {
            bail!("dependency #{seq} -> #{on_seq} would create a cycle");
        }
        if !seen.insert(cur) {
            continue;
        }
        let mut st = tx.prepare("SELECT depends_on FROM deps WHERE task_id=?1")?;
        let next: Vec<i64> = st
            .query_map(params![cur], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        stack.extend(next);
    }
    tx.execute(
        "INSERT INTO deps (task_id, depends_on) VALUES (?1, ?2)",
        params![task.task_id, on.task_id],
    )?;
    if event {
        record_event_in_mutation(
            tx,
            actor,
            "dep",
            Some(task.task_id),
            "add",
            why,
            Some(&serde_json::json!({"on": on_seq})),
        )?;
    }
    Ok(())
}

pub fn remove_dep(conn: &Connection, actor: &str, seq: i64, on_seq: i64, why: &str) -> Result<()> {
    if why.trim().is_empty() {
        bail!("removing a dependency requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let on = get_task(&tx, on_seq)?;
    let n = tx.execute(
        "DELETE FROM deps WHERE task_id=?1 AND depends_on=?2",
        params![task.task_id, on.task_id],
    )?;
    if n == 0 {
        bail!("#{seq} does not depend on #{on_seq}");
    }
    record_event_in_mutation(
        &tx,
        actor,
        "dep",
        Some(task.task_id),
        "remove",
        Some(why),
        Some(&serde_json::json!({"on": on_seq})),
    )?;
    tx.commit()?;
    Ok(())
}

fn entry_blockers(conn: &Connection, task: &Task) -> Result<Vec<String>> {
    let mut blockers = open_deps(conn, task.task_id)?
        .into_iter()
        .map(|dependency| format!("dep:#{dependency}"))
        .collect::<Vec<_>>();
    blockers.extend(
        open_task_blocker_names(conn, task.task_id)?
            .into_iter()
            .map(|name| format!("blocker:{name}")),
    );
    Ok(blockers)
}

pub fn start_task(conn: &Connection, actor: &str, seq: i64, why: Option<&str>) -> Result<()> {
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    match task.status.as_str() {
        "proposed" => {}
        "in_progress" => bail!("#{seq} is already in_progress"),
        "done" | "retired" | "rejected" => {
            bail!("#{seq} is {}; use `reopen` before starting it", task.status)
        }
        other => bail!("#{seq} has unknown status '{other}'"),
    }
    let status = plan_status(&tx, task.plan_id)?;
    if status != "active" {
        bail!("plan is {status}; set it active before starting task #{seq}");
    }
    let blockers = entry_blockers(&tx, &task)?;
    if !blockers.is_empty() {
        bail!(
            "#{seq} is not ready; resolve {} before starting it",
            blockers.join(", ")
        );
    }
    transition_task(&tx, actor, &task, "in_progress", why, None, None)?;
    tx.commit()?;
    Ok(())
}

pub fn complete_task(conn: &Connection, actor: &str, seq: i64, result: Option<&str>) -> Result<()> {
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!("#{seq} is already {}", task.status);
    }
    let plan_status = plan_status(&tx, task.plan_id)?;
    if task.status == "proposed" && plan_status != "active" {
        bail!("plan is {plan_status}; set it active before completing proposed task #{seq}");
    }
    let blockers = entry_blockers(&tx, &task)?;
    if !blockers.is_empty() {
        bail!(
            "#{seq} cannot be completed while {} remain open",
            blockers.join(", ")
        );
    }
    let open = open_gates(&tx, task.task_id)?;
    if !open.is_empty() {
        bail!(
            "#{seq} has open gate(s): {}; close with evidence or waive with --why",
            open.join(", ")
        );
    }
    let live_children = live_child_sequences(&tx, task.task_id)?;
    if !live_children.is_empty() {
        bail!(
            "#{seq} has unfinished child task(s): {}; finish or disposition them first",
            live_children
                .iter()
                .map(|child| format!("#{child}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let result = result.map(str::trim).filter(|text| !text.is_empty());
    if matches!(task.kind.as_str(), "probe" | "decision") && result.is_none() {
        bail!(
            "completing {} task #{seq} requires --result or --result-file so the measured or selected outcome survives the session",
            task.kind
        );
    }
    transition_task(&tx, actor, &task, "done", result, result, None)?;
    tx.commit()?;
    Ok(())
}

pub fn reopen_task(conn: &Connection, actor: &str, seq: i64, why: &str) -> Result<()> {
    if why.trim().is_empty() {
        bail!("reopening a task requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if !matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "#{seq} is {}; only terminal tasks can be reopened",
            task.status
        );
    }
    let status = plan_status(&tx, task.plan_id)?;
    if matches!(status.as_str(), "done" | "retired") {
        bail!("plan is {status}; reactivate it before reopening task #{seq}");
    }
    if let Some(parent_id) = task.parent_id {
        let parent: (i64, String) = tx.query_row(
            "SELECT seq, status FROM tasks WHERE task_id=?1",
            params![parent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if matches!(parent.1.as_str(), "done" | "retired" | "rejected") {
            bail!(
                "parent #{} is {}; reopen it before reopening child #{seq}",
                parent.0,
                parent.1
            );
        }
    }
    let completed_dependents = completed_dependent_sequences(&tx, task.task_id)?;
    if !completed_dependents.is_empty() {
        bail!(
            "#{seq} supports completed dependent task(s): {}; reopen them first",
            completed_dependents
                .iter()
                .map(|dependent| format!("#{dependent}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    transition_task(&tx, actor, &task, "proposed", Some(why), None, None)?;
    tx.commit()?;
    Ok(())
}

pub fn retire_task(
    conn: &Connection,
    actor: &str,
    seq: i64,
    replacement_seq: Option<i64>,
    why: &str,
) -> Result<()> {
    terminate_task(conn, actor, seq, "retired", replacement_seq, why)
}

pub fn reject_task(conn: &Connection, actor: &str, seq: i64, why: &str) -> Result<()> {
    terminate_task(conn, actor, seq, "rejected", None, why)
}

fn terminate_task(
    conn: &Connection,
    actor: &str,
    seq: i64,
    status: &str,
    replacement_seq: Option<i64>,
    why: &str,
) -> Result<()> {
    if !matches!(status, "retired" | "rejected") {
        bail!("terminal disposition must be retired or rejected");
    }
    let why = why.trim();
    if why.is_empty() {
        bail!("{status} requires --why");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if status == "rejected" && replacement_seq.is_some() {
        bail!(
            "reject does not accept a replacement task; use `papertiger retire {seq} --into <task> --why <reason>` when consolidating work"
        );
    }
    if task.status == status {
        bail!("#{seq} is already {status}");
    }
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "#{seq} is {}; reopen it before changing its disposition",
            task.status
        );
    }
    let completed_dependents = completed_dependent_sequences(&tx, task.task_id)?;
    if !completed_dependents.is_empty() {
        bail!(
            "#{seq} supports completed dependent task(s): {}; reopen them before {status}",
            completed_dependents
                .iter()
                .map(|dependent| format!("#{dependent}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let live_children = live_child_sequences(&tx, task.task_id)?;
    if !live_children.is_empty() {
        bail!(
            "#{seq} has unfinished child task(s): {}; finish, disposition, or reparent them before {status}",
            live_children
                .iter()
                .map(|child| format!("#{child}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let replacement_sources = inbound_replacement_sequences(&tx, task.task_id)?;
    if !replacement_sources.is_empty() && status == "rejected" {
        bail!(
            "#{seq} is the canonical replacement for {}; rejection would strand that history; use `papertiger retire {seq} --into <task> --why <reason>` to extend the replacement chain, or reopen and re-disposition the referring tasks first",
            replacement_sources
                .iter()
                .map(|source| format!("#{source}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !replacement_sources.is_empty() && status == "retired" && replacement_seq.is_none() {
        bail!(
            "#{seq} is the canonical replacement for {}; bare retirement would strand that history; use `papertiger retire {seq} --into <task> --why <reason>` to extend the replacement chain",
            replacement_sources
                .iter()
                .map(|source| format!("#{source}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let replacement = replacement_seq
        .map(|replacement_seq| {
            if replacement_seq == seq {
                bail!("#{seq} cannot replace itself");
            }
            let replacement = get_task(&tx, replacement_seq)?;
            if replacement.plan_id != task.plan_id {
                bail!("#{seq} and replacement #{replacement_seq} belong to different plans");
            }
            if matches!(replacement.status.as_str(), "retired" | "rejected") {
                bail!(
                    "replacement #{replacement_seq} is {}; choose a proposed, in_progress, or done same-plan task for --into",
                    replacement.status
                );
            }
            Ok(replacement)
        })
        .transpose()?;
    transition_task(
        &tx,
        actor,
        &task,
        status,
        Some(why),
        Some(why),
        replacement.as_ref(),
    )?;
    if let Some(cycle) = find_cycle(
        &tx,
        "SELECT task_id, replacement_task_id FROM tasks WHERE replacement_task_id IS NOT NULL",
    )? {
        bail!(
            "replacement would create cycle {}",
            cycle
                .iter()
                .map(|task| format!("#{task}"))
                .collect::<Vec<_>>()
                .join(" -> ")
        );
    }
    tx.commit()?;
    Ok(())
}

fn transition_task(
    tx: &Transaction<'_>,
    actor: &str,
    task: &Task,
    status: &str,
    why: Option<&str>,
    result: Option<&str>,
    replacement: Option<&Task>,
) -> Result<()> {
    tx.execute(
        "UPDATE tasks
            SET status=?1, result=?2, replacement_task_id=?3, updated_at=?4
          WHERE task_id=?5",
        params![
            status,
            result,
            replacement.map(|task| task.task_id),
            now(),
            task.task_id
        ],
    )?;
    let mut payload = serde_json::json!({
        "seq": task.seq,
        "from": task.status,
        "to": status,
        "result": result,
    });
    if let Some(replacement) = replacement {
        payload["replacement_seq"] = serde_json::json!(replacement.seq);
    }
    record_event_in_mutation(
        tx,
        actor,
        "task",
        Some(task.task_id),
        "status",
        why,
        Some(&payload),
    )?;
    Ok(())
}

fn open_gates(conn: &Connection, task_id: i64) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT name FROM gates WHERE task_id=?1 AND status='open'")?;
    Ok(st
        .query_map(params![task_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn live_child_sequences(conn: &Connection, task_id: i64) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT seq FROM tasks
          WHERE parent_id=?1 AND status IN ('proposed','in_progress')
          ORDER BY seq",
    )?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn inbound_replacement_sequences(conn: &Connection, task_id: i64) -> Result<Vec<i64>> {
    let mut statement =
        conn.prepare("SELECT seq FROM tasks WHERE replacement_task_id=?1 ORDER BY seq")?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn completed_dependent_sequences(conn: &Connection, task_id: i64) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT task.seq
           FROM deps
           JOIN tasks task ON task.task_id=deps.task_id
          WHERE deps.depends_on=?1 AND task.status='done'
          ORDER BY task.seq",
    )?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn add_gate(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    kind: &str,
    requirement: &str,
) -> Result<()> {
    let name = require_nonblank("gate name", name)?;
    let requirement = require_nonblank("gate requirement", requirement)?;
    if !GATE_KINDS.contains(&kind) {
        bail!(
            "unknown gate kind '{kind}' (expected {})",
            GATE_KINDS.join("|")
        );
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!("#{seq} is {}; reopen it before adding gates", task.status);
    }
    tx.execute(
        "INSERT INTO gates (task_id, name, kind, requirement) VALUES (?1, ?2, ?3, ?4)",
        params![task.task_id, name, kind, requirement],
    )
    .with_context(|| {
        format!("gate '{name}' already exists on #{seq}; choose a different gate name")
    })?;
    record_event_in_mutation(
        &tx,
        actor,
        "gate",
        Some(tx.last_insert_rowid()),
        "create",
        None,
        Some(&serde_json::json!({"task": seq, "name": name, "kind": kind})),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn close_gate(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    evidence: &str,
    sha256: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    resolve_open_gate_then(
        conn,
        actor,
        seq,
        name,
        "closed",
        Some(evidence),
        sha256,
        note,
        None,
    )
}

pub fn waive_gate(conn: &Connection, actor: &str, seq: i64, name: &str, why: &str) -> Result<()> {
    if why.trim().is_empty() {
        bail!("waiving a gate requires a nonblank reason");
    }
    resolve_open_gate_then(
        conn,
        actor,
        seq,
        name,
        "waived",
        None,
        None,
        None,
        Some(why),
    )
}

pub fn remove_open_gate(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    why: &str,
) -> Result<()> {
    if why.trim().is_empty() {
        bail!("removing a gate requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let gate: Option<(i64, String, String)> = tx
        .query_row(
            "SELECT gate_id, kind, requirement FROM gates
             WHERE task_id=?1 AND name=?2 AND status='open'",
            params![task.task_id, name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (gate_id, kind, requirement) =
        gate.ok_or_else(|| anyhow!("no open gate '{name}' on #{seq}"))?;
    tx.execute("DELETE FROM gates WHERE gate_id=?1", params![gate_id])?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "gate_remove",
        Some(why),
        Some(&serde_json::json!({
            "seq": seq,
            "name": name,
            "kind": kind,
            "requirement": requirement,
        })),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn reopen_gate(conn: &Connection, actor: &str, seq: i64, name: &str, why: &str) -> Result<()> {
    if why.trim().is_empty() {
        bail!("reopening a gate requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "#{seq} is {}; reopen the task before reopening gates",
            task.status
        );
    }
    let gate: Option<(i64, String)> = tx
        .query_row(
            "SELECT gate_id, status FROM gates WHERE task_id=?1 AND name=?2",
            params![task.task_id, name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (gate_id, status) = gate.ok_or_else(|| anyhow!("no gate '{name}' on #{seq}"))?;
    if status == "open" {
        bail!("gate '{name}' on #{seq} is already open");
    }
    tx.execute(
        "UPDATE gates
            SET status='open', evidence_locator=NULL, evidence_sha256=NULL,
                note=NULL, closed_at=NULL
          WHERE gate_id=?1",
        params![gate_id],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "gate",
        Some(gate_id),
        "reopen",
        Some(why.trim()),
        Some(&serde_json::json!({
            "task": seq,
            "name": name,
            "from": status,
        })),
    )?;
    tx.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_open_gate_then(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    to: &str,
    evidence: Option<&str>,
    sha256: Option<&str>,
    note: Option<&str>,
    why: Option<&str>,
) -> Result<()> {
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let gate_id: i64 = tx
        .query_row(
            "SELECT gate_id FROM gates WHERE task_id=?1 AND name=?2 AND status='open'",
            params![task.task_id, name],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("no open gate '{name}' on #{seq}"))?;
    if let Some(loc) = evidence {
        validate_new_evidence_locator(loc)?;
    }
    validate_optional_sha256(sha256)?;
    let stored_note = if to == "waived" { why } else { note };
    tx.execute(
        "UPDATE gates SET status=?1, evidence_locator=?2, evidence_sha256=?3, note=?4, closed_at=?5
         WHERE gate_id=?6",
        params![to, evidence, sha256, stored_note, now(), gate_id],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "gate",
        Some(gate_id),
        to,
        why,
        Some(&serde_json::json!({"task": seq, "name": name, "evidence": evidence})),
    )?;
    tx.commit()?;
    Ok(())
}

fn validate_evidence_locator(locator: &str) -> Result<()> {
    let Some((scheme, value)) = locator.split_once(':') else {
        bail!(
            "evidence locator '{locator}' must be scheme:value (e.g. file:runtime/evidence/x.json)"
        );
    };
    if value.trim().is_empty() {
        bail!("evidence locator '{locator}' must have a nonblank scheme and value");
    }
    let mut chars = scheme.chars();
    let valid_scheme = chars.next().is_some_and(|ch| ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'));
    if !valid_scheme {
        bail!("evidence locator '{locator}' has an invalid scheme; use an RFC 3986-style name");
    }
    Ok(())
}

fn validate_new_evidence_locator(locator: &str) -> Result<()> {
    validate_evidence_locator(locator)?;
    if let Some((scheme, value)) = locator.split_once(':')
        && scheme.eq_ignore_ascii_case("commit")
    {
        validate_commit_oid(value)?;
    }
    Ok(())
}

pub fn validate_commit_oid(commit_oid: &str) -> Result<()> {
    let valid_length = matches!(commit_oid.len(), 40 | 64);
    if !valid_length || !commit_oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!(
            "commit object id '{commit_oid}' must be the full 40- or 64-character hexadecimal id; resolve it in the owning repository with `git rev-parse --verify 'HEAD^{{commit}}'`"
        );
    }
    if commit_oid.bytes().any(|byte| byte.is_ascii_uppercase()) {
        bail!(
            "commit object id '{commit_oid}' must use canonical lowercase hexadecimal; resolve it with `git rev-parse --verify 'HEAD^{{commit}}'`"
        );
    }
    Ok(())
}

fn validate_optional_sha256(sha256: Option<&str>) -> Result<()> {
    if let Some(digest) = sha256 {
        validate_sha256(digest, "sha256")?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBlocker {
    pub name: String,
    pub reason: String,
    #[serde(default = "default_open")]
    pub status: String,
    pub evidence_locator: Option<String>,
    pub evidence_sha256: Option<String>,
    pub note: Option<String>,
    pub resolved_at: Option<String>,
}

pub fn task_blockers(conn: &Connection, task_id: i64) -> Result<Vec<TaskBlocker>> {
    let mut statement = conn.prepare(
        "SELECT name, reason, status, evidence_locator, evidence_sha256, note, resolved_at
           FROM task_blockers
          WHERE task_id=?1
          ORDER BY blocker_id",
    )?;
    Ok(statement
        .query_map(params![task_id], |row| {
            Ok(TaskBlocker {
                name: row.get(0)?,
                reason: row.get(1)?,
                status: row.get(2)?,
                evidence_locator: row.get(3)?,
                evidence_sha256: row.get(4)?,
                note: row.get(5)?,
                resolved_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn open_task_blocker_names(conn: &Connection, task_id: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT name FROM task_blockers
          WHERE task_id=?1 AND status='open'
          ORDER BY blocker_id",
    )?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn add_task_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    reason: &str,
) -> Result<()> {
    if name.trim().is_empty() || reason.trim().is_empty() {
        bail!("adding a blocker requires a nonblank name and reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "#{seq} is {}; reopen it before adding blockers",
            task.status
        );
    }
    tx.execute(
        "INSERT INTO task_blockers (task_id, name, reason)
         VALUES (?1, ?2, ?3)",
        params![task.task_id, name.trim(), reason.trim()],
    )
    .with_context(|| {
        format!(
            "blocker '{}' already exists on #{seq}; choose a different blocker name",
            name.trim()
        )
    })?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "blocker_add",
        Some(reason.trim()),
        Some(&serde_json::json!({"seq": seq, "name": name.trim()})),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn resolve_task_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    evidence: &str,
    sha256: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    validate_new_evidence_locator(evidence)?;
    validate_optional_sha256(sha256)?;
    resolve_open_task_blocker(
        conn,
        actor,
        seq,
        name,
        BlockerResolution {
            status: "resolved",
            evidence: Some(evidence),
            sha256,
            note,
        },
    )
}

pub fn waive_task_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    why: &str,
) -> Result<()> {
    if why.trim().is_empty() {
        bail!("waiving a blocker requires a nonblank reason");
    }
    resolve_open_task_blocker(
        conn,
        actor,
        seq,
        name,
        BlockerResolution {
            status: "waived",
            evidence: None,
            sha256: None,
            note: Some(why),
        },
    )
}

struct BlockerResolution<'a> {
    status: &'a str,
    evidence: Option<&'a str>,
    sha256: Option<&'a str>,
    note: Option<&'a str>,
}

fn resolve_open_task_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    resolution: BlockerResolution<'_>,
) -> Result<()> {
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let blocker_id: i64 = tx
        .query_row(
            "SELECT blocker_id FROM task_blockers
              WHERE task_id=?1 AND name=?2 AND status='open'",
            params![task.task_id, name],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("no open blocker '{name}' on #{seq}"))?;
    tx.execute(
        "UPDATE task_blockers
            SET status=?1, evidence_locator=?2, evidence_sha256=?3,
                note=?4, resolved_at=?5
          WHERE blocker_id=?6",
        params![
            resolution.status,
            resolution.evidence,
            resolution.sha256,
            resolution.note,
            now(),
            blocker_id
        ],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        &format!("blocker_{}", resolution.status),
        resolution.note,
        Some(&serde_json::json!({
            "seq": seq,
            "name": name,
            "evidence": resolution.evidence,
        })),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn remove_open_task_blocker(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    why: &str,
) -> Result<()> {
    if why.trim().is_empty() {
        bail!("removing a blocker requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let blocker: Option<(i64, String)> = tx
        .query_row(
            "SELECT blocker_id, reason FROM task_blockers
              WHERE task_id=?1 AND name=?2 AND status='open'",
            params![task.task_id, name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (blocker_id, reason) =
        blocker.ok_or_else(|| anyhow!("no open blocker '{name}' on #{seq}"))?;
    tx.execute(
        "DELETE FROM task_blockers WHERE blocker_id=?1",
        params![blocker_id],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "blocker_remove",
        Some(why),
        Some(&serde_json::json!({
            "seq": seq,
            "name": name,
            "reason": reason,
        })),
    )?;
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct GateRecord {
    pub name: String,
    pub kind: String,
    pub requirement: String,
    pub status: String,
    pub evidence_locator: Option<String>,
    pub evidence_sha256: Option<String>,
    pub note: Option<String>,
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskContext {
    pub schema: String,
    pub plan: Plan,
    pub task: Task,
    pub tags: Vec<String>,
    pub parent: Option<Task>,
    pub replacement: Option<Task>,
    pub dependencies: Vec<Task>,
    pub dependents: Vec<Task>,
    pub children: Vec<Task>,
    pub blockers: Vec<TaskBlocker>,
    pub gates: Vec<GateRecord>,
    pub commit_associations: Vec<CommitAssociation>,
    pub activity: TaskActivity,
    pub mise_projections: Vec<TaskMiseProjectionSummary>,
    pub immediate_unlock_count: usize,
    pub unfinished_downstream_count: usize,
    pub recent_events: Vec<EventRecord>,
    pub recent_events_truncated: bool,
    pub older_events_cursor: Option<EventCursor>,
}

pub fn task_context(conn: &Connection, seq: i64) -> Result<TaskContext> {
    let task = get_task(conn, seq)?;
    let plan = get_plan(conn, task.plan_id)?;
    let parent = task
        .parent_id
        .map(|parent_id| {
            conn.query_row(
                &format!("SELECT {TASK_COLS} FROM tasks WHERE task_id=?1"),
                params![parent_id],
                task_from_row,
            )
            .optional()
            .context("query task parent")
        })
        .transpose()?
        .flatten();
    let replacement = task
        .replacement_task_id
        .map(|replacement_task_id| {
            conn.query_row(
                &format!("SELECT {TASK_COLS} FROM tasks WHERE task_id=?1"),
                params![replacement_task_id],
                task_from_row,
            )
            .optional()
            .context("query task replacement")
        })
        .transpose()?
        .flatten();

    let tags = task_tags_by_id(conn, task.task_id)?;

    let dependencies = query_related_tasks(
        conn,
        &format!(
            "SELECT {TASK_COLS} FROM tasks
              WHERE task_id IN (SELECT depends_on FROM deps WHERE task_id=?1)
              ORDER BY seq"
        ),
        task.task_id,
    )?;
    let dependents = query_related_tasks(
        conn,
        &format!(
            "SELECT {TASK_COLS} FROM tasks
              WHERE task_id IN (SELECT task_id FROM deps WHERE depends_on=?1)
              ORDER BY seq"
        ),
        task.task_id,
    )?;
    let children = query_related_tasks(
        conn,
        &format!("SELECT {TASK_COLS} FROM tasks WHERE parent_id=?1 ORDER BY seq"),
        task.task_id,
    )?;

    let mut statement = conn.prepare(
        "SELECT name, kind, requirement, status, evidence_locator,
                evidence_sha256, note, closed_at
           FROM gates
          WHERE task_id=?1
          ORDER BY gate_id",
    )?;
    let gates = statement
        .query_map(params![task.task_id], |row| {
            Ok(GateRecord {
                name: row.get(0)?,
                kind: row.get(1)?,
                requirement: row.get(2)?,
                status: row.get(3)?,
                evidence_locator: row.get(4)?,
                evidence_sha256: row.get(5)?,
                note: row.get(6)?,
                closed_at: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    const RECENT_EVENT_LIMIT: usize = 12;
    let recent_event_log = event_log(conn, Some(task.seq), RECENT_EVENT_LIMIT, None, None)?;
    let recent_events_truncated = recent_event_log.truncated;
    let older_events_cursor = recent_event_log.continuation;
    let recent_events = recent_event_log.events;

    Ok(TaskContext {
        schema: "papertiger.task_context.v4".into(),
        plan,
        tags,
        parent,
        replacement,
        dependencies,
        dependents,
        children,
        blockers: task_blockers(conn, task.task_id)?,
        gates,
        commit_associations: commit_associations(conn, task.seq)?,
        activity: task_activity(conn, task.seq)?,
        mise_projections: task_mise_projection_summaries(conn, task.seq)?,
        immediate_unlock_count: immediate_unlock_count(conn, task.task_id)?,
        unfinished_downstream_count: unfinished_downstream_count(conn, task.task_id)?,
        recent_events,
        recent_events_truncated,
        older_events_cursor,
        task,
    })
}

fn query_related_tasks(conn: &Connection, sql: &str, task_id: i64) -> Result<Vec<Task>> {
    let mut statement = conn.prepare(sql)?;
    Ok(statement
        .query_map(params![task_id], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Blocking dependencies of a task: every dependency that is not done.
/// Retired and rejected prerequisites remain blockers until the stale edge is
/// explicitly removed as a course correction.
pub fn open_deps(conn: &Connection, task_id: i64) -> Result<Vec<i64>> {
    let mut st = conn.prepare(
        "SELECT t.seq FROM deps d JOIN tasks t ON t.task_id = d.depends_on
         WHERE d.task_id=?1 AND t.status <> 'done' ORDER BY t.seq",
    )?;
    Ok(st
        .query_map(params![task_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadyEntry {
    pub task: Task,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FocusEntry {
    pub task: Task,
    pub readiness: String,
    pub blockers: Vec<String>,
    pub open_gate_count: usize,
    pub immediate_unlock_count: usize,
    pub unfinished_downstream_count: usize,
}

/// Actionable leaf tasks ordered by active work, explicit priority, and the
/// amount of unfinished dependency graph they can unblock. Proposed blocked
/// work is opt-in; active work that became blocked is always surfaced.
pub fn focus(
    conn: &Connection,
    plan_id: i64,
    limit: usize,
    include_blocked: bool,
) -> Result<Vec<FocusEntry>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks task
          WHERE task.plan_id=?1
            AND task.status IN ('proposed','in_progress')
            AND NOT EXISTS (
                SELECT 1 FROM tasks child WHERE child.parent_id=task.task_id
            )"
    ))?;
    let tasks = statement
        .query_map(params![plan_id], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut entries = tasks
        .into_iter()
        .map(|task| {
            let blockers = entry_blockers(conn, &task)?;
            let readiness = match (task.status.as_str(), blockers.is_empty()) {
                ("in_progress", true) => "in_progress",
                ("in_progress", false) => "in_progress_blocked",
                ("proposed", true) => "ready",
                ("proposed", false) => "blocked",
                _ => "unknown",
            };
            Ok(FocusEntry {
                open_gate_count: open_gates(conn, task.task_id)?.len(),
                immediate_unlock_count: immediate_unlock_count(conn, task.task_id)?,
                unfinished_downstream_count: unfinished_downstream_count(conn, task.task_id)?,
                task,
                readiness: readiness.to_string(),
                blockers,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if !include_blocked {
        entries.retain(|entry| entry.readiness != "blocked");
    }
    entries.sort_by(|left, right| {
        focus_readiness_rank(&left.readiness)
            .cmp(&focus_readiness_rank(&right.readiness))
            .then_with(|| right.task.priority.cmp(&left.task.priority))
            .then_with(|| {
                right
                    .immediate_unlock_count
                    .cmp(&left.immediate_unlock_count)
            })
            .then_with(|| {
                right
                    .unfinished_downstream_count
                    .cmp(&left.unfinished_downstream_count)
            })
            .then_with(|| left.task.seq.cmp(&right.task.seq))
    });
    entries.truncate(limit);
    Ok(entries)
}

fn focus_readiness_rank(readiness: &str) -> u8 {
    match readiness {
        "in_progress_blocked" => 0,
        "in_progress" => 1,
        "ready" => 2,
        "blocked" => 3,
        _ => 4,
    }
}

fn immediate_unlock_count(conn: &Connection, task_id: i64) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*)
           FROM deps dependency
           JOIN tasks dependent ON dependent.task_id=dependency.task_id
          WHERE dependency.depends_on=?1
            AND dependent.status='proposed'
            AND NOT EXISTS (
                SELECT 1 FROM tasks child WHERE child.parent_id=dependent.task_id
            )
            AND NOT EXISTS (
                SELECT 1
                  FROM deps other_dependency
                  JOIN tasks prerequisite
                    ON prerequisite.task_id=other_dependency.depends_on
                 WHERE other_dependency.task_id=dependent.task_id
                   AND other_dependency.depends_on<>?1
                   AND prerequisite.status<>'done'
            )
            AND NOT EXISTS (
                SELECT 1
                  FROM task_blockers blocker
                 WHERE blocker.task_id=dependent.task_id
                   AND blocker.status='open'
            )",
        params![task_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

fn unfinished_downstream_count(conn: &Connection, task_id: i64) -> Result<usize> {
    let count: i64 = conn.query_row(
        "WITH RECURSIVE downstream(task_id) AS (
             SELECT dependency.task_id
               FROM deps dependency
              WHERE dependency.depends_on=?1
             UNION
             SELECT dependency.task_id
               FROM deps dependency
               JOIN downstream parent
                 ON dependency.depends_on=parent.task_id
         )
         SELECT COUNT(DISTINCT downstream.task_id)
           FROM downstream
           JOIN tasks task ON task.task_id=downstream.task_id
          WHERE task.status NOT IN ('done','retired','rejected')",
        params![task_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Ready tasks (proposed, no open deps), priority desc then seq asc.
/// With `include_blocked`, blocked proposed tasks follow with named blockers.
pub fn ready_tasks(
    conn: &Connection,
    plan_id: i64,
    limit: usize,
    include_blocked: bool,
) -> Result<Vec<ReadyEntry>> {
    // Tasks with children are containers: their children are the actionable
    // units, so they never enter the ready queue themselves.
    let mut st = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks t WHERE plan_id=?1 AND status='proposed'
         AND NOT EXISTS (SELECT 1 FROM tasks c WHERE c.parent_id = t.task_id)
         ORDER BY priority DESC, seq ASC"
    ))?;
    let tasks: Vec<Task> = st
        .query_map(params![plan_id], task_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    for task in tasks {
        let blockers = entry_blockers(conn, &task)?;
        if blockers.is_empty() {
            ready.push(ReadyEntry {
                task,
                blockers: vec![],
            });
        } else if include_blocked {
            blocked.push(ReadyEntry { task, blockers });
        }
    }
    ready.truncate(limit);
    let remaining = limit.saturating_sub(ready.len());
    ready.extend(blocked.into_iter().take(remaining));
    Ok(ready)
}

pub struct AuditFinding {
    pub kind: String,
    pub detail: String,
}

fn find_cycle(conn: &Connection, edge_sql: &str) -> Result<Option<Vec<i64>>> {
    let mut st = conn.prepare("SELECT task_id, seq FROM tasks")?;
    let seq_by_id: HashMap<i64, i64> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut st = conn.prepare(edge_sql)?;
    let edges: Vec<(i64, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut adjacency: HashMap<i64, Vec<i64>> = HashMap::new();
    for (from, to) in edges {
        adjacency.entry(from).or_default().push(to);
    }
    let mut states = HashMap::new();
    let mut stack = Vec::new();
    for task_id in seq_by_id.keys().copied() {
        if states.get(&task_id).copied().unwrap_or(0) != 0 {
            continue;
        }
        if let Some(ids) = visit_cycle(task_id, &adjacency, &mut states, &mut stack) {
            return Ok(Some(
                ids.into_iter()
                    .filter_map(|id| seq_by_id.get(&id).copied())
                    .collect(),
            ));
        }
    }
    Ok(None)
}

fn visit_cycle(
    node: i64,
    adjacency: &HashMap<i64, Vec<i64>>,
    states: &mut HashMap<i64, u8>,
    stack: &mut Vec<i64>,
) -> Option<Vec<i64>> {
    states.insert(node, 1);
    stack.push(node);
    for next in adjacency.get(&node).into_iter().flatten().copied() {
        match states.get(&next).copied().unwrap_or(0) {
            0 => {
                if let Some(cycle) = visit_cycle(next, adjacency, states, stack) {
                    return Some(cycle);
                }
            }
            1 => {
                let start = stack.iter().position(|id| *id == next).unwrap_or(0);
                let mut cycle = stack[start..].to_vec();
                cycle.push(next);
                return Some(cycle);
            }
            _ => {}
        }
    }
    stack.pop();
    states.insert(node, 2);
    None
}

fn replacement_repair_instruction(seq: i64, status: &str, replacement_seq: Option<i64>) -> String {
    let into = replacement_seq
        .map(|replacement| format!(" --into {replacement}"))
        .unwrap_or_default();
    let retire = format!("papertiger retire {seq}{into} --why <reason>");
    if matches!(status, "proposed" | "in_progress") {
        format!("run `{retire}`")
    } else {
        format!("run `papertiger reopen {seq} --why <reason>`, then `{retire}`")
    }
}

fn invalid_replacement_terminals(conn: &Connection) -> Result<Vec<(i64, i64, String)>> {
    let mut statement = conn.prepare(
        "SELECT source.seq, target.seq, target.status
           FROM tasks source
           JOIN tasks target ON target.task_id=source.replacement_task_id
          WHERE target.status='rejected'
             OR (target.status='retired' AND target.replacement_task_id IS NULL)
          ORDER BY source.seq",
    )?;
    Ok(statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn terminal_replacement_repair_instruction(target: i64) -> String {
    format!(
        "run `papertiger reopen {target} --why <reason>`, then `papertiger retire {target} --into <task> --why <reason>`"
    )
}

pub fn audit(conn: &Connection) -> Result<Vec<AuditFinding>> {
    let mut findings = Vec::new();
    let mut push = |kind: &str, detail: String| {
        findings.push(AuditFinding {
            kind: kind.into(),
            detail,
        })
    };

    for (kind, detail) in mise_projection::audit_mise_projections(conn)? {
        push(&kind, detail);
    }

    let mut statement = conn.prepare(
        "SELECT task.seq, commit_association.repository, commit_association.commit_oid,
                commit_association.recorded_at
           FROM commit_associations commit_association
           JOIN tasks task ON task.task_id=commit_association.task_id
          ORDER BY task.seq, commit_association.commit_association_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (seq, repository, commit_oid, recorded_at) in rows {
        if repository.trim().is_empty() {
            push(
                "blank_commit_repository",
                format!("#{seq} commit {commit_oid} has a blank repository label"),
            );
        } else if repository != repository.trim() {
            push(
                "noncanonical_commit_repository",
                format!(
                    "#{seq} commit {commit_oid} has repository label '{repository}' with surrounding whitespace"
                ),
            );
        }
        if validate_commit_oid(&commit_oid).is_err() {
            push(
                "malformed_commit_oid",
                format!("#{seq} commit '{commit_oid}' in repository '{repository}'"),
            );
        }
        if recorded_at != recorded_at.trim()
            || chrono::DateTime::parse_from_rfc3339(&recorded_at).is_err()
        {
            push(
                "invalid_commit_recorded_at",
                format!(
                    "#{seq} commit '{commit_oid}' in repository '{repository}' has noncanonical or invalid RFC3339 recorded_at '{recorded_at}'"
                ),
            );
        }
    }

    let mut statement = conn.prepare(
        "SELECT event_id, at, entity, entity_seq, kind, payload FROM events ORDER BY event_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (event_id, at, entity, entity_seq, kind, payload) in rows {
        if chrono::DateTime::parse_from_rfc3339(&at).is_err() {
            push(
                "invalid_event_timestamp",
                format!("event {event_id} has non-RFC3339 timestamp '{at}'"),
            );
        }
        if entity == "task" && kind == "status" {
            let valid_target = payload
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|value| {
                    value
                        .get("to")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .is_some_and(|target| TASK_STATUSES.contains(&target.as_str()));
            if !valid_target {
                let task = entity_seq
                    .map(|seq| format!("task #{seq}"))
                    .unwrap_or_else(|| "a task with no stable sequence".to_owned());
                push(
                    "invalid_task_status_event",
                    format!("event {event_id} for {task} lacks a canonical task-status payload.to"),
                );
            }
        }
    }

    // Closed gates with malformed or unknown-scheme evidence.
    let mut st = conn.prepare(
        "SELECT t.seq, g.name, g.evidence_locator FROM gates g JOIN tasks t ON t.task_id=g.task_id
         WHERE g.status='closed'",
    )?;
    let rows: Vec<(i64, String, Option<String>)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, name, loc) in rows {
        match loc {
            None => push(
                "gate_no_evidence",
                format!("#{seq} gate '{name}' closed without locator"),
            ),
            Some(l) => {
                if validate_new_evidence_locator(&l).is_err() {
                    push(
                        "malformed_evidence_locator",
                        format!("#{seq} gate '{name}' locator '{l}'"),
                    );
                }
            }
        }
    }

    let mut st = conn.prepare(
        "SELECT t.seq, g.name, g.evidence_sha256 FROM gates g
         JOIN tasks t ON t.task_id=g.task_id WHERE g.evidence_sha256 IS NOT NULL",
    )?;
    let rows: Vec<(i64, String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, name, digest) in rows {
        if validate_optional_sha256(Some(&digest)).is_err() {
            push(
                "malformed_evidence_sha256",
                format!("#{seq} gate '{name}' has noncanonical sha256 '{digest}'"),
            );
        }
    }

    let mut st = conn.prepare(
        "SELECT t.seq, g.name FROM gates g JOIN tasks t ON t.task_id=g.task_id
         WHERE t.status='done' AND g.status='open'",
    )?;
    let rows: Vec<(i64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, name) in rows {
        push(
            "done_with_open_gate",
            format!("#{seq} is done with open gate '{name}'"),
        );
    }

    let mut st = conn.prepare(
        "SELECT t.seq, g.name FROM gates g JOIN tasks t ON t.task_id=g.task_id
         WHERE g.status='waived' AND (g.note IS NULL OR trim(g.note)='')",
    )?;
    let rows: Vec<(i64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, name) in rows {
        push(
            "waiver_without_reason",
            format!("#{seq} gate '{name}' is waived without a durable reason"),
        );
    }

    let mut st = conn.prepare(
        "SELECT t.seq, b.name, b.status, b.evidence_locator, b.evidence_sha256, b.note
           FROM task_blockers b
           JOIN tasks t ON t.task_id=b.task_id
          WHERE b.status<>'open'",
    )?;
    type ResolvedBlockerRow = (
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<ResolvedBlockerRow> = st
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, name, status, locator, sha256, note) in rows {
        match status.as_str() {
            "resolved" => match locator {
                None => push(
                    "blocker_no_evidence",
                    format!("#{seq} blocker '{name}' resolved without locator"),
                ),
                Some(locator) => {
                    if validate_new_evidence_locator(&locator).is_err() {
                        push(
                            "malformed_evidence_locator",
                            format!("#{seq} blocker '{name}' locator '{locator}'"),
                        );
                    }
                }
            },
            "waived" if note.as_deref().map(str::trim).is_none_or(str::is_empty) => push(
                "blocker_waiver_without_reason",
                format!("#{seq} blocker '{name}' is waived without a durable reason"),
            ),
            _ => {}
        }
        if let Some(digest) = sha256
            && validate_optional_sha256(Some(&digest)).is_err()
        {
            push(
                "malformed_evidence_sha256",
                format!("#{seq} blocker '{name}' has noncanonical sha256 '{digest}'"),
            );
        }
    }

    let mut st = conn.prepare(
        "SELECT t.seq, b.name, t.status
           FROM task_blockers b
           JOIN tasks t ON t.task_id=b.task_id
          WHERE b.status='open' AND t.status='done'",
    )?;
    let rows: Vec<(i64, String, String)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, name, status) in rows {
        push(
            "done_with_open_blocker",
            format!("#{seq} is {status} with open blocker '{name}'"),
        );
    }

    let mut st = conn.prepare(
        "SELECT seq, kind FROM tasks
          WHERE status='done' AND kind IN ('probe','decision')
            AND (result IS NULL OR trim(result)='')",
    )?;
    let rows: Vec<(i64, String)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, kind) in rows {
        push(
            "missing_task_result",
            format!("#{seq} is a completed {kind} without a durable result"),
        );
    }

    let mut st = conn.prepare(
        "SELECT task.seq, dependency.seq
           FROM deps
           JOIN tasks task ON task.task_id=deps.task_id
           JOIN tasks dependency ON dependency.task_id=deps.depends_on
          WHERE task.status='done' AND dependency.status<>'done'",
    )?;
    let rows: Vec<(i64, i64)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (task, dependency) in rows {
        push(
            "done_with_open_dependency",
            format!("#{task} is done while dependency #{dependency} is not done"),
        );
    }

    let mut st = conn.prepare(
        "SELECT parent.seq, child.seq
           FROM tasks parent
           JOIN tasks child ON child.parent_id=parent.task_id
          WHERE parent.status='done' AND child.status IN ('proposed','in_progress')",
    )?;
    let rows: Vec<(i64, i64)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (parent, child) in rows {
        push(
            "done_with_live_child",
            format!("#{parent} is done while child #{child} remains live"),
        );
    }

    let mut st = conn.prepare(
        "SELECT plan.slug, COUNT(task.task_id)
           FROM plans plan
           JOIN tasks task ON task.plan_id=plan.plan_id
          WHERE plan.status='done' AND task.status IN ('proposed','in_progress')
          GROUP BY plan.plan_id",
    )?;
    let rows: Vec<(String, i64)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (plan, count) in rows {
        push(
            "done_plan_with_live_tasks",
            format!("plan '{plan}' is done with {count} live task(s)"),
        );
    }

    let mut st = conn.prepare(
        "SELECT a.seq, b.seq FROM deps d JOIN tasks a ON a.task_id=d.task_id
         JOIN tasks b ON b.task_id=d.depends_on WHERE a.plan_id<>b.plan_id",
    )?;
    let rows: Vec<(i64, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (task, dependency) in rows {
        push(
            "cross_plan_dep",
            format!("#{task} depends on #{dependency} from a different plan"),
        );
    }

    let mut st = conn.prepare(
        "SELECT c.seq, p.seq FROM tasks c JOIN tasks p ON p.task_id=c.parent_id
         WHERE c.plan_id<>p.plan_id",
    )?;
    let rows: Vec<(i64, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (child, parent) in rows {
        push(
            "cross_plan_parent",
            format!("#{child} has parent #{parent} from a different plan"),
        );
    }

    let mut st = conn.prepare(
        "SELECT task.seq, task.status, task.replacement_task_id
           FROM tasks task
           LEFT JOIN tasks replacement ON replacement.task_id=task.replacement_task_id
          WHERE task.replacement_task_id IS NOT NULL AND replacement.task_id IS NULL",
    )?;
    let rows: Vec<(i64, String, i64)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (task, status, replacement_task_id) in rows {
        push(
            "dangling_replacement",
            format!(
                "#{task} names missing replacement row id {replacement_task_id}; restore the referenced task or {} to clear the pointer",
                replacement_repair_instruction(task, &status, None)
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT task.seq, task.status, replacement.seq,
                task.plan_id=replacement.plan_id, replacement.status
           FROM tasks task
           JOIN tasks replacement ON replacement.task_id=task.replacement_task_id
          WHERE task.status<>'retired'",
    )?;
    let rows: Vec<(i64, String, i64, bool, String)> = st
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (task, status, replacement, same_plan, replacement_status) in rows {
        let reusable_replacement = (same_plan
            && matches!(
                replacement_status.as_str(),
                "proposed" | "in_progress" | "done"
            ))
        .then_some(replacement);
        push(
            "replacement_on_nonretired_task",
            format!(
                "#{task} is {status} but names replacement #{replacement}; valid replacement links require a retired source; {}",
                replacement_repair_instruction(task, &status, reusable_replacement)
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT task.seq, task.status, replacement.seq
           FROM tasks task
           JOIN tasks replacement ON replacement.task_id=task.replacement_task_id
          WHERE task.plan_id<>replacement.plan_id",
    )?;
    let rows: Vec<(i64, String, i64)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (task, status, replacement) in rows {
        push(
            "cross_plan_replacement",
            format!(
                "#{task} names replacement #{replacement} from a different plan; {} to clear the pointer before choosing a same-plan replacement",
                replacement_repair_instruction(task, &status, None)
            ),
        );
    }

    for (source, target, status) in invalid_replacement_terminals(conn)? {
        let terminal = if status == "retired" {
            "retired without its own replacement".to_owned()
        } else {
            status
        };
        push(
            "replacement_terminal_dead_end",
            format!(
                "#{source} replacement chain terminates at #{target}, which is {terminal}; {} to extend the chain through a live canonical task",
                terminal_replacement_repair_instruction(target)
            ),
        );
    }

    if let Some(cycle) = find_cycle(conn, "SELECT task_id, depends_on FROM deps")? {
        push(
            "dependency_cycle",
            format!(
                "dependency cycle {}",
                cycle
                    .iter()
                    .map(|seq| format!("#{seq}"))
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
        );
    }
    if let Some(cycle) = find_cycle(
        conn,
        "SELECT task_id, parent_id FROM tasks WHERE parent_id IS NOT NULL",
    )? {
        push(
            "parent_cycle",
            format!(
                "parent cycle {}",
                cycle
                    .iter()
                    .map(|seq| format!("#{seq}"))
                    .collect::<Vec<_>>()
                    .join(" -> ")
            ),
        );
    }
    if let Some(cycle) = find_cycle(
        conn,
        "SELECT task_id, replacement_task_id FROM tasks WHERE replacement_task_id IS NOT NULL",
    )? {
        let repair_task = cycle[0];
        let repair_status = get_task(conn, repair_task)?.status;
        push(
            "replacement_cycle",
            format!(
                "replacement cycle {}; {} to break one edge",
                cycle
                    .iter()
                    .map(|seq| format!("#{seq}"))
                    .collect::<Vec<_>>()
                    .join(" -> "),
                replacement_repair_instruction(repair_task, &repair_status, None)
            ),
        );
    }

    // Live tasks depending on retired/rejected tasks.
    let mut st = conn.prepare(
        "SELECT a.seq, b.seq, b.status FROM deps d
         JOIN tasks a ON a.task_id=d.task_id JOIN tasks b ON b.task_id=d.depends_on
         WHERE a.status IN ('proposed','in_progress') AND b.status IN ('retired','rejected')",
    )?;
    let rows: Vec<(i64, i64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (a, b, s) in rows {
        push("dep_on_dead", format!("#{a} depends on #{b} which is {s}"));
    }

    // Parents lagging: all children done but parent not.
    let mut st = conn.prepare(
        "SELECT p.seq FROM tasks p WHERE p.status IN ('proposed','in_progress')
         AND EXISTS (SELECT 1 FROM tasks c WHERE c.parent_id=p.task_id)
         AND NOT EXISTS (SELECT 1 FROM tasks c WHERE c.parent_id=p.task_id
                         AND c.status IN ('proposed','in_progress'))",
    )?;
    let rows: Vec<i64> = st
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for seq in rows {
        push(
            "parent_lagging",
            format!("#{seq}: all children finished but parent is open"),
        );
    }

    Ok(findings)
}

// ---- export / import ------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub struct Dump {
    pub schema: String,
    pub plans: Vec<PlanDump>,
    pub tasks: Vec<TaskDump>,
    #[serde(default)]
    pub events: Vec<EventDump>,
    #[serde(default)]
    pub mise_projections: Vec<MiseProjectionDump>,
}

/// Parse an exported dump, accepting the UTF-8 BOM emitted by Windows
/// PowerShell 5's common `Set-Content -Encoding utf8` workflow.
pub fn parse_dump_json(text: &str) -> Result<Dump> {
    serde_json::from_str(text.trim_start_matches('\u{feff}')).context("parse papertiger dump")
}

#[derive(Serialize, Deserialize)]
pub struct PlanDump {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default = "default_active")]
    pub status: String,
}

fn default_active() -> String {
    "active".into()
}

#[derive(Serialize, Deserialize)]
pub struct TaskDump {
    #[serde(default)]
    pub seq: Option<i64>,
    pub plan: String,
    pub title: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default = "default_work")]
    pub kind: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default = "default_proposed")]
    pub status: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub parent_seq: Option<i64>,
    #[serde(default)]
    pub replacement_seq: Option<i64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub deps: Vec<i64>,
    #[serde(default)]
    pub gates: Vec<GateDump>,
    #[serde(default)]
    pub blockers: Vec<TaskBlocker>,
    #[serde(default)]
    pub commit_associations: Vec<CommitAssociation>,
}

fn default_work() -> String {
    "work".into()
}

fn default_proposed() -> String {
    "proposed".into()
}

#[derive(Serialize, Deserialize)]
pub struct GateDump {
    pub name: String,
    pub kind: String,
    pub requirement: String,
    #[serde(default = "default_open")]
    pub status: String,
    #[serde(default)]
    pub evidence_locator: Option<String>,
    #[serde(default)]
    pub evidence_sha256: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub closed_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct EventDump {
    pub at: String,
    pub actor: String,
    pub entity: String,
    #[serde(default)]
    pub entity_seq: Option<i64>,
    #[serde(default)]
    pub entity_plan: Option<String>,
    #[serde(default)]
    pub gate_name: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
}

fn default_open() -> String {
    "open".into()
}

pub fn export(conn: &Connection, plan: Option<&str>) -> Result<Dump> {
    let mut plans = Vec::new();
    let mut st =
        conn.prepare("SELECT plan_id, slug, title, intent, status FROM plans ORDER BY plan_id")?;
    let all: Vec<(i64, String, String, String, String)> = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut tasks = Vec::new();
    for (plan_id, slug, title, intent, status) in all {
        if let Some(want) = plan
            && want != slug
        {
            continue;
        }
        plans.push(PlanDump {
            slug: slug.clone(),
            title,
            intent,
            status,
        });
        let mut st = conn.prepare(&format!(
            "SELECT {TASK_COLS} FROM tasks WHERE plan_id=?1 ORDER BY seq"
        ))?;
        let plan_tasks: Vec<Task> = st
            .query_map(params![plan_id], task_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        for t in plan_tasks {
            let parent_seq = match t.parent_id {
                Some(pid) => conn
                    .query_row(
                        "SELECT seq FROM tasks WHERE task_id=?1",
                        params![pid],
                        |r| r.get(0),
                    )
                    .optional()?,
                None => None,
            };
            let replacement_seq = match t.replacement_task_id {
                Some(replacement_task_id) => {
                    let replacement: (i64, String, Option<i64>) = conn
                        .query_row(
                        "SELECT seq, status, replacement_task_id FROM tasks WHERE task_id=?1",
                        params![replacement_task_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                        .optional()?
                        .ok_or_else(|| {
                            anyhow!(
                                "cannot export: #{} names missing replacement row id {}; {}; run `papertiger audit` to inspect every invalid replacement",
                                t.seq,
                                replacement_task_id,
                                replacement_repair_instruction(t.seq, &t.status, None)
                            )
                        })?;
                    if replacement.1 == "rejected"
                        || (replacement.1 == "retired" && replacement.2.is_none())
                    {
                        let terminal = if replacement.1 == "retired" {
                            "retired without its own replacement"
                        } else {
                            replacement.1.as_str()
                        };
                        bail!(
                            "cannot export: #{} replacement chain terminates at #{}, which is {}; {}; run `papertiger audit` to inspect every invalid replacement",
                            t.seq,
                            replacement.0,
                            terminal,
                            terminal_replacement_repair_instruction(replacement.0)
                        );
                    }
                    Some(replacement.0)
                }
                None => None,
            };
            let mut st = conn.prepare(
                "SELECT b.seq FROM deps d JOIN tasks b ON b.task_id=d.depends_on WHERE d.task_id=?1 ORDER BY b.seq",
            )?;
            let deps: Vec<i64> = st
                .query_map(params![t.task_id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut st = conn.prepare("SELECT tag FROM task_tags WHERE task_id=?1 ORDER BY tag")?;
            let tags: Vec<String> = st
                .query_map(params![t.task_id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut st = conn.prepare(
                "SELECT name, kind, requirement, status, evidence_locator, evidence_sha256, note, closed_at
                 FROM gates WHERE task_id=?1 ORDER BY gate_id",
            )?;
            let gates: Vec<GateDump> = st
                .query_map(params![t.task_id], |r| {
                    Ok(GateDump {
                        name: r.get(0)?,
                        kind: r.get(1)?,
                        requirement: r.get(2)?,
                        status: r.get(3)?,
                        evidence_locator: r.get(4)?,
                        evidence_sha256: r.get(5)?,
                        note: r.get(6)?,
                        closed_at: r.get(7)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            let blockers = task_blockers(conn, t.task_id)?;
            let commit_associations = commit_associations(conn, t.seq)?;
            tasks.push(TaskDump {
                seq: Some(t.seq),
                plan: slug.clone(),
                title: t.title,
                intent: t.intent,
                kind: t.kind,
                result: t.result,
                status: t.status,
                priority: t.priority,
                parent_seq,
                replacement_seq,
                tags,
                deps,
                gates,
                blockers,
                commit_associations,
            });
        }
    }
    let selected_plans: HashSet<&str> = plans.iter().map(|p| p.slug.as_str()).collect();
    let mut events = Vec::new();
    let mut st = conn.prepare(
        "SELECT at, actor, entity, entity_plan, entity_seq, gate_name, kind, why, payload
         FROM events ORDER BY event_id",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, Option<String>>(8)?,
        ))
    })?;
    for row in rows {
        let (at, actor, entity, mut entity_plan, entity_seq, gate_name, kind, why, payload) = row?;
        match entity.as_str() {
            "plan" if entity_plan.is_none() => {
                if plans.len() == 1 {
                    entity_plan = Some(plans[0].slug.clone());
                } else if plan.is_none() {
                    // A database-global plan event remains global in a full export.
                } else {
                    continue;
                }
            }
            "plan" | "task" | "dep" | "gate" => {}
            _ => continue,
        }
        if entity_plan
            .as_deref()
            .is_some_and(|slug| !selected_plans.contains(slug))
        {
            continue;
        }
        if matches!(entity.as_str(), "task" | "dep" | "gate")
            && (entity_plan.is_none() || entity_seq.is_none())
        {
            bail!("event history has a {entity} row without a stable plan/task reference");
        }
        if entity == "gate" && gate_name.is_none() {
            bail!("event history has a gate row without a stable gate name");
        }
        events.push(EventDump {
            at,
            actor,
            entity,
            entity_seq,
            entity_plan,
            gate_name,
            kind,
            why,
            payload: payload
                .map(|raw| serde_json::from_str(&raw).context("invalid stored event payload"))
                .transpose()?,
        });
    }

    let selected_task_sequences = tasks
        .iter()
        .filter_map(|task| task.seq)
        .collect::<HashSet<_>>();
    let mise_projections =
        mise_projection::export_mise_projections(conn, &selected_task_sequences)?;

    Ok(Dump {
        schema: "papertiger.dump.v6".into(),
        plans,
        tasks,
        events,
        mise_projections,
    })
}

pub fn import(conn: &mut Connection, actor: &str, dump: &Dump) -> Result<(usize, usize)> {
    if dump.schema != "papertiger.dump.v6" {
        bail!(
            "unsupported dump schema '{}'; use the Papertiger release that produced it to import it into a temporary authority, run current `papertiger --db <temporary-authority> init`, then re-export `papertiger.dump.v6`",
            dump.schema
        );
    }
    let tx = begin_mutation(conn)?;
    for p in &dump.plans {
        require_nonblank("import plan slug", &p.slug)?;
        require_nonblank("import plan title", &p.title)?;
        if !["active", "paused", "done", "retired"].contains(&p.status.as_str()) {
            bail!("import plan '{}' has unknown status '{}'", p.slug, p.status);
        }
        let exists: Option<i64> = tx
            .query_row(
                "SELECT plan_id FROM plans WHERE slug=?1",
                params![p.slug],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            let t = now();
            tx.execute(
                "INSERT INTO plans (slug, title, intent, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![p.slug, p.title, p.intent, p.status, t],
            )?;
        }
    }

    // Allocate every sequence before writing anything. Explicit values are
    // stable; omitted values receive unused numbers above the current maximum.
    let mut used = HashSet::new();
    for td in &dump.tasks {
        if let Some(seq) = td.seq {
            if seq <= 0 {
                bail!("import task '{}' has nonpositive seq {seq}", td.title);
            }
            if !used.insert(seq) {
                bail!("import repeats task seq {seq}");
            }
        }
    }
    for seq in &used {
        let existing_title: Option<String> = tx
            .query_row(
                "SELECT title FROM tasks WHERE seq=?1",
                params![seq],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(title) = existing_title {
            bail!(
                "import task seq {seq} collides with existing task #{seq} '{title}'; import into an authority without that sequence"
            );
        }
    }
    let mut next_seq: i64 =
        tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM tasks", [], |r| r.get(0))?;
    if let Some(explicit_next) = used.iter().max().and_then(|seq| seq.checked_add(1)) {
        next_seq = next_seq.max(explicit_next);
    }
    let mut assigned = Vec::with_capacity(dump.tasks.len());
    for td in &dump.tasks {
        let seq = match td.seq {
            Some(seq) => seq,
            None => {
                while used.contains(&next_seq) {
                    next_seq += 1;
                }
                let seq = next_seq;
                used.insert(seq);
                next_seq += 1;
                seq
            }
        };
        assigned.push(seq);
    }
    let imported_plan_by_seq: HashMap<i64, &str> = assigned
        .iter()
        .copied()
        .zip(dump.tasks.iter().map(|task| task.plan.as_str()))
        .collect();

    // First pass: create tasks and gates.
    let mut created = 0usize;
    let mut imported_task_id_by_seq = HashMap::with_capacity(dump.tasks.len());
    for (td, seq) in dump.tasks.iter().zip(assigned.iter().copied()) {
        require_nonblank("import task title", &td.title)?;
        let plan_id: i64 = tx
            .query_row(
                "SELECT plan_id FROM plans WHERE slug=?1",
                params![td.plan],
                |r| r.get(0),
            )
            .with_context(|| {
                format!(
                    "import task '{0}' names missing plan '{1}'",
                    td.title, td.plan
                )
            })?;
        if !TASK_STATUSES.contains(&td.status.as_str()) {
            bail!("task '{}' has unknown status '{}'", td.title, td.status);
        }
        validate_task_kind(&td.kind)?;
        if td.status == "done" && td.gates.iter().any(|gate| gate.status == "open") {
            bail!("import: done task '{}' has an open gate", td.title);
        }
        if td.status == "done" && td.blockers.iter().any(|blocker| blocker.status == "open") {
            bail!("import: done task '{}' has an open blocker", td.title);
        }
        if td.status == "done"
            && matches!(td.kind.as_str(), "probe" | "decision")
            && td
                .result
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
        {
            bail!(
                "import: completed {} task '{}' lacks a result",
                td.kind,
                td.title
            );
        }
        let t = now();
        tx.execute(
            "INSERT INTO tasks
             (seq, plan_id, title, intent, kind, result, status, priority, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                seq,
                plan_id,
                td.title,
                td.intent,
                td.kind,
                td.result,
                td.status,
                td.priority,
                t
            ],
        )
        .with_context(|| format!("import task seq {seq} '{}'", td.title))?;
        let id = tx.last_insert_rowid();
        imported_task_id_by_seq.insert(seq, id);
        for tag in &td.tags {
            tx.execute(
                "INSERT OR IGNORE INTO task_tags (task_id, tag) VALUES (?1, ?2)",
                params![id, tag],
            )?;
        }
        for g in &td.gates {
            require_nonblank("import gate name", &g.name)?;
            require_nonblank("import gate requirement", &g.requirement)?;
            if !GATE_KINDS.contains(&g.kind.as_str()) {
                bail!(
                    "import: gate '{}' on '{}' has unknown kind '{}'",
                    g.name,
                    td.title,
                    g.kind
                );
            }
            if !["open", "closed", "waived"].contains(&g.status.as_str()) {
                bail!(
                    "import: gate '{}' on '{}' has unknown status '{}'",
                    g.name,
                    td.title,
                    g.status
                );
            }
            let closed_at = match g.status.as_str() {
                "open" => {
                    if g.evidence_locator.is_some()
                        || g.evidence_sha256.is_some()
                        || g.closed_at.is_some()
                    {
                        bail!(
                            "import: open gate '{}' on '{}' carries completion evidence or closed_at",
                            g.name,
                            td.title
                        );
                    }
                    None
                }
                "closed" => {
                    let locator = g.evidence_locator.as_deref().with_context(|| {
                        format!(
                            "import: closed gate '{}' on '{}' lacks evidence_locator",
                            g.name, td.title
                        )
                    })?;
                    validate_evidence_locator(locator)?;
                    validate_optional_sha256(g.evidence_sha256.as_deref())?;
                    let closed_at = g.closed_at.as_deref().with_context(|| {
                        format!(
                            "import: closed gate '{}' on '{}' lacks closed_at",
                            g.name, td.title
                        )
                    })?;
                    let closed_at = closed_at.trim();
                    chrono::DateTime::parse_from_rfc3339(closed_at).with_context(|| {
                        format!(
                            "import: closed gate '{}' on '{}' has invalid closed_at '{}'",
                            g.name,
                            td.title,
                            g.closed_at.as_deref().unwrap_or_default()
                        )
                    })?;
                    Some(closed_at)
                }
                "waived" => {
                    if g.note.as_deref().map(str::trim).is_none_or(str::is_empty) {
                        bail!(
                            "import: waived gate '{}' on '{}' lacks a durable reason",
                            g.name,
                            td.title
                        );
                    }
                    if g.evidence_locator.is_some() || g.evidence_sha256.is_some() {
                        bail!(
                            "import: waived gate '{}' on '{}' carries completion evidence",
                            g.name,
                            td.title
                        );
                    }
                    let closed_at = g.closed_at.as_deref().with_context(|| {
                        format!(
                            "import: waived gate '{}' on '{}' lacks closed_at",
                            g.name, td.title
                        )
                    })?;
                    let closed_at = closed_at.trim();
                    chrono::DateTime::parse_from_rfc3339(closed_at).with_context(|| {
                        format!(
                            "import: waived gate '{}' on '{}' has invalid closed_at '{}'",
                            g.name,
                            td.title,
                            g.closed_at.as_deref().unwrap_or_default()
                        )
                    })?;
                    Some(closed_at)
                }
                _ => unreachable!(),
            };
            tx.execute(
                "INSERT INTO gates (task_id, name, kind, requirement, status, evidence_locator, evidence_sha256, note, closed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id, g.name, g.kind, g.requirement, g.status,
                    g.evidence_locator, g.evidence_sha256, g.note,
                    closed_at
                ],
            )?;
        }
        for blocker in &td.blockers {
            if blocker.name.trim().is_empty() || blocker.reason.trim().is_empty() {
                bail!(
                    "import: blocker on '{}' has a blank name or reason",
                    td.title
                );
            }
            if !["open", "resolved", "waived"].contains(&blocker.status.as_str()) {
                bail!(
                    "import: blocker '{}' on '{}' has unknown status '{}'",
                    blocker.name,
                    td.title,
                    blocker.status
                );
            }
            let resolved_at = match blocker.status.as_str() {
                "open" => {
                    if blocker.evidence_locator.is_some()
                        || blocker.evidence_sha256.is_some()
                        || blocker.resolved_at.is_some()
                    {
                        bail!(
                            "import: open blocker '{}' on '{}' carries resolution evidence",
                            blocker.name,
                            td.title
                        );
                    }
                    None
                }
                "resolved" => {
                    let locator = blocker.evidence_locator.as_deref().with_context(|| {
                        format!(
                            "import: resolved blocker '{}' on '{}' lacks evidence_locator",
                            blocker.name, td.title
                        )
                    })?;
                    validate_evidence_locator(locator)?;
                    validate_optional_sha256(blocker.evidence_sha256.as_deref())?;
                    let resolved_at = blocker.resolved_at.as_deref().with_context(|| {
                        format!(
                            "import: resolved blocker '{}' on '{}' lacks resolved_at",
                            blocker.name, td.title
                        )
                    })?;
                    let resolved_at = resolved_at.trim();
                    chrono::DateTime::parse_from_rfc3339(resolved_at).with_context(|| {
                        format!(
                            "import: resolved blocker '{}' on '{}' has invalid resolved_at '{}'",
                            blocker.name,
                            td.title,
                            blocker.resolved_at.as_deref().unwrap_or_default()
                        )
                    })?;
                    Some(resolved_at)
                }
                "waived" => {
                    if blocker
                        .note
                        .as_deref()
                        .map(str::trim)
                        .is_none_or(str::is_empty)
                    {
                        bail!(
                            "import: waived blocker '{}' on '{}' lacks a durable reason",
                            blocker.name,
                            td.title
                        );
                    }
                    if blocker.evidence_locator.is_some() || blocker.evidence_sha256.is_some() {
                        bail!(
                            "import: waived blocker '{}' on '{}' carries resolution evidence",
                            blocker.name,
                            td.title
                        );
                    }
                    let resolved_at = blocker.resolved_at.as_deref().with_context(|| {
                        format!(
                            "import: waived blocker '{}' on '{}' lacks resolved_at",
                            blocker.name, td.title
                        )
                    })?;
                    let resolved_at = resolved_at.trim();
                    chrono::DateTime::parse_from_rfc3339(resolved_at).with_context(|| {
                        format!(
                            "import: waived blocker '{}' on '{}' has invalid resolved_at '{}'",
                            blocker.name,
                            td.title,
                            blocker.resolved_at.as_deref().unwrap_or_default()
                        )
                    })?;
                    Some(resolved_at)
                }
                _ => unreachable!(),
            };
            tx.execute(
                "INSERT INTO task_blockers
                 (task_id, name, reason, status, evidence_locator, evidence_sha256, note, resolved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    blocker.name,
                    blocker.reason,
                    blocker.status,
                    blocker.evidence_locator,
                    blocker.evidence_sha256,
                    blocker.note,
                    resolved_at
                ],
            )?;
        }
        for commit in &td.commit_associations {
            let repository =
                require_nonblank("import commit repository label", &commit.repository)?;
            validate_commit_oid(&commit.commit_oid)?;
            let recorded_at = commit.recorded_at.trim();
            chrono::DateTime::parse_from_rfc3339(recorded_at).with_context(|| {
                format!(
                    "import commit {} on task #{seq} has invalid recorded_at '{}'",
                    commit.commit_oid, commit.recorded_at
                )
            })?;
            tx.execute(
                "INSERT INTO commit_associations
                 (task_id, repository, commit_oid, note, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, repository, commit.commit_oid, commit.note, recorded_at],
            )?;
        }
        created += 1;
    }

    // Second pass: parents, replacements, and dependencies by the preassigned sequences.
    // References are self-contained within the dump and cannot accidentally
    // bind to an unrelated task already present in the destination database.
    let mut edges = 0usize;
    for (td, seq) in dump.tasks.iter().zip(assigned.iter().copied()) {
        if let Some(pseq) = td.parent_seq {
            let parent_plan = imported_plan_by_seq
                .get(&pseq)
                .with_context(|| format!("import: #{seq} has missing parent #{pseq}"))?;
            if *parent_plan != td.plan {
                bail!("import: #{seq} and parent #{pseq} belong to different plans");
            }
            let parent_id = imported_task_id_by_seq[&pseq];
            tx.execute(
                "UPDATE tasks SET parent_id=?1 WHERE seq=?2",
                params![parent_id, seq],
            )?;
        }
        if let Some(replacement_seq) = td.replacement_seq {
            if td.status != "retired" {
                bail!(
                    "import: #{seq} has replacement #{replacement_seq} but status '{}' is not retired",
                    td.status
                );
            }
            if replacement_seq == seq {
                bail!("import: #{seq} cannot replace itself");
            }
            let replacement_plan =
                imported_plan_by_seq
                    .get(&replacement_seq)
                    .with_context(|| {
                        format!("import: #{seq} has missing replacement #{replacement_seq}")
                    })?;
            if *replacement_plan != td.plan {
                bail!(
                    "import: #{seq} and replacement #{replacement_seq} belong to different plans"
                );
            }
            let replacement_id = imported_task_id_by_seq[&replacement_seq];
            tx.execute(
                "UPDATE tasks SET replacement_task_id=?1 WHERE seq=?2",
                params![replacement_id, seq],
            )?;
        }
        for on in &td.deps {
            let dependency_plan = imported_plan_by_seq
                .get(on)
                .with_context(|| format!("import: #{seq} depends on missing #{on}"))?;
            if *dependency_plan != td.plan {
                bail!("import: #{seq} and dependency #{on} belong to different plans");
            }
            add_dep_inner(&tx, actor, seq, *on, false, None)?;
            edges += 1;
        }
    }
    if let Some(cycle) = find_cycle(
        &tx,
        "SELECT task_id, parent_id FROM tasks WHERE parent_id IS NOT NULL",
    )? {
        bail!(
            "import creates parent cycle {}",
            cycle
                .iter()
                .map(|seq| format!("#{seq}"))
                .collect::<Vec<_>>()
                .join(" -> ")
        );
    }
    if let Some(cycle) = find_cycle(
        &tx,
        "SELECT task_id, replacement_task_id FROM tasks WHERE replacement_task_id IS NOT NULL",
    )? {
        bail!(
            "import creates replacement cycle {}",
            cycle
                .iter()
                .map(|seq| format!("#{seq}"))
                .collect::<Vec<_>>()
                .join(" -> ")
        );
    }
    if let Some((source, target, status)) = invalid_replacement_terminals(&tx)?.into_iter().next() {
        let terminal = if status == "retired" {
            "retired without its own replacement".to_owned()
        } else {
            status
        };
        bail!(
            "import: #{source} replacement chain terminates at #{target}, which is {terminal}; re-export after repairing the source authority: {}",
            terminal_replacement_repair_instruction(target)
        );
    }
    let mut statement =
        tx.prepare("SELECT task_id, seq FROM tasks WHERE status='done' ORDER BY seq")?;
    let done_tasks = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (task_id, seq) in done_tasks {
        let blockers = entry_blockers(&tx, &get_task(&tx, seq)?)?;
        if !blockers.is_empty() {
            bail!(
                "import: done task #{seq} retains open prerequisite(s): {}",
                blockers.join(", ")
            );
        }
        let children = live_child_sequences(&tx, task_id)?;
        if !children.is_empty() {
            bail!(
                "import: done task #{seq} has unfinished child task(s): {}",
                children
                    .iter()
                    .map(|child| format!("#{child}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    mise_projection::import_mise_projections(
        &tx,
        &dump.mise_projections,
        &imported_task_id_by_seq,
    )?;

    // Restore the append-only history using stable task sequences, plan slugs,
    // and gate names rather than database-local row ids.
    for event in &dump.events {
        let at = event.at.trim();
        if at.is_empty() || event.actor.trim().is_empty() || event.kind.trim().is_empty() {
            bail!("import event has a blank timestamp, actor, or kind");
        }
        chrono::DateTime::parse_from_rfc3339(at).with_context(|| {
            format!("import event timestamp '{}' is not valid RFC3339", event.at)
        })?;
        let entity_id: Option<i64> = match event.entity.as_str() {
            "plan" => event
                .entity_plan
                .as_deref()
                .map(|slug| {
                    tx.query_row(
                        "SELECT plan_id FROM plans WHERE slug=?1",
                        params![slug],
                        |r| r.get(0),
                    )
                    .with_context(|| format!("import event names missing plan '{slug}'"))
                })
                .transpose()?,
            "task" | "dep" => {
                let seq = event
                    .entity_seq
                    .context("import task/dep event lacks entity_seq")?;
                let task_plan = imported_plan_by_seq.get(&seq).with_context(|| {
                    format!("import event names task #{seq}, which is absent from the dump")
                })?;
                let event_plan = event.entity_plan.as_deref().with_context(|| {
                    format!(
                        "import {} event for task #{seq} lacks entity_plan",
                        event.entity
                    )
                })?;
                if event_plan != *task_plan {
                    bail!(
                        "import {} event names plan '{event_plan}', but task #{seq} belongs to plan '{task_plan}'",
                        event.entity
                    );
                }
                Some(*imported_task_id_by_seq.get(&seq).with_context(|| {
                    format!("import event names task #{seq}, which is absent from the dump")
                })?)
            }
            "gate" => {
                let seq = event
                    .entity_seq
                    .context("import gate event lacks entity_seq")?;
                let task_plan = imported_plan_by_seq.get(&seq).with_context(|| {
                    format!("import gate event names task #{seq}, which is absent from the dump")
                })?;
                let event_plan = event.entity_plan.as_deref().with_context(|| {
                    format!("import gate event for task #{seq} lacks entity_plan")
                })?;
                if event_plan != *task_plan {
                    bail!(
                        "import gate event names plan '{event_plan}', but task #{seq} belongs to plan '{task_plan}'"
                    );
                }
                let task_id = *imported_task_id_by_seq.get(&seq).with_context(|| {
                    format!("import gate event names task #{seq}, which is absent from the dump")
                })?;
                let name = event
                    .gate_name
                    .as_deref()
                    .context("import gate event lacks gate_name")?;
                tx.query_row(
                    "SELECT gate_id FROM gates WHERE task_id=?1 AND name=?2",
                    params![task_id, name],
                    |r| r.get(0),
                )
                .optional()?
            }
            other => bail!("import event has unknown entity '{other}'"),
        };
        if event.entity == "task" && event.kind == "status" {
            let target = event
                .payload
                .as_ref()
                .and_then(|payload| payload.get("to"))
                .and_then(serde_json::Value::as_str);
            if target.is_none_or(|target| !TASK_STATUSES.contains(&target)) {
                let seq = event.entity_seq.unwrap_or_default();
                bail!(
                    "import task status event for task #{seq} requires payload.to to be one of {}",
                    TASK_STATUSES.join("|")
                );
            }
        }
        tx.execute(
            "INSERT INTO events
             (at, actor, entity, entity_id, entity_plan, entity_seq, gate_name, kind, why, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                at,
                event.actor,
                event.entity,
                entity_id,
                event.entity_plan,
                event.entity_seq,
                event.gate_name,
                event.kind,
                event.why,
                event.payload.as_ref().map(serde_json::Value::to_string)
            ],
        )?;
    }
    record_event_in_mutation(
        &tx,
        actor,
        "plan",
        None,
        "import",
        None,
        Some(&serde_json::json!({
            "tasks": created,
            "deps": edges,
            "events": dump.events.len(),
            "mise_projections": dump.mise_projections.len()
        })),
    )?;
    tx.commit()?;
    Ok((created, edges))
}
