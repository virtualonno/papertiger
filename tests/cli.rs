use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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

fn papertiger_with_stdin(db: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .arg("--db")
        .arg(db)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn papertiger");
    child
        .stdin
        .take()
        .expect("papertiger stdin")
        .write_all(input.as_bytes())
        .expect("write papertiger stdin");
    child.wait_with_output().expect("wait for papertiger")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "papertiger failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn command_help(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(args)
        .arg("--help")
        .output()
        .expect("run papertiger help");
    assert_success(&output);
    String::from_utf8(output.stdout).expect("papertiger help is UTF-8")
}

fn assert_no_internal_identity_keys(value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for forbidden in ["task_id", "plan_id", "parent_id", "replacement_task_id"] {
                assert!(
                    !object.contains_key(forbidden),
                    "public JSON leaked {forbidden}"
                );
            }
            for child in object.values() {
                assert_no_internal_identity_keys(child);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                assert_no_internal_identity_keys(child);
            }
        }
        _ => {}
    }
}

#[test]
fn setup_project_refuses_planning_globals_before_writing() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let project = std::env::temp_dir().join(format!(
        "papertiger-setup-global-refusal-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&project).expect("create empty setup target");
    let ignored_db = project.join("ignored.sqlite");

    let db = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .arg("--db")
        .arg(&ignored_db)
        .arg("setup-project")
        .arg(&project)
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("run setup-project with --db");
    assert!(!db.status.success());
    let error = String::from_utf8_lossy(&db.stderr);
    assert!(
        error.contains("omit --db") && error.contains("--authority-path"),
        "{error}"
    );
    assert!(
        std::fs::read_dir(&project)
            .expect("inspect setup target")
            .next()
            .is_none(),
        "--db refusal must precede every setup write"
    );

    let actor = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .arg("setup-project")
        .arg(&project)
        .arg("--actor")
        .arg("test-agent")
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("run setup-project with --actor");
    assert!(!actor.status.success());
    let error = String::from_utf8_lossy(&actor.stderr);
    assert!(
        error.contains("records no planning events") && error.contains("omit --actor"),
        "{error}"
    );
    assert!(
        std::fs::read_dir(&project)
            .expect("inspect setup target")
            .next()
            .is_none(),
        "--actor refusal must precede every setup write"
    );

    std::fs::remove_dir(&project).expect("remove empty setup target");
}

#[test]
fn planner_help_describes_nested_commands_and_important_arguments() {
    let setup = command_help(&["setup-project"]);
    assert!(setup.contains("invalid with setup-project"), "{setup}");

    let plan = command_help(&["plan"]);
    for description in [
        "Create a plan for durable work",
        "List every plan with its current status",
        "Edit plan orientation without replacing its task/event history",
        "Set plan status",
    ] {
        assert!(
            plan.contains(description),
            "missing {description:?}:\n{plan}"
        );
    }

    let gate = command_help(&["gate"]);
    for description in [
        "Add a named proof obligation",
        "Close an open gate with an evidence locator",
        "Waive an open gate with durable rationale",
        "Reopen a closed or waived gate",
        "Remove an open gate",
        "List every gate on one task",
    ] {
        assert!(
            gate.contains(description),
            "missing {description:?}:\n{gate}"
        );
    }

    let blocker = command_help(&["blocker"]);
    for description in [
        "Add a named external blocker",
        "Resolve an open blocker with external evidence",
        "Waive an open blocker with durable rationale",
        "Remove an open blocker",
        "List every blocker on one task",
    ] {
        assert!(
            blocker.contains(description),
            "missing {description:?}:\n{blocker}"
        );
    }

    for (group, descriptions) in [
        (
            "dep",
            [
                "Make one task depend on another task",
                "Remove a dependency edge",
            ],
        ),
        (
            "tag",
            ["Add a searchable tag to a task", "Remove a tag from a task"],
        ),
    ] {
        let help = command_help(&[group]);
        for description in descriptions {
            assert!(
                help.contains(description),
                "missing {description:?}:\n{help}"
            );
        }
    }

    let task_add = command_help(&["add"]);
    assert!(
        task_add.contains("Concise outcome-oriented task title")
            && task_add.contains("Parent task sequence")
            && task_add.contains("Scheduling priority"),
        "{task_add}"
    );
    let gate_add = command_help(&["gate", "add"]);
    assert!(
        gate_add.contains("Task sequence that owns the gate")
            && gate_add.contains("Exact condition required to close the gate"),
        "{gate_add}"
    );
    let commit_add = command_help(&["commit", "add"]);
    assert!(
        commit_add.contains("Stable repository label")
            && commit_add.contains("Optional context explaining why this snapshot is useful"),
        "{commit_add}"
    );
    let mise_project = command_help(&["mise", "project"]);
    assert!(
        mise_project.contains("Task sequence that owns the projection")
            && mise_project.contains("Mise inspector pipeline from stdin"),
        "{mise_project}"
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
    assert_eq!(value["schema"], "papertiger.focus.v4");
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
fn status_show_and_log_do_not_panic_on_noncanonical_short_or_multibyte_timestamps() {
    let db = TestDatabase::new("noncanonical-event-time");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "campaign", "Campaign"]));
    assert_success(&papertiger(&db.0, &["add", "task", "--plan", "campaign"]));
    assert_success(&papertiger(
        &db.0,
        &["note", "historical timestamp", "--task", "1"],
    ));
    let connection = rusqlite::Connection::open(&db.0).expect("open test authority");
    connection
        .execute("UPDATE events SET at='é'", [])
        .expect("simulate a malformed historical import");
    drop(connection);

    for args in [
        &["status"][..],
        &["show", "1"][..],
        &["log", "--task", "1"][..],
    ] {
        assert_success(&papertiger(&db.0, args));
    }
}

