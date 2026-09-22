use papertiger as pt;
use rusqlite::Connection;

fn damaged() -> (Connection, i64) {
    let conn = Connection::open_in_memory().unwrap();
    pt::init(&conn).unwrap();
    pt::add_plan(&conn, "test", "work", "Work", "").unwrap();
    // Deliberately emulate a legacy writer only inside this disposable fixture.
    conn.execute_batch("DROP TRIGGER events_require_zoned_timestamp; DROP TRIGGER events_require_json_payload; DROP TRIGGER events_require_stable_reference;").unwrap();
    conn.execute("INSERT INTO events (at,actor,entity,entity_id,kind,why,payload) VALUES ('2026-09-19 02:41:00','devin','task',99,'note','old reason',?1)", ["raw\nprose with café and 'quotes'"]).unwrap();
    let id = conn.last_insert_rowid();
    // A migrated authority keeps legacy rows while its guards are intact again.
    pt::repair_write_guards(&conn, "operator", "restore fixture guards").unwrap();
    (conn, id)
}

#[test]
fn quarantine_preserves_exact_evidence_and_round_trips_without_inventing_provenance() {
    let (conn, id) = damaged();
    assert!(!pt::audit(&conn).unwrap().is_empty());
    assert!(pt::export(&conn, None).is_err());
    let before = pt::history_recovery::inspect(&conn, id).unwrap();
    assert_eq!(before.problems.len(), 3);
    let recorder = pt::MutationRecorder::new(&conn, None).unwrap();
    let recovery = pt::history_recovery::quarantine(
        &conn,
        "operator",
        id,
        &before.sha256,
        "Preserve untrusted history without guessing",
    )
    .unwrap();
    let after = pt::history_recovery::inspect(&conn, id).unwrap();
    assert_eq!(before.original, after.original);
    assert_eq!(before.sha256, after.sha256);
    assert_eq!(after.recovery_event_id, Some(recovery));
    assert!(pt::audit(&conn).unwrap().is_empty());
    let receipt = recorder.receipt().unwrap();
    assert_eq!(receipt.events.len(), 1);
    assert_eq!(receipt.events[0].event.kind, "quarantine_event");
    drop(recorder);
    let dump = pt::export(&conn, None).unwrap();
    let envelope = dump
        .events
        .iter()
        .find(|e| e.kind == "quarantine_event")
        .unwrap();
    assert_eq!(envelope.entity, "plan");
    assert_eq!(envelope.entity_seq, None);
    let payload = envelope.payload.as_ref().unwrap();
    assert_eq!(payload["original"]["at"], "2026-09-19 02:41:00");
    assert_eq!(
        payload["original"]["payload"],
        "raw\nprose with café and 'quotes'"
    );
    assert!(payload["original"]["entity_seq"].is_null());
    let restored = Connection::open_in_memory().unwrap();
    pt::init(&restored).unwrap();
    pt::import(&restored, "restore", &dump).unwrap();
    assert!(pt::audit(&restored).unwrap().is_empty());
    let redump = pt::export(&restored, None).unwrap();
    let recovered = redump
        .events
        .iter()
        .find(|e| e.kind == "quarantine_event")
        .unwrap();
    assert_eq!(recovered.payload, envelope.payload);
    let mut forged = dump;
    forged
        .events
        .iter_mut()
        .find(|e| e.kind == "quarantine_event")
        .unwrap()
        .payload
        .as_mut()
        .unwrap()["source_event_sha256"] = serde_json::json!("0".repeat(64));
    let fresh = Connection::open_in_memory().unwrap();
    pt::init(&fresh).unwrap();
    assert!(pt::import(&fresh, "restore", &forged).is_err());
    assert_eq!(
        fresh
            .query_row("SELECT count(*) FROM plans", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn quarantine_requires_exact_review_reason_and_structural_defect() {
    let (conn, id) = damaged();
    let inspected = pt::history_recovery::inspect(&conn, id).unwrap();
    assert!(
        pt::history_recovery::quarantine(&conn, "operator", id, &"0".repeat(64), "reason").is_err()
    );
    assert!(
        pt::history_recovery::quarantine(&conn, "operator", id, &inspected.sha256, " ").is_err()
    );
    assert!(
        pt::history_recovery::inspect(&conn, id)
            .unwrap()
            .recovery_event_id
            .is_none()
    );
    let normal = pt::history_recovery::inspect(&conn, 1).unwrap();
    assert!(
        pt::history_recovery::quarantine(
            &conn,
            "operator",
            1,
            &normal.sha256,
            "rewrite valid history"
        )
        .is_err()
    );
    pt::history_recovery::quarantine(&conn, "operator", id, &inspected.sha256, "reason").unwrap();
    assert!(
        pt::history_recovery::quarantine(&conn, "operator", id, &inspected.sha256, "repeat")
            .is_err()
    );
    assert!(conn.execute("DELETE FROM event_quarantines", []).is_err());
    assert!(
        conn.execute("UPDATE event_quarantines SET recovery_event_id=1", [])
            .is_err()
    );
    assert!(
        conn.execute("DELETE FROM events WHERE event_id=?1", [id])
            .is_err()
    );
}

#[test]
fn forged_quarantine_mapping_never_hides_damaged_evidence() {
    let (conn, id) = damaged();
    pt::add_note(&conn, "test", None, "not a recovery envelope").unwrap();
    let unrelated = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO event_quarantines (event_id,recovery_event_id) VALUES (?1,?2)",
        [id, unrelated],
    )
    .unwrap();
    assert!(pt::audit(&conn).is_err());
    assert!(pt::export(&conn, None).is_err());
}

#[test]
fn audit_names_each_missing_reference_even_when_timestamps_and_json_are_valid() {
    let (conn, first) = damaged();
    conn.execute_batch("DROP TRIGGER events_require_stable_reference; DROP TRIGGER papertiger_admit_events_insert;")
        .unwrap();
    conn.execute("INSERT INTO events (at,actor,entity,entity_id,kind,payload) VALUES ('2026-09-19T05:40:00Z','legacy','task',99,'note','{}')", []).unwrap();
    let second = conn.last_insert_rowid();
    let findings = pt::audit(&conn).unwrap();
    for id in [first, second] {
        assert!(
            findings.iter().any(|f| f.kind == "invalid_event_identity"
                && f.detail.contains(&format!("event {id} ")))
        );
        let inspected = pt::history_recovery::inspect(&conn, id).unwrap();
        pt::history_recovery::quarantine(
            &conn,
            "operator",
            id,
            &inspected.sha256,
            "Unknown historical association",
        )
        .unwrap();
    }
    pt::repair_write_guards(&conn, "operator", "restore fixture guards").unwrap();
    assert!(pt::audit(&conn).unwrap().is_empty());
}

#[test]
fn numeric_plan_id_is_never_guessed_into_a_slug() {
    let (conn, _) = damaged();
    conn.execute("INSERT INTO events (at,actor,entity,entity_plan,kind,payload) VALUES ('2026-09-19T05:40:00Z','legacy','plan','1','created','{}')", []).unwrap();
    let id = conn.last_insert_rowid();
    let inspected = pt::history_recovery::inspect(&conn, id).unwrap();
    assert_eq!(inspected.problems.len(), 1);
    assert!(inspected.problems[0].contains("plan slug is absent"));
    pt::history_recovery::quarantine(
        &conn,
        "operator",
        id,
        &inspected.sha256,
        "Retain malformed plan identity",
    )
    .unwrap();
    assert_eq!(
        pt::history_recovery::inspect(&conn, id)
            .unwrap()
            .original
            .entity_plan
            .as_deref(),
        Some("1")
    );
}
