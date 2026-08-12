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

#[test]
fn open_existing_refuses_missing_database_without_creating_it() {
    let path = unique_test_path("missing-library-open");
    assert!(!path.exists());

    let err = pt::open_existing(path.to_str().unwrap()).unwrap_err();

    assert!(
        err.to_string()
            .contains("open existing papertiger database")
    );
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("open existing papertiger database"));
    assert!(
        !path.exists(),
        "failed command must not create the database"
    );
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
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("is not an initialized papertiger database")
    );
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
    let seq = pt::add_task(
        &conn,
        "test",
        plan,
        "build thing",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    assert_eq!(seq, 1);
    pt::add_gate(&conn, "test", seq, "smoke", "test", "smoke test passes").unwrap();
    pt::start_task(&conn, "test", seq, None).unwrap();
    // done refused while gate open
    let err = pt::complete_task(&conn, "test", seq, None).unwrap_err();
    assert!(err.to_string().contains("open gate"));
    // bad locator shape refused
    let err = pt::close_gate(&conn, "test", seq, "smoke", "no-scheme", None, None).unwrap_err();
    assert!(err.to_string().contains("scheme:value"));
    pt::close_gate(
        &conn,
        "test",
        seq,
        "smoke",
        "file:evidence/x.json",
        Some(&"ab".repeat(32)),
        None,
    )
    .unwrap();
    pt::complete_task(&conn, "test", seq, None).unwrap();
    assert_eq!(pt::get_task(&conn, seq).unwrap().status, "done");
}

#[test]
fn waive_requires_why_via_retire_reject_paths() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let seq = pt::add_task(&conn, "test", plan, "t", "", None, &[], &[], 0, None).unwrap();
    assert!(pt::retire_task(&conn, "test", seq, None, "").is_err());
    assert!(pt::reject_task(&conn, "test", seq, "").is_err());
    pt::reject_task(&conn, "test", seq, "approach disproven").unwrap();
}

#[test]
fn dependency_cycles_rejected_and_readiness_derived() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let a = pt::add_task(&conn, "test", plan, "a", "", None, &[], &[], 0, None).unwrap();
    let b = pt::add_task(&conn, "test", plan, "b", "", None, &[a], &[], 0, None).unwrap();
    let c = pt::add_task(&conn, "test", plan, "c", "", None, &[b], &[], 5, None).unwrap();
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
    pt::complete_task(&conn, "test", a, None).unwrap();
    let ready = pt::ready_tasks(&conn, plan, 10, false).unwrap();
    assert_eq!(
        ready.iter().map(|e| e.task.seq).collect::<Vec<_>>(),
        vec![b]
    );
}

#[test]
fn priority_orders_ready_queue() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let low = pt::add_task(&conn, "test", plan, "low", "", None, &[], &[], 0, None).unwrap();
    let high = pt::add_task(&conn, "test", plan, "high", "", None, &[], &[], 9, None).unwrap();
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
    let blocker =
        pt::add_task(&conn, "test", plan, "blocker", "", None, &[], &[], 0, None).unwrap();
    for title in ["ready-a", "ready-b"] {
        pt::add_task(&conn, "test", plan, title, "", None, &[], &[], 1, None).unwrap();
    }
    for title in ["blocked-a", "blocked-b"] {
        pt::add_task(
            &conn,
            "test",
            plan,
            title,
            "",
            None,
            &[blocker],
            &[],
            0,
            None,
        )
        .unwrap();
    }
    pt::start_task(&conn, "test", blocker, None).unwrap();

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
    let dead = pt::add_task(&conn, "test", plan, "dead", "", None, &[], &[], 0, None).unwrap();
    let live = pt::add_task(&conn, "test", plan, "live", "", None, &[dead], &[], 0, None).unwrap();
    pt::reject_task(&conn, "test", dead, "nope").unwrap();
    let ready = pt::ready_tasks(&conn, plan, 10, false).unwrap();
    assert!(
        ready.iter().all(|entry| entry.task.seq != live),
        "a rejected prerequisite must keep its consumer blocked"
    );
    let parent = pt::add_task(
        &conn,
        "test",
        plan,
        "milestone",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let child = pt::add_task(
        &conn,
        "test",
        plan,
        "child",
        "",
        Some(parent),
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    pt::complete_task(&conn, "test", child, None).unwrap();
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
        "a",
        "intent a",
        None,
        &[],
        &["track:x".into()],
        2,
        Some("roundtrip fixture"),
    )
    .unwrap();
    let b = pt::add_task(&conn, "test", plan, "b", "", None, &[a], &[], 0, None).unwrap();
    pt::add_gate(&conn, "test", a, "smoke", "test", "passes").unwrap();
    pt::close_gate(&conn, "test", a, "smoke", "file:e.json", None, None).unwrap();
    pt::complete_task(&conn, "test", a, None).unwrap();
    let dump = pt::export(&conn, None).unwrap();
    let json = serde_json::to_string(&dump).unwrap();

    let mut conn2 = db();
    let dump2: pt::Dump = serde_json::from_str(&json).unwrap();
    let (tasks, deps) = pt::import(&mut conn2, "test", &dump2).unwrap();
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
        "\u{feff}{\"schema\":\"papertiger.dump.v6\",\"plans\":[],\"tasks\":[]}",
    )
    .unwrap();
    assert_eq!(dump.schema, "papertiger.dump.v6");
}