#[test]
fn commit_lookup_lifecycle_json_and_activity_sort_are_agent_usable() {
    let db = TestDatabase::new("commit-and-time");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(&db.0, &["add", "first", "--plan", "work"]));
    assert_success(&papertiger(&db.0, &["add", "second", "--plan", "work"]));
    let oid = "b".repeat(40);
    assert_success(&papertiger(
        &db.0,
        &[
            "commit",
            "add",
            "1",
            &oid,
            "--repo",
            "nested/component",
            "--note",
            "local snapshot",
        ],
    ));

    let show = papertiger(&db.0, &["show", "1", "--json"]);
    assert_success(&show);
    let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(value["schema"], "papertiger.task_context.v4");
    assert!(value["activity"]["created_event"]["at"].is_string());
    assert!(value["activity"]["last_event"]["at"].is_string());
    assert_eq!(value["commit_associations"][0]["commit_oid"], oid);

    let find = papertiger(&db.0, &["commit", "find", &oid, "--json"]);
    assert_success(&find);
    let matches: serde_json::Value = serde_json::from_slice(&find.stdout).unwrap();
    assert_eq!(matches[0]["task_seq"], 1);

    let list = papertiger(&db.0, &["list", "--plan", "work", "--sort", "activity"]);
    assert_success(&list);
    let text = String::from_utf8(list.stdout).unwrap();
    assert!(text.lines().next().unwrap().contains("#1 first"));

    let short = papertiger(&db.0, &["commit", "add", "1", "abc1234"]);
    assert!(!short.status.success());
    assert!(
        String::from_utf8_lossy(&short.stderr).contains("git rev-parse --verify 'HEAD^{commit}'")
    );

    assert_success(&papertiger(
        &db.0,
        &[
            "commit",
            "remove",
            "1",
            &oid,
            "--repo",
            "nested/component",
            "--why",
            "association was only a command fixture",
        ],
    ));
    let removed_spelling = papertiger(&db.0, &["commit", "rm", "1", &oid, "--why", "old"]);
    assert!(!removed_spelling.status.success());
    assert!(String::from_utf8_lossy(&removed_spelling.stderr).contains("unrecognized subcommand"));
}

