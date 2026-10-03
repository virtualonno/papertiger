//! Planner schema creation and in-place migration; `init` is the only migration path.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    AUTHORITY_IDENTITY, AUTHORITY_IDENTITY_KEY, InitOutcome, SCHEMA_VERSION, authority_identity,
    begin_mutation, external_reference, history_recovery, mise_projection, pickup,
    require_planner_identity, schema_version, write_guard,
};

pub fn init(conn: &Connection) -> Result<InitOutcome> {
    init_at(conn, "the selected database")
}

pub fn init_at(conn: &Connection, path: &str) -> Result<InitOutcome> {
    let has_meta: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |_| Ok(true),
        )
        .optional()
        .with_context(|| {
            format!(
                "{path} is not a readable SQLite database; select a Papertiger authority or replace it only after preserving the original bytes, then run `papertiger --db <new-path> init`"
            )
        })?
        .unwrap_or(false);
    if has_meta {
        require_planner_identity(conn, path, true)?;
        let from = schema_version(conn, path)?;
        if from == SCHEMA_VERSION {
            if authority_identity(conn)?.as_deref() != Some(AUTHORITY_IDENTITY) {
                bail!(
                    "{path} is schema v{SCHEMA_VERSION} but lacks Papertiger authority identity; restore a verified export instead of repairing identity in place"
                );
            }
            write_guard::verify(conn)?;
            history_recovery::validate(conn)?;
            return Ok(InitOutcome::Current);
        }
        migrate(conn, from)?;
        return Ok(InitOutcome::Migrated {
            from,
            to: SCHEMA_VERSION,
        });
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
    tx.execute_batch(&format!(
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
{tasks}
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
{gates}
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
        tasks = tasks_table("tasks"),
        gates = gates_table("gates")
    ))?;
    tx.execute_batch(external_reference::REFERENCE_SCHEMA)?;
    tx.execute_batch(mise_projection::MISE_PROJECTION_SCHEMA_V4)?;
    history_recovery::install_schema(&tx)?;
    write_guard::install(&tx)?;
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;
    tx.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)",
        params![AUTHORITY_IDENTITY_KEY, AUTHORITY_IDENTITY],
    )?;
    tx.commit()?;
    Ok(InitOutcome::Created)
}

/// The current tasks table. Fresh authorities create it directly; the v14
/// migration rebuilds the stored table into it, because SQLite cannot alter
/// a CHECK constraint in place.
fn tasks_table(name: &str) -> String {
    format!(
        r#"CREATE TABLE {name} (
  task_id INTEGER PRIMARY KEY,
  seq INTEGER NOT NULL UNIQUE,
  plan_id INTEGER NOT NULL REFERENCES plans(plan_id),
  parent_id INTEGER REFERENCES tasks(task_id),
  replacement_task_id INTEGER REFERENCES tasks(task_id),
  title TEXT NOT NULL,
  intent TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL
    CHECK (kind IN ('goal','feature','change','bug','probe','decision','release','work')),
  result TEXT,
  status TEXT NOT NULL DEFAULT 'proposed'
    CHECK (status IN ('proposed','in_progress','done','retired','rejected')),
  priority INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  pickup_session TEXT,
  pickup_at TEXT
    CHECK ((pickup_session IS NULL AND pickup_at IS NULL) OR
           (pickup_at IS NOT NULL AND status = 'in_progress'))
);"#
    )
}

/// The current gates table. An `external` gate is a condition outside the
/// task that keeps it from starting until resolved; it replaced blockers.
fn gates_table(name: &str) -> String {
    format!(
        r#"CREATE TABLE {name} (
  gate_id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  name TEXT NOT NULL,
  kind TEXT NOT NULL
    CHECK (kind IN ('test','benchmark','review','capture','fixture','build','doc','external','other')),
  requirement TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','waived')),
  evidence_locator TEXT,
  evidence_sha256 TEXT,
  note TEXT,
  resolved_at TEXT,
  UNIQUE (task_id, name)
);"#
    )
}

const TASK_COLUMNS_V14: &str = "task_id, seq, plan_id, parent_id, replacement_task_id, title,     intent, kind, result, status, priority, created_at, updated_at, pickup_session, pickup_at";