#[test]
fn import_refuses_closed_gate_without_evidence() {
    let mut conn = db();
    let dump: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v6",
            "plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":1,"plan":"p","title":"t","status":"done",
                      "gates":[{"name":"g","kind":"test","requirement":"r","status":"closed"}]}]}"#,
    )
    .unwrap();
    let err = pt::import(&mut conn, "test", &dump).unwrap_err();
    assert!(err.to_string().contains("lacks evidence_locator"));
}

#[test]
fn import_refuses_superseded_dump_with_a_complete_recovery_path() {
    let mut conn = db();
    let dump: pt::Dump =
        serde_json::from_str(r#"{"schema":"papertiger.dump.v5","plans":[],"tasks":[]}"#).unwrap();
    let error = pt::import(&mut conn, "test", &dump)
        .unwrap_err()
        .to_string();
    assert!(error.contains("release that produced it"));
    assert!(error.contains("papertiger --db <temporary-authority> init"));
    assert!(error.contains("re-export `papertiger.dump.v6`"));
}

#[test]
fn container_tasks_never_enter_ready_queue() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let parent = pt::add_task(
        &conn,
        "test",
        plan,
        "milestone",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let child = pt::add_task(
        &conn,
        "test",
        plan,
        "child",
        "",
        Some(parent),
        &[],
        &[],
        0,
        None,
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
    let existing =
        pt::add_task(&conn, "test", plan, "existing", "", None, &[], &[], 0, None).unwrap();
    let before_events: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    let error = pt::add_task(
        &conn,
        "test",
        plan,
        "must roll back",
        "",
        None,
        &[existing, 999],
        &[],
        0,
        None,
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
    let mut conn = db();
    let cycle: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v6","plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":1,"plan":"p","title":"a","deps":[2]},
                     {"seq":2,"plan":"p","title":"b","deps":[1]}]}"#,
    )
    .unwrap();
    assert!(pt::import(&mut conn, "test", &cycle).is_err());

    let missing_parent: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v6","plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":3,"plan":"p","title":"child","parent_seq":999}]}"#,
    )
    .unwrap();
    assert!(pt::import(&mut conn, "test", &missing_parent).is_err());

    let done_open: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v6","plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":4,"plan":"p","title":"false done","status":"done",
                      "gates":[{"name":"g","kind":"test","requirement":"r"}]}]}"#,
    )
    .unwrap();
    assert!(pt::import(&mut conn, "test", &done_open).is_err());

    let tasks: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tasks, 0, "every failed import must roll back completely");
}

#[test]
fn import_allocates_omitted_sequences_before_linking() {
    let mut conn = db();
    let dump: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v6","plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":7,"plan":"p","title":"parent"},
                     {"plan":"p","title":"child","parent_seq":7,"deps":[7]}]}"#,
    )
    .unwrap();
    pt::import(&mut conn, "test", &dump).unwrap();
    let child = pt::get_task(&conn, 8).unwrap();
    assert!(child.parent_id.is_some());
    assert_eq!(pt::open_deps(&conn, child.task_id).unwrap(), vec![7]);
}

