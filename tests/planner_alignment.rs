use papertiger as pt;

fn planner() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    pt::init(&conn).unwrap();
    pt::add_plan(&conn, "operator", "old", "Old", "").unwrap();
    pt::add_plan(&conn, "operator", "new", "New", "").unwrap();
    conn
}

fn task(conn: &rusqlite::Connection, title: &str) -> i64 {
    pt::add_task(
        conn,
        "recorder",
        Some("old"),
        pt::TaskCreation {
            intent: "standalone purpose",
            ..pt::TaskCreation::new(title)
        },
    )
    .unwrap()
}

#[test]
fn model_history_and_receipts_capture_only_committed_operations() {
    let conn = planner();
    let seq;
    {
        let recorder = pt::MutationRecorder::new(&conn, Some("gpt-5.6-luna")).unwrap();
        seq = task(&conn, "proposal");
        pt::add_gate(&conn, "recorder", seq, "proof", "test", "must pass").unwrap();
        let before = serde_json::to_value(recorder.receipt().unwrap()).unwrap();
        assert!(pt::complete_task(&conn, "other", seq, Some("unsupported success"), None).is_err());
        assert_eq!(
            serde_json::to_value(recorder.receipt().unwrap()).unwrap(),
            before
        );
        assert_eq!(before["events"][0]["event"]["model"], "gpt-5.6-luna");
        assert_eq!(before["events"][0]["task"]["status"], "proposed");
        assert_eq!(before["events"][0]["plan"]["slug"], "old");
    }
    {
        let recorder = pt::MutationRecorder::new(&conn, Some("review-model")).unwrap();
        pt::waive_gate(&conn, "reviewer", seq, "proof", "bounded fixture waiver").unwrap();
        pt::complete_task(&conn, "reviewer", seq, Some("verified outcome"), None).unwrap();
        assert_eq!(
            recorder
                .receipt()
                .unwrap()
                .events
                .last()
                .unwrap()
                .task
                .as_ref()
                .unwrap()
                .status,
            "done"
        );
    }
    let activity = pt::task_activity(&conn, seq).unwrap();
    assert_eq!(
        activity.created_event.unwrap().model.as_deref(),
        Some("gpt-5.6-luna")
    );
    assert_eq!(
        activity.completed_event.unwrap().model.as_deref(),
        Some("review-model")
    );
    pt::reopen_task(&conn, "human", seq, "new proof required").unwrap();
    assert!(
        pt::task_activity(&conn, seq)
            .unwrap()
            .completed_event
            .is_none()
    );
    let unknown = task(&conn, "unknown model");
    assert!(
        pt::task_activity(&conn, unknown)
            .unwrap()
            .created_event
            .unwrap()
            .model
            .is_none()
    );
    let dump = pt::export(&conn, None).unwrap();
    let restored = rusqlite::Connection::open_in_memory().unwrap();
    pt::init(&restored).unwrap();
    pt::import(&restored, "restore", &dump).unwrap();
    assert_eq!(
        pt::task_activity(&restored, seq)
            .unwrap()
            .created_event
            .unwrap()
            .model
            .as_deref(),
        Some("gpt-5.6-luna")
    );
    assert!(pt::audit(&restored).unwrap().is_empty());
    for invalid in ["", " ", "model\nforged", "display name", "🦁"] {
        assert!(pt::MutationRecorder::new(&conn, Some(invalid)).is_err());
    }
    {
        let recorder = pt::MutationRecorder::new(&conn, Some("unfinished-scope")).unwrap();
        conn.execute_batch("BEGIN DEFERRED TRANSACTION").unwrap();
        assert!(recorder.receipt().is_err());
    }
    assert!(conn.is_autocommit());
    let after = task(&conn, "after unfinished scope");
    assert!(
        pt::task_activity(&conn, after)
            .unwrap()
            .created_event
            .unwrap()
            .model
            .is_none()
    );
    let mut invalid = pt::export(&conn, None).unwrap();
    let event = invalid
        .events
        .iter_mut()
        .find(|event| event.entity_seq == Some(seq))
        .unwrap();
    event.payload.as_mut().unwrap()["model"] = serde_json::json!(17);
    let refused = rusqlite::Connection::open_in_memory().unwrap();
    pt::init(&refused).unwrap();
    assert!(pt::import(&refused, "restore", &invalid).is_err());
    assert!(pt::export(&refused, None).unwrap().tasks.is_empty());
}

