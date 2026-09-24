use papertiger as pt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn unique_test_path(stem: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("{stem}-{}-{nonce}.sqlite", std::process::id()))
}

fn db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    pt::init(&conn).unwrap();
    conn
}

#[test]
fn audit_reports_oversized_terminal_titles_that_would_block_recovery_import() {
    let conn = db();
    let plan = pt::add_plan(&conn, "fixture", "work", "Work", "").unwrap();
    pt::add_task(&conn, "fixture", plan, pt::TaskCreation::new("Original")).unwrap();
    // Explicitly admitted disposable fixture represents a legacy terminal task.
    conn.execute(
        "UPDATE tasks SET title=?1,status='retired'",
        ["x".repeat(pt::MAX_TASK_TITLE_CHARS + 1)],
    )
    .unwrap();
    assert!(
        pt::audit(&conn)
            .unwrap()
            .iter()
            .any(|f| f.kind == "oversized_task_title")
    );
}

fn assert_exact_error<T>(result: anyhow::Result<T>, expected: &str) {
    match result {
        Ok(_) => panic!("expected refusal: {expected}"),
        Err(error) => assert_eq!(error.to_string(), expected),
    }
}

fn assert_bounded_lock_refusal(output: &Output, elapsed: Duration, phase: &str) {
    assert!(!output.status.success(), "{phase} unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("papertiger SQLite lock admission refused after a 500ms grace"),
        "{phase} returned the wrong refusal: {stderr}"
    );
    assert!(
        stderr.contains("retry the command after the current database operation finishes"),
        "{phase} omitted the corrective retry action: {stderr}"
    );
    assert!(
        !stderr.contains("Caused by:") && !stderr.contains("database is locked"),
        "{phase} leaked a second raw SQLite refusal: {stderr}"
    );
    assert!(
        elapsed >= Duration::from_millis(400),
        "{phase} did not honor the fixed lock grace: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "{phase} exceeded the bounded lock grace: {elapsed:?}"
    );
}

fn create_v1_database(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        r#"
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO meta (key, value) VALUES ('schema_version', '1');
CREATE TABLE plans (
  plan_id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL UNIQUE,
  title TEXT NOT NULL,
  intent TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'active',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE tasks (
  task_id INTEGER PRIMARY KEY,
  seq INTEGER NOT NULL UNIQUE,
  plan_id INTEGER NOT NULL REFERENCES plans(plan_id),
  parent_id INTEGER REFERENCES tasks(task_id),
  title TEXT NOT NULL,
  intent TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'proposed',
  priority INTEGER NOT NULL DEFAULT 0,
  alias TEXT,
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
  UNIQUE (task_id, depends_on)
);
CREATE TABLE gates (
  gate_id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  requirement TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open',
  evidence_locator TEXT,
  evidence_sha256 TEXT,
  note TEXT,
  closed_at TEXT,
  UNIQUE (task_id, name)
);
CREATE TABLE events (
  event_id INTEGER PRIMARY KEY,
  at TEXT NOT NULL,
  actor TEXT NOT NULL,
  entity TEXT NOT NULL,
  entity_id INTEGER,
  kind TEXT NOT NULL,
  why TEXT,
  payload TEXT
);
INSERT INTO plans
  (slug, title, intent, status, created_at, updated_at)
VALUES ('old', 'Old plan', '', 'active', '2026-01-01', '2026-01-01');
INSERT INTO tasks
  (seq, plan_id, title, intent, status, priority, created_at, updated_at)
VALUES (1, 1, 'old task', '', 'proposed', 0, '2026-01-01', '2026-01-01');
"#,
    )
    .unwrap();
}

#[test]
fn schema_migration_is_explicit_and_preserves_v1_plan_state() {
    let path = unique_test_path("explicit-v1-migration");
    create_v1_database(&path);

    let output = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "status"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("run `papertiger"));

    let old = rusqlite::Connection::open(&path).unwrap();
    let version: String = old
        .query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "1", "a read command must not migrate the database");
    let has_kind: bool = old
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_info('tasks') WHERE name='kind'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!has_kind);
    drop(old);

    let init = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let migrated = pt::open_existing(path.to_str().unwrap()).unwrap();
    let task = pt::get_task(&migrated, 1).unwrap();
    assert_eq!(task.kind, "work");
    assert_eq!(task.result, None);
    let has_alias: bool = migrated
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_info('tasks') WHERE name='alias'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !has_alias,
        "the current schema must not retain task aliases"
    );
    let has_blockers: bool = migrated
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_schema
                 WHERE type='table' AND name='task_blockers'
             )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(has_blockers);
    drop(migrated);
    std::fs::remove_file(path).unwrap();
}

const V12_GATES: &str = r#"
CREATE TABLE gates_v12 (
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
INSERT INTO gates_v12
SELECT gate_id, task_id, name, kind, requirement,
       CASE status WHEN 'resolved' THEN 'closed' ELSE status END,
       evidence_locator, evidence_sha256, note, resolved_at
  FROM gates;
DROP TABLE gates;
ALTER TABLE gates_v12 RENAME TO gates;
ALTER TABLE task_blockers RENAME COLUMN condition TO reason;
UPDATE meta SET value='12' WHERE key='schema_version';
"#;

/// Rewrite a current authority into the exact v12 storage shape: legacy gate
/// vocabulary, blocker `reason`, and admission triggers calling the retired
/// function name.
fn downgrade_to_v12(path: &std::path::Path) {
    let raw = rusqlite::Connection::open(path).unwrap();
    for name in [
        "papertiger_write_requires_public_api",
        "papertiger_write_requires_executable",
    ] {
        raw.create_scalar_function(
            name,
            0,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8,
            |_| Ok(1_i64),
        )
        .unwrap();
    }
    raw.execute_batch(V12_GATES).unwrap();
    let mut statement = raw
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap();
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    drop(statement);
    for table in tables {
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            let name = format!("papertiger_admit_{table}_{}", operation.to_lowercase());
            raw.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS \"{name}\"; CREATE TRIGGER \"{name}\" BEFORE {operation} ON \"{table}\" BEGIN SELECT papertiger_write_requires_executable(); END;"
            ))
            .unwrap();
        }
    }
}

fn event_rows(conn: &rusqlite::Connection) -> Vec<(i64, String, Option<String>)> {
    let mut statement = conn
        .prepare("SELECT event_id, kind, payload FROM events ORDER BY event_id")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[test]
fn v12_migration_renames_gate_and_blocker_vocabulary_and_reinstalls_admission() {
    let path = unique_test_path("v12-migration");
    let conn = pt::open_for_init(path.to_str().unwrap()).unwrap();
    pt::init(&conn).unwrap();
    let plan = pt::add_plan(&conn, "test", "p", "P", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("gated")).unwrap();
    pt::add_gate(&conn, "test", task, "proof", "test", "tests pass").unwrap();
    pt::add_gate(&conn, "test", task, "doc", "doc", "docs updated").unwrap();
    pt::resolve_gate(&conn, "test", task, "proof", "file:proof.json", None, None).unwrap();
    pt::waive_gate(&conn, "test", task, "doc", "not user-facing").unwrap();
    pt::add_blocker(&conn, "test", task, "vendor", "vendor fix released").unwrap();
    let resolved_at = pt::task_context(&conn, task).unwrap().gates[0]
        .resolved_at
        .clone()
        .unwrap();
    let events_before = event_rows(&conn);
    drop(conn);
    downgrade_to_v12(&path);
    // A projection recorded before 0.18 carries the retired id inside its
    // hashed bytes; the immutable row can never be rewritten.
    let legacy = legacy_mise_projection_fixture();
    let legacy_sha256 = legacy.projection_sha256().unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.create_scalar_function(
        "papertiger_write_requires_executable",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        |_| Ok(1_i64),
    )
    .unwrap();
    raw.execute(
        "INSERT INTO task_mise_projections
         (projection_sha256, task_id, campaign_id, manifest_sha256, candidate_id,
          nomination_id, disposition, projection_json, recorded_by, recorded_at)
         VALUES (?1, (SELECT task_id FROM tasks WHERE seq=?2), ?3, ?4, ?5, ?6, 'nominated', ?7,
                 'pre-0.18', '2026-08-01T00:00:00Z')",
        rusqlite::params![
            legacy_sha256,
            task,
            legacy.campaign_id,
            legacy.manifest_sha256,
            legacy.candidate_id,
            legacy.nomination_id,
            serde_json::to_string(&legacy).unwrap(),
        ],
    )
    .unwrap();
    drop(raw);

    let refused = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "status"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("init` explicitly to upgrade to v13")
    );
    let init = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(String::from_utf8_lossy(&init.stdout).contains("from schema v12 to v13"));

    let migrated = pt::open_existing(path.to_str().unwrap()).unwrap();
    let context = pt::task_context(&migrated, task).unwrap();
    assert_eq!(context.gates[0].status, "resolved");
    assert_eq!(context.gates[0].resolved_at.as_deref(), Some(&*resolved_at));
    assert_eq!(context.gates[1].status, "waived");
    assert_eq!(context.blockers[0].condition, "vendor fix released");
    assert_eq!(
        event_rows(&migrated),
        events_before,
        "stored history must survive the migration verbatim"
    );
    let stale: i64 = migrated
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE sql LIKE '%papertiger_write_requires_executable%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stale, 0);
    assert!(pt::write_guard_drift(&migrated).unwrap().is_empty());
    let stored = pt::task_mise_projections(&migrated, task).unwrap();
    assert_eq!(stored[0].projection_sha256, legacy_sha256);
    assert_eq!(
        stored[0].projection.schema,
        pt::MISE_PLANNER_PROJECTION_SCHEMA_V1
    );
    assert!(pt::audit(&migrated).unwrap().is_empty());
    let dump = pt::export(&migrated, None).unwrap();
    assert_eq!(dump.mise_projections[0].projection_sha256, legacy_sha256);
    let restored = db();
    pt::import(&restored, "restore", &dump).unwrap();
    assert!(
        pt::mise_projection(&restored, &legacy_sha256)
            .unwrap()
            .is_some()
    );
    pt::add_gate(&migrated, "test", task, "later", "test", "later proof").unwrap();
    drop(migrated);

    let raw = rusqlite::Connection::open(&path).unwrap();
    let error = raw
        .execute("UPDATE gates SET note='direct' WHERE name='later'", [])
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("papertiger_write_requires_public_api"),
        "{error}"
    );
    drop(raw);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn v9_dump_converts_once_to_v10_vocabulary() {
    let v9 = r#"{"schema":"papertiger.dump.v9","plans":[{"slug":"p","title":"P"}],
        "tasks":[{"seq":1,"plan":"p","title":"gated","status":"in_progress",
          "pickup":{"session":null,"at":"2026-08-01T00:00:00Z"},
          "gates":[{"name":"proof","kind":"test","requirement":"r","status":"closed",
            "evidence_locator":"file:proof.json","closed_at":"2026-08-02T00:00:00Z"}],
          "blockers":[{"name":"vendor","reason":"vendor fix released","status":"open",
            "evidence_locator":null,"evidence_sha256":null,"note":null,"resolved_at":null}]}]}"#;
    let dump = pt::parse_dump_json(v9).unwrap();
    assert_eq!(dump.schema, "papertiger.dump.v10");
    let conn = db();
    pt::import(&conn, "test", &dump).unwrap();
    let context = pt::task_context(&conn, 1).unwrap();
    assert_eq!(context.gates[0].status, "resolved");
    assert_eq!(
        context.gates[0].resolved_at.as_deref(),
        Some("2026-08-02T00:00:00Z")
    );
    assert_eq!(context.blockers[0].condition, "vendor fix released");
    assert_eq!(
        pt::export(&conn, None).unwrap().schema,
        "papertiger.dump.v10"
    );

    let malformed = v9.replace(r#""reason":"vendor fix released","#, "");
    let error = pt::parse_dump_json(&malformed).err().unwrap().to_string();
    assert!(error.contains("papertiger.dump.v9 is malformed"), "{error}");
    let v10_with_legacy_field = v9
        .replace("papertiger.dump.v9", "papertiger.dump.v10")
        .replace(r#""status":"closed""#, r#""status":"resolved""#);
    assert!(pt::parse_dump_json(&v10_with_legacy_field).is_err());
}

#[test]
fn open_existing_refuses_missing_database_without_creating_it() {
    let path = unique_test_path("missing-library-open");
    assert!(!path.exists());

    let err = pt::open_existing(path.to_str().unwrap()).unwrap_err();

    let message = err.to_string();
    assert!(
        message.contains("no Papertiger authority exists"),
        "{message}"
    );
    assert!(message.contains("with `init`"), "{message}");
    assert!(message.contains("same authority selectors"), "{message}");
    assert!(message.contains("restore an export"), "{message}");
    assert!(!path.exists(), "failed open must not create the database");
}

#[test]
fn cli_status_refuses_missing_database_without_creating_it() {
    let path = unique_test_path("missing-cli-open");
    assert!(!path.exists());

    let output = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "status"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no Papertiger authority exists"),
        "{stderr}"
    );
    assert!(stderr.contains("restore an export"), "{stderr}");
    assert!(stderr.contains("same authority selectors"), "{stderr}");
    assert!(
        !path.exists(),
        "failed command must not create the database"
    );
    let initialized = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    assert!(initialized.status.success());
    let status = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "status"])
        .output()
        .unwrap();
    assert!(status.status.success());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_status_refuses_uninitialized_existing_file_without_mutating_it() {
    let path = unique_test_path("uninitialized-cli-open");
    std::fs::write(&path, []).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "status"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("empty SQLite database without a Papertiger authority"),
        "{stderr}"
    );
    assert!(stderr.contains(" init` to initialize it"), "{stderr}");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_init_creates_database_that_operational_commands_can_reopen() {
    let path = unique_test_path("explicit-cli-init");
    assert!(!path.exists());

    let init = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    assert!(init.status.success());
    assert!(path.exists());

    let status = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "status"])
        .output()
        .unwrap();
    assert!(status.status.success());

    let current = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "init"])
        .output()
        .unwrap();
    assert!(current.status.success());
    assert!(
        String::from_utf8_lossy(&current.stdout).contains("nothing changed"),
        "{}",
        String::from_utf8_lossy(&current.stdout)
    );

    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_reads_and_init_refuse_non_sqlite_bytes_with_corrective_context() {
    let path = unique_test_path("non-sqlite-authority");
    std::fs::write(&path, b"not a SQLite authority\n").unwrap();
    let before = std::fs::read(&path).unwrap();

    for command in ["status", "init"] {
        let output = Command::new(env!("CARGO_BIN_EXE_papertiger"))
            .args(["--db", path.to_str().unwrap(), command])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{command} accepted non-SQLite bytes"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("not a readable SQLite database"),
            "{stderr}"
        );
        assert!(
            stderr.contains("papertiger --db <new-path> init"),
            "{stderr}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_init_refuses_foreign_sqlite_without_mutating_bytes() {
    let path = unique_test_path("foreign-cli-init");
    let foreign = rusqlite::Connection::open(&path).unwrap();
    foreign
        .execute_batch(
            "CREATE TABLE foreign_records (id INTEGER PRIMARY KEY, payload BLOB NOT NULL);
             INSERT INTO foreign_records (payload) VALUES (x'00112233445566778899aabbccddeeff');",
        )
        .unwrap();
    drop(foreign);
    let before = std::fs::read(&path).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "init"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("refusing to initialize nonempty database without papertiger metadata")
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "refused initialization must leave every database byte unchanged"
    );
    let foreign = rusqlite::Connection::open(&path).unwrap();
    let payload: Vec<u8> = foreign
        .query_row("SELECT payload FROM foreign_records", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        payload,
        vec![
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ]
    );
    let has_meta: bool = foreign
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='meta')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!has_meta);
    drop(foreign);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn cli_init_refuses_foreign_header_state_without_mutating_bytes() {
    let path = unique_test_path("foreign-header-cli-init");
    let foreign = rusqlite::Connection::open(&path).unwrap();
    foreign.pragma_update(None, "user_version", 73).unwrap();
    drop(foreign);
    let before = std::fs::read(&path).unwrap();
    assert!(!before.is_empty());

    let output = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "init"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "refusing to initialize nonempty database without papertiger metadata: found 1 allocated page(s)"
        )
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "refused initialization must preserve foreign SQLite header state byte-for-byte"
    );
    let foreign = rusqlite::Connection::open(&path).unwrap();
    let user_version: i64 = foreign
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(user_version, 73);
    drop(foreign);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn typed_authority_identity_is_migrated_and_refuses_mise_databases() {
    let legacy_path = unique_test_path("legacy-v7-identity");
    let legacy = pt::open_for_init(legacy_path.to_str().unwrap()).unwrap();
    assert_eq!(pt::init(&legacy).unwrap(), pt::InitOutcome::Created);
    legacy
        .execute_batch("DROP VIEW canonical_events; DROP TABLE event_quarantines; DROP TABLE external_references;")
        .unwrap();
    legacy
        .execute("DELETE FROM meta WHERE key='authority'", [])
        .unwrap();
    legacy.execute_batch("ALTER TABLE tasks DROP COLUMN pickup_at; ALTER TABLE tasks DROP COLUMN pickup_session;").unwrap();
    legacy
        .execute_batch(
            "ALTER TABLE task_blockers RENAME COLUMN condition TO reason; ALTER TABLE gates RENAME COLUMN resolved_at TO closed_at;
             UPDATE meta SET value='7' WHERE key='schema_version';",
        )
        .unwrap();
    drop(legacy);

    let error = pt::open_existing_read_only(legacy_path.to_str().unwrap()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("legacy Papertiger planning authority")
    );
    assert!(error.to_string().contains(" init` explicitly"));
    let legacy = pt::open_for_init(legacy_path.to_str().unwrap()).unwrap();
    assert_eq!(
        pt::init(&legacy).unwrap(),
        pt::InitOutcome::Migrated { from: 7, to: 13 }
    );
    assert_eq!(
        legacy
            .query_row("SELECT value FROM meta WHERE key='authority'", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        pt::AUTHORITY_IDENTITY
    );
    drop(legacy);
    std::fs::remove_file(&legacy_path).unwrap();

    let mise_path = unique_test_path("typed-mise-identity");
    let mise = rusqlite::Connection::open(&mise_path).unwrap();
    mise.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO meta VALUES ('schema_version', '8');
         INSERT INTO meta VALUES ('authority', 'papertiger.mise');",
    )
    .unwrap();
    drop(mise);
    let before = std::fs::read(&mise_path).unwrap();
    let error = pt::open_existing(mise_path.to_str().unwrap()).unwrap_err();
    assert!(error.to_string().contains("papertiger-mise authority"));
    assert!(error.to_string().contains("papertiger-mise --db"));
    let mise = pt::open_for_init(mise_path.to_str().unwrap()).unwrap();
    let error = pt::init(&mise).unwrap_err();
    assert!(error.to_string().contains("papertiger-mise authority"));
    drop(mise);
    assert_eq!(std::fs::read(&mise_path).unwrap(), before);
    std::fs::remove_file(mise_path).unwrap();
}