#[test]
fn evidence_and_waiver_reasons_are_durable() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, "task", "", None, &[], &[], 0, None).unwrap();
    pt::add_gate(&conn, "test", task, "g", "test", "r").unwrap();
    let bad = pt::close_gate(
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
    pt::add_note(&conn, "test", Some(task), "cold-readable handoff note").unwrap();
    let dump = pt::export(&conn, None).unwrap();
    assert!(!dump.events.is_empty());

    let mut restored = db();
    pt::import(&mut restored, "restore", &dump).unwrap();
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
        "container",
        "",
        None,
        &[],
        &[],
        100,
        None,
    )
    .unwrap();
    let active_leaf = pt::add_task(
        &conn,
        "test",
        plan,
        "active leaf",
        "",
        Some(container),
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let prerequisite = pt::add_task(
        &conn,
        "test",
        plan,
        "prerequisite",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let independent = pt::add_task(
        &conn,
        "test",
        plan,
        "independent",
        "",
        None,
        &[],
        &[],
        50,
        None,
    )
    .unwrap();
    let dependent = pt::add_task(
        &conn,
        "test",
        plan,
        "dependent",
        "",
        None,
        &[prerequisite],
        &[],
        0,
        None,
    )
    .unwrap();
    let downstream = pt::add_task(
        &conn,
        "test",
        plan,
        "downstream",
        "",
        None,
        &[dependent],
        &[],
        0,
        None,
    )
    .unwrap();
    pt::start_task(&conn, "test", container, None).unwrap();
    pt::start_task(&conn, "test", active_leaf, None).unwrap();

    let focus = pt::focus(&conn, plan, 20, true).unwrap();
    let sequences = focus.iter().map(|entry| entry.task.seq).collect::<Vec<_>>();
    assert!(!sequences.contains(&container));
    assert_eq!(sequences[0], active_leaf);
    assert!(
        sequences.iter().position(|seq| *seq == independent)
            < sequences.iter().position(|seq| *seq == prerequisite),
        "explicit operator priority must outrank inferred graph breadth"
    );
    let prerequisite_entry = focus
        .iter()
        .find(|entry| entry.task.seq == prerequisite)
        .unwrap();
    assert_eq!(prerequisite_entry.immediate_unlock_count, 1);
    assert_eq!(prerequisite_entry.unfinished_downstream_count, 2);
    let dependent_entry = focus
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
    let task = pt::add_task(&conn, "test", plan, "task", "", None, &[], &[], 0, None).unwrap();
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
    assert!(pt::remove_open_gate(&conn, "test", task, "wrong gate", "").is_err());
    pt::remove_open_gate(
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
        "collect evidence",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let decision = pt::add_task_with_kind(
        &conn,
        "test",
        plan,
        "select design",
        "",
        "decision",
        None,
        &[prerequisite],
        &[],
        5,
        None,
    )
    .unwrap();
    pt::add_task_blocker(
        &conn,
        "test",
        decision,
        "operator input",
        "the owner must choose the compatibility boundary",
    )
    .unwrap();

    let err = pt::start_task(&conn, "test", decision, None).unwrap_err();
    assert!(err.to_string().contains(&format!("dep:#{prerequisite}")));
    assert!(err.to_string().contains("blocker:operator input"));
    let default_focus = pt::focus(&conn, plan, 20, false).unwrap();
    assert!(!default_focus.iter().any(|entry| entry.task.seq == decision));
    let full_focus = pt::focus(&conn, plan, 20, true).unwrap();
    let entry = full_focus
        .iter()
        .find(|entry| entry.task.seq == decision)
        .unwrap();
    assert_eq!(entry.readiness, "blocked");

    pt::complete_task(&conn, "test", prerequisite, None).unwrap();
    assert!(pt::start_task(&conn, "test", decision, None).is_err());
    pt::resolve_task_blocker(
        &conn,
        "test",
        decision,
        "operator input",
        "project-db:decision/compatibility-boundary",
        None,
        Some("owner selected the bounded interface"),
    )
    .unwrap();
    pt::start_task(&conn, "test", decision, None).unwrap();
    assert!(pt::complete_task(&conn, "test", decision, None).is_err());
    pt::complete_task(
        &conn,
        "test",
        decision,
        Some("Use a bounded interface and reject implicit compatibility."),
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
    let task = pt::add_task(
        &conn,
        "test",
        plan,
        "implement",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    pt::start_task(&conn, "test", task, None).unwrap();
    pt::add_task_blocker(
        &conn,
        "test",
        task,
        "missing fixture",
        "the required fixture has not been supplied",
    )
    .unwrap();

    let focus = pt::focus(&conn, plan, 10, false).unwrap();
    assert_eq!(focus[0].task.seq, task);
    assert_eq!(focus[0].readiness, "in_progress_blocked");
    assert!(
        !pt::audit(&conn)
            .unwrap()
            .iter()
            .any(|finding| finding.kind == "done_with_open_blocker")
    );

    pt::waive_task_blocker(
        &conn,
        "test",
        task,
        "missing fixture",
        "the task was narrowed so the fixture is no longer applicable",
    )
    .unwrap();
    pt::complete_task(&conn, "test", task, None).unwrap();
    assert!(pt::audit(&conn).unwrap().is_empty());
}

#[test]
fn reopening_preserves_valid_gate_evidence_until_the_gate_is_explicitly_reopened() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "reopen", "Reopen", "").unwrap();
    let task = pt::add_task_with_kind(
        &conn,
        "test",
        plan,
        "measure behavior",
        "",
        "probe",
        None,
        &[],
        &[],
        0,
        None,
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
    pt::close_gate(
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
    assert_eq!(context.gates[0].status, "closed");
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
    assert!(pt::complete_task(&conn, "test", task, Some("premature")).is_err());
}

#[test]
fn parent_dependency_and_plan_transitions_preserve_terminal_truth() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "truth", "Truth", "").unwrap();
    let parent = pt::add_task(&conn, "test", plan, "outcome", "", None, &[], &[], 0, None).unwrap();
    let child = pt::add_task(
        &conn,
        "test",
        plan,
        "deliverable",
        "",
        Some(parent),
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    pt::complete_task(&conn, "test", child, None).unwrap();
    pt::complete_task(&conn, "test", parent, None).unwrap();
    assert!(
        pt::add_task(
            &conn,
            "test",
            plan,
            "late child",
            "",
            Some(parent),
            &[],
            &[],
            0,
            None,
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

    let prerequisite = pt::add_task(
        &conn,
        "test",
        plan,
        "prerequisite",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let dependent = pt::add_task(
        &conn,
        "test",
        plan,
        "dependent",
        "",
        None,
        &[prerequisite],
        &[],
        0,
        None,
    )
    .unwrap();
    pt::complete_task(&conn, "test", prerequisite, None).unwrap();
    pt::complete_task(&conn, "test", dependent, None).unwrap();
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
    assert!(pt::start_task(&conn, "test", prerequisite, None).is_err());
    pt::set_plan_status(&conn, "test", "truth", "active", "resume execution").unwrap();
    pt::start_task(&conn, "test", prerequisite, None).unwrap();
}

#[test]
fn current_export_import_preserves_task_kind_result_blocker_and_mise_evidence() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "roundtrip-v2", "Roundtrip v2", "").unwrap();
    let task = pt::add_task_with_kind(
        &conn,
        "test",
        plan,
        "choose engine",
        "Test the two viable engines and select one.",
        "decision",
        None,
        &[],
        &["architecture".into()],
        7,
        Some("decision fixture"),
    )
    .unwrap();
    pt::add_task_blocker(
        &conn,
        "test",
        task,
        "benchmark data",
        "the benchmark has not completed",
    )
    .unwrap();
    pt::resolve_task_blocker(
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
    )
    .unwrap();

    let dump = pt::export(&conn, Some("roundtrip-v2")).unwrap();
    assert_eq!(dump.schema, "papertiger.dump.v6");
    let mut restored = db();
    pt::import(&mut restored, "test", &dump).unwrap();
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

fn mise_projection_fixture() -> pt::MisePlannerProjection {
    use pt::{
        MISE_PLANNER_PROJECTION_SCHEMA_V1, MiseBudgetProjection, MiseMutationProjection,
        MisePlannerProjection, MiseProjectionDisposition, MiseSourceProjection, sha256,
    };

    let material = r#"{"schema":"papertiger-mise.candidate-material.v1","kind":"git_change_set","protocol":"papertiger-mise.git-change-set.v1","media_type":"application/vnd.papertiger-mise.git-change-set+json","payload_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","scope":{"changed_paths":["src/lib.rs"],"operations":["modify"]},"change_set":{"schema":"papertiger-mise.git-change-set.v1","changes":[]}}"#;
    MisePlannerProjection {
        schema: MISE_PLANNER_PROJECTION_SCHEMA_V1.to_owned(),
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
    let task = pt::add_task(
        &conn,
        "test",
        plan,
        "own evidence",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let other = pt::add_task(
        &conn,
        "test",
        plan,
        "unrelated",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
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
    let mut restored = db();
    pt::import(&mut restored, "test", &dump).unwrap();
    let restored_record = pt::mise_projection(&restored, &record.projection_sha256)
        .unwrap()
        .unwrap();
    assert_eq!(restored_record.task_seq, task);
    assert!(pt::audit(&restored).unwrap().is_empty());
}

#[test]
fn task_context_includes_gate_waiver_and_reopen_history() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "gate-history", "Gate history", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, "task", "", None, &[], &[], 0, None).unwrap();
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
    let task = pt::add_task(&conn, "test", plan, "task", "", None, &[], &[], 0, None).unwrap();
    for index in 0..12 {
        pt::add_note(&conn, "test", Some(task), &format!("note {index}"))
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
        "parent",
        "",
        None,
        &[],
        &["selected".to_owned()],
        10,
        None,
    )
    .unwrap();
    let child = pt::add_task(
        &conn,
        "test",
        plan,
        "child",
        "",
        Some(parent),
        &[],
        &["selected".to_owned()],
        20,
        None,
    )
    .unwrap();
    pt::start_task(&conn, "test", parent, None).unwrap();
    pt::start_task(&conn, "test", child, None).unwrap();

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
    let task = pt::add_task(&conn, "test", plan, "task", "", None, &[], &[], 0, None).unwrap();
    pt::add_gate(&conn, "test", task, "mistake", "review", "wrong gate").unwrap();
    pt::remove_open_gate(&conn, "test", task, "mistake", "attached by mistake").unwrap();

    let dump = pt::export(&conn, Some("removed-gate")).unwrap();
    let create = dump
        .events
        .iter()
        .find(|event| event.entity == "gate" && event.kind == "create")
        .expect("removed gate create event must remain exportable");
    assert_eq!(create.entity_plan.as_deref(), Some("removed-gate"));
    assert_eq!(create.entity_seq, Some(task));
    assert_eq!(create.gate_name.as_deref(), Some("mistake"));

    let mut restored = db();
    pt::import(&mut restored, "restore", &dump).unwrap();
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
    dump.events[0].at = "é".into();

    let mut restored = db();
    let error = pt::import(&mut restored, "restore", &dump).unwrap_err();
    assert!(error.to_string().contains("not valid RFC3339"), "{error:#}");
}

#[test]
fn import_canonicalizes_event_timestamps_and_preserves_lifecycle_history() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "timestamps", "Timestamps", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, "task", "", None, &[], &[], 0, None).unwrap();
    pt::start_task(&conn, "agent", task, Some("begin")).unwrap();
    pt::complete_task(&conn, "agent", task, None).unwrap();
    let expected = pt::task_activity(&conn, task).unwrap();
    let mut dump = pt::export(&conn, None).unwrap();
    for event in &mut dump.events {
        event.at = format!("  {}  ", event.at);
    }

    let mut restored = db();
    pt::import(&mut restored, "restore", &dump).unwrap();
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
    let task = pt::add_task(&conn, "test", plan, "task", "", None, &[], &[], 0, None).unwrap();
    pt::add_gate(&conn, "agent", task, "proof", "test", "prove it").unwrap();
    pt::close_gate(
        &conn,
        "agent",
        task,
        "proof",
        "file:evidence.json",
        None,
        None,
    )
    .unwrap();
    pt::add_task_blocker(
        &conn,
        "agent",
        task,
        "external receipt",
        "receipt not available",
    )
    .unwrap();
    pt::resolve_task_blocker(
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
    let expected_closed_at = source.gates[0].closed_at.clone();
    let expected_resolved_at = source.blockers[0].resolved_at.clone();
    let dump = pt::export(&conn, None).unwrap();

    let mut restored = db();
    pt::import(&mut restored, "restore", &dump).unwrap();
    let actual = pt::task_context(&restored, task).unwrap();
    assert_eq!(actual.gates[0].closed_at, expected_closed_at);
    assert_eq!(actual.blockers[0].resolved_at, expected_resolved_at);
}

#[test]
fn import_refuses_missing_invalid_or_stray_terminal_timestamps_atomically() {
    for (fixture, expected) in [
        (
            r#"{
                "schema":"papertiger.dump.v6",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task","gates":[{
                    "name":"proof","kind":"test","requirement":"prove it",
                    "status":"closed","evidence_locator":"file:evidence.json"
                }]}]
            }"#,
            "lacks closed_at",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v6",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task","gates":[{
                    "name":"proof","kind":"test","requirement":"prove it",
                    "closed_at":"2026-08-04T12:00:00Z"
                }]}]
            }"#,
            "carries completion evidence or closed_at",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v6",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task","blockers":[{
                    "name":"receipt","reason":"missing","status":"resolved",
                    "evidence_locator":"file:receipt.json","resolved_at":"yesterday"
                }]}]
            }"#,
            "invalid resolved_at",
        ),
        (
            r#"{
                "schema":"papertiger.dump.v6",
                "plans":[{"slug":"p","title":"P"}],
                "tasks":[{"seq":1,"plan":"p","title":"task","blockers":[{
                    "name":"receipt","reason":"missing","status":"waived","note":"not needed"
                }]}]
            }"#,
            "lacks resolved_at",
        ),
    ] {
        let mut conn = db();
        let dump: pt::Dump = serde_json::from_str(fixture).unwrap();
        let error = pt::import(&mut conn, "restore", &dump).unwrap_err();
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
                "schema":"papertiger.dump.v6",
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
                "schema":"papertiger.dump.v6",
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
                "schema":"papertiger.dump.v6",
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
        let mut conn = db();
        let dump: pt::Dump = serde_json::from_str(fixture).unwrap();
        let error = pt::import(&mut conn, "restore", &dump).unwrap_err();
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
            "schema":"papertiger.dump.v6",
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
    let mut conn = db();
    let error = pt::import(&mut conn, "restore", &dump).unwrap_err();
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
    let mut destination = db();
    let existing_plan = pt::add_plan(&destination, "test", "existing", "Existing", "").unwrap();
    let existing_task = pt::add_task(
        &destination,
        "test",
        existing_plan,
        "existing task",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    assert_eq!(existing_task, 1);

    let external_event: pt::Dump = serde_json::from_str(
        r#"{
            "schema":"papertiger.dump.v6",
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
    let error = pt::import(&mut destination, "restore", &external_event).unwrap_err();
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
            "schema":"papertiger.dump.v6",
            "plans":[{"slug":"incoming","title":"Incoming"}],
            "tasks":[{"seq":1,"plan":"incoming","title":"colliding task"}]
        }"#,
    )
    .unwrap();
    let error = pt::import(&mut destination, "restore", &collision).unwrap_err();
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
    let task = pt::add_task(
        &conn,
        "test",
        plan,
        "implement",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
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
    let mut restored = db();
    pt::import(&mut restored, "restore", &dump).unwrap();
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
    let first = pt::add_task(&conn, "test", plan, "first", "", None, &[], &[], 0, None).unwrap();
    let second = pt::add_task(&conn, "test", plan, "second", "", None, &[], &[], 0, None).unwrap();
    pt::start_task(&conn, "agent", first, Some("begin")).unwrap();
    pt::add_note(&conn, "agent", Some(first), "latest evidence").unwrap();
    let ordered = pt::list_tasks_by_activity(&conn, plan, None, None).unwrap();
    assert_eq!(ordered[0].seq, first);
    assert_eq!(ordered[1].seq, second);

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

    pt::complete_task(&conn, "agent", first, None).unwrap();
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
    let task = pt::add_task(&conn, "test", plan, "task", "", None, &[], &[], 0, None).unwrap();
    pt::add_gate(&conn, "agent", task, "proof", "review", "commit proof").unwrap();
    let error =
        pt::close_gate(&conn, "agent", task, "proof", "commit:abc1234", None, None).unwrap_err();
    assert!(error.to_string().contains("full 40- or 64-character"));

    conn.execute(
        "UPDATE gates SET status='closed', evidence_locator='commit:abc1234', closed_at=?1",
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
    let first = pt::add_task(&conn, "test", plan, "first", "", None, &[], &[], 0, None).unwrap();
    let second = pt::add_task(&conn, "test", plan, "second", "", None, &[], &[], 0, None).unwrap();
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
fn audit_reports_invalid_event_time_and_status_target_without_breaking_context_reads() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "history", "History", "").unwrap();
    let task = pt::add_task(&conn, "test", plan, "task", "", None, &[], &[], 0, None).unwrap();
    pt::start_task(&conn, "agent", task, Some("begin")).unwrap();
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
}

#[test]
fn retirement_replacements_are_same_plan_evented_and_cycle_safe() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "main", "Main", "").unwrap();
    let other_plan = pt::add_plan(&conn, "test", "other", "Other", "").unwrap();
    let duplicate = pt::add_task(
        &conn,
        "test",
        plan,
        "duplicate",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let canonical = pt::add_task(
        &conn,
        "test",
        plan,
        "canonical",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let other = pt::add_task(
        &conn,
        "test",
        other_plan,
        "other-plan task",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let retired_target = pt::add_task(
        &conn,
        "test",
        plan,
        "retired target",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let rejected_target = pt::add_task(
        &conn,
        "test",
        plan,
        "rejected target",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let final_target = pt::add_task(
        &conn,
        "test",
        plan,
        "final target",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let cycle_target = pt::add_task(
        &conn,
        "test",
        plan,
        "cycle target",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
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
        let duplicate = pt::add_task(
            &conn,
            "test",
            plan,
            "duplicate",
            "",
            None,
            &[],
            &[],
            0,
            None,
        )
        .unwrap();
        let canonical = pt::add_task(
            &conn,
            "test",
            plan,
            "canonical",
            "",
            None,
            &[],
            &[],
            0,
            None,
        )
        .unwrap();
        let final_target = pt::add_task(
            &conn,
            "test",
            plan,
            "final canonical task",
            "",
            None,
            &[],
            &[],
            0,
            None,
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
        assert_eq!(dump.schema, "papertiger.dump.v6");
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

        let mut restored = db();
        pt::import(&mut restored, "restore", &dump).unwrap();
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
            r#"{"schema":"papertiger.dump.v6","plans":[{"slug":"p","title":"P"}],"tasks":[{"seq":1,"plan":"p","title":"live","replacement_seq":2},{"seq":2,"plan":"p","title":"target"}]}"#,
            "is not retired",
        ),
        (
            r#"{"schema":"papertiger.dump.v6","plans":[{"slug":"p","title":"P"}],"tasks":[{"seq":1,"plan":"p","title":"old","status":"retired","replacement_seq":99}]}"#,
            "missing replacement #99",
        ),
        (
            r#"{"schema":"papertiger.dump.v6","plans":[{"slug":"p","title":"P"}],"tasks":[{"seq":1,"plan":"p","title":"a","status":"retired","replacement_seq":2},{"seq":2,"plan":"p","title":"b","status":"retired","replacement_seq":1}]}"#,
            "replacement cycle",
        ),
        (
            r#"{"schema":"papertiger.dump.v6","plans":[{"slug":"p","title":"P"}],"tasks":[{"seq":1,"plan":"p","title":"old","status":"retired","replacement_seq":2},{"seq":2,"plan":"p","title":"rejected endpoint","status":"rejected"}]}"#,
            "replacement chain terminates",
        ),
        (
            r#"{"schema":"papertiger.dump.v6","plans":[{"slug":"p","title":"P"}],"tasks":[{"seq":1,"plan":"p","title":"old","status":"retired","replacement_seq":2},{"seq":2,"plan":"p","title":"retired endpoint","status":"retired"}]}"#,
            "retired without its own replacement",
        ),
    ] {
        let mut conn = db();
        let dump: pt::Dump = serde_json::from_str(fixture).unwrap();
        let error = pt::import(&mut conn, "restore", &dump).unwrap_err();
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
    let dangling = pt::add_task(
        &conn,
        "test",
        first_plan,
        "dangling",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let live = pt::add_task(
        &conn,
        "test",
        first_plan,
        "live",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let cross = pt::add_task(
        &conn,
        "test",
        first_plan,
        "cross",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let cycle_a = pt::add_task(
        &conn,
        "test",
        first_plan,
        "cycle a",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let cycle_b = pt::add_task(
        &conn,
        "test",
        first_plan,
        "cycle b",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let remote = pt::add_task(
        &conn,
        "test",
        second_plan,
        "remote",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
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
    let rejected_source = pt::add_task(
        &conn,
        "test",
        plan,
        "source one",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let rejected_target = pt::add_task(
        &conn,
        "test",
        plan,
        "rejected endpoint",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let retired_source = pt::add_task(
        &conn,
        "test",
        plan,
        "source two",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let retired_target = pt::add_task(
        &conn,
        "test",
        plan,
        "retired endpoint",
        "",
        None,
        &[],
        &[],
        0,
        None,
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
fn schema_v5_requires_explicit_init_before_adding_replacement_storage() {
    let path = unique_test_path("explicit-v5-replacement-migration");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    pt::init(&conn).unwrap();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    pt::add_task(
        &conn,
        "test",
        plan,
        "preserved",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    conn.execute_batch(
        "ALTER TABLE tasks DROP COLUMN replacement_task_id;
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
    drop(conn);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn event_cursors_page_history_and_refuse_divergent_authorities() {
    let conn = db();
    let plan = pt::add_plan(&conn, "planner", "history", "History", "").unwrap();
    let first = pt::add_task(&conn, "planner", plan, "first", "", None, &[], &[], 0, None).unwrap();
    pt::add_task(
        &conn,
        "planner",
        plan,
        "second",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    pt::add_note(&conn, "reviewer", Some(first), "fresh evidence").unwrap();

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
        "different",
        "",
        None,
        &[],
        &[],
        0,
        None,
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
        "continue work",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    pt::start_task(&conn, "ended-session", task, Some("begin the durable task")).unwrap();
    pt::add_note(
        &conn,
        "fresh-session",
        Some(task),
        "continued after reading live state",
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
        "Object store recovery",
        "repair retained evidence",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let intent_hit = pt::add_task(
        &conn,
        "planner",
        primary,
        "Recovery mechanics",
        "repair the object store",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    pt::add_task(
        &conn,
        "planner",
        primary,
        "Start activity",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let rejected = pt::add_task(
        &conn,
        "planner",
        primary,
        "Historical checksum report",
        "",
        None,
        &[],
        &[],
        0,
        None,
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
        "Object store elsewhere",
        "",
        None,
        &[],
        &[],
        0,
        None,
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
        vec![title_hit, intent_hit]
    );
    assert_eq!(ranked.results[0].excerpt.field, "title");
    assert!(ranked.results[0].score > ranked.results[1].score);

    let terminal = pt::search_tasks(&conn, "phantom checksum", None, None, 20).unwrap();
    assert_eq!(terminal.results[0].task.seq, rejected);
    assert_eq!(terminal.results[0].task.status, "rejected");
    assert!(
        terminal.results[0]
            .matched_fields
            .contains(&"rationale".into())
    );
    let filtered = pt::search_tasks(&conn, "phantom checksum", None, Some("done"), 20).unwrap();
    assert!(filtered.results.is_empty());

    let all_plans = pt::search_tasks(&conn, "object store", None, None, 20).unwrap();
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
fn recovery_export_file_is_atomic_hash_bound_and_refuses_unreviewed_replace() {
    let conn = db();
    let plan = pt::add_plan(&conn, "planner", "recovery", "Recovery", "").unwrap();
    pt::add_task(
        &conn,
        "planner",
        plan,
        "preserve this",
        "",
        None,
        &[],
        &[],
        0,
        None,
    )
    .unwrap();
    let path = unique_test_path("recovery-export").with_extension("json");
    let dump = pt::export(&conn, None).unwrap();
    let receipt = pt::write_export_file(&path, &dump, false).unwrap();
    let first_bytes = std::fs::read(&path).unwrap();
    assert_eq!(receipt.schema, "papertiger.export_file.v1");
    assert_eq!(receipt.dump_schema, "papertiger.dump.v6");
    assert_eq!(receipt.sha256, pt::sha256(&first_bytes));
    assert_eq!(receipt.bytes, first_bytes.len());
    assert!(first_bytes.ends_with(b"\n"));

    let error = pt::write_export_file(&path, &dump, false).unwrap_err();
    assert!(error.to_string().contains("--replace"));
    assert_eq!(std::fs::read(&path).unwrap(), first_bytes);

    pt::add_note(&conn, "planner", None, "new recovery state").unwrap();
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
