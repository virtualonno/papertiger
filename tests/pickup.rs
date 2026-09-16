use papertiger as pt;
use rusqlite::Connection;
use std::sync::{Arc, Barrier};

#[test]
fn focus_bounds_narrative_without_losing_pickup_or_real_blockers() {
    let conn = Connection::open_in_memory().unwrap();
    pt::init(&conn).unwrap();
    let narrative = "long durable context ".repeat(3000);
    let plan = pt::add_plan(&conn, "test", "work", "Work", &narrative).unwrap();
    let add = |title| {
        pt::add_task(
            &conn,
            "test",
            plan,
            title,
            &narrative,
            None,
            &[],
            &[],
            0,
            None,
        )
        .unwrap()
    };
    let mine = add("My work");
    let other = add("Other session's work");
    let ready = add("Available work");
    let blocked = add("Waiting work");
    pt::start_task(&conn, "same-harness", mine, None, Some("me")).unwrap();
    pt::start_task(&conn, "same-harness", other, None, Some("other")).unwrap();
    pt::add_task_blocker(&conn, "test", other, "input", "waiting for input").unwrap();
    pt::add_dep(&conn, "test", blocked, ready, "prerequisite").unwrap();
    let before = serde_json::to_value(pt::export(&conn, None).unwrap()).unwrap();

    let selection = pt::focus(&conn, plan, 10, true, Some("me")).unwrap();
    let json = serde_json::to_value(&selection).unwrap();
    let rows = json["entries"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r["task"]["seq"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![mine, ready, other, blocked]
    );
    assert_eq!(rows[0]["readiness"], "mine");
    assert_eq!(rows[0]["pickup"]["session"], "me");
    assert_eq!(rows[2]["readiness"], "picked_up_elsewhere");
    assert_eq!(rows[2]["pickup"]["session"], "other");
    assert_eq!(
        rows[2]["pickup"]["at"],
        pt::get_task(&conn, other).unwrap().pickup.unwrap().at
    );
    assert!(!rows[2]["blockers"].as_array().unwrap().is_empty());
    assert_eq!(rows[3]["readiness"], "blocked");
    assert_eq!(rows[3]["blockers"][0], format!("dep:#{ready}"));
    assert!(rows[1]["pickup"].is_null());
    assert!(rows[1]["blockers"].as_array().unwrap().is_empty());
    // Narrative growth must not turn a selection read into a context dump.
    assert!(serde_json::to_vec_pretty(&selection).unwrap().len() < 5000);
    assert!(json["plan"].get("intent").is_none());
    assert!(rows.iter().all(|r| r["task"].get("intent").is_none()));
    assert_eq!(pt::get_task(&conn, other).unwrap().intent, narrative);
    assert_eq!(
        serde_json::to_value(pt::export(&conn, None).unwrap()).unwrap(),
        before
    );
}

fn seed(conn: &Connection) -> (i64, i64, i64) {
    pt::init(conn).unwrap();
    pt::add_plan(conn, "test", "work", "Work", "").unwrap();
    let (plan, _) = pt::resolve_plan(conn, Some("work")).unwrap();
    let add = |title| pt::add_task(conn, "test", plan, title, "", None, &[], &[], 0, None).unwrap();
    (plan, add("First outcome"), add("Second outcome"))
}

#[test]
fn independent_sessions_prefer_other_work_without_stranding_abandoned_pickups() {
    let conn = Connection::open_in_memory().unwrap();
    let (plan, first, second) = seed(&conn);
    let select = |session| pt::focus(&conn, plan, 10, false, Some(session)).unwrap();
    assert_eq!(select("a").projection.entries[0].task.seq, first);
    pt::start_task(&conn, "same-harness", first, None, Some("a")).unwrap();
    assert_eq!(select("a").projection.entries[0].readiness, "mine");
    let other = select("b");
    assert_eq!(other.projection.entries[0].task.seq, second);
    assert_eq!(other.projection.entries[1].readiness, "picked_up_elsewhere");
    assert!(other.projection.entries[1].blockers.is_empty());

    // A vanishes without releasing anything. With no alternate work, the task
    // remains discoverable and B resumes immediately: no age, TTL, or hook.
    pt::complete_task(&conn, "test", second, None).unwrap();
    assert_eq!(select("b").projection.entries[0].task.seq, first);
    pt::start_task(&conn, "same-harness", first, None, Some("b")).unwrap();
    let resumed = pt::get_task(&conn, first).unwrap();
    assert_eq!(resumed.status, "in_progress");
    assert_eq!(resumed.pickup.unwrap().session.as_deref(), Some("b"));
    let events = pt::export(&conn, None).unwrap().events;
    let last = events.last().unwrap();
    assert_eq!(last.kind, "pickup");
    let payload = last.payload.as_ref().unwrap();
    assert_eq!(payload["before"]["session"], "a");
    assert_eq!(payload["after"]["session"], "b");
    // Identity is advisory, so it cannot prevent completion by another caller.
    pt::complete_task(&conn, "operator", first, None).unwrap();
    assert!(pt::get_task(&conn, first).unwrap().pickup.is_none());
    assert!(pt::audit(&conn).unwrap().is_empty());
}

#[test]
fn identified_retries_are_eventless_and_real_blockers_remain_enforced() {
    let conn = Connection::open_in_memory().unwrap();
    let (plan, first, second) = seed(&conn);
    pt::add_dep(&conn, "test", second, first, "prerequisite").unwrap();
    assert!(pt::start_task(&conn, "test", second, None, Some("b")).is_err());
    pt::start_task(&conn, "test", first, None, Some("a")).unwrap();
    let before = serde_json::to_value(pt::export(&conn, None).unwrap()).unwrap();
    pt::start_task(&conn, "test", first, None, Some("a")).unwrap();
    assert_eq!(
        before,
        serde_json::to_value(pt::export(&conn, None).unwrap()).unwrap()
    );
    pt::add_task_blocker(&conn, "test", first, "input", "missing input").unwrap();
    pt::start_task(&conn, "test", first, None, Some("b")).unwrap();
    assert!(pt::complete_task(&conn, "test", first, None).is_err());
    let focus = pt::focus(&conn, plan, 1, false, Some("b")).unwrap();
    assert_eq!(focus.projection.entries[0].readiness, "mine");
    assert!(!focus.projection.entries[0].blockers.is_empty());
    assert!(pt::start_task(&conn, "test", first, None, Some("bad session")).is_err());
    assert_eq!(
        pt::get_task(&conn, first)
            .unwrap()
            .pickup
            .unwrap()
            .session
            .as_deref(),
        Some("b")
    );
}

#[test]
fn pickup_survives_recovery_but_never_becomes_an_imported_lock() {
    let conn = Connection::open_in_memory().unwrap();
    let (_, first, _) = seed(&conn);
    pt::start_task(&conn, "test", first, None, Some("gone")).unwrap();
    let dump = serde_json::to_value(pt::export(&conn, None).unwrap()).unwrap();
    let restored = Connection::open_in_memory().unwrap();
    pt::init(&restored).unwrap();
    pt::import(
        &restored,
        "restore",
        &serde_json::from_value(dump.clone()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        pt::get_task(&conn, first).unwrap().pickup,
        pt::get_task(&restored, first).unwrap().pickup
    );
    pt::start_task(&restored, "fresh", first, None, Some("fresh")).unwrap();

    for (field, value) in [("at", "not-a-time"), ("session", "bad session")] {
        let empty = Connection::open_in_memory().unwrap();
        pt::init(&empty).unwrap();
        let mut corrupt = dump.clone();
        corrupt["tasks"][0]["pickup"][field] = value.into();
        assert!(pt::import(&empty, "restore", &serde_json::from_value(corrupt).unwrap()).is_err());
        assert!(pt::export(&empty, None).unwrap().tasks.is_empty());
    }
}

#[test]
fn concurrent_explicit_pickup_is_serialized_history_not_exclusive_execution() {
    let path = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "pickup-race-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let conn = pt::open_for_init(path.to_str().unwrap()).unwrap();
    let (_, first, _) = seed(&conn);
    let barrier = Arc::new(Barrier::new(2));
    let threads = ["a", "b"].map(|session| {
        let path = path.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let conn = pt::open_existing(path.to_str().unwrap()).unwrap();
            barrier.wait();
            pt::start_task(&conn, "same-harness", first, None, Some(session)).unwrap();
        })
    });
    for thread in threads {
        thread.join().unwrap();
    }
    let dump = pt::export(&conn, None).unwrap();
    let events: Vec<_> = dump
        .events
        .iter()
        .filter(|e| e.entity_seq == Some(first) && matches!(e.kind.as_str(), "status" | "pickup"))
        .collect();
    assert_eq!(events.len(), 2);
    let initial = &events[0].payload.as_ref().unwrap()["pickup"]["session"];
    let final_pickup = events[1].payload.as_ref().unwrap();
    assert_eq!(&final_pickup["before"]["session"], initial);
    assert_ne!(
        final_pickup["before"]["session"],
        final_pickup["after"]["session"]
    );
    assert_eq!(
        serde_json::to_value(pt::get_task(&conn, first).unwrap().pickup).unwrap(),
        final_pickup["after"]
    );
    assert!(pt::audit(&conn).unwrap().is_empty());
    drop(conn);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn migration_preserves_history_without_inventing_session_identity() {
    let conn = Connection::open_in_memory().unwrap();
    let (_, first, _) = seed(&conn);
    pt::start_task(&conn, "historical-actor", first, None, None).unwrap();
    let before = pt::export(&conn, None).unwrap().events.len();
    // Exact v9 task shape, built only inside this disposable test authority.
    conn.execute_batch(
        "ALTER TABLE tasks DROP COLUMN pickup_at;
        ALTER TABLE tasks DROP COLUMN pickup_session;
        UPDATE meta SET value='9' WHERE key='schema_version';",
    )
    .unwrap();
    assert!(matches!(
        pt::init(&conn).unwrap(),
        pt::InitOutcome::Migrated { from: 9, to: 10 }
    ));
    assert!(matches!(pt::init(&conn).unwrap(), pt::InitOutcome::Current));
    let task = pt::get_task(&conn, first).unwrap();
    assert_eq!(task.status, "in_progress");
    assert!(task.pickup.is_none());
    assert_eq!(pt::export(&conn, None).unwrap().events.len(), before);
    pt::start_task(&conn, "fresh", first, None, Some("new-session")).unwrap();
    assert!(pt::audit(&conn).unwrap().is_empty());
}