#[test]
fn sqlite_lock_grace_covers_validation_writer_admission_and_commit_without_replay() {
    let path = unique_test_path("sqlite-lock-grace");
    let conn = pt::open_for_init(path.to_str().unwrap()).unwrap();
    pt::init(&conn).unwrap();
    pt::add_plan(&conn, "test", "first", "First", "").unwrap();
    let held_mutation = pt::begin_mutation(&conn).unwrap();

    let overlapping = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args([
            "--db",
            path.to_str().unwrap(),
            "plan",
            "add",
            "second",
            "Second",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    drop(held_mutation);
    let serialized = overlapping.wait_with_output().unwrap();
    assert!(
        serialized.status.success(),
        "short overlap should serialize within the admission grace: {}",
        String::from_utf8_lossy(&serialized.stderr)
    );

    let held_mutation = pt::begin_mutation(&conn).unwrap();
    let started = Instant::now();
    let refused = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args([
            "--db",
            path.to_str().unwrap(),
            "plan",
            "add",
            "third",
            "Third",
        ])
        .output()
        .unwrap();
    let elapsed = started.elapsed();

    assert_bounded_lock_refusal(&refused, elapsed, "writer admission");
    let plan_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
        .unwrap();
    assert_eq!(plan_count, 2, "refused command must not partially mutate");
    let event_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE entity='plan' AND kind='create'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 2, "refused command must not append an event");

    drop(held_mutation);
    let retried = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args([
            "--db",
            path.to_str().unwrap(),
            "plan",
            "add",
            "third",
            "Third",
        ])
        .output()
        .unwrap();
    assert!(
        retried.status.success(),
        "{}",
        String::from_utf8_lossy(&retried.stderr)
    );
    let plan_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
        .unwrap();
    assert_eq!(plan_count, 3);

    let reader = pt::open_existing_read_only(path.to_str().unwrap()).unwrap();
    let read_transaction = reader.unchecked_transaction().unwrap();
    let _: i64 = read_transaction
        .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
        .unwrap();
    let started = Instant::now();
    let refused_commit = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args([
            "--db",
            path.to_str().unwrap(),
            "plan",
            "add",
            "fourth",
            "Fourth",
        ])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    assert_bounded_lock_refusal(&refused_commit, elapsed, "mutation commit");
    let plan_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
        .unwrap();
    assert_eq!(plan_count, 3, "failed commit must roll back its plan row");
    let event_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE entity='plan' AND kind='create'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_count, 3, "failed commit must roll back its event");

    drop(read_transaction);
    drop(reader);
    let retried_commit = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args([
            "--db",
            path.to_str().unwrap(),
            "plan",
            "add",
            "fourth",
            "Fourth",
        ])
        .output()
        .unwrap();
    assert!(
        retried_commit.status.success(),
        "{}",
        String::from_utf8_lossy(&retried_commit.stderr)
    );

    let exclusive =
        rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Exclusive)
            .unwrap();
    let started = Instant::now();
    let refused_validation = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "plan", "list"])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    assert_bounded_lock_refusal(&refused_validation, elapsed, "authority validation");
    drop(exclusive);

    let retried_read = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["--db", path.to_str().unwrap(), "plan", "list"])
        .output()
        .unwrap();
    assert!(
        retried_read.status.success(),
        "{}",
        String::from_utf8_lossy(&retried_read.stderr)
    );

    drop(conn);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn task_lifecycle_and_gate_honesty_rule() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let seq = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("build thing")).unwrap();
    assert_eq!(seq, 1);
    pt::add_gate(&conn, "test", seq, "smoke", "test", "smoke test passes").unwrap();
    pt::start_task(&conn, "test", seq, None, None).unwrap();
    // done refused while gate open
    let err = pt::complete_task(&conn, "test", seq, None, None).unwrap_err();
    assert!(err.to_string().contains("open gate"));
    // bad locator shape refused
    let err = pt::resolve_gate(&conn, "test", seq, "smoke", "no-scheme", None, None).unwrap_err();
    assert!(err.to_string().contains("scheme:value"));
    pt::resolve_gate(
        &conn,
        "test",
        seq,
        "smoke",
        "file:evidence/x.json",
        Some(&"ab".repeat(32)),
        None,
    )
    .unwrap();
    pt::complete_task(&conn, "test", seq, None, None).unwrap();
    assert_eq!(pt::get_task(&conn, seq).unwrap().status, "done");
}

#[test]
fn waive_requires_why_via_retire_reject_paths() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let seq = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("t")).unwrap();
    assert!(pt::retire_task(&conn, "test", seq, None, "").is_err());
    assert!(pt::reject_task(&conn, "test", seq, "").is_err());
    pt::reject_task(&conn, "test", seq, "approach disproven").unwrap();
}

#[test]
fn audit_reports_stored_dependency_deadlocks_with_their_removals() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let parent = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("parent")).unwrap();
    let child = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            parent: Some(parent),
            ..pt::TaskCreation::new("child")
        },
    )
    .unwrap();
    let downstream = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[parent],
            ..pt::TaskCreation::new("downstream")
        },
    )
    .unwrap();
    assert!(pt::add_dep(&conn, "test", child, downstream, "deadlock probe").is_err());
    assert!(
        !pt::audit(&conn)
            .unwrap()
            .iter()
            .any(|finding| finding.kind == "dependency_deadlock")
    );
    // Explicitly admitted disposable fixture represents an edge an older
    // release accepted and import restores unchanged.
    conn.execute(
        "INSERT INTO deps (task_id, depends_on)
         SELECT child.task_id, downstream.task_id FROM tasks child, tasks downstream
          WHERE child.seq=?1 AND downstream.seq=?2",
        [child, downstream],
    )
    .unwrap();
    let deadlocks = pt::audit(&conn)
        .unwrap()
        .into_iter()
        .filter(|finding| finding.kind == "dependency_deadlock")
        .map(|finding| finding.detail)
        .collect::<Vec<_>>();
    assert_eq!(
        deadlocks,
        [format!(
            "#{parent} waits for unfinished child #{child}, which depends on #{downstream}, which depends on #{parent}, so none of these tasks can finish; remove one dependency with `papertiger dep remove {child} {downstream} --why <reason>` or `papertiger dep remove {downstream} {parent} --why <reason>`"
        )]
    );
}

#[test]
fn dependency_cycles_rejected_and_readiness_derived() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let a = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("a")).unwrap();
    let b = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[a],
            ..pt::TaskCreation::new("b")
        },
    )
    .unwrap();
    let c = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[b],
            priority: 5,
            ..pt::TaskCreation::new("c")
        },
    )
    .unwrap();
    // a <- b <- c; closing the loop is refused
    assert!(pt::add_dep(&conn, "test", a, c, "cycle probe").is_err());
    assert!(pt::add_dep(&conn, "test", a, a, "self-cycle probe").is_err());
    // only a is ready
    let ready = pt::ready_tasks(&conn, plan, 10, false).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].task.seq, a);
    // blocked view names blockers
    let all = pt::ready_tasks(&conn, plan, 10, true).unwrap();
    let c_entry = all.iter().find(|e| e.task.seq == c).unwrap();
    assert_eq!(c_entry.blockers, vec![format!("dep:#{b}")]);
    // completing a readies b (priority ordering: c still blocked)
    pt::complete_task(&conn, "test", a, None, None).unwrap();
    let ready = pt::ready_tasks(&conn, plan, 10, false).unwrap();
    assert_eq!(
        ready.iter().map(|e| e.task.seq).collect::<Vec<_>>(),
        vec![b]
    );
}

#[test]
fn dependency_and_read_projection_refusals_have_exact_messages() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let other_plan = pt::add_plan(&conn, "test", "other", "Other", "").unwrap();
    let a = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("a")).unwrap();
    let b = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[a],
            ..pt::TaskCreation::new("b")
        },
    )
    .unwrap();
    let c = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[b],
            ..pt::TaskCreation::new("c")
        },
    )
    .unwrap();
    let other = pt::add_task(&conn, "test", other_plan, pt::TaskCreation::new("other")).unwrap();

    assert_exact_error(
        pt::add_dep(&conn, "test", a, a, "self dependency"),
        &format!("#{a} cannot depend on itself"),
    );
    assert_exact_error(
        pt::add_dep(&conn, "test", a, other, "cross-plan dependency"),
        &format!("#{a} and dependency #{other} belong to different plans"),
    );
    assert_exact_error(
        pt::add_dep(&conn, "test", a, c, "cycle"),
        &format!("dependency #{a} -> #{c} would create a cycle"),
    );
    assert_exact_error(
        pt::add_dep(&conn, "test", c, b, "duplicate"),
        &format!("#{c} already depends on #{b}"),
    );
    assert_exact_error(
        pt::remove_dep(&conn, "test", a, b, "not present"),
        &format!("#{a} does not depend on #{b}"),
    );
    assert_exact_error(
        pt::event_cursor(&conn, 0),
        "event cursor requires a positive event ID",
    );
    assert_exact_error(
        pt::event_log(&conn, None, 0, None, None),
        "event log --limit must be between 1 and 500",
    );
    assert_exact_error(
        pt::event_log(&conn, None, 1, Some("x"), Some("y")),
        "event log accepts only one of --before-cursor or --after-cursor",
    );
    assert_exact_error(
        pt::search_tasks(&conn, "---", None, None, 20),
        "search query must contain at least one letter or digit",
    );
    assert_exact_error(
        pt::search_tasks(&conn, "task", None, None, 0),
        "search --limit must be between 1 and 200",
    );
    assert_exact_error(
        pt::search_tasks(&conn, "task", None, Some("complete"), 20),
        "unknown task status 'complete' (expected proposed|in_progress|done|retired|rejected)",
    );
}

#[test]
fn lifecycle_refusals_have_exact_corrective_messages() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let dependency =
        pt::add_task(&conn, "test", plan, pt::TaskCreation::new("dependency")).unwrap();
    let blocked = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[dependency],
            ..pt::TaskCreation::new("blocked")
        },
    )
    .unwrap();
    assert_exact_error(
        pt::start_task(&conn, "test", blocked, None, None),
        &format!("#{blocked} is not ready; resolve dep:#{dependency} before starting it"),
    );
    pt::start_task(&conn, "test", dependency, None, None).unwrap();
    // Explicit resumption is allowed; pickup is advisory, not an exclusive hold.
    pt::start_task(&conn, "test", dependency, None, None).unwrap();
    pt::complete_task(&conn, "test", dependency, None, None).unwrap();
    assert_exact_error(
        pt::start_task(&conn, "test", dependency, None, None),
        &format!("#{dependency} is done; use `reopen` before starting it"),
    );
    assert_exact_error(
        pt::complete_task(&conn, "test", dependency, None, None),
        &format!("#{dependency} is already done"),
    );
    assert_exact_error(
        pt::reopen_task(&conn, "test", blocked, "not terminal"),
        &format!("#{blocked} is proposed; only terminal tasks can be reopened"),
    );
    assert_exact_error(
        pt::reopen_task(&conn, "test", dependency, ""),
        "reopening a task requires a nonblank reason",
    );

    let decision = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            kind: "decision",
            ..pt::TaskCreation::new("decision")
        },
    )
    .unwrap();
    assert_exact_error(
        pt::complete_task(&conn, "test", decision, None, None),
        &format!(
            "completing decision task #{decision} requires --result or --result-file so the measured or selected outcome survives the session"
        ),
    );
    assert_exact_error(
        pt::complete_task(&conn, "test", decision, None, Some("agent")),
        "--result-source requires --result or --result-file with a durable outcome",
    );
    assert_exact_error(
        pt::validate_meaning_source(Some("operator")),
        "unknown meaning source 'operator' (expected user|agent|external)",
    );

    pt::set_plan_status(&conn, "test", "p", "paused", "exercise plan refusal").unwrap();
    assert_exact_error(
        pt::start_task(&conn, "test", blocked, None, None),
        &format!("plan is paused; set it active before starting task #{blocked}"),
    );
    assert_exact_error(
        pt::complete_task(&conn, "test", blocked, None, None),
        &format!("plan is paused; set it active before completing proposed task #{blocked}"),
    );
}

