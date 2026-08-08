use papertiger as pt;
use std::path::PathBuf;
use std::process::Command;
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
fn concurrent_cli_mutation_is_refused_immediately_and_can_retry_after_release() {
    let path = unique_test_path("writer-admission");
    let conn = pt::open_for_init(path.to_str().unwrap()).unwrap();
    pt::init(&conn).unwrap();
    pt::add_plan(&conn, "test", "first", "First", "").unwrap();
    let held_mutation = pt::begin_mutation(&conn).unwrap();

    let started = Instant::now();
    let refused = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args([
            "--db",
            path.to_str().unwrap(),
            "plan",
            "add",
            "second",
            "Second",
        ])
        .output()
        .unwrap();
    let elapsed = started.elapsed();

    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("papertiger mutation admission refused")
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "writer contention must fail fast, not wait for a hidden retry timeout: {elapsed:?}"
    );
    let plan_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM plans", [], |row| row.get(0))
        .unwrap();
    assert_eq!(plan_count, 1);

    drop(held_mutation);
    let retried = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args([
            "--db",
            path.to_str().unwrap(),
            "plan",
            "add",
            "second",
            "Second",
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
    assert_eq!(plan_count, 2);

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
    let seq = pt::add_task(&conn, "test", plan, "t", "", None, &[], &[], 0, None, None).unwrap();
    assert!(pt::retire_task(&conn, "test", seq, "").is_err());
    assert!(pt::reject_task(&conn, "test", seq, "").is_err());
    pt::reject_task(&conn, "test", seq, "approach disproven").unwrap();
}

#[test]
fn dependency_cycles_rejected_and_readiness_derived() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let a = pt::add_task(&conn, "test", plan, "a", "", None, &[], &[], 0, None, None).unwrap();
    let b = pt::add_task(&conn, "test", plan, "b", "", None, &[a], &[], 0, None, None).unwrap();
    let c = pt::add_task(&conn, "test", plan, "c", "", None, &[b], &[], 5, None, None).unwrap();
    // a <- b <- c; closing the loop is refused
    assert!(pt::add_dep(&conn, "test", a, c, "cycle probe").is_err());
    assert!(pt::add_dep(&conn, "test", a, a, "self-cycle probe").is_err());
    // only a is ready
    let ready = pt::next(&conn, plan, 10, false).unwrap();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].task.seq, a);
    // blocked view names blockers
    let all = pt::next(&conn, plan, 10, true).unwrap();
    let c_entry = all.iter().find(|e| e.task.seq == c).unwrap();
    assert_eq!(c_entry.blockers, vec![format!("dep:#{b}")]);
    // completing a readies b (priority ordering: c still blocked)
    pt::complete_task(&conn, "test", a, None).unwrap();
    let ready = pt::next(&conn, plan, 10, false).unwrap();
    assert_eq!(
        ready.iter().map(|e| e.task.seq).collect::<Vec<_>>(),
        vec![b]
    );
}

#[test]
fn priority_orders_ready_queue() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let low = pt::add_task(
        &conn,
        "test",
        plan,
        "low",
        "",
        None,
        &[],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
    let high = pt::add_task(
        &conn,
        "test",
        plan,
        "high",
        "",
        None,
        &[],
        &[],
        9,
        None,
        None,
    )
    .unwrap();
    let ready = pt::next(&conn, plan, 10, false).unwrap();
    assert_eq!(
        ready.iter().map(|e| e.task.seq).collect::<Vec<_>>(),
        vec![high, low]
    );
}