#[test]
fn long_text_files_and_stdin_round_trip_without_losing_intent_clear() {
    let db = TestDatabase::new("long-text-round-trip");
    let intent_file = db.0.with_extension("intent.txt");
    let result_file = db.0.with_extension("result.txt");
    let note_file = db.0.with_extension("note.txt");
    std::fs::write(
        &intent_file,
        b"\xef\xbb\xbf  first line\r\nsecond line\r\n  ",
    )
    .unwrap();
    std::fs::write(&result_file, "  observed\nverified\n  ").unwrap();
    std::fs::write(&note_file, "  durable\ncontext\n  ").unwrap();
    let intent_path = intent_file.to_string_lossy();
    let result_path = result_file.to_string_lossy();
    let note_path = note_file.to_string_lossy();

    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(
        &db.0,
        &[
            "plan",
            "add",
            "campaign",
            "Campaign",
            "--intent-file",
            &intent_path,
        ],
    ));
    let export = papertiger(&db.0, &["export", "--plan", "campaign"]);
    assert_success(&export);
    let exported: serde_json::Value = serde_json::from_slice(&export.stdout).unwrap();
    assert_eq!(exported["plans"][0]["intent"], "first line\r\nsecond line");

    assert_success(&papertiger_with_stdin(
        &db.0,
        &[
            "add",
            "probe",
            "--plan",
            "campaign",
            "--kind",
            "probe",
            "--intent-file",
            "-",
        ],
        "\u{feff}  stdin intent\r\nwith detail\r\n  ",
    ));

    let show = papertiger(&db.0, &["show", "1", "--json"]);
    assert_success(&show);
    let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(value["task"]["intent"], "stdin intent\r\nwith detail");

    assert_success(&papertiger(
        &db.0,
        &[
            "edit",
            "1",
            "--intent",
            "",
            "--why",
            "clear obsolete orientation",
        ],
    ));
    let show = papertiger(&db.0, &["show", "1", "--json"]);
    assert_success(&show);
    let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(value["task"]["intent"], "");

    let missing_result = papertiger(&db.0, &["done", "1"]);
    assert!(!missing_result.status.success());
    assert!(
        String::from_utf8_lossy(&missing_result.stderr)
            .contains("requires --result or --result-file")
    );
    assert_success(&papertiger(
        &db.0,
        &["done", "1", "--result-file", &result_path],
    ));
    assert_success(&papertiger(&db.0, &["note", "positional note"]));
    assert_success(&papertiger(
        &db.0,
        &["note", "--text-file", &note_path, "--task", "1"],
    ));
    assert_success(&papertiger_with_stdin(
        &db.0,
        &["note", "--text-file", "-", "--task", "1"],
        "  stdin note\n  ",
    ));

    let show = papertiger(&db.0, &["show", "1", "--json"]);
    assert_success(&show);
    let value: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(value["task"]["result"], "observed\nverified");

    for path in [intent_file, result_file, note_file] {
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn long_text_sources_refuse_ambiguous_missing_and_blank_input() {
    let db = TestDatabase::new("long-text-refusals");
    let empty_file = db.0.with_extension("empty.txt");
    std::fs::write(&empty_file, "  \n").unwrap();
    let empty_path = empty_file.to_string_lossy();
    let missing_file = db.0.with_extension("missing.txt");
    let missing_path = missing_file.to_string_lossy();
    let utf16_file = db.0.with_extension("utf16.txt");
    std::fs::write(
        &utf16_file,
        [
            0xff, 0xfe, b'n', 0, b'o', 0, b't', 0, b' ', 0, b'u', 0, b't', 0, b'f', 0, b'8', 0,
        ],
    )
    .unwrap();
    let utf16_path = utf16_file.to_string_lossy();

    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(&db.0, &["add", "first", "--plan", "work"]));

    let ambiguous = papertiger_with_stdin(
        &db.0,
        &[
            "add",
            "ambiguous",
            "--plan",
            "work",
            "--intent-file",
            "-",
            "--why-file",
            "-",
        ],
        "one stream",
    );
    assert!(!ambiguous.status.success());
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr).contains("stdin can supply only one text field")
    );

    let conflict = papertiger(
        &db.0,
        &[
            "add",
            "conflict",
            "--plan",
            "work",
            "--intent",
            "inline",
            "--intent-file",
            &empty_path,
        ],
    );
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("cannot be used with"));

    let blank = papertiger(&db.0, &["retire", "1", "--why-file", &empty_path]);
    assert!(!blank.status.success());
    assert!(String::from_utf8_lossy(&blank.stderr).contains("requires nonblank text"));

    let missing = papertiger(&db.0, &["note", "--text-file", &missing_path]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("read --text-file"));

    let utf16 = papertiger(
        &db.0,
        &[
            "add",
            "invalid encoding",
            "--plan",
            "work",
            "--intent-file",
            &utf16_path,
        ],
    );
    assert!(!utf16.status.success());
    assert!(String::from_utf8_lossy(&utf16.stderr).contains("valid UTF-8"));
    assert!(!papertiger(&db.0, &["show", "2"]).status.success());

    for path in [empty_file, utf16_file] {
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn long_text_help_names_utf8_files_and_stdin() {
    let add = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["add", "--help"])
        .output()
        .unwrap();
    assert_success(&add);
    let add = String::from_utf8(add.stdout).unwrap();
    assert!(add.contains("--intent-file <PATH|->"), "{add}");
    assert!(
        add.contains("Read durable orientation as UTF-8 from PATH, or stdin with '-'"),
        "{add}"
    );

    let note = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(["note", "--help"])
        .output()
        .unwrap();
    assert_success(&note);
    let note = String::from_utf8(note.stdout).unwrap();
    assert!(note.contains("--text-file <PATH|->"), "{note}");
    assert!(
        note.contains("Read the note as UTF-8 from PATH, or stdin with '-'"),
        "{note}"
    );
}