#[test]
fn gate_and_blocker_refusals_and_reopen_paths_are_exact_and_evented() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();

    assert_exact_error(
        pt::add_gate(&conn, "test", task, "proof", "invalid", "requirement"),
        "unknown gate kind 'invalid' (expected test|benchmark|review|capture|fixture|build|doc|other)",
    );
    pt::add_gate(&conn, "test", task, "proof", "test", "requirement").unwrap();
    assert_exact_error(
        pt::add_gate(&conn, "test", task, "proof", "test", "requirement"),
        &format!("gate 'proof' already exists on #{task}; choose a different gate name"),
    );
    for (locator, expected) in [
        (
            "missing-scheme",
            "evidence locator 'missing-scheme' must be scheme:value (e.g. file:runtime/evidence/x.json)",
        ),
        (
            "file:   ",
            "evidence locator 'file:   ' must have a nonblank scheme and value",
        ),
        (
            "1file:proof",
            "evidence locator '1file:proof' has an invalid scheme; use an RFC 3986-style name",
        ),
    ] {
        assert_exact_error(
            pt::resolve_gate(&conn, "test", task, "proof", locator, None, None),
            expected,
        );
    }
    assert_exact_error(
        pt::reopen_gate(&conn, "test", task, "proof", "already open"),
        &format!("gate 'proof' on #{task} is already open"),
    );
    assert_exact_error(
        pt::remove_gate(&conn, "test", task, "proof", ""),
        "removing a gate requires a nonblank reason",
    );
    pt::resolve_gate(&conn, "test", task, "proof", "file:proof.json", None, None).unwrap();
    assert_exact_error(
        pt::remove_gate(&conn, "test", task, "proof", "closed"),
        &format!("no open gate 'proof' on #{task}"),
    );
    pt::reopen_gate(&conn, "test", task, "proof", "replace evidence").unwrap();

    pt::add_blocker(&conn, "test", task, "input", "operator input").unwrap();
    assert_exact_error(
        pt::reopen_blocker(&conn, "test", task, "input", "already open"),
        &format!("blocker 'input' on #{task} is already open"),
    );
    pt::resolve_blocker(&conn, "test", task, "input", "file:input.json", None, None).unwrap();
    assert_exact_error(
        pt::remove_blocker(&conn, "test", task, "input", "closed"),
        &format!("no open blocker 'input' on #{task}"),
    );
    pt::reopen_blocker(&conn, "test", task, "input", "replace evidence").unwrap();
    let blockers = pt::task_blockers(&conn, pt::get_task(&conn, task).unwrap().task_id).unwrap();
    assert_eq!(blockers[0].status, "open");
    assert_eq!(blockers[0].evidence_locator, None);
    let event: String = conn
        .query_row(
            "SELECT kind FROM events WHERE kind='blocker_reopen' ORDER BY event_id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event, "blocker_reopen");
}

#[test]
fn evidence_verifier_classifies_unhashed_missing_escaping_and_unsupported_bindings() {
    let root = unique_test_path("evidence-classification-root").with_extension("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/proof.txt"), b"proof\n").unwrap();
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();
    for (name, locator) in [
        ("unhashed", "file:docs/proof.txt"),
        ("missing", "file:docs/missing.txt"),
        ("escape", "file:../outside.txt"),
        (
            "unsupported",
            "commit:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    ] {
        pt::add_gate(&conn, "test", task, name, "review", "retained evidence").unwrap();
        pt::resolve_gate(&conn, "test", task, name, locator, None, None).unwrap();
    }

    let report = pt::verify_evidence(
        &conn,
        &root,
        &pt::EvidenceVerificationOptions {
            task_seq: Some(task),
            classification: pt::EvidenceClassificationFilter::All,
            ..Default::default()
        },
    )
    .unwrap();
    let statuses = report
        .projection
        .bindings
        .iter()
        .map(|binding| (binding.name.as_str(), binding.status.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(statuses["unhashed"], "unhashed");
    assert_eq!(statuses["missing"], "missing");
    assert_eq!(statuses["escape"], "path_escape");
    assert_eq!(statuses["unsupported"], "unsupported_scheme");
    assert_eq!(report.summary.failed_count, 3);
    assert_eq!(report.summary.unsupported_count, 1);
    assert!(!report.summary.verification_complete);
    assert!(
        report
            .projection
            .bindings
            .iter()
            .filter(|binding| binding.status != "unsupported_scheme")
            .all(|binding| !binding.corrective_commands.is_empty())
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn evidence_verifier_pages_mixed_authority_details_with_scope_bound_cursors() {
    let root = unique_test_path("evidence-pagination-root").with_extension("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/proof.txt"), b"proof\n").unwrap();
    let digest = pt::sha256(b"proof\n");
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();

    for index in 0..24 {
        let seq = pt::add_task(
            &conn,
            "test",
            plan,
            pt::TaskCreation::new(&format!("task-{index:02}")),
        )
        .unwrap();
        let (name, locator, sha256) = match index % 3 {
            0 => ("verified", "file:docs/proof.txt", Some(digest.as_str())),
            1 => ("failed", "file:docs/missing.txt", Some(digest.as_str())),
            _ => (
                "unsupported",
                "commit:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                None,
            ),
        };
        pt::add_gate(&conn, "test", seq, name, "review", "retained evidence").unwrap();
        pt::resolve_gate(&conn, "test", seq, name, locator, sha256, None).unwrap();
        if index % 2 == 1 {
            pt::complete_task(&conn, "test", seq, None, None).unwrap();
        }
    }

    let first = pt::verify_evidence(
        &conn,
        &root,
        &pt::EvidenceVerificationOptions {
            limit: 5,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(first.schema, "papertiger.evidence_verification.v3");
    assert_eq!(first.summary.binding_count, 24);
    assert_eq!(first.summary.verified_count, 8);
    assert_eq!(first.summary.failed_count, 8);
    assert_eq!(first.summary.unsupported_count, 8);
    assert_eq!(first.summary.status_counts["verified"], 8);
    assert_eq!(first.summary.status_counts["missing"], 8);
    assert_eq!(first.summary.status_counts["unsupported_scheme"], 8);
    assert_eq!(first.summary.unsupported_scheme_counts["commit"], 8);
    assert!(!first.summary.verification_complete);
    assert_eq!(first.projection.eligible_count, 16);
    assert_eq!(first.projection.returned_count, 5);
    assert_eq!(first.projection.remaining_count, 11);
    assert_eq!(first.projection.omitted_count, 11);
    assert!(!first.projection.complete);
    assert!(first.projection.has_more);
    let cursor = first.projection.next_cursor.clone().unwrap();

    let second = pt::verify_evidence(
        &conn,
        &root,
        &pt::EvidenceVerificationOptions {
            limit: 5,
            after_cursor: Some(cursor.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(second.projection.page_start, 5);
    assert_eq!(second.projection.returned_count, 5);
    assert_eq!(second.projection.remaining_count, 6);
    assert_eq!(second.projection.omitted_count, 11);
    assert!(!second.projection.complete);

    let failed_open = pt::verify_evidence(
        &conn,
        &root,
        &pt::EvidenceVerificationOptions {
            classification: pt::EvidenceClassificationFilter::Failed,
            task_state: pt::EvidenceTaskStateFilter::Open,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(failed_open.projection.eligible_count, 4);
    assert!(failed_open.projection.bindings.iter().all(|binding| {
        binding.classification == pt::EvidenceClassification::Failed
            && binding.task_status == "proposed"
    }));

    let unsupported_terminal = pt::verify_evidence(
        &conn,
        &root,
        &pt::EvidenceVerificationOptions {
            classification: pt::EvidenceClassificationFilter::Unsupported,
            task_state: pt::EvidenceTaskStateFilter::Terminal,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(unsupported_terminal.projection.eligible_count, 4);
    assert!(
        unsupported_terminal
            .projection
            .bindings
            .iter()
            .all(|binding| {
                binding.classification == pt::EvidenceClassification::Unsupported
                    && binding.task_status == "done"
            })
    );

    let changed_filter = pt::verify_evidence(
        &conn,
        &root,
        &pt::EvidenceVerificationOptions {
            classification: pt::EvidenceClassificationFilter::Failed,
            limit: 5,
            after_cursor: Some(cursor),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        changed_filter
            .to_string()
            .contains("cursor is invalid for this live verification scope")
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn priority_orders_ready_queue() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let low = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("low")).unwrap();
    let high = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            priority: 9,
            ..pt::TaskCreation::new("high")
        },
    )
    .unwrap();
    let ready = pt::ready_tasks(&conn, plan, 10, false).unwrap();
    assert_eq!(
        ready.iter().map(|e| e.task.seq).collect::<Vec<_>>(),
        vec![high, low]
    );
}

#[test]
fn ready_limit_applies_to_the_whole_result() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let blocker = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("blocker")).unwrap();
    for title in ["ready-a", "ready-b"] {
        pt::add_task(
            &conn,
            "test",
            plan,
            pt::TaskCreation {
                priority: 1,
                ..pt::TaskCreation::new(title)
            },
        )
        .unwrap();
    }
    for title in ["blocked-a", "blocked-b"] {
        pt::add_task(
            &conn,
            "test",
            plan,
            pt::TaskCreation {
                deps: &[blocker],
                ..pt::TaskCreation::new(title)
            },
        )
        .unwrap();
    }
    pt::start_task(&conn, "test", blocker, None, None).unwrap();

    let entries = pt::ready_tasks(&conn, plan, 3, true).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.blockers.is_empty())
            .count(),
        2
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| !entry.blockers.is_empty())
            .count(),
        1
    );
}

#[test]
fn audit_flags_dead_deps_and_lagging_parents() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let dead = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("dead")).unwrap();
    let live = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[dead],
            ..pt::TaskCreation::new("live")
        },
    )
    .unwrap();
    pt::reject_task(&conn, "test", dead, "nope").unwrap();
    let ready = pt::ready_tasks(&conn, plan, 10, false).unwrap();
    assert!(
        ready.iter().all(|entry| entry.task.seq != live),
        "a rejected prerequisite must keep its consumer blocked"
    );
    let parent = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("milestone")).unwrap();
    let child = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            parent: Some(parent),
            ..pt::TaskCreation::new("child")
        },
    )
    .unwrap();
    pt::complete_task(&conn, "test", child, None, None).unwrap();
    let findings = pt::audit(&conn).unwrap();
    let kinds: Vec<&str> = findings.iter().map(|f| f.kind.as_str()).collect();
    assert!(kinds.contains(&"dep_on_dead"), "{kinds:?}");
    assert!(kinds.contains(&"parent_lagging"), "{kinds:?}");
    let _ = live;
}

#[test]
fn export_import_roundtrip_preserves_graph() {
    let mut conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "why not").unwrap();
    let a = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            intent: "intent a",
            tags: &["track:x".into()],
            priority: 2,
            why: Some("roundtrip fixture"),
            ..pt::TaskCreation::new("a")
        },
    )
    .unwrap();
    let b = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[a],
            ..pt::TaskCreation::new("b")
        },
    )
    .unwrap();
    pt::add_gate(&conn, "test", a, "smoke", "test", "passes").unwrap();
    pt::resolve_gate(&conn, "test", a, "smoke", "file:e.json", None, None).unwrap();
    pt::complete_task(&conn, "test", a, None, None).unwrap();
    let dump = pt::export(&conn, None).unwrap();
    let json = serde_json::to_string(&dump).unwrap();

    let conn2 = db();
    let dump2: pt::Dump = serde_json::from_str(&json).unwrap();
    let (tasks, deps) = pt::import(&conn2, "test", &dump2).unwrap();
    assert_eq!((tasks, deps), (2, 1));
    let a2 = pt::get_task(&conn2, a).unwrap();
    assert_eq!(a2.status, "done");
    let ready = pt::ready_tasks(
        &conn2,
        pt::resolve_plan(&conn2, Some("p")).unwrap().0,
        10,
        false,
    )
    .unwrap();
    assert_eq!(
        ready.iter().map(|e| e.task.seq).collect::<Vec<_>>(),
        vec![b]
    );
    let _ = conn.transaction().unwrap(); // silence unused-mut lint paths
}

#[test]
fn dump_parser_accepts_windows_utf8_bom() {
    let dump = pt::parse_dump_json(
        "\u{feff}{\"schema\":\"papertiger.dump.v10\",\"plans\":[],\"tasks\":[]}",
    )
    .unwrap();
    assert_eq!(dump.schema, "papertiger.dump.v10");
}

#[test]
fn import_refuses_resolved_gate_without_evidence() {
    let conn = db();
    let dump: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v10",
            "plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":1,"plan":"p","title":"t","status":"done",
                      "gates":[{"name":"g","kind":"test","requirement":"r","status":"resolved"}]}]}"#,
    )
    .unwrap();
    let err = pt::import(&conn, "test", &dump).unwrap_err();
    assert!(err.to_string().contains("lacks evidence_locator"));
}

#[test]
fn import_reuses_live_validators_and_never_creates_auditable_corruption() {
    for (fixture, expected) in [
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task",
                    "result":"  ","result_source":"agent"}]
            }"#,
            "result_source without a durable result",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task","gates":[{
                    "name":"proof","kind":"test","requirement":"prove it",
                    "status":"resolved","evidence_locator":"commit:notahash",
                    "resolved_at":"2026-08-23T12:00:00Z"
                }]}]
            }"#,
            "invalid evidence_locator",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task"}],
                "events":[{
                    "at":"2026-08-23T12:00:00Z","actor":"fixture",
                    "entity":"task","entity_seq":1,"entity_plan":"p",
                    "kind":"status","payload":{"to":"done","result_source":"fabricated"}
                }]
            }"#,
            "invalid payload.result_source",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task"}],
                "events":[{
                    "at":"2026-08-23T12:00:00Z","actor":"fixture",
                    "entity":"task","entity_seq":1,"entity_plan":"p",
                    "kind":"note","payload":{"meaning_source":7}
                }]
            }"#,
            "payload.meaning_source must be a string or null",
        ),
    ] {
        let conn = db();
        let dump: pt::Dump = serde_json::from_str(fixture).unwrap();
        let error = pt::import(&conn, "restore", &dump).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0,
            "a refused import must roll back every row"
        );
    }

    let normalized = db();
    let dump: pt::Dump = serde_json::from_str(
        r#"{
            "schema":"papertiger.dump.v10",
            "plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":1,"plan":"p","title":"task","intent":"asked",
                "intent_source":" user ","tags":[" alpha "]}]
        }"#,
    )
    .unwrap();
    pt::import(&normalized, "restore", &dump).unwrap();
    let task = pt::get_task(&normalized, 1).unwrap();
    assert_eq!(task.intent_source.as_deref(), Some("user"));
    assert_eq!(
        pt::task_context(&normalized, 1).unwrap().tags,
        vec!["alpha"]
    );
    assert!(pt::audit(&normalized).unwrap().is_empty());
}