#[test]
fn next_limit_applies_to_the_whole_result() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let blocker = pt::add_task(
        &conn,
        "test",
        plan,
        "blocker",
        "",
        None,
        &[],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
    for title in ["ready-a", "ready-b"] {
        pt::add_task(
            &conn,
            "test",
            plan,
            title,
            "",
            None,
            &[],
            &[],
            1,
            None,
            None,
        )
        .unwrap();
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
            None,
        )
        .unwrap();
    }
    pt::start_task(&conn, "test", blocker, None).unwrap();

    let entries = pt::next(&conn, plan, 3, true).unwrap();
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
    let dead = pt::add_task(
        &conn,
        "test",
        plan,
        "dead",
        "",
        None,
        &[],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
    let live = pt::add_task(
        &conn,
        "test",
        plan,
        "live",
        "",
        None,
        &[dead],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
    pt::reject_task(&conn, "test", dead, "nope").unwrap();
    let ready = pt::next(&conn, plan, 10, false).unwrap();
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
        Some("CHM-0405"),
        None,
    )
    .unwrap();
    let b = pt::add_task(&conn, "test", plan, "b", "", None, &[a], &[], 0, None, None).unwrap();
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
    assert_eq!(a2.alias.as_deref(), Some("CHM-0405"));
    let ready = pt::next(
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
        "\u{feff}{\"schema\":\"papertiger.dump.v1\",\"plans\":[],\"tasks\":[]}",
    )
    .unwrap();
    assert_eq!(dump.schema, "papertiger.dump.v1");
}

#[test]
fn import_refuses_closed_gate_without_evidence() {
    let mut conn = db();
    let dump: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v1",
            "plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":1,"plan":"p","title":"t","status":"done",
                      "gates":[{"name":"g","kind":"test","requirement":"r","status":"closed"}]}]}"#,
    )
    .unwrap();
    let err = pt::import(&mut conn, "test", &dump).unwrap_err();
    assert!(err.to_string().contains("lacks evidence_locator"));
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
        None,
    )
    .unwrap();
    let ready = pt::next(&conn, plan, 10, false).unwrap();
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
    assert_eq!(pt::parse_task_ref("12").unwrap(), 12);
    assert_eq!(pt::parse_task_ref("#12").unwrap(), 12);
    let error = pt::parse_task_ref("MECH-BATCH-01").unwrap_err().to_string();
    assert!(error.contains("expected task.seq as N or #N"));
}

#[test]
fn failed_add_is_atomic() {
    let conn = db();
    let plan = pt::add_plan(&conn, "test", "p", "Plan", "").unwrap();
    let existing = pt::add_task(
        &conn,
        "test",
        plan,
        "existing",
        "",
        None,
        &[],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
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
        r#"{"schema":"papertiger.dump.v1","plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":1,"plan":"p","title":"a","deps":[2]},
                     {"seq":2,"plan":"p","title":"b","deps":[1]}]}"#,
    )
    .unwrap();
    assert!(pt::import(&mut conn, "test", &cycle).is_err());

    let missing_parent: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v1","plans":[{"slug":"p","title":"P"}],
            "tasks":[{"seq":3,"plan":"p","title":"child","parent_seq":999}]}"#,
    )
    .unwrap();
    assert!(pt::import(&mut conn, "test", &missing_parent).is_err());

    let done_open: pt::Dump = serde_json::from_str(
        r#"{"schema":"papertiger.dump.v1","plans":[{"slug":"p","title":"P"}],
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
        r#"{"schema":"papertiger.dump.v1","plans":[{"slug":"p","title":"P"}],
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
    let task = pt::add_task(
        &conn,
        "test",
        plan,
        "task",
        "",
        None,
        &[],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
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
    let task = pt::add_task(
        &conn,
        "test",
        plan,
        "task",
        "",
        None,
        &[],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
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
    let parent = pt::add_task(
        &conn,
        "test",
        plan,
        "outcome",
        "",
        None,
        &[],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
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
fn v3_export_import_preserves_task_kind_result_blocker_and_mise_evidence() {
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
        Some("engine-choice"),
        None,
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
    assert_eq!(dump.schema, "papertiger.dump.v3");
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
    let task = pt::add_task(
        &conn,
        "test",
        plan,
        "task",
        "",
        None,
        &[],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
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
    let task = pt::add_task(
        &conn,
        "test",
        plan,
        "task",
        "",
        None,
        &[],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
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
    let task = pt::add_task(
        &conn,
        "test",
        plan,
        "task",
        "",
        None,
        &[],
        &[],
        0,
        None,
        None,
    )
    .unwrap();
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
        None,
    )
    .unwrap();
    assert_eq!(existing_task, 1);

    let external_event: pt::Dump = serde_json::from_str(
        r#"{
            "schema":"papertiger.dump.v2",
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
            "schema":"papertiger.dump.v2",
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
