use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "papertiger-{label}-{}-{nonce}.sqlite",
            std::process::id()
        )))
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{}", self.0.display(), suffix));
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("remove Papertiger test database sidecar: {error}"),
            }
        }
    }
}

fn papertiger(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .expect("run papertiger")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "papertiger failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn focus_json_reports_a_structured_empty_selection_for_a_paused_plan() {
    let db = TestDatabase::new("focus-no-active-plan");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(
        &db.0,
        &["plan", "add", "paused-plan", "Paused plan"],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "plan",
            "set",
            "paused-plan",
            "paused",
            "--why",
            "production campaign complete",
        ],
    ));

    let output = papertiger(&db.0, &["focus", "--json"]);
    assert_success(&output);
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse focus JSON");
    assert_eq!(value["schema"], "papertiger.focus.v3");
    assert_eq!(value["selection_state"], "no_active_plan");
    assert!(value["plan"].is_null());
    assert_eq!(value["entries"].as_array().map(Vec::len), Some(0));
}

#[test]
fn plan_edit_updates_exported_orientation() {
    let db = TestDatabase::new("plan-edit");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(
        &db.0,
        &[
            "plan",
            "add",
            "campaign",
            "Old title",
            "--intent",
            "Old intent",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "plan",
            "edit",
            "campaign",
            "--title",
            "Current title",
            "--intent",
            "Current intent",
            "--why",
            "live scope changed",
        ],
    ));

    let output = papertiger(&db.0, &["export"]);
    assert_success(&output);
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse export JSON");
    assert_eq!(value["plans"][0]["title"], "Current title");
    assert_eq!(value["plans"][0]["intent"], "Current intent");
}

#[test]
fn status_show_and_log_do_not_panic_on_legacy_short_or_multibyte_timestamps() {
    let db = TestDatabase::new("legacy-event-time");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "campaign", "Campaign"]));
    assert_success(&papertiger(&db.0, &["add", "task", "--plan", "campaign"]));
    assert_success(&papertiger(
        &db.0,
        &["note", "legacy timestamp", "--task", "1"],
    ));
    let connection = rusqlite::Connection::open(&db.0).expect("open test authority");
    connection
        .execute("UPDATE events SET at='é'", [])
        .expect("simulate a legacy malformed import");
    drop(connection);

    for args in [
        &["status"][..],
        &["show", "1"][..],
        &["log", "--task", "1"][..],
    ] {
        assert_success(&papertiger(&db.0, args));
    }
}