#[test]
fn import_plan_events_are_dump_local_and_plan_reuse_requires_identical_definition() {
    let destination = db();
    pt::add_plan(
        &destination,
        "fixture",
        "existing",
        "Existing",
        "destination definition",
    )
    .unwrap();
    let external_event: pt::Dump = serde_json::from_str(
        r#"{
            "schema":"papertiger.dump.v10",
            "plans":[{"slug":"incoming","title":"Incoming"}],
            "tasks":[],
            "events":[{
                "at":"2026-08-23T12:00:00Z","actor":"fixture",
                "entity":"plan","entity_plan":"existing","kind":"note"
            }]
        }"#,
    )
    .unwrap();
    let error = pt::import(&destination, "restore", &external_event).unwrap_err();
    assert!(
        error.to_string().contains("absent from the dump"),
        "{error:#}"
    );
    assert!(pt::resolve_plan(&destination, Some("incoming")).is_err());

    let conflicting: pt::Dump = serde_json::from_str(
        r#"{
            "schema":"papertiger.dump.v10",
            "plans":[{"slug":"existing","title":"Different",
                "intent":"destination definition"}],
            "tasks":[]
        }"#,
    )
    .unwrap();
    let error = pt::import(&destination, "restore", &conflicting).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("conflicts with the existing definition")
    );
}

#[test]
fn import_refuses_superseded_dump_with_a_complete_recovery_path() {
    let conn = db();
    let dump: pt::Dump =
        serde_json::from_str(r#"{"schema":"papertiger.dump.v5","plans":[],"tasks":[]}"#).unwrap();
    let error = pt::import(&conn, "test", &dump).unwrap_err().to_string();
    assert!(error.contains("release that produced it"));
    assert!(error.contains("papertiger --db <temporary-authority> init"));
    assert!(error.contains("then re-export papertiger.dump.v10"));
}

#[test]
fn container_tasks_never_enter_ready_queue() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let parent = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("milestone")).unwrap();
    let child = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            parent: Some(parent),
            ..pt::TaskCreation::new("child")
        },
    )
    .unwrap();
    let ready = pt::ready_tasks(&conn, plan, 10, false).unwrap();
    assert_eq!(
        ready.iter().map(|e| e.task.seq).collect::<Vec<_>>(),
        vec![child]
    );
}

#[test]
fn single_active_plan_is_implied_and_ambiguity_errors() {
    let conn = db();
    assert!(pt::active_plan(&conn).unwrap().is_none());
    assert!(pt::resolve_plan(&conn, None).is_err());
    pt::add_plan(&conn, "test", "one", "One", "").unwrap();
    assert_eq!(pt::active_plan(&conn).unwrap().unwrap().1, "one");
    assert_eq!(pt::resolve_plan(&conn, None).unwrap().1, "one");
    pt::add_plan(&conn, "test", "two", "Two", "").unwrap();
    assert!(pt::active_plan(&conn).is_err());
    let err = pt::resolve_plan(&conn, None).unwrap_err();
    assert!(err.to_string().contains("multiple active plans"));
    assert_eq!(pt::resolve_plan(&conn, Some("two")).unwrap().1, "two");
}

#[test]
fn plan_edits_are_evented_and_atomic() {
    let conn = db();
    let plan_id = pt::add_plan(&conn, "test", "one", "Old title", "Old intent").unwrap();

    let changed = pt::edit_plan(
        &conn,
        "agent",
        "one",
        Some("New title"),
        Some("New intent"),
        "current evidence changed the campaign boundary",
    )
    .unwrap();
    assert_eq!(changed, vec!["title", "intent"]);
    let plan = pt::get_plan(&conn, plan_id).unwrap();
    assert_eq!(plan.title, "New title");
    assert_eq!(plan.intent, "New intent");

    let (kind, why, payload): (String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT kind, why, payload FROM events
             WHERE entity='plan' AND entity_id=?1
             ORDER BY event_id DESC LIMIT 1",
            [plan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(kind, "edit");
    assert_eq!(
        why.as_deref(),
        Some("current evidence changed the campaign boundary")
    );
    let payload: serde_json::Value = serde_json::from_str(payload.as_deref().unwrap()).unwrap();
    assert_eq!(payload["slug"], "one");
    assert_eq!(payload["fields"], serde_json::json!(["title", "intent"]));

    assert!(pt::edit_plan(&conn, "agent", "one", Some(""), None, "invalid").is_err());
    assert_eq!(pt::get_plan(&conn, plan_id).unwrap().title, "New title");
    assert!(pt::edit_plan(&conn, "agent", "one", None, None, "invalid").is_err());
    assert!(pt::edit_plan(&conn, "agent", "one", Some("Other"), None, "").is_err());
}

#[test]
fn task_references_are_canonical_sequences_only() {
    for (task_ref, expected) in [
        ("1", 1),
        ("12", 12),
        ("#12", 12),
        ("9223372036854775807", i64::MAX),
    ] {
        assert_eq!(
            pt::parse_task_ref(task_ref).unwrap(),
            expected,
            "{task_ref}"
        );
    }

    for task_ref in [
        "",
        "#",
        "##12",
        "+12",
        "-12",
        "0",
        "#0",
        "01",
        "#01",
        " 12",
        "12 ",
        "12\n",
        "\u{ff11}\u{ff12}",
        "\u{661}\u{662}",
        "9223372036854775808",
        "MECH-BATCH-01",
    ] {
        let error = pt::parse_task_ref(task_ref).unwrap_err().to_string();
        assert!(
            error.contains("expected task.seq as N or #N")
                && error.contains("canonical positive ASCII decimal"),
            "{task_ref:?}: {error}"
        );
    }
}

#[test]
fn failed_add_is_atomic() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let existing = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("existing")).unwrap();
    let before_events: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    let error = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[existing, 999],
            ..pt::TaskCreation::new("must roll back")
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("no task #999"));
    let tasks: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tasks, 1);
    assert_eq!(events, before_events);
}

#[test]
fn import_rejects_cycles_missing_parents_and_done_open_gates() {
    let conn = db();
    let cycle: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v10","plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":1,"plan":"p","title":"a","deps":[2]},
                     {"seq":2,"plan":"p","title":"b","deps":[1]}]}"#,
    )
    .unwrap();
    assert!(pt::import(&conn, "test", &cycle).is_err());

    let missing_parent: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v10","plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":3,"plan":"p","title":"child","parent_seq":999}]}"#,
    )
    .unwrap();
    assert!(pt::import(&conn, "test", &missing_parent).is_err());

    let done_open: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v10","plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":4,"plan":"p","title":"false done","status":"done",
                      "gates":[{"name":"g","kind":"test","requirement":"r"}]}]}"#,
    )
    .unwrap();
    assert!(pt::import(&conn, "test", &done_open).is_err());

    let tasks: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tasks, 0, "every failed import must roll back completely");
}

#[test]
fn import_allocates_omitted_sequences_before_linking() {
    let conn = db();
    let dump: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v10","plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":7,"plan":"p","title":"parent"},
                     {"plan":"p","title":"child","parent_seq":7,"deps":[7]}]}"#,
    )
    .unwrap();
    pt::import(&conn, "test", &dump).unwrap();
    let child = pt::get_task(&conn, 8).unwrap();
    assert!(child.parent_id.is_some());
    assert_eq!(pt::open_deps(&conn, child.task_id).unwrap(), vec![7]);
}

#[test]
fn evidence_and_waiver_reasons_are_durable() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();
    pt::add_gate(&conn, "test", task, "g", "test", "r").unwrap();
    let bad = pt::resolve_gate(
        &conn,
        "test",
        task,
        "g",
        "file:evidence.json",
        Some("ab12"),
        None,
    )
    .unwrap_err();
    assert!(bad.to_string().contains("64 lowercase"));
    pt::waive_gate(&conn, "test", task, "g", "upstream fixture unavailable").unwrap();
    pt::add_note(
        &conn,
        "test",
        Some(task),
        "cold-readable handoff note",
        None,
    )
    .unwrap();
    let dump = pt::export(&conn, None).unwrap();
    assert!(!dump.events.is_empty());

    let restored = db();
    pt::import(&restored, "restore", &dump).unwrap();
    let (status, note): (String, Option<String>) = restored
        .query_row("SELECT status, note FROM gates WHERE name='g'", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(status, "waived");
    assert_eq!(note.as_deref(), Some("upstream fixture unavailable"));
    let notes: i64 = restored
        .query_row(
            "SELECT COUNT(*) FROM events WHERE kind='note' AND why='cold-readable handoff note'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(notes, 1);
}

#[test]
fn focus_excludes_containers_and_honors_priority_before_unlock_impact() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "focus", "Focus", "").unwrap();
    let container = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            priority: 100,
            ..pt::TaskCreation::new("container")
        },
    )
    .unwrap();
    let active_leaf = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            parent: Some(container),
            ..pt::TaskCreation::new("active leaf")
        },
    )
    .unwrap();
    let prerequisite =
        pt::add_task(&conn, "test", plan, pt::TaskCreation::new("prerequisite")).unwrap();
    let independent = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            priority: 50,
            ..pt::TaskCreation::new("independent")
        },
    )
    .unwrap();
    let dependent = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[prerequisite],
            ..pt::TaskCreation::new("dependent")
        },
    )
    .unwrap();
    let downstream = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[dependent],
            ..pt::TaskCreation::new("downstream")
        },
    )
    .unwrap();
    pt::start_task(&conn, "test", container, None, None).unwrap();
    pt::start_task(&conn, "test", active_leaf, None, None).unwrap();

    let focus = pt::focus(&conn, plan, 20, true, None).unwrap();
    let sequences = focus
        .projection
        .entries
        .iter()
        .map(|entry| entry.task.seq)
        .collect::<Vec<_>>();
    assert!(!sequences.contains(&container));
    assert_eq!(sequences[0], active_leaf);
    assert!(
        sequences.iter().position(|seq| *seq == independent)
            < sequences.iter().position(|seq| *seq == prerequisite),
        "explicit operator priority must outrank inferred graph breadth"
    );
    let prerequisite_entry = focus
        .projection
        .entries
        .iter()
        .find(|entry| entry.task.seq == prerequisite)
        .unwrap();
    assert_eq!(prerequisite_entry.immediate_unlock_count, 1);
    assert_eq!(prerequisite_entry.unfinished_downstream_count, 2);
    let dependent_entry = focus
        .projection
        .entries
        .iter()
        .find(|entry| entry.task.seq == dependent)
        .unwrap();
    assert_eq!(dependent_entry.readiness, "blocked");
    assert_eq!(
        dependent_entry.blockers,
        vec![format!("dep:#{prerequisite}")]
    );
    assert!(sequences.contains(&downstream));
}

#[test]
fn tags_and_mistaken_open_gates_are_correctable_with_evented_reasons() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "corrections", "Corrections", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();
    pt::add_tag(
        &conn,
        "test",
        task,
        "calibration",
        "route measured residual",
    )
    .unwrap();
    assert!(pt::add_tag(&conn, "test", task, "calibration", "duplicate").is_err());
    pt::remove_tag(&conn, "test", task, "calibration", "classification changed").unwrap();
    assert!(pt::remove_tag(&conn, "test", task, "calibration", "duplicate removal").is_err());

    pt::add_gate(&conn, "test", task, "wrong gate", "test", "obsolete").unwrap();
    assert!(pt::remove_gate(&conn, "test", task, "wrong gate", "").is_err());
    pt::remove_gate(
        &conn,
        "test",
        task,
        "wrong gate",
        "gate was attached to the wrong task",
    )
    .unwrap();
    let gate_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM gates", [], |row| row.get(0))
        .unwrap();
    assert_eq!(gate_count, 0);
    let correction_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events
             WHERE kind IN ('tag_add','tag_remove','gate_remove') AND why IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(correction_events, 3);
}

#[test]
fn dependencies_and_external_blockers_gate_execution_but_not_planning() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "blockers", "Blockers", "").unwrap();
    let prerequisite = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation::new("collect evidence"),
    )
    .unwrap();
    let decision = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            kind: "decision",
            deps: &[prerequisite],
            priority: 5,
            ..pt::TaskCreation::new("select design")
        },
    )
    .unwrap();
    pt::add_blocker(
        &conn,
        "test",
        decision,
        "operator input",
        "the owner must choose the compatibility boundary",
    )
    .unwrap();

    let err = pt::start_task(&conn, "test", decision, None, None).unwrap_err();
    assert!(err.to_string().contains(&format!("dep:#{prerequisite}")));
    assert!(err.to_string().contains("blocker:operator input"));
    let default_focus = pt::focus(&conn, plan, 20, false, None).unwrap();
    assert!(
        !default_focus
            .projection
            .entries
            .iter()
            .any(|entry| entry.task.seq == decision)
    );
    let full_focus = pt::focus(&conn, plan, 20, true, None).unwrap();
    let entry = full_focus
        .projection
        .entries
        .iter()
        .find(|entry| entry.task.seq == decision)
        .unwrap();
    assert_eq!(entry.readiness, "blocked");

    pt::complete_task(&conn, "test", prerequisite, None, None).unwrap();
    assert!(pt::start_task(&conn, "test", decision, None, None).is_err());
    pt::resolve_blocker(
        &conn,
        "test",
        decision,
        "operator input",
        "project-db:decision/compatibility-boundary",
        None,
        Some("owner selected the bounded interface"),
    )
    .unwrap();
    pt::start_task(&conn, "test", decision, None, None).unwrap();
    assert!(pt::complete_task(&conn, "test", decision, None, None).is_err());
    pt::complete_task(
        &conn,
        "test",
        decision,
        Some("Use a bounded interface and reject implicit compatibility."),
        None,
    )
    .unwrap();
    assert_eq!(
        pt::get_task(&conn, decision).unwrap().result.as_deref(),
        Some("Use a bounded interface and reject implicit compatibility.")
    );
    assert!(pt::audit(&conn).unwrap().is_empty());
}

#[test]
fn a_discovered_blocker_on_active_work_is_first_class_not_an_audit_error() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "active-blocker", "Active blocker", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("implement")).unwrap();
    pt::start_task(&conn, "test", task, None, None).unwrap();
    pt::add_blocker(
        &conn,
        "test",
        task,
        "missing fixture",
        "the required fixture has not been supplied",
    )
    .unwrap();

    let focus = pt::focus(&conn, plan, 10, false, None).unwrap();
    assert_eq!(focus.projection.entries[0].task.seq, task);
    assert_eq!(focus.projection.entries[0].readiness, "in_progress");
    assert!(
        !pt::audit(&conn)
            .unwrap()
            .iter()
            .any(|finding| finding.kind == "done_with_open_blocker")
    );

    pt::waive_blocker(
        &conn,
        "test",
        task,
        "missing fixture",
        "the task was narrowed so the fixture is no longer applicable",
    )
    .unwrap();
    pt::complete_task(&conn, "test", task, None, None).unwrap();
    assert!(pt::audit(&conn).unwrap().is_empty());
}

