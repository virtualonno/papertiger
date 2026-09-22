//! Storage-level refusal of planner writes that bypass the executable.
//!
//! Reads already refuse malformed history, but only after it is stored; a
//! direct SQLite writer then corrupts the authority until a later export or
//! read fails. These triggers refuse the write itself. They validate new rows
//! only: rows stored before schema v11 stay untouched and remain visible to
//! `papertiger audit` as evidence.
//!
//! Event inserts mirror the contract `import` enforces for dumps: an RFC3339
//! timestamp with a zone, a JSON payload, a known entity, and stable plan/task
//! (and gate) references for task-scoped events. Installation replaces any
//! same-named trigger so every migrated authority carries the canonical bodies.

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
