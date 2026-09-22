//! Storage-level refusal of planner writes without explicit public API entry.
//!
//! Reads already refuse malformed history, but only after it is stored; a
//! direct SQLite writer then corrupts the authority until a later export or
//! read fails. These triggers refuse the write itself. They validate new rows
//! only: rows stored before schema v11 stay untouched and remain visible to
//! `papertiger audit` as evidence.
//!
//! Schema v12 also requires a connection-local function installed by
//! `begin_mutation`. An ordinary SQLite writer lacks that function, even when
//! its SQL is well formed. Calling the public mutation API explicitly admits
//! that connection; callers of that API are trusted. This is an accident and
//! agent-misuse boundary, not a sandbox against a filesystem owner capable of
//! replacing triggers, registering functions, or rewriting the database file.

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags};

pub(crate) fn admit(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        "papertiger_write_requires_executable",
        0,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_INNOCUOUS,
        |_| Ok(1_i64),
    )?;
    Ok(())
}

fn canonical_guards(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut guards = Vec::new();
    for body in [
        WRITE_GUARD_SCHEMA_V11,
        crate::history_recovery::IMMUTABILITY,
    ]
    .into_iter()
    .flat_map(|schema| schema.split("CREATE TRIGGER ").skip(1))
    {
        let sql = format!("CREATE TRIGGER {}", body.trim().trim_end_matches(';'));
        guards.push((body.split_whitespace().next().unwrap().to_owned(), sql));
    }
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    for table in stmt.query_map([], |row| row.get::<_, String>(0))? {
        let table = table?;
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            let name = format!("papertiger_admit_{table}_{}", operation.to_lowercase());
            let sql = format!(
                "CREATE TRIGGER \"{}\" BEFORE {operation} ON \"{}\" BEGIN SELECT papertiger_write_requires_executable(); END",
                name.replace('"', "\"\""),
                table.replace('"', "\"\"")
            );
            guards.push((name, sql));
        }
    }
    Ok(guards)
}

pub(crate) fn install(conn: &Connection) -> Result<()> {
    for (name, sql) in canonical_guards(conn)? {
        conn.execute_batch(&format!(
            "DROP TRIGGER IF EXISTS \"{}\"; {sql};",
            name.replace('"', "\"\"")
        ))?;
    }
    Ok(())
}

pub(crate) fn verify(conn: &Connection) -> Result<()> {
    for (name, expected) in canonical_guards(conn)? {
        let actual: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?1",
                [&name],
                |row| row.get(0),
            )
            .optional()?;
        if actual.as_deref() != Some(expected.as_str()) {
            bail!(
                "Papertiger write guard {name:?} is missing or altered; stop direct SQLite access, preserve the database with `papertiger backup --output <new-path>`, and restore a verified authority; `init` does not silently bless altered guards"
            );
        }
    }
    Ok(())
}

pub(crate) const WRITE_GUARD_SCHEMA_V11: &str = r#"
DROP TRIGGER IF EXISTS events_append_only_update;
DROP TRIGGER IF EXISTS events_append_only_delete;
DROP TRIGGER IF EXISTS events_require_zoned_timestamp;
DROP TRIGGER IF EXISTS events_require_json_payload;
DROP TRIGGER IF EXISTS events_require_stable_reference;
DROP TRIGGER IF EXISTS tasks_require_integer_priority_insert;
DROP TRIGGER IF EXISTS tasks_require_integer_priority_update;
CREATE TRIGGER events_append_only_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'papertiger planning history is append-only; record a new event through the papertiger executable instead of editing stored events'); END;
CREATE TRIGGER events_append_only_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'papertiger planning history is append-only; record a new event through the papertiger executable instead of deleting stored events'); END;
CREATE TRIGGER events_require_zoned_timestamp BEFORE INSERT ON events
WHEN NOT (
  NEW.at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][Tt ][0-9][0-9]:[0-9][0-9]:[0-9][0-9]*'
  AND (NEW.at GLOB '*[Zz]' OR NEW.at GLOB '*[+-][0-9][0-9]:[0-9][0-9]')
)
BEGIN SELECT RAISE(ABORT, 'papertiger refuses an event without an RFC3339 timestamp and zone; record planning history only through the papertiger executable, never by direct SQLite writes'); END;
CREATE TRIGGER events_require_json_payload BEFORE INSERT ON events
WHEN NEW.payload IS NOT NULL AND NOT json_valid(NEW.payload)
BEGIN SELECT RAISE(ABORT, 'papertiger refuses an event whose payload is not JSON; record planning history only through the papertiger executable, never by direct SQLite writes'); END;
CREATE TRIGGER events_require_stable_reference BEFORE INSERT ON events
WHEN trim(NEW.actor) = '' OR trim(NEW.kind) = ''
  OR NEW.entity NOT IN ('plan','task','dep','gate')
  OR (NEW.entity IN ('task','dep','gate') AND (NEW.entity_seq IS NULL OR NEW.entity_plan IS NULL))
  OR (NEW.entity = 'gate' AND NEW.gate_name IS NULL)
BEGIN SELECT RAISE(ABORT, 'papertiger refuses an event without an actor, kind, known entity and stable plan/task reference; record planning history only through the papertiger executable, never by direct SQLite writes'); END;
CREATE TRIGGER tasks_require_integer_priority_insert BEFORE INSERT ON tasks
WHEN typeof(NEW.priority) <> 'integer'
BEGIN SELECT RAISE(ABORT, 'papertiger task priority must be an integer; create tasks through the papertiger executable with --priority <integer>'); END;
CREATE TRIGGER tasks_require_integer_priority_update BEFORE UPDATE OF priority ON tasks
WHEN typeof(NEW.priority) <> 'integer'
BEGIN SELECT RAISE(ABORT, 'papertiger task priority must be an integer; run `papertiger edit <task> --priority <integer> --why <reason>`'); END;
"#;