#[test]
fn reopening_preserves_valid_gate_evidence_until_the_gate_is_explicitly_reopened() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "reopen", "Reopen", "").unwrap();
    let task = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            kind: "probe",
            ..pt::TaskCreation::new("measure behavior")
        },
    )
    .unwrap();
    pt::add_gate(
        &conn,
        "test",
        task,
        "reproduction",
        "test",
        "the observed result is reproducible",
    )
    .unwrap();
    pt::resolve_gate(
        &conn,
        "test",
        task,
        "reproduction",
        "experiment:run/1",
        None,
        None,
    )
    .unwrap();
    pt::complete_task(
        &conn,
        "test",
        task,
        Some("The first hypothesis was falsified."),
        None,
    )
    .unwrap();

    pt::reopen_task(
        &conn,
        "test",
        task,
        "the scope expanded to a second input family",
    )
    .unwrap();
    let reopened = pt::get_task(&conn, task).unwrap();
    assert_eq!(reopened.status, "proposed");
    assert_eq!(reopened.result, None);
    let context = pt::task_context(&conn, task).unwrap();
    assert_eq!(context.gates[0].status, "resolved");
    assert_eq!(
        context.gates[0].evidence_locator.as_deref(),
        Some("experiment:run/1")
    );

    pt::reopen_gate(
        &conn,
        "test",
        task,
        "reproduction",
        "the original run does not cover the expanded input family",
    )
    .unwrap();
    let context = pt::task_context(&conn, task).unwrap();
    assert_eq!(context.gates[0].status, "open");
    assert_eq!(context.gates[0].evidence_locator, None);
    assert!(pt::complete_task(&conn, "test", task, Some("premature"), None).is_err());
}

#[test]
fn parent_dependency_and_plan_transitions_preserve_terminal_truth() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "truth", "Truth", "").unwrap();
    let parent = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("outcome")).unwrap();
    let child = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            parent: Some(parent),
            ..pt::TaskCreation::new("deliverable")
        },
    )
    .unwrap();
    pt::complete_task(&conn, "test", child, None, None).unwrap();
    pt::complete_task(&conn, "test", parent, None, None).unwrap();
    assert!(
        pt::add_task(
            &conn,
            "test",
            plan,
            pt::TaskCreation {
                parent: Some(parent),
                ..pt::TaskCreation::new("late child")
            }
        )
        .is_err()
    );
    assert!(
        pt::reopen_task(&conn, "test", child, "new evidence").is_err(),
        "a child cannot become live beneath a terminal parent"
    );
    pt::reopen_task(
        &conn,
        "test",
        parent,
        "the outcome needs another deliverable",
    )
    .unwrap();
    pt::reopen_task(&conn, "test", child, "the deliverable needs revision").unwrap();

    let prerequisite =
        pt::add_task(&conn, "test", plan, pt::TaskCreation::new("prerequisite")).unwrap();
    let dependent = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            deps: &[prerequisite],
            ..pt::TaskCreation::new("dependent")
        },
    )
    .unwrap();
    pt::complete_task(&conn, "test", prerequisite, None, None).unwrap();
    pt::complete_task(&conn, "test", dependent, None, None).unwrap();
    assert!(
        pt::reopen_task(&conn, "test", prerequisite, "recheck evidence").is_err(),
        "reopening a prerequisite must not silently invalidate a completed dependent"
    );
    pt::reopen_task(
        &conn,
        "test",
        dependent,
        "the prerequisite will be rechecked",
    )
    .unwrap();
    pt::reopen_task(&conn, "test", prerequisite, "recheck evidence").unwrap();

    pt::set_plan_status(&conn, "test", "truth", "paused", "hold execution").unwrap();
    assert!(pt::start_task(&conn, "test", prerequisite, None, None).is_err());
    pt::set_plan_status(&conn, "test", "truth", "active", "resume execution").unwrap();
    pt::start_task(&conn, "test", prerequisite, None, None).unwrap();
}

#[test]
fn current_export_import_preserves_task_kind_result_blocker_and_mise_evidence() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "roundtrip-v2", "Roundtrip v2", "").unwrap();
    let task = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            intent: "Test the two viable engines and select one.",
            kind: "decision",
            tags: &["architecture".into()],
            priority: 7,
            why: Some("decision fixture"),
            ..pt::TaskCreation::new("choose engine")
        },
    )
    .unwrap();
    pt::add_blocker(
        &conn,
        "test",
        task,
        "benchmark data",
        "the benchmark has not completed",
    )
    .unwrap();
    pt::resolve_blocker(
        &conn,
        "test",
        task,
        "benchmark data",
        "benchmark:engine-comparison/2026-07-26",
        None,
        Some("both engines tested on the same fixture"),
    )
    .unwrap();
    pt::complete_task(
        &conn,
        "test",
        task,
        Some("Select engine B because it satisfies the bounded latency target."),
        None,
    )
    .unwrap();

    let dump = pt::export(&conn, Some("roundtrip-v2")).unwrap();
    assert_eq!(dump.schema, "papertiger.dump.v10");
    let restored = db();
    pt::import(&restored, "test", &dump).unwrap();
    let restored_task = pt::get_task(&restored, task).unwrap();
    assert_eq!(restored_task.kind, "decision");
    assert_eq!(
        restored_task.result.as_deref(),
        Some("Select engine B because it satisfies the bounded latency target.")
    );
    let blockers = pt::task_blockers(&restored, restored_task.task_id).unwrap();
    assert_eq!(blockers[0].status, "resolved");
    assert_eq!(
        blockers[0].evidence_locator.as_deref(),
        Some("benchmark:engine-comparison/2026-07-26")
    );
    assert!(pt::audit(&restored).unwrap().is_empty());
}

/// A projection exactly as recorded before 0.18: both the projection id and
/// the embedded candidate material id are the retired hyphenated forms.
fn legacy_mise_projection_fixture() -> pt::MisePlannerProjection {
    let mut legacy = mise_projection_fixture();
    legacy.schema = pt::MISE_PLANNER_PROJECTION_SCHEMA_V1.to_owned();
    legacy.candidate_material_json = legacy.candidate_material_json.replace(
        "papertiger-mise.candidate_material.v2",
        "papertiger-mise.candidate-material.v1",
    );
    legacy.candidate_material_sha256 = pt::sha256(legacy.candidate_material_json.as_bytes());
    legacy
}

#[test]
fn projection_and_candidate_material_ids_must_share_a_generation() {
    let legacy = legacy_mise_projection_fixture();
    let mut current_outer = legacy.clone();
    current_outer.schema = pt::MISE_PLANNER_PROJECTION_SCHEMA.to_owned();
    let mut legacy_outer = mise_projection_fixture();
    legacy_outer.schema = pt::MISE_PLANNER_PROJECTION_SCHEMA_V1.to_owned();
    for mixed in [current_outer, legacy_outer] {
        let error = mixed.validate().unwrap_err().to_string();
        assert!(
            error.contains("does not match projection schema")
                && error.contains("papertiger-mise projection export"),
            "{error}"
        );
    }
    legacy.validate().unwrap();
    mise_projection_fixture().validate().unwrap();
}

fn mise_projection_fixture() -> pt::MisePlannerProjection {
    use pt::{
        MISE_PLANNER_PROJECTION_SCHEMA, MiseBudgetProjection, MiseMutationProjection,
        MisePlannerProjection, MiseProjectionDisposition, MiseSourceProjection, sha256,
    };

    let material = r#"{"schema":"papertiger-mise.candidate_material.v2","kind":"git_change_set","protocol":"papertiger-mise.git_change_set.v2","media_type":"application/vnd.papertiger-mise.git-change-set+json","payload_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","scope":{"changed_paths":["src/lib.rs"],"operations":["modify"]},"change_set":{"schema":"papertiger-mise.git_change_set.v2","changes":[]}}"#;
    MisePlannerProjection {
        schema: MISE_PLANNER_PROJECTION_SCHEMA.to_owned(),
        campaign_id: "subject-objective-a01".to_owned(),
        manifest_sha256: "1".repeat(64),
        candidate_id: "2".repeat(64),
        nomination_id: Some("3".repeat(64)),
        source: MiseSourceProjection {
            repository_id: "fixture".to_owned(),
            base_commit: "abc".to_owned(),
            base_tree: "def".to_owned(),
        },
        mutation: MiseMutationProjection {
            allowlist: vec!["src".to_owned()],
            protected_paths: vec!["tests".to_owned()],
            changed_paths: vec!["src/lib.rs".to_owned()],
        },
        disposition: MiseProjectionDisposition::Nominated,
        evidence_grade: Some("deterministic_development".to_owned()),
        candidate_material_sha256: sha256(material.as_bytes()),
        candidate_material_json: material.to_owned(),
        result: serde_json::json!({"schema": "fixture.result.v1"}),
        relied_upon_evidence_ids: vec!["3".repeat(64)],
        limitations: vec![
            "deterministic-development-evidence-is-not-deployment-authority".to_owned(),
            "nomination-is-evidence-not-integration-or-promotion".to_owned(),
        ],
        budgets: vec![MiseBudgetProjection {
            resource: "trials".to_owned(),
            unit: "count".to_owned(),
            hard_limit: 5,
            reserved_amount: 0,
            spent_amount: 4,
            available_amount: 1,
        }],
    }
}

#[test]
fn mise_projection_is_immutable_idempotent_non_authoritative_and_transferable() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "projection", "Projection", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("own evidence")).unwrap();
    let other = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("unrelated")).unwrap();
    let bytes = serde_json::to_vec_pretty(&mise_projection_fixture()).unwrap();

    let (outcome, record) = pt::record_mise_projection(&conn, "test", task, &bytes).unwrap();
    assert_eq!(outcome, pt::MiseProjectionRecordOutcome::Recorded);
    assert_eq!(pt::get_task(&conn, task).unwrap().status, "proposed");
    assert_eq!(
        pt::task_context(&conn, task)
            .unwrap()
            .mise_projections
            .len(),
        1
    );

    let (outcome, replayed) = pt::record_mise_projection(&conn, "test", task, &bytes).unwrap();
    assert_eq!(outcome, pt::MiseProjectionRecordOutcome::Existing);
    assert_eq!(replayed.projection_sha256, record.projection_sha256);
    let error = pt::record_mise_projection(&conn, "test", other, &bytes)
        .expect_err("the same immutable evidence must not acquire a second owning task");
    assert!(error.to_string().contains("already projected"));
    assert!(
        conn.execute(
            "UPDATE task_mise_projections SET disposition='rejected' WHERE projection_sha256=?1",
            [&record.projection_sha256],
        )
        .is_err()
    );

    let dump = pt::export(&conn, Some("projection")).unwrap();
    assert_eq!(dump.mise_projections.len(), 1);
    let restored = db();
    pt::import(&restored, "test", &dump).unwrap();
    let restored_record = pt::mise_projection(&restored, &record.projection_sha256)
        .unwrap()
        .unwrap();
    assert_eq!(restored_record.task_seq, task);
    assert!(pt::audit(&restored).unwrap().is_empty());
}

#[test]
fn legacy_mise_projection_id_is_readable_history_but_not_recordable() {
    let legacy = legacy_mise_projection_fixture();
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "projection", "Projection", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("evidence")).unwrap();
    let error =
        pt::record_mise_projection(&conn, "test", task, &serde_json::to_vec(&legacy).unwrap())
            .unwrap_err()
            .to_string();
    assert!(
        error.contains("papertiger.mise-planner-projection.v1 is readable but not recordable")
            && error.contains("papertiger mise record"),
        "{error}"
    );

    let current = serde_json::to_vec(&mise_projection_fixture()).unwrap();
    pt::record_mise_projection(&conn, "test", task, &current).unwrap();
    let mut dump = pt::export(&conn, None).unwrap();
    dump.mise_projections[0].projection_sha256 = legacy.projection_sha256().unwrap();
    dump.mise_projections[0].projection = legacy;
    let restored = db();
    pt::import(&restored, "test", &dump).unwrap();
    let stored = pt::task_mise_projections(&restored, task).unwrap();
    assert_eq!(
        stored[0].projection.schema,
        pt::MISE_PLANNER_PROJECTION_SCHEMA_V1
    );
    assert!(pt::audit(&restored).unwrap().is_empty());
}

#[test]
fn task_context_includes_gate_waiver_and_reopen_history() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "gate-history", "Gate history", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();
    pt::add_gate(&conn, "test", task, "proof", "test", "must pass").unwrap();
    pt::waive_gate(&conn, "test", task, "proof", "fixture unavailable").unwrap();
    pt::reopen_gate(&conn, "test", task, "proof", "fixture restored").unwrap();

    let context = pt::task_context(&conn, task).unwrap();
    let kinds = context
        .recent_events
        .iter()
        .map(|event| event.kind.as_str())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"create"), "{kinds:?}");
    assert!(kinds.contains(&"waived"), "{kinds:?}");
    assert!(kinds.contains(&"reopen"), "{kinds:?}");
    assert!(context.recent_events.iter().any(|event| {
        event.kind == "waived" && event.why.as_deref() == Some("fixture unavailable")
    }));
}

#[test]
fn task_context_reports_event_truncation_instead_of_hiding_it() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "event-page", "Event page", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();
    for index in 0..12 {
        pt::add_note(&conn, "test", Some(task), &format!("note {index}"), None)
            .expect("record task note");
    }

    let context = pt::task_context(&conn, task).unwrap();
    assert_eq!(context.recent_events.len(), 12);
    assert!(context.recent_events_truncated);
    assert_eq!(context.recent_events[0].why.as_deref(), Some("note 11"));
}

#[test]
fn canonical_task_queries_cover_status_tag_and_leaf_filters() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "query-tasks", "Query tasks", "").unwrap();
    let parent = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            tags: &["selected".to_owned()],
            priority: 10,
            ..pt::TaskCreation::new("parent")
        },
    )
    .unwrap();
    let child = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            parent: Some(parent),
            tags: &["selected".to_owned()],
            priority: 20,
            ..pt::TaskCreation::new("child")
        },
    )
    .unwrap();
    pt::start_task(&conn, "test", parent, None, None).unwrap();
    pt::start_task(&conn, "test", child, None, None).unwrap();

    let selected = pt::list_tasks(&conn, plan, None, Some("selected")).unwrap();
    assert_eq!(
        selected.iter().map(|task| task.seq).collect::<Vec<_>>(),
        vec![parent, child]
    );
    let active_selected =
        pt::list_tasks(&conn, plan, Some("in_progress"), Some("selected")).unwrap();
    assert_eq!(active_selected.len(), 2);
    let leaves = pt::leaf_tasks_with_status(&conn, plan, "in_progress").unwrap();
    assert_eq!(leaves.len(), 1);
    assert_eq!(leaves[0].seq, child);
}