/// Typed task kinds widen the kind CHECK and blockers fold into external
/// gates, so the tasks and gates tables are rebuilt.
/// Stored kinds keep their values: `probe` and `decision` keep their meaning
/// and `work` becomes the legacy kind. The intent and result source columns
/// are dropped: authorship and review state replace them, and the sources
/// already recorded stay in the creation and completion event payloads. Every trigger is dropped first and the
/// canonical guards are reinstalled after; the caller has turned foreign-key
/// enforcement off and this step verifies the references before returning.
fn migrate_v14(tx: &Connection) -> Result<()> {
    let triggers = tx
        .prepare("SELECT name FROM sqlite_schema WHERE type='trigger'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for trigger in triggers {
        tx.execute_batch(&format!(
            "DROP TRIGGER \"{}\";",
            trigger.replace('"', "\"\"")
        ))?;
    }
    tx.execute_batch(&format!(
        "DROP VIEW IF EXISTS canonical_events;
         {table}
         INSERT INTO tasks_v14 ({TASK_COLUMNS_V14}) SELECT {TASK_COLUMNS_V14} FROM tasks;
         DROP TABLE tasks;
         ALTER TABLE tasks_v14 RENAME TO tasks;
         CREATE INDEX idx_tasks_plan ON tasks(plan_id);",
        table = tasks_table("tasks_v14")
    ))?;
    // Blockers become `external` gates with the same evidence and outcome. A
    // blocker named like a gate on the same task keeps a `blocker-` prefix.
    let unrenamable: Option<(i64, String)> = tx
        .query_row(
            "SELECT t.seq, b.name FROM task_blockers b JOIN tasks t ON t.task_id=b.task_id
              WHERE EXISTS (SELECT 1 FROM gates g WHERE g.task_id=b.task_id AND g.name=b.name)
                AND (EXISTS (SELECT 1 FROM gates g
                              WHERE g.task_id=b.task_id AND g.name='blocker-' || b.name)
                     OR EXISTS (SELECT 1 FROM task_blockers other
                                 WHERE other.task_id=b.task_id AND other.name='blocker-' || b.name))
              ORDER BY t.seq LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((seq, name)) = unrenamable {
        bail!(
            "#{seq} has a blocker '{name}' that cannot become an external gate: both '{name}' and 'blocker-{name}' are taken on that task; nothing was migrated. With the previous release, remove and re-add one of them under another name, then run init again"
        );
    }
    tx.execute_batch(&format!(
        "{gates}
         INSERT INTO gates_v14
           (gate_id, task_id, name, kind, requirement, status, evidence_locator, evidence_sha256, note, resolved_at)
         SELECT gate_id, task_id, name, kind, requirement, status, evidence_locator, evidence_sha256, note, resolved_at
           FROM gates;
         INSERT INTO gates_v14
           (task_id, name, kind, requirement, status, evidence_locator, evidence_sha256, note, resolved_at)
         SELECT b.task_id,
                CASE WHEN EXISTS (SELECT 1 FROM gates g WHERE g.task_id=b.task_id AND g.name=b.name)
                     THEN 'blocker-' || b.name ELSE b.name END,
                'external', b.condition, b.status, b.evidence_locator, b.evidence_sha256, b.note,
                b.resolved_at
           FROM task_blockers b ORDER BY b.blocker_id;
         DROP TABLE task_blockers;
         DROP TABLE gates;
         ALTER TABLE gates_v14 RENAME TO gates;",
        gates = gates_table("gates_v14")
    ))?;
    let violations: i64 =
        tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if violations > 0 {
        bail!(
            "the v14 tasks rebuild found {violations} broken foreign-key reference(s); nothing was migrated. Restore a verified export into a fresh database with `papertiger --db <new-path> import <dump>`"
        );
    }
    write_guard::install(tx)
}

/// Blockers name their `condition`; gates share the blockers' `resolved`
/// vocabulary. SQLite cannot alter a CHECK constraint, so gates are rebuilt.
/// Stored events keep their original kinds and payloads.
const SCHEMA_V13_MIGRATION: &str = r#"
ALTER TABLE task_blockers RENAME COLUMN reason TO condition;
CREATE TABLE gates_v13 (
  gate_id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  name TEXT NOT NULL,
  kind TEXT NOT NULL
    CHECK (kind IN ('test','benchmark','review','capture','fixture','build','doc','other')),
  requirement TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','waived')),
  evidence_locator TEXT,
  evidence_sha256 TEXT,
  note TEXT,
  resolved_at TEXT,
  UNIQUE (task_id, name)
);
INSERT INTO gates_v13
  (gate_id, task_id, name, kind, requirement, status, evidence_locator, evidence_sha256, note, resolved_at)
SELECT gate_id, task_id, name, kind, requirement,
       CASE status WHEN 'closed' THEN 'resolved' ELSE status END,
       evidence_locator, evidence_sha256, note, closed_at
  FROM gates;
DROP TABLE gates;
ALTER TABLE gates_v13 RENAME TO gates;
"#;

fn migrate(conn: &Connection, from: i64) -> Result<()> {
    if from > SCHEMA_VERSION {
        bail!(
            "database schema v{from} is newer than this papertiger (v{SCHEMA_VERSION}); upgrade the tool"
        );
    }
    if from == SCHEMA_VERSION {
        return Ok(());
    }
    // Rebuilding a table that other tables reference needs foreign-key
    // enforcement off, and SQLite ignores that pragma inside a transaction.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let migrated = migrate_in_transaction(conn, from);
    let restored = conn.pragma_update(None, "foreign_keys", "ON");
    migrated?;
    Ok(restored?)
}

fn migrate_in_transaction(conn: &Connection, from: i64) -> Result<()> {
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
    if version == 6 {
        tx.execute_batch(
            r#"
ALTER TABLE tasks
  ADD COLUMN intent_source TEXT CHECK (intent_source IN ('user','agent','external'));
ALTER TABLE tasks
  ADD COLUMN result_source TEXT CHECK (result_source IN ('user','agent','external'));
"#,
        )?;
        version = 7;
    }
    if version == 7 {
        tx.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, ?2)",
            params![AUTHORITY_IDENTITY_KEY, AUTHORITY_IDENTITY],
        )?;
        version = 8;
    }
    if version == 8 {
        // Older readers filter task history by its former plan and would lose
        // relocated events. This semantic boundary requires explicit migration.
        tx.execute_batch(external_reference::REFERENCE_SCHEMA)?;
        version = 9;
    }
    if version == 9 {
        tx.execute_batch(pickup::SCHEMA)?;
        version = 10;
    }
    if version == 10 {
        // Guards validate new writes only; earlier malformed rows stay audit evidence.
        tx.execute_batch(write_guard::WRITE_GUARD_SCHEMA_V11)?;
        version = 11;
    }
    if version == 11 {
        history_recovery::install_schema(&tx)?;
        write_guard::install(&tx)?;
        version = 12;
    }
    if version == 12 {
        // v12 admission triggers still call the retired function name; admit
        // them for this migration, then reinstall the v13 boundary.
        write_guard::admit_v12_migration(&tx)?;
        tx.execute_batch(SCHEMA_V13_MIGRATION)?;
        write_guard::install(&tx)?;
        version = 13;
    }
    if version == 13 {
        migrate_v14(&tx)?;
        version = 14;
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
