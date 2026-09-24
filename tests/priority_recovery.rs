use papertiger as pt;
use rusqlite::Connection;

fn no_edit() -> pt::TaskEdit<'static> {
    pt::TaskEdit {
        title: None,
        intent: None,
        intent_source: None,
        parent: None,
        kind: None,
        priority: None,
    }
}

fn fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    pt::init(&conn).unwrap();
    let plan = pt::add_plan(&conn, "test", "work", "Work", "Preserve work").unwrap();
    pt::add_task(
        &conn,
        "test",
        plan,
        pt::TaskCreation {
            intent: "Keep the evidence",
            ..pt::TaskCreation::new("Recover priority")
        },
    )
    .unwrap();
    // Deliberately corrupt only a disposable test fixture, never an authority,
    // modeling a text priority written before schema v11 refused it.
    conn.execute_batch("DROP TRIGGER tasks_require_integer_priority_update;")
        .unwrap();
    conn.execute("UPDATE tasks SET priority='normal' WHERE seq=1", [])
        .unwrap();
    conn
}

#[test]
fn text_priority_refuses_reads_and_explicit_edit_preserves_original_evidence() {
    let conn = fixture();
    let error = pt::get_task(&conn, 1).unwrap_err().to_string();
    assert!(
        error.contains("papertiger edit 1 --priority <integer>"),
        "{error}"
    );
    assert!(
        pt::audit(&conn)
            .unwrap()
            .iter()
            .any(|finding| finding.kind == "invalid_task_priority")
    );
    let recorder = pt::MutationRecorder::new(&conn, None).unwrap();
    let changed = pt::edit_task(
        &conn,
        "recovery",
        1,
        pt::TaskEdit {
            priority: Some(0),
            ..no_edit()
        },
        "Restore numeric scheduling after unsupported text write",
    )
    .unwrap();
    assert_eq!(changed, ["priority"]);
    assert_eq!(pt::get_task(&conn, 1).unwrap().priority, 0);
    let (actor, kind, why, raw): (String, String, String, String) = conn
        .query_row(
            "SELECT actor,kind,why,payload FROM events ORDER BY event_id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(actor, "recovery");
    assert_eq!(kind, "repair_priority");
    assert!(why.contains("unsupported text write"));
    let payload: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(payload["before"]["value"], "normal");
    assert_eq!(payload["before"]["storage_type"], "text");
    assert_eq!(payload["after"], 0);
    let receipt = recorder.receipt().unwrap();
    assert!(receipt.changed);
    assert_eq!(receipt.events.len(), 1);
    assert_eq!(receipt.events[0].event.kind, "repair_priority");
    assert_eq!(receipt.events[0].task.as_ref().unwrap().priority, 0);
    drop(recorder);
    // The fixture dropped a guard to plant the legacy value; reinstate it.
    pt::repair_write_guards(&conn, "operator", "restore fixture guard").unwrap();
    assert!(pt::audit(&conn).unwrap().is_empty());
    let dump = pt::export(&conn, None).unwrap();
    let restored = Connection::open_in_memory().unwrap();
    pt::init(&restored).unwrap();
    pt::import(&restored, "restore", &dump).unwrap();
    assert_eq!(pt::get_task(&restored, 1).unwrap().priority, 0);
    assert!(pt::audit(&restored).unwrap().is_empty());
    let mut invalid = serde_json::to_value(dump).unwrap();
    invalid["tasks"][0]["priority"] = serde_json::json!("normal");
    assert!(serde_json::from_value::<pt::Dump>(invalid).is_err());
}

#[test]
fn recovery_requires_explicit_isolated_edit_and_rolls_back_on_other_corruption() {
    let conn = fixture();
    for edit in [
        pt::TaskEdit {
            title: Some("Changed"),
            ..no_edit()
        },
        pt::TaskEdit {
            title: Some("Changed"),
            priority: Some(0),
            ..no_edit()
        },
    ] {
        assert!(pt::edit_task(&conn, "test", 1, edit, "Repair").is_err());
    }
    assert!(
        pt::edit_task(
            &conn,
            "test",
            1,
            pt::TaskEdit {
                priority: Some(0),
                ..no_edit()
            },
            " "
        )
        .is_err()
    );
    conn.execute("UPDATE tasks SET title=X'80' WHERE seq=1", [])
        .unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert!(
        pt::edit_task(
            &conn,
            "test",
            1,
            pt::TaskEdit {
                priority: Some(0),
                ..no_edit()
            },
            "Repair"
        )
        .is_err()
    );
    let priority: String = conn
        .query_row("SELECT priority FROM tasks WHERE seq=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(priority, "normal");
    assert_eq!(
        count,
        conn.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap()
    );
}

#[test]
fn decompose_reports_an_unreadable_dependency_with_its_corrective_error() {
    let conn = fixture();
    let plan = conn
        .query_row("SELECT plan_id FROM plans WHERE slug='work'", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    let parent = pt::add_task(&conn, "test", plan, pt::TaskCreation::new("Parent")).unwrap();
    let outline = pt::parse_task_outline(
        br#"{"schema":"papertiger.task_outline.v1","children":[{"key":"a","title":"A","deps":[1]}]}"#,
    )
    .unwrap();
    let error = pt::decompose_task(&conn, "test", parent, &outline, false, None)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("papertiger edit 1 --priority <integer>"),
        "{error}"
    );
    assert!(!error.contains("does not exist"), "{error}");
}

#[test]
fn non_text_corruption_is_not_coerced_by_recovery() {
    let conn = fixture();
    conn.execute("UPDATE tasks SET priority=1.5 WHERE seq=1", [])
        .unwrap();
    let error = pt::edit_task(
        &conn,
        "test",
        1,
        pt::TaskEdit {
            priority: Some(0),
            ..no_edit()
        },
        "Repair",
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("restore a verified authority"), "{error}");
    assert_eq!(
        conn.query_row("SELECT priority FROM tasks", [], |r| r.get::<_, f64>(0))
            .unwrap(),
        1.5
    );
}