#[test]
fn removed_gate_events_survive_export_import_without_recreating_the_gate() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "removed-gate", "Removed gate", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();
    pt::add_gate(&conn, "test", task, "mistake", "review", "wrong gate").unwrap();
    pt::remove_gate(&conn, "test", task, "mistake", "attached by mistake").unwrap();

    let dump = pt::export(&conn, Some("removed-gate")).unwrap();
    let create = dump
        .events
        .iter()
        .find(|event| event.entity == "gate" && event.kind == "create")
        .expect("removed gate create event must remain exportable");
    assert_eq!(create.entity_plan.as_deref(), Some("removed-gate"));
    assert_eq!(create.entity_seq, Some(task));
    assert_eq!(create.gate_name.as_deref(), Some("mistake"));

    let restored = db();
    pt::import(&restored, "restore", &dump).unwrap();
    let gate_count: i64 = restored
        .query_row("SELECT COUNT(*) FROM gates", [], |row| row.get(0))
        .unwrap();
    assert_eq!(gate_count, 0);
    let restored_dump = pt::export(&restored, Some("removed-gate")).unwrap();
    assert!(restored_dump.events.iter().any(|event| {
        event.entity == "gate"
            && event.kind == "create"
            && event.entity_seq == Some(task)
            && event.gate_name.as_deref() == Some("mistake")
    }));
    let context = pt::task_context(&restored, task).unwrap();
    assert!(
        context
            .recent_events
            .iter()
            .any(|event| event.kind == "gate_remove")
    );
    assert!(
        context
            .recent_events
            .iter()
            .any(|event| event.kind == "create")
    );
}

#[test]
fn import_refuses_non_rfc3339_event_timestamps() {
    let conn = db();
    pt::add_plan(&conn, "test", "timestamps", "Timestamps", "").unwrap();
    let mut dump = pt::export(&conn, None).unwrap();
    dump.events[0].at = "Ã©".into();

    let restored = db();
    let error = pt::import(&restored, "restore", &dump).unwrap_err();
    assert!(error.to_string().contains("not valid RFC3339"), "{error:#}");
}

#[test]
fn import_canonicalizes_event_timestamps_and_preserves_lifecycle_history() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "timestamps", "Timestamps", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();
    pt::start_task(&conn, "agent", task, Some("begin"), None).unwrap();
    pt::complete_task(&conn, "agent", task, None, None).unwrap();
    let expected = pt::task_activity(&conn, task).unwrap();
    let mut dump = pt::export(&conn, None).unwrap();
    for event in &mut dump.events {
        event.at = format!("  {}  ", event.at);
    }

    let restored = db();
    pt::import(&restored, "restore", &dump).unwrap();
    let actual = pt::task_activity(&restored, task).unwrap();
    assert_eq!(actual.created_event, expected.created_event);
    assert_eq!(actual.last_event, expected.last_event);
    assert_eq!(actual.status_event, expected.status_event);
    assert_eq!(actual.started_event, expected.started_event);
    assert_eq!(actual.completed_event, expected.completed_event);
    let restored_dump = pt::export(&restored, None).unwrap();
    assert!(
        restored_dump
            .events
            .iter()
            .all(|event| event.at == event.at.trim())
    );
}

#[test]
fn terminal_gate_and_blocker_timestamps_roundtrip_without_import_fiction() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "receipts", "Receipts", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();
    pt::add_gate(&conn, "agent", task, "proof", "test", "prove it").unwrap();
    pt::resolve_gate(
        &conn,
        "agent",
        task,
        "proof",
        "file:evidence.json",
        None,
        None,
    )
    .unwrap();
    pt::add_blocker(
        &conn,
        "agent",
        task,
        "external receipt",
        "receipt not available",
    )
    .unwrap();
    pt::resolve_blocker(
        &conn,
        "agent",
        task,
        "external receipt",
        "file:receipt.json",
        None,
        Some("received"),
    )
    .unwrap();
    let source = pt::task_context(&conn, task).unwrap();
    let expected_gate_resolved_at = source.gates[0].resolved_at.clone();
    let expected_resolved_at = source.blockers[0].resolved_at.clone();
    let dump = pt::export(&conn, None).unwrap();

    let restored = db();
    pt::import(&restored, "restore", &dump).unwrap();
    let actual = pt::task_context(&restored, task).unwrap();
    assert_eq!(actual.gates[0].resolved_at, expected_gate_resolved_at);
    assert_eq!(actual.blockers[0].resolved_at, expected_resolved_at);
}

#[test]
fn import_refuses_missing_invalid_or_stray_terminal_timestamps_atomically() {
    for (fixture, expected) in [
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task","gates":[{
                    "name":"proof","kind":"test","requirement":"prove it",
                    "status":"resolved","evidence_locator":"file:evidence.json"
                }]}]
            }"#,
            "lacks resolved_at",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task","gates":[{
                    "name":"proof","kind":"test","requirement":"prove it",
                    "resolved_at":"2026-08-04T12:00:00Z"
                }]}]
            }"#,
            "carries completion evidence or resolved_at",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task","blockers":[{
                    "name":"receipt","condition":"missing","status":"resolved",
                    "evidence_locator":"file:receipt.json","resolved_at":"yesterday"
                }]}]
            }"#,
            "invalid resolved_at",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task","blockers":[{
                    "name":"receipt","condition":"missing","status":"waived","note":"not needed"
                }]}]
            }"#,
            "lacks resolved_at",
        ),
    ] {
        let conn = db();
        let dump: pt::Dump = serde_json::from_str(fixture).unwrap();
        let error = pt::import(&conn, "restore", &dump).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        let task_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(task_count, 0, "a refused timestamp import must roll back");
    }
}

#[test]
fn import_refuses_unstable_or_cross_plan_task_event_identity_atomically() {
    for (fixture, expected) in [
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task"}],
                "events":[{
                    "at":"2026-08-04T12:00:00Z",
                    "actor":"fixture",
                    "entity":"task",
                    "entity_seq":1,
                    "kind":"create"
                }]
            }"#,
            "lacks entity_plan",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"},{"slug":"q","title":"Q"}],
                "tasks":[{"seq":1,"plan":"p","title":"task"}],
                "events":[{
                    "at":"2026-08-04T12:00:00Z",
                    "actor":"fixture",
                    "entity":"dep",
                    "entity_seq":1,
                    "entity_plan":"q",
                    "kind":"add"
                }]
            }"#,
            "task #1 belongs to plan 'p'",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v10",
                "plans":[{"slug":"p","title":"P"},{"slug":"q","title":"Q"}],
                "tasks":[{"seq":1,"plan":"p","title":"task"}],
                "events":[{
                    "at":"2026-08-04T12:00:00Z",
                    "actor":"fixture",
                    "entity":"gate",
                    "entity_seq":1,
                    "entity_plan":"q",
                    "gate_name":"removed-gate",
                    "kind":"remove"
                }]
            }"#,
            "task #1 belongs to plan 'p'",
        ),
    ] {
        let conn = db();
        let dump: pt::Dump = serde_json::from_str(fixture).unwrap();
        let error = pt::import(&conn, "restore", &dump).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        let task_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(task_count, 0, "a refused event import must roll back");
    }
}

#[test]
fn import_refuses_invalid_task_status_event_targets_atomically() {
    let dump: pt::Dump = serde_json::from_str(
        r#"{
            "schema":"papertiger.dump.v10",
            "plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":1,"plan":"p","title":"task"}],
            "events":[{
                "at":"2026-08-04T12:00:00Z",
                "actor":"fixture",
                "entity":"task",
                "entity_seq":1,
                "entity_plan":"p",
                "kind":"status",
                "payload":{"to":"queued"}
            }]
        }"#,
    )
    .unwrap();
    let conn = db();
    let error = pt::import(&conn, "restore", &dump).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires payload.to to be one of"),
        "{error:#}"
    );
    let task_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(task_count, 0, "a refused status event must roll back");
}

#[test]
fn import_refuses_dump_external_event_tasks_and_names_sequence_collisions() {
    let destination = db();
    let existing_plan = pt::add_plan(&destination, "test", "existing", "Existing", "").unwrap();
    let existing_task = pt::add_task(
        &destination,
        "test",
        existing_plan,
        pt::TaskCreation::new("existing task"),
    )
    .unwrap();
    assert_eq!(existing_task, 1);

    let external_event: pt::Dump = serde_json::from_str(
        r#"{
            "schema":"papertiger.dump.v10",
            "plans":[{"slug":"incoming","title":"Incoming"}],
            "tasks":[{"seq":2,"plan":"incoming","title":"imported task"}],
            "events":[{
                "at":"2026-08-04T12:00:00Z",
                "actor":"fixture",
                "entity":"task",
                "entity_seq":1,
                "entity_plan":"incoming",
                "kind":"create"
            }]
        }"#,
    )
    .unwrap();
    let error = pt::import(&destination, "restore", &external_event).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("task #1, which is absent from the dump"),
        "{error:#}"
    );
    assert!(
        pt::resolve_plan(&destination, Some("incoming")).is_err(),
        "the refused import must roll back its plan and task"
    );
    assert_eq!(
        pt::get_task(&destination, 1).unwrap().title,
        "existing task"
    );

    let collision: pt::Dump = serde_json::from_str(
        r#"{
            "schema":"papertiger.dump.v10",
            "plans":[{"slug":"incoming","title":"Incoming"}],
            "tasks":[{"seq":1,"plan":"incoming","title":"colliding task"}]
        }"#,
    )
    .unwrap();
    let error = pt::import(&destination, "restore", &collision).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("task seq 1 collides with existing task #1 'existing task'"),
        "{error:#}"
    );
}

#[test]
fn commit_associations_are_exact_evented_reversible_and_transferable() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "commit_associations", "Commits", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("implement")).unwrap();
    let oid = "a".repeat(40);
    let record = pt::add_commit_association(
        &conn,
        "agent",
        task,
        "  crates/widget  ",
        &oid,
        Some("useful snapshot, not completion"),
    )
    .unwrap();
    assert_eq!(record.repository, "crates/widget");
    assert_eq!(
        pt::commit_associations(&conn, task).unwrap(),
        vec![record.clone()]
    );
    let found = pt::find_commit_associations(&conn, &oid, None).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].task_seq, task);
    assert_eq!(found[0].commit, record);
    let scoped = pt::find_commit_associations(&conn, &oid, Some("  crates/widget  ")).unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].commit, record);
    let duplicate = pt::add_commit_association(&conn, "agent", task, "crates/widget", &oid, None)
        .unwrap_err()
        .to_string();
    assert!(duplicate.contains("already recorded"));
    assert!(duplicate.contains("papertiger commit list"));
    let short_oid = pt::add_commit_association(&conn, "agent", task, ".", "abc1234", None)
        .unwrap_err()
        .to_string();
    assert!(
        short_oid.contains("git rev-parse --verify 'HEAD^{commit}'"),
        "{short_oid}"
    );

    let mut dump = pt::export(&conn, None).unwrap();
    dump.tasks[0].commit_associations[0].recorded_at =
        format!("  {}  ", dump.tasks[0].commit_associations[0].recorded_at);
    let restored = db();
    pt::import(&restored, "restore", &dump).unwrap();
    assert_eq!(
        pt::commit_associations(&restored, task).unwrap(),
        vec![record]
    );

    pt::remove_commit_association(
        &restored,
        "agent",
        task,
        "  crates/widget  ",
        &oid,
        "the snapshot included unrelated work",
    )
    .unwrap();
    assert!(pt::commit_associations(&restored, task).unwrap().is_empty());
    let context = pt::task_context(&restored, task).unwrap();
    assert!(
        context
            .recent_events
            .iter()
            .any(|event| event.kind == "commit_association_remove")
    );
}

#[test]
fn lifecycle_activity_follows_event_authority_and_activity_sorting() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "time", "Time", "").unwrap();
    let first = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("first")).unwrap();
    let second = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("second")).unwrap();
    pt::start_task(&conn, "agent", first, Some("begin"), None).unwrap();
    pt::add_note(&conn, "agent", Some(first), "latest evidence", None).unwrap();
    let ordered = pt::list_tasks_by_activity(&conn, plan, None, None).unwrap();
    assert_eq!(ordered[0].seq, first);
    assert_eq!(ordered[1].seq, second);

    // Fix event times and later lose history, as only a pre-v11 authority could.
    conn.execute_batch(
        "DROP TRIGGER events_append_only_update; DROP TRIGGER events_append_only_delete;",
    )
    .unwrap();
    conn.execute(
        "UPDATE events SET at=CASE event_id
           WHEN 2 THEN '2026-08-11T10:00:00Z'
           WHEN 4 THEN '2026-08-11T10:01:00Z'
           WHEN 5 THEN '2026-08-11T10:02:00Z'
           ELSE at END",
        [],
    )
    .unwrap();
    let activity = pt::task_activity(&conn, first).unwrap();
    assert_eq!(
        activity
            .created_event
            .as_ref()
            .map(|event| event.at.as_str()),
        Some("2026-08-11T10:00:00Z")
    );
    assert_eq!(
        activity
            .status_event
            .as_ref()
            .map(|event| event.at.as_str()),
        Some("2026-08-11T10:01:00Z")
    );
    assert_eq!(activity.started_event, activity.status_event);
    assert_eq!(
        activity.last_event.as_ref().map(|event| event.at.as_str()),
        Some("2026-08-11T10:02:00Z")
    );
    assert_eq!(activity.completed_event, None);

    pt::complete_task(&conn, "agent", first, None, None).unwrap();
    let done = pt::task_activity(&conn, first).unwrap();
    assert!(done.completed_event.is_some());
    assert_eq!(done.started_event, None);
    pt::reopen_task(&conn, "agent", first, "more work emerged").unwrap();
    let reopened = pt::task_activity(&conn, first).unwrap();
    assert_eq!(reopened.completed_event, None);
    assert_eq!(reopened.started_event, None);

    conn.execute(
        "DELETE FROM events WHERE entity_seq=?1",
        rusqlite::params![second],
    )
    .unwrap();
    let unknown = pt::task_activity(&conn, second).unwrap();
    assert_eq!(unknown.created_event, None);
    assert_eq!(unknown.last_event, None);
}

#[test]
fn commit_evidence_requires_full_oid_while_audit_finds_noncanonical_values() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "evidence", "Evidence", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();
    pt::add_gate(&conn, "agent", task, "proof", "review", "commit proof").unwrap();
    let error =
        pt::resolve_gate(&conn, "agent", task, "proof", "commit:abc1234", None, None).unwrap_err();
    assert!(error.to_string().contains("full 40- or 64-character"));

    conn.execute(
        "UPDATE gates SET status='resolved', evidence_locator='commit:abc1234', resolved_at=?1",
        rusqlite::params![pt::now()],
    )
    .unwrap();
    assert!(pt::audit(&conn).unwrap().iter().any(|finding| {
        finding.kind == "malformed_evidence_locator" && finding.detail.contains("abc1234")
    }));
}

