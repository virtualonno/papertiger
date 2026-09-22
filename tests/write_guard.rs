use papertiger as pt;
use rusqlite::Connection;

const GUARDS: [&str; 7] = [
    "events_append_only_update",
    "events_append_only_delete",
    "events_require_zoned_timestamp",
    "events_require_json_payload",
    "events_require_stable_reference",
    "tasks_require_integer_priority_insert",
    "tasks_require_integer_priority_update",
];

fn authority() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    pt::init(&conn).unwrap();
    let plan = pt::add_plan(&conn, "test", "work", "Work", "Guard history").unwrap();
    pt::add_task(&conn, "test", plan, "Task", "", None, &[], &[], 0, None).unwrap();
    pt::add_note(&conn, "test", Some(1), "canonical history").unwrap();
    conn
}

fn refusal(conn: &Connection, sql: &str) -> String {
    conn.execute_batch(sql)
        .expect_err("direct write must be refused")
        .to_string()
}

fn insert_event(
    at: &str,
    entity_plan: &str,
    entity_seq: &str,
    kind: &str,
    payload: &str,
) -> String {
    format!(
        "INSERT INTO events (at, actor, entity, entity_id, entity_plan, entity_seq, kind, why, payload)
         VALUES ('{at}', 'direct-writer', 'task', 1, {entity_plan}, {entity_seq}, '{kind}', 'note', {payload})"
    )
}

#[test]
fn stored_history_is_append_only() {
    let conn = authority();
    let edit = refusal(&conn, "UPDATE events SET why='rewritten'");
    assert!(edit.contains("append-only"), "{edit}");
    assert!(edit.contains("papertiger executable"), "{edit}");
    let delete = refusal(&conn, "DELETE FROM events");
    assert!(delete.contains("append-only"), "{delete}");
    let why: String = conn
        .query_row("SELECT why FROM events WHERE kind='note'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(why, "canonical history");
}

#[test]
fn observed_out_of_band_event_shapes_are_refused() {
    let conn = authority();
    let before: i64 = conn
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap();
    // Shapes found in a consumer authority written by direct SQLite access.
    for at in [
        "2026-09-19T01:20:34.499097",
        "2026-09-19 02:41:00",
        "not-a-time",
    ] {
        let error = refusal(&conn, &insert_event(at, "'work'", "1", "note", "NULL"));
        assert!(
            error.contains("RFC3339 timestamp and zone"),
            "{at}: {error}"
        );
        assert!(error.contains("papertiger executable"), "{error}");
    }
    let text_payload = insert_event(
        "2026-09-22T17:44:46.045269+00:00",
        "'10'",
        "1",
        "created",
        "'Function is knowledge_complete'",
    );
    assert!(refusal(&conn, &text_payload).contains("payload is not JSON"));
    let unstable = insert_event("2026-09-19T01:20:34.499Z", "NULL", "NULL", "note", "NULL");
    assert!(refusal(&conn, &unstable).contains("stable plan/task reference"));
    let unknown_entity = "INSERT INTO events (at, actor, entity, kind)
         VALUES ('2026-09-19T01:20:34.499Z', 'direct-writer', 'session', 'note')";
    assert!(refusal(&conn, unknown_entity).contains("known entity"));
    let gate_without_name = "INSERT INTO events (at, actor, entity, entity_plan, entity_seq, kind)
         VALUES ('2026-09-19T01:20:34.499Z', 'direct-writer', 'gate', 'work', 1, 'close')";
    assert!(refusal(&conn, gate_without_name).contains("stable plan/task reference"));
    let after: i64 = conn
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(before, after);
    assert!(pt::audit(&conn).unwrap().is_empty());
}

#[test]
fn text_priorities_are_refused_on_insert_and_update() {
    let conn = authority();
    let update = refusal(&conn, "UPDATE tasks SET priority='high' WHERE seq=1");
    assert!(
        update.contains("papertiger edit <task> --priority <integer>"),
        "{update}"
    );
    let insert = refusal(
        &conn,
        "INSERT INTO tasks (seq, plan_id, title, priority, created_at, updated_at)
         VALUES (2, 1, 'direct', 'urgent', '2026-09-19T01:20:34.499Z', '2026-09-19T01:20:34.499Z')",
    );
    assert!(insert.contains("priority must be an integer"), "{insert}");
    pt::edit_task(
        &conn,
        "agent",
        1,
        pt::TaskEdit {
            title: None,
            intent: None,
            intent_source: None,
            parent: None,
            kind: None,
            priority: Some(3),
        },
        "raise",
    )
    .unwrap();
    assert_eq!(pt::get_task(&conn, 1).unwrap().priority, 3);
}

#[test]
fn migration_guards_new_writes_and_preserves_earlier_malformed_rows() {
    let conn = authority();
    // Exact v10 shape, built only inside this disposable test authority.
    for guard in GUARDS {
        conn.execute_batch(&format!("DROP TRIGGER {guard};"))
            .unwrap();
    }
    conn.execute_batch("UPDATE meta SET value='10' WHERE key='schema_version';")
        .unwrap();
    conn.execute_batch(&insert_event(
        "2026-09-19 02:41:00",
        "'work'",
        "1",
        "note",
        "'plain text'",
    ))
    .unwrap();

    assert!(matches!(
        pt::init(&conn).unwrap(),
        pt::InitOutcome::Migrated { from: 10, to: 11 }
    ));
    let installed: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name IN
             ('events_append_only_update','events_append_only_delete',
              'events_require_zoned_timestamp','events_require_json_payload',
              'events_require_stable_reference','tasks_require_integer_priority_insert',
              'tasks_require_integer_priority_update')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(installed, GUARDS.len() as i64);
    let kinds = pt::audit(&conn)
        .unwrap()
        .into_iter()
        .map(|finding| finding.kind)
        .collect::<Vec<_>>();
    assert!(
        kinds.contains(&"invalid_event_timestamp".to_string()),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&"invalid_event_payload".to_string()),
        "{kinds:?}"
    );
    let stored: String = conn
        .query_row(
            "SELECT payload FROM events WHERE actor='direct-writer'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, "plain text");
    assert!(
        refusal(
            &conn,
            &insert_event("2026-09-19 02:41:00", "'work'", "1", "note", "NULL")
        )
        .contains("RFC3339 timestamp and zone")
    );
    assert!(matches!(pt::init(&conn).unwrap(), pt::InitOutcome::Current));
}

#[test]
fn import_accepts_rfc3339_offsets_under_the_guard() {
    let conn = authority();
    let mut dump = pt::export(&conn, None).unwrap();
    for event in &mut dump.events {
        event.at = "2026-09-19T03:20:34.499+02:00".into();
    }
    let restored = Connection::open_in_memory().unwrap();
    pt::init(&restored).unwrap();
    pt::import(&restored, "restore", &dump).unwrap();
    let offset_events: i64 = restored
        .query_row(
            "SELECT count(*) FROM events WHERE at='2026-09-19T03:20:34.499+02:00'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(offset_events, dump.events.len() as i64);
    assert!(pt::audit(&restored).unwrap().is_empty());
}