#[test]
fn retire_into_is_visible_without_redirecting_show() {
    let db = TestDatabase::new("retire-into");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(&db.0, &["add", "duplicate", "--plan", "work"]));
    assert_success(&papertiger(&db.0, &["add", "canonical", "--plan", "work"]));

    let retirement = papertiger(
        &db.0,
        &[
            "retire",
            "1",
            "--into",
            "2",
            "--why",
            "the canonical task carries the outcome",
        ],
    );
    assert_success(&retirement);
    assert_eq!(
        String::from_utf8_lossy(&retirement.stdout).trim(),
        "#1 retired into #2"
    );

    let text = papertiger(&db.0, &["show", "1"]);
    assert_success(&text);
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.lines().next().unwrap().contains("#1 duplicate"));
    assert!(
        text.contains("replacement ") && text.contains("#2 canonical"),
        "{text}"
    );

    let json = papertiger(&db.0, &["show", "1", "--json"]);
    assert_success(&json);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["schema"], "papertiger.task_context.v4");
    assert_eq!(value["task"]["seq"], 1);
    assert_eq!(value["replacement"]["seq"], 2);
    assert_eq!(value["recent_events"][0]["payload"]["replacement_seq"], 2);
    assert_eq!(
        value["recent_events"][0]["why"],
        "the canonical task carries the outcome"
    );

    let log = papertiger(&db.0, &["log", "--task", "1"]);
    assert_success(&log);
    let log = String::from_utf8(log.stdout).unwrap();
    assert!(log.contains("\"replacement_seq\":2"), "{log}");
    assert!(
        log.contains("the canonical task carries the outcome"),
        "{log}"
    );

    let exported = papertiger(&db.0, &["export", "--plan", "work"]);
    assert_success(&exported);
    let dump_path = PathBuf::from(format!("{}.dump.json", db.0.display()));
    std::fs::write(&dump_path, &exported.stdout).unwrap();
    let restored = TestDatabase::new("retire-into-restored");
    assert_success(&papertiger(&restored.0, &["init"]));
    assert_success(&papertiger(
        &restored.0,
        &["import", dump_path.to_str().unwrap()],
    ));
    let restored_log = papertiger(&restored.0, &["log", "--task", "1"]);
    assert_success(&restored_log);
    let restored_log = String::from_utf8(restored_log.stdout).unwrap();
    assert!(
        restored_log.contains("\"replacement_seq\":2")
            && restored_log.contains("the canonical task carries the outcome"),
        "{restored_log}"
    );
    let restored_show = papertiger(&restored.0, &["show", "1", "--json"]);
    assert_success(&restored_show);
    let restored_value: serde_json::Value = serde_json::from_slice(&restored_show.stdout).unwrap();
    assert_eq!(restored_value["replacement"]["seq"], 2);
    assert!(
        restored_value["recent_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |event| event["why"] == "the canonical task carries the outcome"
                    && event["payload"]["replacement_seq"] == 2
            )
    );
    std::fs::remove_file(dump_path).unwrap();

    let stranded_reject = papertiger(
        &db.0,
        &[
            "reject",
            "2",
            "--why",
            "would strand inbound replacement history",
        ],
    );
    assert!(!stranded_reject.status.success());
    assert!(
        String::from_utf8_lossy(&stranded_reject.stderr)
            .contains("papertiger retire 2 --into <task>")
    );
    let stranded_retire = papertiger(
        &db.0,
        &[
            "retire",
            "2",
            "--why",
            "would strand inbound replacement history",
        ],
    );
    assert!(!stranded_retire.status.success());
    assert!(
        String::from_utf8_lossy(&stranded_retire.stderr)
            .contains("papertiger retire 2 --into <task>")
    );
    assert_success(&papertiger(
        &db.0,
        &["add", "final canonical", "--plan", "work"],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "retire",
            "2",
            "--into",
            "3",
            "--why",
            "extend the replacement chain",
        ],
    ));
    let chained = papertiger(&db.0, &["show", "2", "--json"]);
    assert_success(&chained);
    let chained: serde_json::Value = serde_json::from_slice(&chained.stdout).unwrap();
    assert_eq!(chained["replacement"]["seq"], 3);
    let audit = papertiger(&db.0, &["audit"]);
    assert_success(&audit);
    assert_eq!(String::from_utf8_lossy(&audit.stdout).trim(), "no findings");

    let reject = papertiger(
        &db.0,
        &["reject", "2", "--into", "1", "--why", "not supported"],
    );
    assert!(!reject.status.success());
    assert!(String::from_utf8_lossy(&reject.stderr).contains("unexpected argument '--into'"));
}