#[test]
fn audit_reports_corrupt_commit_association_identity_and_timestamp_fields() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "commits", "Commits", "").unwrap();
    let first = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("first")).unwrap();
    let second = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("second")).unwrap();
    let first_oid = "a".repeat(40);
    let second_oid = "b".repeat(40);
    pt::add_commit_association(&conn, "agent", first, "crates/widget", &first_oid, None).unwrap();
    pt::add_commit_association(&conn, "agent", second, ".", &second_oid, None).unwrap();
    conn.execute(
        "UPDATE commit_associations
            SET repository='  crates/widget  ', commit_oid='abc1234', recorded_at='yesterday'
          WHERE task_id=(SELECT task_id FROM tasks WHERE seq=?1)",
        [first],
    )
    .unwrap();
    conn.execute(
        "UPDATE commit_associations SET repository='   '
          WHERE task_id=(SELECT task_id FROM tasks WHERE seq=?1)",
        [second],
    )
    .unwrap();

    let findings = pt::audit(&conn).unwrap();
    let kinds = findings
        .iter()
        .map(|finding| finding.kind.as_str())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"blank_commit_repository"), "{kinds:?}");
    assert!(
        kinds.contains(&"noncanonical_commit_repository"),
        "{kinds:?}"
    );
    assert!(kinds.contains(&"malformed_commit_oid"), "{kinds:?}");
    assert!(kinds.contains(&"invalid_commit_recorded_at"), "{kinds:?}");
}

#[test]
fn audit_reports_invalid_event_time_status_target_and_payload_corruption() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "history", "History", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("task")).unwrap();
    pt::start_task(&conn, "agent", task, Some("begin"), None).unwrap();
    // Model history corrupted before schema v11 refused malformed events.
    conn.execute_batch("DROP TRIGGER events_append_only_update;")
        .unwrap();
    conn.execute(
        "UPDATE events SET at='not-a-time' WHERE entity_seq=?1 AND kind='create'",
        [task],
    )
    .unwrap();
    conn.execute(
        "UPDATE events SET payload=?1 WHERE entity_seq=?2 AND kind='status'",
        rusqlite::params![r#"{"to":"queued"}"#, task],
    )
    .unwrap();

    let findings = pt::audit(&conn).unwrap();
    let kinds = findings
        .iter()
        .map(|finding| finding.kind.as_str())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"invalid_event_timestamp"), "{kinds:?}");
    assert!(kinds.contains(&"invalid_task_status_event"), "{kinds:?}");
    pt::task_context(&conn, task).expect("advisory history findings must not break context reads");

    conn.execute(
        "UPDATE events SET payload='{' WHERE entity_seq=?1 AND kind='status'",
        [task],
    )
    .unwrap();
    let findings = pt::audit(&conn).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == "invalid_event_payload")
    );
    let error = pt::task_context(&conn, task).unwrap_err();
    assert!(error.to_string().contains("invalid stored JSON payload"));
    assert!(error.to_string().contains("papertiger audit"));
}

#[test]
fn retirement_replacements_are_same_plan_evented_and_cycle_safe() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "main", "Main", "").unwrap();
    let other_plan = pt::add_plan(&conn, "test", "other", "Other", "").unwrap();
    let duplicate = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("duplicate")).unwrap();
    let canonical = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("canonical")).unwrap();
    let other = pt::add_task(
        &conn,
        "test",
        other_plan,
        pt::TaskCreation::new("other-plan task"),
    )
    .unwrap();
    let retired_target =
        pt::add_task(&conn, "test", plan, pt::TaskCreation::new("retired target")).unwrap();
    let rejected_target = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation::new("rejected target"),
    )
    .unwrap();
    let final_target =
        pt::add_task(&conn, "test", plan, pt::TaskCreation::new("final target")).unwrap();
    let cycle_target =
        pt::add_task(&conn, "test", plan, pt::TaskCreation::new("cycle target")).unwrap();
    pt::retire_task(&conn, "test", retired_target, None, "no longer canonical").unwrap();
    pt::reject_task(&conn, "test", rejected_target, "disproven approach").unwrap();

    assert!(
        pt::retire_task(&conn, "test", duplicate, Some(duplicate), "same task")
            .unwrap_err()
            .to_string()
            .contains("cannot replace itself")
    );
    assert!(
        pt::retire_task(&conn, "test", duplicate, Some(other), "cross-plan")
            .unwrap_err()
            .to_string()
            .contains("different plans")
    );
    for terminal_target in [retired_target, rejected_target] {
        let error = pt::retire_task(
            &conn,
            "test",
            duplicate,
            Some(terminal_target),
            "not a live canonical target",
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("choose a proposed, in_progress, or done"),
            "{error}"
        );
        assert_eq!(pt::get_task(&conn, duplicate).unwrap().status, "proposed");
    }

    pt::retire_task(
        &conn,
        "test",
        duplicate,
        Some(canonical),
        "the canonical task owns the same outcome",
    )
    .unwrap();
    let retired = pt::get_task(&conn, duplicate).unwrap();
    assert_eq!(retired.status, "retired");
    assert_eq!(
        retired.replacement_task_id,
        Some(pt::get_task(&conn, canonical).unwrap().task_id)
    );
    let context = pt::task_context(&conn, duplicate).unwrap();
    assert_eq!(
        context.replacement.as_ref().map(|task| task.seq),
        Some(canonical)
    );
    let status_event = context
        .recent_events
        .iter()
        .find(|event| event.kind == "status")
        .unwrap();
    assert_eq!(
        status_event.payload.as_ref().unwrap()["replacement_seq"],
        canonical
    );

    for error in [
        pt::reject_task(
            &conn,
            "test",
            canonical,
            "would strand inbound replacement history",
        )
        .unwrap_err()
        .to_string(),
        pt::retire_task(
            &conn,
            "test",
            canonical,
            None,
            "would end the replacement chain",
        )
        .unwrap_err()
        .to_string(),
    ] {
        assert!(error.contains(&format!("canonical replacement for #{duplicate}")));
        assert!(error.contains(&format!("papertiger retire {canonical} --into <task>")));
    }
    pt::retire_task(
        &conn,
        "test",
        canonical,
        Some(final_target),
        "extend the consolidation chain",
    )
    .unwrap();
    let canonical_context = pt::task_context(&conn, canonical).unwrap();
    assert_eq!(canonical_context.task.status, "retired");
    assert_eq!(
        canonical_context.replacement.as_ref().map(|task| task.seq),
        Some(final_target)
    );
    assert!(
        !pt::audit(&conn)
            .unwrap()
            .iter()
            .any(|finding| finding.kind == "replacement_terminal_dead_end")
    );

    pt::reopen_task(&conn, "test", duplicate, "the tasks diverged again").unwrap();
    assert_eq!(
        pt::get_task(&conn, duplicate).unwrap().replacement_task_id,
        None,
        "reopening must clear the retired-only pointer"
    );

    let duplicate_id = pt::get_task(&conn, duplicate).unwrap().task_id;
    conn.execute(
        "UPDATE tasks SET replacement_task_id=?1 WHERE seq=?2",
        [duplicate_id, cycle_target],
    )
    .unwrap();
    let cycle = pt::retire_task(
        &conn,
        "test",
        duplicate,
        Some(cycle_target),
        "would close the loop",
    )
    .unwrap_err();
    assert!(cycle.to_string().contains("replacement would create cycle"));
    assert_eq!(pt::get_task(&conn, duplicate).unwrap().status, "proposed");
}

#[test]
fn replacement_roundtrips_in_plan_and_full_dumps() {
    for plan_scope in [true, false] {
        let conn = db();
        let plan = pt::add_plan(&conn, "test", "main", "Main", "").unwrap();
        let duplicate =
            pt::add_task(&conn, "test", plan, pt::TaskCreation::new("duplicate")).unwrap();
        let canonical =
            pt::add_task(&conn, "test", plan, pt::TaskCreation::new("canonical")).unwrap();
        let final_target = pt::add_task(
            &conn,
            "test",
            plan,
            pt::TaskCreation::new("final canonical task"),
        )
        .unwrap();
        pt::retire_task(
            &conn,
            "test",
            duplicate,
            Some(canonical),
            "one durable task is enough",
        )
        .unwrap();
        pt::retire_task(
            &conn,
            "test",
            canonical,
            Some(final_target),
            "the canonical work was consolidated again",
        )
        .unwrap();

        let dump = pt::export(&conn, plan_scope.then_some("main")).unwrap();
        assert_eq!(dump.schema, "papertiger.dump.v10");
        let duplicate_dump = dump
            .tasks
            .iter()
            .find(|task| task.seq == Some(duplicate))
            .unwrap();
        assert_eq!(duplicate_dump.replacement_seq, Some(canonical));
        let canonical_dump = dump
            .tasks
            .iter()
            .find(|task| task.seq == Some(canonical))
            .unwrap();
        assert_eq!(canonical_dump.replacement_seq, Some(final_target));

        let restored = db();
        pt::import(&restored, "restore", &dump).unwrap();
        let restored_context = pt::task_context(&restored, duplicate).unwrap();
        assert_eq!(
            restored_context.replacement.as_ref().map(|task| task.seq),
            Some(canonical)
        );
        assert_eq!(
            pt::task_context(&restored, canonical)
                .unwrap()
                .replacement
                .map(|task| task.seq),
            Some(final_target)
        );
        let restored_retirement = restored_context
            .recent_events
            .iter()
            .find(|event| {
                event.kind == "status"
                    && event
                        .payload
                        .as_ref()
                        .is_some_and(|payload| payload["replacement_seq"] == canonical)
            })
            .expect("restored retirement event");
        assert_eq!(
            restored_retirement.why.as_deref(),
            Some("one durable task is enough")
        );
    }
}

#[test]
fn import_refuses_invalid_replacement_graphs_atomically() {
    for (fixture, expected) in [
        (
            r#"{"schema":"papertiger.dump.v10","plans":[{"slug":"p","title":"P"}],"tasks":[{"seq":1,"plan":"p","title":"live","replacement_seq":2},{"seq":2,"plan":"p","title":"target"}]}"#,
            "is not retired",
        ),
        (
            r#"{"schema":"papertiger.dump.v10","plans":[{"slug":"p","title":"P"}],"tasks":[{"seq":1,"plan":"p","title":"old","status":"retired","replacement_seq":99}]}"#,
            "missing replacement #99",
        ),
        (
            r#"{"schema":"papertiger.dump.v10","plans":[{"slug":"p","title":"P"}],"tasks":[{"seq":1,"plan":"p","title":"a","status":"retired","replacement_seq":2},{"seq":2,"plan":"p","title":"b","status":"retired","replacement_seq":1}]}"#,
            "replacement cycle",
        ),
        (
            r#"{"schema":"papertiger.dump.v10","plans":[{"slug":"p","title":"P"}],"tasks":[{"seq":1,"plan":"p","title":"old","status":"retired","replacement_seq":2},{"seq":2,"plan":"p","title":"rejected endpoint","status":"rejected"}]}"#,
            "replacement chain terminates",
        ),
        (
            r#"{"schema":"papertiger.dump.v10","plans":[{"slug":"p","title":"P"}],"tasks":[{"seq":1,"plan":"p","title":"old","status":"retired","replacement_seq":2},{"seq":2,"plan":"p","title":"retired endpoint","status":"retired"}]}"#,
            "retired without its own replacement",
        ),
    ] {
        let conn = db();
        let dump: pt::Dump = serde_json::from_str(fixture).unwrap();
        let error = pt::import(&conn, "restore", &dump).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        let task_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(task_count, 0, "a refused graph must roll back atomically");
    }
}

#[test]
fn audit_reports_corrupt_replacement_shapes() {
    let conn = db();
    let first_plan = pt::add_plan(&conn, "test", "first", "First", "").unwrap();
    let second_plan = pt::add_plan(&conn, "test", "second", "Second", "").unwrap();
    let dangling =
        pt::add_task(&conn, "test", first_plan, pt::TaskCreation::new("dangling")).unwrap();
    let live = pt::add_task(&conn, "test", first_plan, pt::TaskCreation::new("live")).unwrap();
    let cross = pt::add_task(&conn, "test", first_plan, pt::TaskCreation::new("cross")).unwrap();
    let cycle_a =
        pt::add_task(&conn, "test", first_plan, pt::TaskCreation::new("cycle a")).unwrap();
    let cycle_b =
        pt::add_task(&conn, "test", first_plan, pt::TaskCreation::new("cycle b")).unwrap();
    let remote = pt::add_task(&conn, "test", second_plan, pt::TaskCreation::new("remote")).unwrap();
    let id = |seq| pt::get_task(&conn, seq).unwrap().task_id;
    conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
    conn.execute(
        "UPDATE tasks SET status='retired', replacement_task_id=999999 WHERE seq=?1",
        [dangling],
    )
    .unwrap();
    conn.execute(
        "UPDATE tasks SET replacement_task_id=?1 WHERE seq=?2",
        [id(remote), live],
    )
    .unwrap();
    conn.execute(
        "UPDATE tasks SET status='retired', replacement_task_id=?1 WHERE seq=?2",
        [id(remote), cross],
    )
    .unwrap();
    conn.execute(
        "UPDATE tasks SET status='retired', replacement_task_id=?1 WHERE seq=?2",
        [id(cycle_b), cycle_a],
    )
    .unwrap();
    conn.execute(
        "UPDATE tasks SET status='retired', replacement_task_id=?1 WHERE seq=?2",
        [id(cycle_a), cycle_b],
    )
    .unwrap();

    let export_error = pt::export(&conn, None)
        .err()
        .expect("dangling replacement must refuse export")
        .to_string();
    assert!(export_error.contains("cannot export"), "{export_error}");
    assert!(export_error.contains("papertiger audit"), "{export_error}");
    assert!(
        export_error.contains(&format!("papertiger reopen {dangling}"))
            && export_error.contains(&format!("papertiger retire {dangling}")),
        "{export_error}"
    );

    let findings = pt::audit(&conn).unwrap();
    let detail_for = |kind: &str, seq: i64| {
        findings
            .iter()
            .find(|finding| finding.kind == kind && finding.detail.starts_with(&format!("#{seq} ")))
            .map(|finding| finding.detail.as_str())
            .unwrap_or_else(|| panic!("missing {kind} finding for #{seq}"))
    };
    let dangling_detail = detail_for("dangling_replacement", dangling);
    assert!(
        dangling_detail.contains(&format!("papertiger reopen {dangling}"))
            && dangling_detail.contains(&format!("papertiger retire {dangling}")),
        "{dangling_detail}"
    );
    let live_detail = detail_for("replacement_on_nonretired_task", live);
    assert!(
        live_detail.contains(&format!("papertiger retire {live}"))
            && !live_detail.contains(&format!("papertiger reopen {live}")),
        "{live_detail}"
    );
    let cross_detail = detail_for("cross_plan_replacement", cross);
    assert!(
        cross_detail.contains(&format!("papertiger reopen {cross}"))
            && cross_detail.contains(&format!("papertiger retire {cross}")),
        "{cross_detail}"
    );
    let cycle_detail = findings
        .iter()
        .find(|finding| finding.kind == "replacement_cycle")
        .map(|finding| finding.detail.as_str())
        .expect("replacement cycle finding");
    assert!(
        cycle_detail.contains("papertiger reopen ") && cycle_detail.contains("papertiger retire "),
        "{cycle_detail}"
    );
}