#[test]
fn reasoning_effort_follows_each_author_and_survives_recovery() {
    let conn = planner();
    let seq;
    {
        let recorder =
            pt::MutationRecorder::with_reasoning_effort(&conn, Some("gpt-6-astra"), Some("high"))
                .unwrap();
        seq = task(&conn, "precise attribution");
        pt::add_gate(&conn, "creator", seq, "proof", "test", "must pass").unwrap();
        let before = serde_json::to_value(recorder.receipt().unwrap()).unwrap();
        assert_eq!(before["events"][0]["event"]["reasoning_effort"], "high");
        assert!(pt::complete_task(&conn, "creator", seq, Some("unsupported"), None).is_err());
        assert_eq!(
            serde_json::to_value(recorder.receipt().unwrap()).unwrap(),
            before
        );
    }
    {
        let _recorder =
            pt::MutationRecorder::with_reasoning_effort(&conn, Some("other-model"), Some("medium"))
                .unwrap();
        pt::waive_gate(&conn, "reviewer", seq, "proof", "fixture waiver").unwrap();
        let activity = pt::task_activity(&conn, seq).unwrap();
        assert_eq!(
            activity.last_event.unwrap().reasoning_effort.as_deref(),
            Some("medium")
        );
        pt::complete_task(&conn, "reviewer", seq, Some("verified"), None).unwrap();
    }
    let activity = pt::task_activity(&conn, seq).unwrap();
    let created = activity.created_event.unwrap();
    let completed = activity.completed_event.unwrap();
    assert_eq!(created.model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(created.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(completed.model.as_deref(), Some("other-model"));
    assert_eq!(completed.reasoning_effort.as_deref(), Some("medium"));
    let unknown = task(&conn, "after attributed scope");
    assert!(
        pt::task_activity(&conn, unknown)
            .unwrap()
            .created_event
            .unwrap()
            .reasoning_effort
            .is_none()
    );
    let dump = pt::export(&conn, None).unwrap();
    let restored = rusqlite::Connection::open_in_memory().unwrap();
    pt::init(&restored).unwrap();
    pt::import(&restored, "restore", &dump).unwrap();
    assert_eq!(
        pt::task_activity(&conn, seq).unwrap(),
        pt::task_activity(&restored, seq).unwrap()
    );
    assert!(pt::audit(&restored).unwrap().is_empty());

    for invalid in ["", " ", "high\nforged", "very high"] {
        assert!(
            pt::MutationRecorder::with_reasoning_effort(&conn, Some("model"), Some(invalid))
                .is_err()
        );
    }
    assert!(pt::MutationRecorder::with_reasoning_effort(&conn, None, Some("high")).is_err());
    for invalid in [serde_json::json!(17), serde_json::json!("high\nforged")] {
        let mut malformed = pt::export(&conn, None).unwrap();
        let event = malformed
            .events
            .iter_mut()
            .find(|event| event.entity_seq == Some(seq))
            .unwrap();
        event.payload.as_mut().unwrap()["reasoning_effort"] = invalid;
        let empty = rusqlite::Connection::open_in_memory().unwrap();
        pt::init(&empty).unwrap();
        assert!(pt::import(&empty, "restore", &malformed).is_err());
        assert!(pt::export(&empty, None).unwrap().tasks.is_empty());
    }
    let mut orphan = dump;
    let event = orphan
        .events
        .iter_mut()
        .find(|event| event.entity_seq == Some(seq))
        .unwrap();
    event.payload.as_mut().unwrap()["model"] = serde_json::Value::Null;
    let empty = rusqlite::Connection::open_in_memory().unwrap();
    pt::init(&empty).unwrap();
    assert!(pt::import(&empty, "restore", &orphan).is_err());
    assert!(pt::export(&empty, None).unwrap().tasks.is_empty());
}

#[test]
fn receipt_snapshots_exclude_later_concurrent_writers() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "papertiger-receipt-{}-{nonce}.sqlite",
        std::process::id()
    ));
    let first = pt::open_for_init(path.to_str().unwrap()).unwrap();
    pt::init(&first).unwrap();
    pt::add_plan(&first, "setup", "old", "Old", "").unwrap();
    let second = pt::open_existing(path.to_str().unwrap()).unwrap();
    {
        let recorder = pt::MutationRecorder::new(&first, Some("author-model")).unwrap();
        let seq = task(&first, "a concurrent task");
        pt::start_task(&second, "concurrent", seq, Some("separate writer"), None).unwrap();
        let receipt = recorder.receipt().unwrap();
        assert_eq!(receipt.events.len(), 1);
        assert_eq!(receipt.events[0].task.as_ref().unwrap().status, "proposed");
        assert_eq!(pt::get_task(&first, seq).unwrap().status, "in_progress");
    }
    drop(first);
    drop(second);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn relocation_preserves_edges_gate_refusals_cursors_and_scoped_recovery() {
    let conn = planner();
    let parent = task(&conn, "parent");
    let child = pt::add_task(
        &conn,
        "author",
        Some("old"),
        pt::TaskCreation {
            intent: "child context",
            parent: Some(parent),
            ..pt::TaskCreation::new("child")
        },
    )
    .unwrap();
    let duplicate = task(&conn, "duplicate");
    pt::retire_task(
        &conn,
        "author",
        duplicate,
        Some(child),
        "same intended outcome",
    )
    .unwrap();
    pt::add_gate(&conn, "author", child, "proof", "test", "real proof").unwrap();
    let cursor = pt::event_head(&conn).unwrap().unwrap();
    let before = serde_json::to_value(pt::export(&conn, None).unwrap()).unwrap();
    assert!(
        pt::move_tasks_to_plan(&conn, "author", &[child], "new", "rehome")
            .unwrap_err()
            .to_string()
            .contains("also supply related tasks")
    );
    assert_eq!(
        serde_json::to_value(pt::export(&conn, None).unwrap()).unwrap(),
        before
    );
    pt::move_tasks_to_plan(
        &conn,
        "author",
        &[child, parent, duplicate],
        "new",
        "correct initiative",
    )
    .unwrap();
    assert_eq!(
        pt::event_cursor(&conn, cursor.event_id).unwrap().token,
        cursor.token
    );
    let context = pt::task_context(&conn, child).unwrap();
    assert_eq!(context.plan.slug, "new");
    assert_eq!(context.parent.unwrap().seq, parent);
    assert_eq!(context.gates[0].status, "open");
    assert!(pt::complete_task(&conn, "author", child, Some("not proven"), None).is_err());
    assert!(
        pt::move_tasks_to_plan(&conn, "author", &[child, parent, duplicate], "new", "no-op")
            .is_err()
    );
    let old = pt::export(&conn, Some("old")).unwrap();
    assert!(old.tasks.is_empty());
    assert!(old.events.iter().all(|event| event.entity_seq.is_none()));
    let moved = pt::export(&conn, Some("new")).unwrap();
    assert_eq!(moved.tasks.len(), 3);
    assert!(moved.plans.iter().any(|plan| plan.slug == "old"));
    let restored = rusqlite::Connection::open_in_memory().unwrap();
    pt::init(&restored).unwrap();
    pt::import(&restored, "restore", &moved).unwrap();
    assert_eq!(pt::task_context(&restored, child).unwrap().plan.slug, "new");
    assert!(pt::audit(&restored).unwrap().is_empty());
    // Returning to the original plan still validates every historical boundary.
    pt::move_tasks_to_plan(
        &conn,
        "author",
        &[parent, child, duplicate],
        "old",
        "return initiative",
    )
    .unwrap();
    assert!(pt::audit(&conn).unwrap().is_empty());
    let refused = rusqlite::Connection::open_in_memory().unwrap();
    pt::init(&refused).unwrap();
    // With no intermediate events, deleting both moves cannot be distinguished
    // from absence. Corrupt one boundary instead, which must fail closed.
    let mut corrupted = pt::export(&conn, None).unwrap();
    let index = corrupted
        .events
        .iter()
        .position(|event| {
            event
                .payload
                .as_ref()
                .and_then(|payload| payload.pointer("/changes/plan"))
                .is_some()
        })
        .unwrap();
    corrupted.events.remove(index);
    assert!(pt::import(&refused, "restore", &corrupted).is_err());
    assert!(pt::export(&refused, None).unwrap().tasks.is_empty());
}