#[test]
fn structured_reads_search_cursors_and_recovery_export_are_cli_usable() {
    let db = TestDatabase::new("structured-reads");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(
        &db.0,
        &["plan", "add", "work", "Structured work"],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "Object store recovery",
            "--plan",
            "work",
            "--intent",
            "preserve exact evidence",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &["add", "Continue implementation", "--plan", "work"],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "--actor",
            "ended-session",
            "start",
            "2",
            "--why",
            "begin durable work",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "--actor",
            "fresh-session",
            "note",
            "continued from live state",
            "--task",
            "2",
        ],
    ));

    let status = papertiger(&db.0, &["status", "--json"]);
    assert_success(&status);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["schema"], "papertiger.status.v1");
    assert_eq!(status["authority"]["schema_version"], 6);
    assert!(
        status["authority"]["resolved_path"]
            .as_str()
            .unwrap()
            .contains("papertiger-structured-reads")
    );
    assert_eq!(
        status["active_plans"][0]["in_progress"][0]["activity"]["started_event"]["actor"],
        "ended-session"
    );
    assert_eq!(
        status["active_plans"][0]["in_progress"][0]["activity"]["last_event"]["actor"],
        "fresh-session"
    );
    assert_no_internal_identity_keys(&status);

    let list = papertiger(&db.0, &["list", "--plan", "work", "--json"]);
    assert_success(&list);
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(list["schema"], "papertiger.task_list.v1");
    assert_eq!(list["tasks"].as_array().map(Vec::len), Some(2));
    assert_no_internal_identity_keys(&list);

    let search = papertiger(&db.0, &["search", "object store", "--json"]);
    assert_success(&search);
    let search: serde_json::Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["schema"], "papertiger.search.v1");
    assert_eq!(search["results"][0]["task"]["seq"], 1);
    assert_eq!(search["results"][0]["excerpt"]["field"], "title");
    assert_no_internal_identity_keys(&search);

    let latest = papertiger(&db.0, &["log", "--limit", "2", "--json"]);
    assert_success(&latest);
    let latest: serde_json::Value = serde_json::from_slice(&latest.stdout).unwrap();
    assert_eq!(latest["schema"], "papertiger.event_log.v1");
    assert_eq!(latest["events"].as_array().map(Vec::len), Some(2));
    assert_eq!(latest["truncated"], true);
    let older_cursor = latest["continuation"]["token"].as_str().unwrap();
    let older = papertiger(&db.0, &["log", "--before-cursor", older_cursor, "--json"]);
    assert_success(&older);
    let head_cursor = latest["head"]["token"].as_str().unwrap().to_owned();
    assert_success(&papertiger(
        &db.0,
        &["note", "event after cursor", "--task", "1"],
    ));
    let incremental = papertiger(&db.0, &["log", "--after-cursor", &head_cursor, "--json"]);
    assert_success(&incremental);
    let incremental: serde_json::Value = serde_json::from_slice(&incremental.stdout).unwrap();
    assert_eq!(incremental["direction"], "after");
    assert_eq!(incremental["events"].as_array().map(Vec::len), Some(1));
    assert_eq!(incremental["events"][0]["why"], "event after cursor");

    let export_path = PathBuf::from(format!("{}.recovery.json", db.0.display()));
    let export = papertiger(
        &db.0,
        &["export", "--output", export_path.to_str().unwrap()],
    );
    assert_success(&export);
    let receipt: serde_json::Value = serde_json::from_slice(&export.stdout).unwrap();
    assert_eq!(receipt["schema"], "papertiger.export_file.v1");
    assert_eq!(receipt["dump_schema"], "papertiger.dump.v6");
    let dump: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&export_path).unwrap()).unwrap();
    assert_eq!(dump["schema"], "papertiger.dump.v6");
    let same_authority = papertiger(
        &db.0,
        &["export", "--output", db.0.to_str().unwrap(), "--replace"],
    );
    assert!(!same_authority.status.success());
    assert!(String::from_utf8_lossy(&same_authority.stderr).contains("live authority"));
    std::fs::remove_file(export_path).unwrap();

    let root_help = command_help(&[]);
    assert!(root_help.contains("search"));
    assert!(
        !root_help
            .lines()
            .any(|line| line.trim_start().starts_with("next "))
    );
}