#[test]
fn audit_and_export_refuse_terminal_replacement_dead_ends() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "terminal", "Terminal", "").unwrap();
    let rejected_source =
        pt::add_task(&conn, "test", plan, pt::TaskCreation::new("source one")).unwrap();
    let rejected_target = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation::new("rejected endpoint"),
    )
    .unwrap();
    let retired_source =
        pt::add_task(&conn, "test", plan, pt::TaskCreation::new("source two")).unwrap();
    let retired_target = pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation::new("retired endpoint"),
    )
    .unwrap();
    pt::retire_task(
        &conn,
        "test",
        rejected_source,
        Some(rejected_target),
        "initially canonical",
    )
    .unwrap();
    pt::retire_task(
        &conn,
        "test",
        retired_source,
        Some(retired_target),
        "initially canonical",
    )
    .unwrap();
    conn.execute(
        "UPDATE tasks SET status='rejected' WHERE seq=?1",
        [rejected_target],
    )
    .unwrap();
    conn.execute(
        "UPDATE tasks SET status='retired' WHERE seq=?1",
        [retired_target],
    )
    .unwrap();

    let findings = pt::audit(&conn).unwrap();
    for (source, target) in [
        (rejected_source, rejected_target),
        (retired_source, retired_target),
    ] {
        let detail = findings
            .iter()
            .find(|finding| {
                finding.kind == "replacement_terminal_dead_end"
                    && finding.detail.starts_with(&format!("#{source} "))
            })
            .map(|finding| finding.detail.as_str())
            .unwrap_or_else(|| panic!("missing terminal finding for #{source}"));
        assert!(
            detail.contains(&format!("papertiger reopen {target}"))
                && detail.contains(&format!("papertiger retire {target} --into <task>")),
            "{detail}"
        );
    }

    let export_error = pt::export(&conn, Some("terminal"))
        .err()
        .expect("terminal replacement endpoint must refuse export")
        .to_string();
    assert!(
        export_error.contains("replacement chain terminates")
            && export_error.contains("papertiger audit"),
        "{export_error}"
    );
}

#[test]
fn schema_v5_requires_explicit_init_before_adding_v6_and_v7_storage() {
    let path = unique_test_path("explicit-v5-through-v7-migration");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    pt::init(&conn).unwrap();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    pt::add_task(&conn, "test", plan, pt::TaskCreation::new("preserved")).unwrap();
    conn.execute_batch(
        "ALTER TABLE tasks DROP COLUMN pickup_at;
         ALTER TABLE tasks DROP COLUMN pickup_session;
         ALTER TABLE tasks DROP COLUMN intent_source;
         ALTER TABLE tasks DROP COLUMN result_source;
         ALTER TABLE tasks DROP COLUMN replacement_task_id;
         DROP VIEW canonical_events;
         DROP TABLE event_quarantines;
         DROP TABLE external_references;
         ALTER TABLE task_blockers RENAME COLUMN condition TO reason; ALTER TABLE gates RENAME COLUMN resolved_at TO closed_at;
         UPDATE meta SET value='5' WHERE key='schema_version';",
    )
    .unwrap();
    drop(conn);

    let refusal = pt::open_existing(path.to_str().unwrap()).unwrap_err();
    assert!(refusal.to_string().contains("run `papertiger"));
    assert!(refusal.to_string().contains("init` explicitly"));

    let conn = pt::open_for_init(path.to_str().unwrap()).unwrap();
    pt::init(&conn).unwrap();
    assert_eq!(pt::get_task(&conn, 1).unwrap().title, "preserved");
    let has_replacement: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('tasks') WHERE name='replacement_task_id')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(has_replacement);
    for column in ["intent_source", "result_source"] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('tasks') WHERE name=?1)",
                [column],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "missing v7 column {column}");
    }
    drop(conn);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn event_cursors_page_history_and_refuse_divergent_authorities() {
    let conn = db();
    let plan = pt::add_plan(&conn, "planner", "history", "History", "").unwrap();
    let first = pt::add_task(&conn, "planner", plan, pt::TaskCreation::new("first")).unwrap();
    pt::add_task(&conn, "planner", plan, pt::TaskCreation::new("second")).unwrap();
    pt::add_note(&conn, "reviewer", Some(first), "fresh evidence", None).unwrap();

    let latest = pt::event_log(&conn, None, 2, None, None).unwrap();
    assert_eq!(latest.schema, "papertiger.event_log.v1");
    assert_eq!(
        latest
            .events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        vec![4, 3]
    );
    assert!(latest.truncated);
    let older_cursor = latest.continuation.unwrap();
    let older = pt::event_log(&conn, None, 10, Some(&older_cursor.token), None).unwrap();
    assert_eq!(
        older
            .events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert!(!older.truncated);

    let after = pt::event_log(
        &conn,
        None,
        10,
        None,
        Some(&pt::event_cursor(&conn, 2).unwrap().token),
    )
    .unwrap();
    assert_eq!(after.direction, "after");
    assert_eq!(
        after
            .events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert_eq!(after.continuation.unwrap().event_id, 4);

    let head = pt::event_cursor(&conn, 4).unwrap();
    let empty_poll = pt::event_log(&conn, None, 10, None, Some(&head.token)).unwrap();
    assert!(empty_poll.events.is_empty());
    assert_eq!(empty_poll.continuation.as_ref().unwrap().token, head.token);
    pt::add_note(&conn, "writer", None, "arrived after the empty poll", None).unwrap();
    let next_poll = pt::event_log(
        &conn,
        None,
        10,
        None,
        Some(&empty_poll.continuation.unwrap().token),
    )
    .unwrap();
    assert_eq!(
        next_poll
            .events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        vec![5]
    );

    let task_only = pt::event_log(&conn, Some(first), 10, None, None).unwrap();
    assert_eq!(task_only.task_seq, Some(first));
    assert!(
        task_only
            .events
            .iter()
            .all(|event| event.task_seq == Some(first))
    );

    let other = db();
    let other_plan = pt::add_plan(&other, "other", "other", "Other", "").unwrap();
    pt::add_task(
        &other,
        "other",
        other_plan,
        pt::TaskCreation::new("different"),
    )
    .unwrap();
    let error = pt::event_log(
        &other,
        None,
        10,
        None,
        Some(&pt::event_cursor(&conn, 2).unwrap().token),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not belong to this history")
    );

    let overflow = format!("event-v1:{}:{}", "9".repeat(40), "0".repeat(64));
    let error = pt::event_log(&conn, None, 10, None, Some(&overflow)).unwrap_err();
    assert!(error.to_string().contains("invalid event cursor"));
}

#[test]
fn task_activity_records_authors_without_creating_session_ownership() {
    let conn = db();
    let plan = pt::add_plan(&conn, "planner", "handoff", "Handoff", "").unwrap();
    let task = pt::add_task(
        &conn,
        "planner",
        plan,
        pt::TaskCreation::new("continue work"),
    )
    .unwrap();
    pt::start_task(
        &conn,
        "ended-session",
        task,
        Some("begin the durable task"),
        None,
    )
    .unwrap();
    pt::add_note(
        &conn,
        "fresh-session",
        Some(task),
        "continued after reading live state",
        None,
    )
    .unwrap();

    let task_record = pt::get_task(&conn, task).unwrap();
    assert_eq!(task_record.status, "in_progress");
    let activity = pt::task_activity(&conn, task).unwrap();
    assert_eq!(
        activity
            .started_event
            .as_ref()
            .map(|event| event.actor.as_str()),
        Some("ended-session")
    );
    assert_eq!(
        activity
            .last_event
            .as_ref()
            .map(|event| event.actor.as_str()),
        Some("fresh-session")
    );
    let json = serde_json::to_value(activity).unwrap();
    assert!(json.get("owner").is_none());
    assert!(json.get("assignee").is_none());
    assert!(json.get("session_id").is_none());
}

#[test]
fn search_is_field_ranked_exact_term_and_includes_terminal_history() {
    let conn = db();
    let primary = pt::add_plan(&conn, "planner", "primary", "Primary", "").unwrap();
    let secondary = pt::add_plan(&conn, "planner", "secondary", "Secondary", "").unwrap();
    let title_hit = pt::add_task(
        &conn,
        "planner",
        primary,
        pt::TaskCreation {
            intent: "repair retained evidence",
            ..pt::TaskCreation::new("Object store recovery")
        },
    )
    .unwrap();
    let intent_hit = pt::add_task(
        &conn,
        "planner",
        primary,
        pt::TaskCreation {
            intent: "repair the object store",
            ..pt::TaskCreation::new("Recovery mechanics")
        },
    )
    .unwrap();
    pt::add_task(
        &conn,
        "planner",
        primary,
        pt::TaskCreation::new("Start activity"),
    )
    .unwrap();
    let rejected = pt::add_task(
        &conn,
        "planner",
        primary,
        pt::TaskCreation::new("Historical checksum report"),
    )
    .unwrap();
    pt::reject_task(
        &conn,
        "reviewer",
        rejected,
        "phantom checksum corruption was disproved",
    )
    .unwrap();
    let other_plan_hit = pt::add_task(
        &conn,
        "planner",
        secondary,
        pt::TaskCreation::new("Object store elsewhere"),
    )
    .unwrap();
    let cross_field_hit = pt::add_task(
        &conn,
        "planner",
        primary,
        pt::TaskCreation {
            intent: "store the recovered bytes",
            ..pt::TaskCreation::new("Restore object")
        },
    )
    .unwrap();

    let ranked = pt::search_tasks(&conn, "object store", Some("primary"), None, 20).unwrap();
    assert_eq!(ranked.terms, vec!["object", "store"]);
    assert_eq!(
        ranked
            .results
            .iter()
            .map(|hit| hit.task.seq)
            .collect::<Vec<_>>(),
        vec![title_hit, intent_hit, cross_field_hit]
    );
    assert_eq!(ranked.results[0].excerpt.field, "title");
    assert_eq!(ranked.plan.as_ref().unwrap().slug, "primary");
    assert!(ranked.results.iter().all(|hit| hit.plan == "primary"));
    assert!(ranked.results[0].score > ranked.results[1].score);
    let cross_field = ranked
        .results
        .iter()
        .find(|hit| hit.task.seq == cross_field_hit)
        .expect("cross-field exact token match");
    assert_eq!(
        cross_field.score, 34,
        "restore must not earn a phrase bonus for store"
    );

    let terminal = pt::search_tasks(&conn, "phantom checksum", None, None, 20).unwrap();
    assert_eq!(terminal.results[0].task.seq, rejected);
    assert_eq!(terminal.results[0].task.status, "rejected");
    assert_eq!(terminal.results[0].plan, "primary");
    assert!(
        terminal.results[0]
            .matched_fields
            .contains(&"rationale".into())
    );
    let filtered = pt::search_tasks(&conn, "phantom checksum", None, Some("done"), 20).unwrap();
    assert!(filtered.results.is_empty());

    let all_plans = pt::search_tasks(&conn, "object store", None, None, 20).unwrap();
    assert!(all_plans.plan.is_none());
    for hit in &all_plans.results {
        let expected = if hit.task.seq == other_plan_hit {
            "secondary"
        } else {
            "primary"
        };
        assert_eq!(hit.plan, expected);
    }
    assert!(
        all_plans
            .results
            .iter()
            .any(|hit| hit.task.seq == other_plan_hit)
    );
    assert!(
        pt::search_tasks(&conn, "art", None, None, 20)
            .unwrap()
            .results
            .is_empty()
    );
    let json = serde_json::to_value(all_plans).unwrap();
    let text = json.to_string();
    assert!(!text.contains("task_id"));
    assert!(!text.contains("plan_id"));
}

#[test]
fn search_refuses_an_unpageable_result_set_with_narrowing_commands() {
    let conn = db();
    let plan = pt::add_plan(&conn, "planner", "many", "Many", "").unwrap();
    for index in 0..=200 {
        pt::add_task(
            &conn,
            "planner",
            plan,
            pt::TaskCreation::new(&format!("common outcome {index}")),
        )
        .unwrap();
    }
    let error = pt::search_tasks(&conn, "common", Some("many"), None, 200).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("matched 201 tasks"), "{message}");
    assert!(message.contains("--plan") && message.contains("--status"));
}

#[test]
fn audit_advises_duplicate_live_titles_and_single_plan_export_keeps_global_events_global() {
    let conn = db();
    let plan = pt::add_plan(&conn, "planner", "work", "Work", "").unwrap();
    for title in ["Same outcome", "same outcome"] {
        pt::add_task(&conn, "planner", plan, pt::TaskCreation::new(title)).unwrap();
    }
    pt::add_note(&conn, "planner", None, "authority-global note", None).unwrap();
    let findings = pt::audit(&conn).unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding.kind == "duplicate_live_title")
    );

    let dump = pt::export(&conn, Some("work")).unwrap();
    let global = dump
        .events
        .iter()
        .find(|event| event.kind == "note" && event.entity_plan.is_none())
        .expect("global note remains global in one-plan export");
    assert!(global.entity_plan.is_none());
    let restored = db();
    pt::import(&restored, "restore", &dump).unwrap();
    let restored_dump = pt::export(&restored, Some("work")).unwrap();
    assert!(
        restored_dump
            .events
            .iter()
            .any(|event| event.kind == "note" && event.entity_plan.is_none())
    );
}

#[test]
fn recovery_export_file_is_atomic_hash_bound_and_refuses_unreviewed_replace() {
    let conn = db();
    let plan = pt::add_plan(&conn, "planner", "recovery", "Recovery", "").unwrap();
    pt::add_task(
        &conn,
        "planner",
        plan,
        pt::TaskCreation::new("preserve this"),
    )
    .unwrap();
    let path = unique_test_path("recovery-export").with_extension("json");
    let dump = pt::export(&conn, None).unwrap();
    let receipt = pt::write_export_file(&path, &dump, false).unwrap();
    let first_bytes = std::fs::read(&path).unwrap();
    assert_eq!(receipt.schema, "papertiger.export_file.v1");
    assert_eq!(receipt.dump_schema, "papertiger.dump.v10");
    assert_eq!(receipt.sha256, pt::sha256(&first_bytes));
    assert_eq!(receipt.bytes, first_bytes.len());
    assert!(first_bytes.ends_with(b"\n"));

    let error = pt::write_export_file(&path, &dump, false).unwrap_err();
    assert!(error.to_string().contains("--replace"));
    assert_eq!(std::fs::read(&path).unwrap(), first_bytes);

    pt::add_note(&conn, "planner", None, "new recovery state", None).unwrap();
    let updated = pt::export(&conn, None).unwrap();
    let updated_receipt = pt::write_export_file(&path, &updated, true).unwrap();
    assert_ne!(updated_receipt.sha256, receipt.sha256);
    assert_eq!(
        updated_receipt.sha256,
        pt::sha256(&std::fs::read(&path).unwrap())
    );

    let directory = unique_test_path("recovery-export-directory");
    std::fs::create_dir(&directory).unwrap();
    let error = pt::write_export_file(&directory, &updated, true).unwrap_err();
    assert!(error.to_string().contains("not a regular file"));
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