#[test]
fn references_are_exact_evented_inward_links_not_completion_evidence() {
    let conn = planner();
    let seq = task(&conn, "reference owner");
    pt::add_gate(&conn, "author", seq, "test", "test", "required proof").unwrap();
    let reference = pt::new_external_reference(
        "input",
        "file:fixtures/report.json",
        Some(&"a".repeat(64)),
        Some("source context"),
    )
    .unwrap();
    pt::add_external_reference(&conn, "author", seq, &reference).unwrap();
    assert!(pt::add_external_reference(&conn, "author", seq, &reference).is_err());
    assert_eq!(
        pt::find_external_references(&conn, &reference.locator)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        pt::task_context(&conn, seq).unwrap().external_references[0],
        reference
    );
    assert!(pt::complete_task(&conn, "author", seq, Some("reference is not proof"), None).is_err());
    pt::move_tasks_to_plan(&conn, "author", &[seq], "new", "relocate evidence owner").unwrap();
    let dump = pt::export(&conn, None).unwrap();
    let restored = rusqlite::Connection::open_in_memory().unwrap();
    pt::init(&restored).unwrap();
    pt::import(&restored, "restore", &dump).unwrap();
    assert_eq!(
        pt::external_references(&restored, seq).unwrap(),
        vec![reference.clone()]
    );
    pt::remove_external_reference(
        &conn,
        "author",
        seq,
        "input",
        &reference.locator,
        "correct the locator",
    )
    .unwrap();
    assert!(
        pt::find_external_references(&conn, &reference.locator)
            .unwrap()
            .is_empty()
    );
    let event = pt::event_log(&conn, Some(seq), 1, None, None)
        .unwrap()
        .events
        .remove(0);
    assert_eq!(
        event.payload.unwrap()["reference"]["sha256"],
        "a".repeat(64)
    );
    assert!(pt::audit(&conn).unwrap().is_empty());
    for locator in [
        "not-a-locator",
        " https://example.test/a",
        "https://example.test/a b",
        "file:\nforged",
    ] {
        assert!(pt::new_external_reference("other", locator, None, None).is_err());
    }
}

#[test]
fn recovery_export_never_silently_omits_orphan_task_history() {
    let conn = planner();
    conn.execute("INSERT INTO events(at,actor,entity,entity_plan,entity_seq,kind) VALUES (?1,'fixture','task','old',999,'create')",[pt::now()]).unwrap();
    for plan in [None, Some("old"), Some("new")] {
        let error = pt::export(&conn, plan).err().unwrap().to_string();
        assert!(error.contains("missing task #999"));
        assert!(error.contains("papertiger audit"));
    }
}
