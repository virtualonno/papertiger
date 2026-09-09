use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn mutation_receipts_model_attribution_and_task_moves_are_cli_usable() {
    let db = TestDatabase::new("mutation-receipts");
    assert_success(&papertiger(&db.0, &["init"]));
    for plan in ["old", "new"] {
        let output = papertiger(&db.0, &["plan", "add", plan, plan, "--json"]);
        assert_success(&output);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema"], "papertiger.mutation.v1");
        assert_eq!(value["events"][0]["plan"]["slug"], plan);
    }
    let output = papertiger(
        &db.0,
        &[
            "add",
            "a proposal",
            "--plan",
            "old",
            "--model",
            "gpt-5.6-luna",
            "--json",
        ],
    );
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let seq = value["events"][0]["task"]["seq"]
        .as_i64()
        .unwrap()
        .to_string();
    assert_eq!(value["events"][0]["event"]["model"], "gpt-5.6-luna");
    for args in [
        vec![
            "reference",
            "add",
            &seq,
            "https://example.test/issue/1",
            "--kind",
            "issue",
            "--json",
        ],
        vec![
            "move-plan",
            &seq,
            "--plan",
            "new",
            "--why",
            "correct initiative",
            "--json",
        ],
        vec![
            "done",
            &seq,
            "--result",
            "verified outcome",
            "--model",
            "review-model",
            "--json",
        ],
    ] {
        let output = papertiger(&db.0, &args);
        assert_success(&output);
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["changed"], true);
    }
    let output = papertiger(&db.0, &["show", &seq, "--json"]);
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["plan"]["slug"], "new");
    assert_eq!(
        context["activity"]["created_event"]["model"],
        "gpt-5.6-luna"
    );
    assert_eq!(
        context["activity"]["completed_event"]["model"],
        "review-model"
    );
    let failed = papertiger(
        &db.0,
        &[
            "move-plan",
            &seq,
            "--plan",
            "new",
            "--why",
            "no-op",
            "--json",
        ],
    );
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    let invalid = papertiger(
        &db.0,
        &[
            "add",
            "invalid model",
            "--plan",
            "old",
            "--model",
            " ",
            "--json",
        ],
    );
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
}

struct TestDatabase(PathBuf);

#[test]
fn reasoning_effort_uses_environment_and_explicit_flags_without_inference() {
    let db = TestDatabase::new("reasoning-effort");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    let run = |args: &[&str], model: Option<&str>, effort: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_papertiger"));
        command
            .arg("--db")
            .arg(&db.0)
            .args(args)
            .env_remove("PAPERTIGER_MODEL")
            .env_remove("PAPERTIGER_REASONING_EFFORT");
        if let Some(model) = model {
            command.env("PAPERTIGER_MODEL", model);
        }
        if let Some(effort) = effort {
            command.env("PAPERTIGER_REASONING_EFFORT", effort);
        }
        command.output().unwrap()
    };
    let created = run(
        &["add", "precise identity", "--json"],
        Some("gpt-6-astra"),
        Some("high"),
    );
    assert_success(&created);
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let seq = created["events"][0]["task"]["seq"]
        .as_i64()
        .unwrap()
        .to_string();
    assert_eq!(created["events"][0]["event"]["model"], "gpt-6-astra");
    assert_eq!(created["events"][0]["event"]["reasoning_effort"], "high");
    let edited = run(
        &[
            "edit",
            &seq,
            "--title",
            "updated identity",
            "--why",
            "exercise a different event author",
            "--model",
            "other-model",
            "--reasoning-effort",
            "low",
            "--json",
        ],
        Some("gpt-6-astra"),
        Some("high"),
    );
    assert_success(&edited);
    let edited: serde_json::Value = serde_json::from_slice(&edited.stdout).unwrap();
    assert_eq!(edited["events"][0]["event"]["model"], "other-model");
    assert_eq!(edited["events"][0]["event"]["reasoning_effort"], "low");
    assert_success(&run(
        &["done", &seq, "--result", "verified fixture"],
        Some("gpt-6-astra"),
        Some("medium"),
    ));
    let shown = run(&["show", &seq, "--json"], None, None);
    assert_success(&shown);
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(
        shown["activity"]["created_event"]["reasoning_effort"],
        "high"
    );
    assert_eq!(
        shown["activity"]["completed_event"]["reasoning_effort"],
        "medium"
    );
    let known_model = run(
        &["add", "unknown effort", "--json"],
        Some("gpt-6-astra"),
        None,
    );
    assert_success(&known_model);
    let known_model: serde_json::Value = serde_json::from_slice(&known_model.stdout).unwrap();
    assert!(known_model["events"][0]["event"]["reasoning_effort"].is_null());
    let before = run(&["export"], None, None);
    assert_success(&before);
    for output in [
        run(
            &["add", "orphan", "--reasoning-effort", "high", "--json"],
            None,
            None,
        ),
        run(
            &["add", "invalid", "--json"],
            Some("gpt-6-astra"),
            Some("very high"),
        ),
        run(
            &["show", &seq, "--reasoning-effort", "high", "--json"],
            None,
            None,
        ),
    ] {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let after = run(&["export"], None, None);
    assert_success(&after);
    assert_eq!(before.stdout, after.stdout);
    // Inherited metadata must not affect ordinary read-only operations.
    assert_success(&run(
        &["show", &seq],
        Some("invalid model"),
        Some("invalid effort"),
    ));
}

#[test]
fn backup_preserves_legacy_schema_and_committed_wal_without_importing_evidence() {
    let directory = TestDirectory::new("backup-wal");
    let source = directory.0.join("source.sqlite");
    let output = directory.0.join("recovery.sqlite");
    for args in [
        vec!["init"],
        vec!["plan", "add", "work", "Work"],
        vec!["add", "historical outcome", "--plan", "work"],
        vec![
            "gate",
            "add",
            "1",
            "proof",
            "--kind",
            "test",
            "--requirement",
            "historical evidence",
        ],
    ] {
        assert_success(&papertiger(&source, &args));
    }
    let writer = rusqlite::Connection::open(&source).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        DROP TABLE external_references;
        UPDATE meta SET value='8' WHERE key='schema_version';
        UPDATE gates SET status='closed', evidence_locator='commit:ca9ff90';
        BEGIN IMMEDIATE;
        UPDATE tasks SET title='uncommitted change';",
        )
        .unwrap();
    let wal = source.with_file_name("source.sqlite-wal");
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    let before = std::fs::read(&source).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    let backup = papertiger(
        &source,
        &["backup", "--output", output.to_str().unwrap(), "--json"],
    );
    assert_success(&backup);
    let receipt: serde_json::Value = serde_json::from_slice(&backup.stdout).unwrap();
    assert_eq!(receipt["schema"], "papertiger.backup.v1");
    assert_eq!(receipt["source_schema_version"], 8);
    assert_eq!(receipt["tasks"], 1);
    assert_eq!(receipt["semantic_validation"], "not_performed");
    assert_eq!(
        receipt["sha256"],
        papertiger::sha256(&std::fs::read(&output).unwrap())
    );
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    for suffix in ["-journal", "-wal", "-shm"] {
        assert!(
            !directory
                .0
                .join(format!("recovery.sqlite{suffix}"))
                .exists()
        );
    }
    let recovered =
        rusqlite::Connection::open_with_flags(&output, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    assert_eq!(
        recovered
            .query_row("SELECT title FROM tasks", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "historical outcome"
    );
    assert_eq!(
        recovered
            .query_row("SELECT evidence_locator FROM gates", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "commit:ca9ff90"
    );
    assert_eq!(
        recovered
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        receipt["events"].as_i64().unwrap()
    );
    assert!(!papertiger(&output, &["status"]).status.success());
    drop(recovered);
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    assert_success(&papertiger(&output, &["init"]));
    assert_success(&papertiger(&output, &["status", "--json"]));
    // Recovery deliberately preserves a historical locator that semantic import refuses.
    let recovered = papertiger::open_existing_read_only(output.to_str().unwrap()).unwrap();
    let dump = papertiger::export(&recovered, None).unwrap();
    let fresh = rusqlite::Connection::open_in_memory().unwrap();
    papertiger::init(&fresh).unwrap();
    assert!(
        papertiger::import(&fresh, "test", &dump)
            .unwrap_err()
            .to_string()
            .contains("invalid evidence_locator")
    );
}

#[test]
fn backup_refuses_existing_destinations_and_sidecars_without_touching_them() {
    let directory = TestDirectory::new("backup-paths");
    let source = directory.0.join("source.sqlite");
    assert_success(&papertiger(&source, &["init"]));
    let source_before = std::fs::read(&source).unwrap();
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let output = directory.0.join(format!("out{suffix}.sqlite"));
        let occupied = directory.0.join(format!("out{suffix}.sqlite{suffix}"));
        std::fs::write(&occupied, b"preserve existing bytes").unwrap();
        let result = papertiger(
            &source,
            &["backup", "--output", output.to_str().unwrap(), "--json"],
        );
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8_lossy(&result.stderr).contains("choose a new --output path"));
        assert_eq!(
            std::fs::read(&occupied).unwrap(),
            b"preserve existing bytes"
        );
        if !suffix.is_empty() {
            assert!(!output.exists());
        }
    }
    let alias = directory.0.join("source-alias.sqlite");
    std::fs::hard_link(&source, &alias).unwrap();
    assert!(
        !papertiger(&source, &["backup", "--output", alias.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(std::fs::read(&source).unwrap(), source_before);
    for suffix in ["-journal", "-wal", "-shm", "-JOURNAL", "-WAL", "-SHM"] {
        let sidecar = directory.0.join(format!("source.sqlite{suffix}"));
        let aliased_sidecar = directory
            .0
            .join("nested")
            .join("..")
            .join(sidecar.file_name().unwrap());
        let result = papertiger(
            &source,
            &["backup", "--output", aliased_sidecar.to_str().unwrap()],
        );
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("SQLite sidecar suffix"));
        assert!(!sidecar.exists());
    }
    assert_eq!(std::fs::read(&source).unwrap(), source_before);
}

#[test]
fn backup_refuses_missing_foreign_empty_and_unsupported_authorities() {
    let directory = TestDirectory::new("backup-admission");
    for fixture in ["missing", "empty", "foreign", "mise", "future", "corrupt"] {
        let source = directory.0.join(format!("{fixture}.sqlite"));
        let output = directory.0.join(format!("{fixture}-backup.sqlite"));
        match fixture {
            "empty" => {
                std::fs::write(&source, b"").unwrap();
            }
            "foreign" => {
                rusqlite::Connection::open(&source)
                    .unwrap()
                    .execute_batch("CREATE TABLE foreign_data (value TEXT)")
                    .unwrap();
            }
            "mise" | "future" => {
                assert_success(&papertiger(&source, &["init"]));
                let conn = rusqlite::Connection::open(&source).unwrap();
                conn.execute_batch(if fixture == "mise" {
                    "UPDATE meta SET value='papertiger.mise' WHERE key='authority'"
                } else {
                    "UPDATE meta SET value='999' WHERE key='schema_version'"
                })
                .unwrap();
            }
            "corrupt" => {
                std::fs::write(&source, b"not sqlite").unwrap();
            }
            _ => {}
        }
        let before = source.exists().then(|| std::fs::read(&source).unwrap());
        let result = papertiger(
            &source,
            &["backup", "--output", output.to_str().unwrap(), "--json"],
        );
        assert!(!result.status.success(), "{fixture}");
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8_lossy(&result.stderr).contains("backup --output <new-path>"));
        assert!(!output.exists());
        assert_eq!(
            source.exists().then(|| std::fs::read(&source).unwrap()),
            before
        );
    }
}

#[test]
fn backup_records_no_events_and_refuses_model_attribution() {
    let directory = TestDirectory::new("backup-events");
    let source = directory.0.join("source.sqlite");
    let output = directory.0.join("recovery.sqlite");
    assert_success(&papertiger(&source, &["init"]));
    let rejected = papertiger(
        &source,
        &[
            "backup",
            "--output",
            output.to_str().unwrap(),
            "--model",
            "invented",
        ],
    );
    assert!(!rejected.status.success());
    assert!(!output.exists());
    let before = papertiger(&source, &["export"]);
    assert_success(&before);
    assert_success(&papertiger(
        &source,
        &["backup", "--output", output.to_str().unwrap(), "--json"],
    ));
    assert_eq!(papertiger(&source, &["export"]).stdout, before.stdout);
    assert_eq!(papertiger(&output, &["export"]).stdout, before.stdout);
}

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

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("papertiger-{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove Papertiger test directory: {error}"),
        }
    }
}

fn installed_papertiger(project: &Path) -> PathBuf {
    project.join("tools/papertiger/bin").join(if cfg!(windows) {
        "papertiger.exe"
    } else {
        "papertiger"
    })
}

#[test]
fn add_start_is_atomic_and_rolls_back_every_refusal() {
    let db = TestDatabase::new("atomic-add-start");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(
        &db.0,
        &["add", "open dependency", "--plan", "work"],
    ));

    let missing_why = papertiger(
        &db.0,
        &["add", "missing rationale", "--plan", "work", "--start"],
    );
    assert!(!missing_why.status.success());
    assert!(String::from_utf8_lossy(&missing_why.stderr).contains("add --start requires --why"));
    let before_refusal = papertiger(&db.0, &["log", "--json"]);
    assert_success(&before_refusal);
    let before_refusal: serde_json::Value = serde_json::from_slice(&before_refusal.stdout).unwrap();

    let blocked = papertiger(
        &db.0,
        &[
            "add",
            "blocked atomic task",
            "--plan",
            "work",
            "--dep",
            "1",
            "--start",
            "--why",
            "exercise rollback",
        ],
    );
    assert!(!blocked.status.success());
    let blocked_stderr = String::from_utf8_lossy(&blocked.stderr);
    assert!(blocked_stderr.contains("task was not created"));
    assert!(blocked_stderr.contains("resolve dep:#1"));
    assert!(!blocked_stderr.contains("#2 is not ready"));
    let after_refusal = papertiger(&db.0, &["log", "--json"]);
    assert_success(&after_refusal);
    let after_refusal: serde_json::Value = serde_json::from_slice(&after_refusal.stdout).unwrap();
    assert_eq!(after_refusal["head"], before_refusal["head"]);
    assert_eq!(after_refusal["events"], before_refusal["events"]);

    let list = papertiger(&db.0, &["list", "--plan", "work", "--json"]);
    assert_success(&list);
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(list["tasks"].as_array().unwrap().len(), 1);

    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "atomic task",
            "--plan",
            "work",
            "--intent",
            "implement one durable outcome",
            "--intent-source",
            "user",
            "--start",
            "--why",
            "the outcome is ready",
        ],
    ));

    let show = papertiger(&db.0, &["show", "2", "--json"]);
    assert_success(&show);
    let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show["task"]["status"], "in_progress");
    assert_eq!(show["task"]["intent_source"], "user");
    let lifecycle = show["recent_events"].as_array().unwrap();
    assert_eq!(lifecycle.len(), 2);
    assert_eq!(lifecycle[0]["kind"], "status");
    assert_eq!(lifecycle[1]["kind"], "create");
    assert!(
        lifecycle
            .iter()
            .all(|event| event["why"] == "the outcome is ready")
    );
}

#[test]
fn meaning_provenance_is_correctable_visible_and_transferable() {
    let db = TestDatabase::new("meaning-provenance");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));

    let source_without_intent = papertiger(
        &db.0,
        &[
            "add",
            "invalid source",
            "--plan",
            "work",
            "--intent-source",
            "agent",
        ],
    );
    assert!(!source_without_intent.status.success());
    assert!(
        String::from_utf8_lossy(&source_without_intent.stderr)
            .contains("--intent-source requires --intent or --intent-file")
    );

    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "attributed outcome",
            "--plan",
            "work",
            "--intent",
            "operator requested outcome",
            "--intent-source",
            "user",
            "--start",
            "--why",
            "begin attributed work",
        ],
    ));
    let unproven_rewrite = papertiger(
        &db.0,
        &[
            "edit",
            "1",
            "--intent",
            "agent rewrote the stored meaning",
            "--why",
            "exercise explicit source requirement",
        ],
    );
    assert!(!unproven_rewrite.status.success());
    let stderr = String::from_utf8_lossy(&unproven_rewrite.stderr);
    assert!(stderr.contains("--intent-source <user|agent|external>"));
    assert!(stderr.contains("--clear-intent-source"));
    assert_success(&papertiger(
        &db.0,
        &[
            "edit",
            "1",
            "--intent-source",
            "external",
            "--why",
            "correct the source attribution",
        ],
    ));

    let unsafe_clear = papertiger(
        &db.0,
        &[
            "edit",
            "1",
            "--intent",
            "",
            "--why",
            "attempt to strand provenance",
        ],
    );
    assert!(!unsafe_clear.status.success());
    assert!(String::from_utf8_lossy(&unsafe_clear.stderr).contains("--clear-intent-source"));

    assert_success(&papertiger(
        &db.0,
        &[
            "note",
            "operator confirmed the boundary",
            "--task",
            "1",
            "--source",
            "user",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "done",
            "1",
            "--result",
            "verified implementation",
            "--result-source",
            "agent",
        ],
    ));

    let show = papertiger(&db.0, &["show", "1", "--json"]);
    assert_success(&show);
    let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show["task"]["intent_source"], "external");
    assert_eq!(show["task"]["result_source"], "agent");
    assert_eq!(
        show["recent_events"][1]["payload"]["meaning_source"],
        "user"
    );

    let human = papertiger(&db.0, &["show", "1"]);
    assert_success(&human);
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("intent [external]: operator requested outcome"));
    assert!(human.contains("result [agent]: verified implementation"));
    assert_eq!(
        human
            .matches("result [agent]: verified implementation")
            .count(),
        1
    );

    let export = papertiger(&db.0, &["export"]);
    assert_success(&export);
    let dump_path = db.0.with_extension("dump.json");
    std::fs::write(&dump_path, &export.stdout).unwrap();
    let restored = TestDatabase::new("meaning-provenance-restored");
    assert_success(&papertiger(&restored.0, &["init"]));
    let import = papertiger(&restored.0, &["import", dump_path.to_str().unwrap()]);
    assert_success(&import);
    std::fs::remove_file(dump_path).unwrap();
    let restored_show = papertiger(&restored.0, &["show", "1", "--json"]);
    assert_success(&restored_show);
    let restored_show: serde_json::Value = serde_json::from_slice(&restored_show.stdout).unwrap();
    assert_eq!(restored_show["task"]["intent_source"], "external");
    assert_eq!(restored_show["task"]["result_source"], "agent");

    assert_success(&papertiger(
        &restored.0,
        &[
            "edit",
            "1",
            "--clear-intent-source",
            "--why",
            "remove an incorrect attribution",
        ],
    ));
    let cleared = papertiger(&restored.0, &["show", "1", "--json"]);
    assert_success(&cleared);
    let cleared: serde_json::Value = serde_json::from_slice(&cleared.stdout).unwrap();
    assert_eq!(cleared["task"]["intent_source"], serde_json::Value::Null);
    assert_eq!(
        cleared["recent_events"][0]["payload"]["changes"]["intent_source"]["before"],
        "external"
    );
    assert_eq!(
        cleared["recent_events"][0]["payload"]["changes"]["intent_source"]["after"],
        serde_json::Value::Null
    );

    let connection = rusqlite::Connection::open(&restored.0).unwrap();
    connection
        .pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    connection
        .execute(
            "UPDATE tasks SET intent_source='fabricated' WHERE seq=1",
            [],
        )
        .unwrap();
    drop(connection);
    let audit = papertiger(&restored.0, &["audit"]);
    assert_success(&audit);
    assert!(String::from_utf8_lossy(&audit.stdout).contains("invalid_meaning_source"));
}

#[test]
fn entry_bounds_tag_identity_and_focus_projection_are_explicit() {
    let db = TestDatabase::new("entry-bounds-focus");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));

    let long_title = "x".repeat(papertiger::MAX_TASK_TITLE_CHARS + 1);
    let refused = papertiger(&db.0, &["add", &long_title, "--plan", "work"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("shorten it to at most 160 characters"),
        "{stderr}"
    );

    let blank_tag = papertiger(
        &db.0,
        &["add", "tagged", "--plan", "work", "--tag", "a,,  b"],
    );
    assert!(!blank_tag.status.success());
    let stderr = String::from_utf8_lossy(&blank_tag.stderr);
    assert!(stderr.contains("tag must not be blank"), "{stderr}");
    let list = papertiger(&db.0, &["list", "--plan", "work", "--json"]);
    assert_success(&list);
    let list: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(list["tasks"].as_array().unwrap().len(), 0);

    for title in ["first", "second", "third"] {
        assert_success(&papertiger(
            &db.0,
            &["add", title, "--plan", "work", "--tag", " alpha "],
        ));
    }
    let focus = papertiger(
        &db.0,
        &["focus", "--plan", "work", "--limit", "1", "--json"],
    );
    assert_success(&focus);
    let focus: serde_json::Value = serde_json::from_slice(&focus.stdout).unwrap();
    assert_eq!(focus["schema"], "papertiger.focus.v5");
    assert_eq!(focus["eligible_count"], 3);
    assert_eq!(focus["returned_count"], 1);
    assert_eq!(focus["omitted_count"], 2);
    assert_eq!(focus["complete"], false);
    assert!(
        focus["continuation_command"]
            .as_str()
            .unwrap()
            .contains("--limit 3")
    );
    let zero = papertiger(&db.0, &["focus", "--plan", "work", "--limit", "0"]);
    assert!(!zero.status.success());
    assert!(String::from_utf8_lossy(&zero.stderr).contains("focus --limit must be at least 1"));
}

#[test]
fn evidence_verification_is_read_only_and_fails_closed_on_byte_drift() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "papertiger-evidence-verification-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("state")).unwrap();
    std::fs::create_dir_all(root.join("docs/evidence")).unwrap();
    let db = TestDatabase(root.join("state/papertiger.sqlite"));
    let evidence = root.join("docs/evidence/proof.json");
    std::fs::write(&evidence, b"exact proof bytes\n").unwrap();
    let digest = papertiger::sha256(&std::fs::read(&evidence).unwrap());
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(
        &db.0,
        &["add", "verified task", "--plan", "work"],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "gate",
            "add",
            "1",
            "proof",
            "--kind",
            "capture",
            "--requirement",
            "exact retained bytes",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "gate",
            "close",
            "1",
            "proof",
            "--evidence",
            "file:docs/evidence/proof.json",
            "--sha256",
            &digest,
        ],
    ));
    let root_text = root.to_str().unwrap();
    let before = std::fs::read(&db.0).unwrap();
    let verified = papertiger(
        &db.0,
        &[
            "evidence",
            "verify",
            "--task",
            "1",
            "--project-root",
            root_text,
            "--outcome",
            "all",
            "--json",
        ],
    );
    assert_success(&verified);
    let verified: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified["summary"]["verification_complete"], true);
    assert_eq!(verified["projection"]["bindings"][0]["status"], "verified");
    assert_eq!(std::fs::read(&db.0).unwrap(), before);

    std::fs::write(&evidence, b"drifted proof bytes\n").unwrap();
    let drifted = papertiger(
        &db.0,
        &[
            "evidence",
            "verify",
            "--task",
            "1",
            "--project-root",
            root_text,
            "--json",
        ],
    );
    assert!(!drifted.status.success());
    let drifted_json: serde_json::Value = serde_json::from_slice(&drifted.stdout).unwrap();
    assert_eq!(
        drifted_json["projection"]["bindings"][0]["status"],
        "digest_mismatch"
    );
    assert_eq!(
        drifted_json["projection"]["bindings"][0]["corrective_commands"][0]["arguments"],
        serde_json::json!([
            "gate",
            "reopen",
            "1",
            "proof",
            "--why",
            "replace invalid evidence binding after papertiger evidence verify"
        ])
    );
    let close_arguments =
        drifted_json["projection"]["bindings"][0]["corrective_commands"][1]["arguments"]
            .as_array()
            .unwrap();
    assert_eq!(
        &close_arguments[..7],
        serde_json::json!([
            "gate",
            "close",
            "1",
            "proof",
            "--evidence",
            "file:docs/evidence/proof.json",
            "--sha256"
        ])
        .as_array()
        .unwrap()
    );
    assert_eq!(std::fs::read(&db.0).unwrap(), before);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn evidence_verification_json_is_summary_first_filtered_and_pageable() {
    let root = TestDirectory::new("evidence-pageable");
    std::fs::create_dir(root.0.join("state")).unwrap();
    let db = TestDatabase(root.0.join("state/papertiger.sqlite"));
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(
        &db.0,
        &["add", "mixed evidence", "--plan", "work"],
    ));
    for (name, locator) in [
        ("missing-a", "file:docs/missing-a.txt"),
        ("missing-b", "file:docs/missing-b.txt"),
        (
            "unsupported",
            "commit:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    ] {
        assert_success(&papertiger(
            &db.0,
            &[
                "gate",
                "add",
                "1",
                name,
                "--kind",
                "review",
                "--requirement",
                "retained evidence",
            ],
        ));
        assert_success(&papertiger(
            &db.0,
            &["gate", "close", "1", name, "--evidence", locator],
        ));
    }

    let root_text = root.0.to_str().unwrap();
    let first = papertiger(
        &db.0,
        &[
            "evidence",
            "verify",
            "--project-root",
            root_text,
            "--limit",
            "1",
            "--json",
        ],
    );
    assert!(!first.status.success());
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first["schema"], "papertiger.evidence_verification.v2");
    assert_eq!(first["summary"]["binding_count"], 3);
    assert_eq!(first["summary"]["failed_count"], 2);
    assert_eq!(first["summary"]["unsupported_count"], 1);
    assert_eq!(first["summary"]["status_counts"]["missing"], 2);
    assert_eq!(first["summary"]["unsupported_scheme_counts"]["commit"], 1);
    assert_eq!(first["projection"]["outcome"], "incomplete");
    assert_eq!(first["projection"]["eligible_count"], 3);
    assert_eq!(first["projection"]["returned_count"], 1);
    assert_eq!(first["projection"]["remaining_count"], 2);
    assert_eq!(first["projection"]["complete"], false);
    let continuation = &first["projection"]["continuation_command"];
    assert_eq!(continuation["program"], "papertiger");
    let arguments = continuation["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(arguments.windows(2).any(|pair| pair[0] == "--db"));
    assert!(arguments.windows(2).any(|pair| pair[0] == "--after-cursor"));
    assert!(arguments.ends_with(&["--json".to_owned()]));

    let second = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .args(&arguments)
        .output()
        .expect("run evidence continuation argv");
    assert!(!second.status.success());
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(second["projection"]["page_start"], 1);
    assert_eq!(second["projection"]["returned_count"], 1);
    assert_eq!(second["projection"]["remaining_count"], 1);

    let unsupported = papertiger(
        &db.0,
        &[
            "evidence",
            "verify",
            "--project-root",
            root_text,
            "--outcome",
            "unsupported",
            "--json",
        ],
    );
    assert!(!unsupported.status.success());
    let unsupported: serde_json::Value = serde_json::from_slice(&unsupported.stdout).unwrap();
    assert_eq!(unsupported["summary"]["failed_count"], 2);
    assert_eq!(unsupported["projection"]["eligible_count"], 1);
    assert_eq!(
        unsupported["projection"]["bindings"][0]["classification"],
        "unsupported"
    );

    let zero_limit = papertiger(
        &db.0,
        &[
            "evidence",
            "verify",
            "--project-root",
            root_text,
            "--limit",
            "0",
        ],
    );
    assert!(!zero_limit.status.success());
    assert!(
        String::from_utf8_lossy(&zero_limit.stderr)
            .contains("evidence verify --limit must be between 1 and 500")
    );
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

    for (flag, value, expected) in [
        ("--db", ignored_db.as_os_str(), "does not accept --db"),
        (
            "--actor",
            std::ffi::OsStr::new("test-agent"),
            "records no planning events",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_papertiger"))
            .arg(flag)
            .arg(value)
            .arg("uninstall-project")
            .arg(&project)
            .env_remove("PAPERTIGER_DB")
            .env_remove("PAPERTIGER_ACTOR")
            .output()
            .expect("run uninstall-project with planning global");
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(expected), "{error}");
    }

    std::fs::remove_dir(&project).expect("remove empty setup target");
}

#[test]
fn inspect_project_guidance_is_bounded_deterministic_and_authority_free() {
    let sandbox = TestDirectory::new("inspect-project-guidance");
    let project = sandbox.0.join("project");
    let outside = sandbox.0.join("outside");
    let nested = project.join("nested/deeper");
    std::fs::create_dir(&project).expect("create guidance project");
    std::fs::create_dir(&outside).expect("create outside directory");
    std::fs::create_dir_all(&nested).expect("create nested project directory");
    std::fs::write(
        project.join("AGENTS.md"),
        "Before the first edit on multi-outcome work, read `.agents/skills/papertiger/SKILL.md` completely and follow it.\n",
    )
    .expect("write AGENTS guidance");
    std::fs::write(project.join("CLAUDE.md"), "Follow `AGENTS.md`.\n")
        .expect("write CLAUDE guidance");

    let setup = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .arg("setup-project")
        .arg(&project)
        .args(["--skill-target", "both"])
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("install guidance fixture");
    assert_success(&setup);
    let binary = installed_papertiger(&project);

    let nested_result = Command::new(&binary)
        .args(["inspect-project-guidance", "--json"])
        .current_dir(&nested)
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("inspect guidance from nested directory");
    assert_success(&nested_result);
    let result: serde_json::Value =
        serde_json::from_slice(&nested_result.stdout).expect("parse guidance inspection");
    assert_eq!(result["schema"], "papertiger.project_guidance.v1");
    assert_eq!(
        result["scope"],
        "repository-root AGENTS.md and CLAUDE.md only"
    );
    assert_eq!(result["max_file_bytes"], 65_536);
    assert_eq!(result["inspection_complete"], true);
    assert_eq!(result["files"][0]["path"], "AGENTS.md");
    assert_eq!(
        result["files"][0]["classification"],
        "selected_skill_trigger"
    );
    assert_eq!(
        result["files"][0]["matched_selected_skill_paths"][0],
        ".agents/skills/papertiger/SKILL.md"
    );
    assert_eq!(result["files"][1]["path"], "CLAUDE.md");
    assert_eq!(
        result["files"][1]["classification"],
        "agents_guidance_reference"
    );
    assert_eq!(result["pair"]["byte_identical"], false);

    let explicit_result = Command::new(&binary)
        .arg("--project-root")
        .arg(&project)
        .args(["inspect-project-guidance", "--json"])
        .current_dir(&outside)
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("inspect guidance through exact project root");
    assert_success(&explicit_result);
    assert_eq!(nested_result.stdout, explicit_result.stdout);
    assert!(!project.join("state/papertiger.sqlite").exists());

    for (flag, value, expected) in [
        (
            "--db",
            "ignored.sqlite",
            "never opens the planning authority",
        ),
        ("--actor", "test-agent", "records no planning events"),
    ] {
        let refusal = Command::new(&binary)
            .arg(flag)
            .arg(value)
            .arg("--project-root")
            .arg(&project)
            .arg("inspect-project-guidance")
            .current_dir(&outside)
            .env_remove("PAPERTIGER_DB")
            .env_remove("PAPERTIGER_ACTOR")
            .output()
            .expect("run guidance inspection with invalid planning global");
        assert!(!refusal.status.success());
        let error = String::from_utf8_lossy(&refusal.stderr);
        assert!(error.contains(expected), "{error}");
    }
    assert!(!outside.join("ignored.sqlite").exists());
}

#[test]
fn explicit_project_root_preserves_one_authority_across_installed_projects() {
    let sandbox = TestDirectory::new("explicit-project-root");
    let canonical = sandbox.0.join("canonical");
    let foreign = sandbox.0.join("foreign");
    std::fs::create_dir(&canonical).expect("create canonical project");
    std::fs::create_dir(&foreign).expect("create foreign project");

    for project in [&canonical, &foreign] {
        let setup = Command::new(env!("CARGO_BIN_EXE_papertiger"))
            .arg("setup-project")
            .arg(project)
            .args(["--skill-target", "none"])
            .env_remove("PAPERTIGER_DB")
            .env_remove("PAPERTIGER_ACTOR")
            .output()
            .expect("install Papertiger consumer");
        assert_success(&setup);

        let init = Command::new(installed_papertiger(project))
            .arg("init")
            .current_dir(project)
            .env_remove("PAPERTIGER_DB")
            .env_remove("PAPERTIGER_ACTOR")
            .output()
            .expect("initialize consumer authority");
        assert_success(&init);

        let slug = if project == &canonical {
            "canonical"
        } else {
            "foreign"
        };
        let plan = Command::new(installed_papertiger(project))
            .args(["plan", "add", slug, "Consumer plan"])
            .current_dir(project)
            .env_remove("PAPERTIGER_DB")
            .env_remove("PAPERTIGER_ACTOR")
            .output()
            .expect("add consumer plan");
        assert_success(&plan);
    }

    let foreign_before = Command::new(installed_papertiger(&foreign))
        .args(["status", "--json"])
        .current_dir(&foreign)
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("read foreign authority before cross-project mutation");
    assert_success(&foreign_before);
    let foreign_before: serde_json::Value =
        serde_json::from_slice(&foreign_before.stdout).expect("parse foreign status");

    let add = Command::new(installed_papertiger(&canonical))
        .arg("--project-root")
        .arg(&canonical)
        .args([
            "--actor",
            "cross-project-test",
            "add",
            "Canonical cross-project outcome",
            "--plan",
            "canonical",
            "--start",
            "--intent",
            "One outcome remains in its initiating authority while implementation enters another repository.",
            "--intent-source",
            "user",
            "--why",
            "Exercise explicit receipt-bound authority selection from a foreign installed project.",
        ])
        .current_dir(&foreign)
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("add canonical task from foreign project");
    assert_success(&add);

    let canonical_task = Command::new(installed_papertiger(&canonical))
        .args(["show", "1", "--json"])
        .current_dir(&canonical)
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("read canonical task");
    assert_success(&canonical_task);
    let canonical_task: serde_json::Value =
        serde_json::from_slice(&canonical_task.stdout).expect("parse canonical task");
    assert_eq!(
        canonical_task["task"]["title"],
        "Canonical cross-project outcome"
    );

    let foreign_after = Command::new(installed_papertiger(&foreign))
        .args(["status", "--json"])
        .current_dir(&foreign)
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("read foreign authority after cross-project mutation");
    assert_success(&foreign_after);
    let foreign_after: serde_json::Value =
        serde_json::from_slice(&foreign_after.stdout).expect("parse foreign status");
    assert_eq!(
        foreign_after["authority"]["event_head"],
        foreign_before["authority"]["event_head"]
    );
    assert_eq!(foreign_after["active_plans"][0]["counts"]["in_progress"], 0);
}

#[test]
fn explicit_project_root_refuses_missing_receipt_and_ambiguous_database_selection() {
    let sandbox = TestDirectory::new("project-root-refusals");
    let missing = sandbox.0.join("missing-receipt");
    std::fs::create_dir(&missing).expect("create uninstalled project");

    let missing_receipt = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .arg("--project-root")
        .arg(&missing)
        .arg("status")
        .current_dir(&sandbox.0)
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("run with missing project receipt");
    assert!(!missing_receipt.status.success());
    let error = String::from_utf8_lossy(&missing_receipt.stderr);
    assert!(
        error.contains("no project-install receipt was found"),
        "{error}"
    );
    assert!(error.contains("setup-project"), "{error}");
    assert!(!missing.join("state/papertiger.sqlite").exists());

    let installed = sandbox.0.join("installed");
    std::fs::create_dir(&installed).expect("create installed project");
    let setup = Command::new(env!("CARGO_BIN_EXE_papertiger"))
        .arg("setup-project")
        .arg(&installed)
        .args(["--skill-target", "none"])
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("install Papertiger consumer");
    assert_success(&setup);

    let nested = installed.join("nested");
    std::fs::create_dir(&nested).expect("create nested project directory");
    let nested_root = Command::new(installed_papertiger(&installed))
        .arg("--project-root")
        .arg(&nested)
        .arg("status")
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("run exact selector against nested directory");
    assert!(!nested_root.status.success());
    let error = String::from_utf8_lossy(&nested_root.stderr);
    assert!(
        error.contains("no project-install receipt was found"),
        "{error}"
    );
    assert!(
        error.contains("pass the exact installed project root"),
        "{error}"
    );

    let override_db = sandbox.0.join("override.sqlite");
    let explicit_override = Command::new(installed_papertiger(&installed))
        .arg("--db")
        .arg(&override_db)
        .arg("--project-root")
        .arg(&installed)
        .arg("status")
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("run ambiguous explicit database selection");
    assert!(!explicit_override.status.success());
    let error = String::from_utf8_lossy(&explicit_override.stderr);
    assert!(error.contains("one canonical authority"), "{error}");
    assert!(!override_db.exists());

    let environment_override = Command::new(installed_papertiger(&installed))
        .arg("--project-root")
        .arg(&installed)
        .arg("status")
        .env("PAPERTIGER_DB", &override_db)
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("run ambiguous environment database selection");
    assert!(!environment_override.status.success());
    let error = String::from_utf8_lossy(&environment_override.stderr);
    assert!(error.contains("PAPERTIGER_DB"), "{error}");
    assert!(!override_db.exists());
}

#[test]
fn planner_help_describes_nested_commands_and_important_arguments() {
    let root = command_help(&[]);
    assert!(root.contains("--project-root <DIR>"), "{root}");
    assert!(
        root.contains(
            "Receipt-bound project root used for authority selection or project inspection"
        ),
        "{root}"
    );

    let setup = command_help(&["setup-project"]);
    assert!(
        setup.contains("invalid with project integration"),
        "{setup}"
    );
    assert!(setup.contains("--skill-target"), "{setup}");
    assert!(setup.contains("auto|agents|claude|both|none"), "{setup}");
    assert!(setup.contains("papertiger.project_setup.v5"), "{setup}");

    let guidance = command_help(&["inspect-project-guidance"]);
    assert!(
        guidance.contains("papertiger.project_guidance.v1"),
        "{guidance}"
    );
    assert!(
        guidance.contains("project inspection")
            && guidance.contains("invalid with project integration"),
        "{guidance}"
    );

    let uninstall = command_help(&["uninstall-project"]);
    assert!(
        uninstall.contains("preserves authority and repository policy"),
        "{uninstall}"
    );

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
        "Reopen a resolved or waived blocker",
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
    assert_eq!(value["schema"], "papertiger.focus.v5");
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
    assert_eq!(value["schema"], "papertiger.task_context.v6");
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
    assert_eq!(value["schema"], "papertiger.task_context.v6");
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
    assert_eq!(status["schema"], "papertiger.status.v2");
    assert_eq!(status["authority"]["schema_version"], 9);
    assert!(
        status["authority"]["resolved_path"]
            .as_str()
            .unwrap()
            .contains("papertiger-structured-reads")
    );
    assert_eq!(
        status["active_plans"][0]["in_progress"]["leaves"]["entries"][0]["activity"]["started_event"]
            ["actor"],
        "ended-session"
    );
    assert_eq!(
        status["active_plans"][0]["in_progress"]["leaves"]["entries"][0]["activity"]["last_event"]
            ["actor"],
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
    assert_eq!(search["results"][0]["plan"], "work");
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
    assert_eq!(receipt["dump_schema"], "papertiger.dump.v8");
    let dump: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&export_path).unwrap()).unwrap();
    assert_eq!(dump["schema"], "papertiger.dump.v8");
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

#[test]
fn status_exposes_hierarchy_and_bounded_projection_completeness() {
    let db = TestDatabase::new("status-projection-completeness");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(
        &db.0,
        &["plan", "add", "work", "Structured work"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["plan", "add", "empty", "Empty active plan"],
    ));
    for pair in 1..=5 {
        let parent_seq = pair * 2 - 1;
        let leaf_seq = pair * 2;
        let parent_title = format!("Canonical parent {pair}");
        let leaf_title = format!("Active leaf {pair}");
        let parent_seq = parent_seq.to_string();
        let leaf_seq = leaf_seq.to_string();
        assert_success(&papertiger(
            &db.0,
            &["add", &parent_title, "--plan", "work"],
        ));
        assert_success(&papertiger(&db.0, &["start", &parent_seq]));
        assert_success(&papertiger(
            &db.0,
            &[
                "add",
                &leaf_title,
                "--plan",
                "work",
                "--parent",
                &parent_seq,
            ],
        ));
        assert_success(&papertiger(&db.0, &["start", &leaf_seq]));
    }
    for priority in 1..=6 {
        let title = format!("Ready {priority}");
        let priority = priority.to_string();
        assert_success(&papertiger(
            &db.0,
            &["add", &title, "--plan", "work", "--priority", &priority],
        ));
    }
    for index in 1..=4 {
        let note = format!("note {index}");
        assert_success(&papertiger(&db.0, &["note", &note]));
    }

    let status = papertiger(&db.0, &["status", "--json"]);
    assert_success(&status);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["schema"], "papertiger.status.v2");
    let work = &status["active_plans"][0];
    assert_eq!(work["plan"]["slug"], "work");
    assert_eq!(work["counts"]["in_progress"], 10);
    assert_eq!(work["in_progress"]["all_count"], 10);
    assert_eq!(work["in_progress"]["parent_count"], 5);
    assert_eq!(work["in_progress"]["leaf_count"], 5);
    assert_eq!(
        work["in_progress"]["parents"]["entries"][0]["task"]["seq"],
        1
    );
    assert_eq!(
        work["in_progress"]["leaves"]["entries"][0]["task"]["seq"],
        2
    );
    for role in ["parents", "leaves"] {
        assert_eq!(work["in_progress"][role]["complete"], true);
        assert_eq!(work["in_progress"][role]["omitted_count"], 0);
        assert_eq!(
            work["in_progress"][role]["eligible_count"],
            work["in_progress"][role]["returned_count"]
        );
    }
    assert_eq!(work["ready"]["eligible_count"], 6);
    assert_eq!(work["ready"]["returned_count"], 5);
    assert_eq!(work["ready"]["omitted_count"], 1);
    assert_eq!(work["ready"]["complete"], false);
    assert_eq!(
        work["ready"]["continuation_command"],
        "papertiger focus --plan work --limit 11 --json"
    );
    assert_eq!(work["ready"]["entries"][0]["task"]["title"], "Ready 6");

    let empty = &status["active_plans"][1];
    assert_eq!(empty["plan"]["slug"], "empty");
    assert_eq!(empty["in_progress"]["all_count"], 0);
    assert_eq!(empty["ready"]["eligible_count"], 0);
    assert_eq!(empty["ready"]["complete"], true);
    assert_eq!(
        empty["ready"]["continuation_command"],
        serde_json::Value::Null
    );

    assert_eq!(status["recent_notes"]["eligible_count"], 4);
    assert_eq!(status["recent_notes"]["returned_count"], 3);
    assert_eq!(status["recent_notes"]["omitted_count"], 1);
    assert_eq!(status["recent_notes"]["complete"], false);
    assert_eq!(
        status["recent_notes"]["continuation_command"],
        "papertiger log --json"
    );
    assert_eq!(status["recent_notes"]["entries"][0]["why"], "note 4");
    assert_no_internal_identity_keys(&status);

    let human = papertiger(&db.0, &["status"]);
    assert_success(&human);
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("> parent #1 Canonical parent 1"), "{human}");
    assert!(human.contains("> leaf #2 Active leaf 1"), "{human}");
    assert!(human.contains("1 ready task(s) omitted"), "{human}");
    assert!(human.contains("1 older note(s) omitted"), "{human}");
}

#[test]
fn task_edits_preserve_reconstructible_revisions_and_legacy_honesty() {
    let db = TestDatabase::new("task-definition-revisions");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(
        &db.0,
        &["plan", "add", "work", "Revision work"],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "Parent original",
            "--plan",
            "work",
            "--intent",
            "parent intent",
            "--priority",
            "1",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "Child original",
            "--plan",
            "work",
            "--intent",
            "child intent",
            "--priority",
            "2",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "edit",
            "2",
            "--title",
            "Child revised",
            "--intent",
            "revised intent",
            "--parent",
            "1",
            "--kind",
            "probe",
            "--priority",
            "9",
            "--why",
            "align the durable task definition",
        ],
    ));

    let log = papertiger(&db.0, &["log", "--task", "2", "--json"]);
    assert_success(&log);
    let log: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
    let edit = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "edit")
        .unwrap();
    assert_eq!(edit["task_definition_revision_state"], "complete");
    assert_eq!(
        edit["payload"]["revision_schema"],
        "papertiger.task_definition_revision.v1"
    );
    assert_eq!(
        edit["payload"]["changes"]["title"]["before"],
        "Child original"
    );
    assert_eq!(
        edit["payload"]["changes"]["title"]["after"],
        "Child revised"
    );
    assert_eq!(
        edit["payload"]["changes"]["parent"]["before"],
        serde_json::Value::Null
    );
    assert_eq!(edit["payload"]["changes"]["parent"]["after"], 1);
    assert_eq!(edit["payload"]["changes"]["priority"]["before"], 2);
    assert_eq!(edit["payload"]["changes"]["priority"]["after"], 9);
    assert_no_internal_identity_keys(edit);

    let export_output = papertiger(&db.0, &["export", "--plan", "work"]);
    assert_success(&export_output);
    let export: serde_json::Value = serde_json::from_slice(&export_output.stdout).unwrap();
    let exported_edit = export["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "edit")
        .unwrap();
    assert_eq!(
        exported_edit["payload"]["changes"]["intent"]["before"],
        "child intent"
    );
    assert_eq!(
        exported_edit["payload"]["changes"]["intent"]["after"],
        "revised intent"
    );

    let restored = TestDatabase::new("task-definition-revisions-restored");
    assert_success(&papertiger(&restored.0, &["init"]));
    let export_path = db.0.with_extension("revision-export.json");
    std::fs::write(&export_path, &export_output.stdout).unwrap();
    assert_success(&papertiger(
        &restored.0,
        &["import", export_path.to_str().unwrap()],
    ));
    let restored_log = papertiger(&restored.0, &["log", "--task", "2", "--json"]);
    assert_success(&restored_log);
    let restored_log: serde_json::Value = serde_json::from_slice(&restored_log.stdout).unwrap();
    let restored_edit = restored_log["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "edit")
        .unwrap();
    assert_eq!(restored_edit["task_definition_revision_state"], "complete");
    assert_eq!(restored_edit["payload"]["changes"]["parent"]["after"], 1);
    std::fs::remove_file(export_path).unwrap();

    let event_count_before = log["events"].as_array().unwrap().len();
    let no_op = papertiger(
        &db.0,
        &[
            "edit",
            "2",
            "--title",
            "Child revised",
            "--priority",
            "9",
            "--why",
            "must not mint history",
        ],
    );
    assert!(!no_op.status.success());
    assert!(
        String::from_utf8_lossy(&no_op.stderr).contains("edit made no changes"),
        "{}",
        String::from_utf8_lossy(&no_op.stderr)
    );
    let after_no_op = papertiger(&db.0, &["log", "--task", "2", "--json"]);
    assert_success(&after_no_op);
    let after_no_op: serde_json::Value = serde_json::from_slice(&after_no_op.stdout).unwrap();
    assert_eq!(
        after_no_op["events"].as_array().unwrap().len(),
        event_count_before
    );

    assert_success(&papertiger(
        &db.0,
        &[
            "edit",
            "2",
            "--intent",
            "",
            "--clear-parent",
            "--why",
            "clear obsolete orientation and containment",
        ],
    ));
    let cleared = papertiger(&db.0, &["log", "--task", "2", "--json"]);
    assert_success(&cleared);
    let cleared: serde_json::Value = serde_json::from_slice(&cleared.stdout).unwrap();
    let cleared = &cleared["events"][0];
    assert_eq!(cleared["payload"]["changes"]["intent"]["after"], "");
    assert_eq!(cleared["payload"]["changes"]["parent"]["before"], 1);
    assert_eq!(
        cleared["payload"]["changes"]["parent"]["after"],
        serde_json::Value::Null
    );

    assert_success(&papertiger(
        &db.0,
        &[
            "edit",
            "2",
            "--parent",
            "1",
            "--why",
            "restore containment for cycle rollback proof",
        ],
    ));
    let cycle = papertiger(
        &db.0,
        &[
            "edit",
            "1",
            "--title",
            "Must roll back",
            "--parent",
            "2",
            "--why",
            "attempt an invalid cyclic definition",
        ],
    );
    assert!(!cycle.status.success());
    assert!(String::from_utf8_lossy(&cycle.stderr).contains("would create cycle"));
    let parent = papertiger(&db.0, &["show", "1", "--json"]);
    assert_success(&parent);
    let parent: serde_json::Value = serde_json::from_slice(&parent.stdout).unwrap();
    assert_eq!(parent["task"]["title"], "Parent original");
    assert_eq!(parent["parent"], serde_json::Value::Null);
    assert!(
        parent["recent_events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["kind"] != "edit")
    );

    assert_success(&papertiger(&db.0, &["start", "2"]));
    assert_success(&papertiger(
        &db.0,
        &["done", "2", "--result", "probe result"],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "edit",
            "2",
            "--title",
            "Completed wording correction",
            "--why",
            "correct wording without erasing the completed definition",
        ],
    ));
    let terminal_log = papertiger(&db.0, &["log", "--task", "2", "--json"]);
    assert_success(&terminal_log);
    let terminal_log: serde_json::Value = serde_json::from_slice(&terminal_log.stdout).unwrap();
    assert_eq!(
        terminal_log["events"][0]["task_definition_revision_state"],
        "complete"
    );
    assert_eq!(
        terminal_log["events"][0]["payload"]["changes"]["title"]["before"],
        "Child revised"
    );

    {
        let connection = rusqlite::Connection::open(&db.0).unwrap();
        let task_id: i64 = connection
            .query_row("SELECT task_id FROM tasks WHERE seq=2", [], |row| {
                row.get(0)
            })
            .unwrap();
        connection
            .execute(
                "INSERT INTO events
                 (at, actor, entity, entity_id, entity_plan, entity_seq, kind, why, payload)
                 VALUES ('2026-01-01T00:00:00.000Z', 'legacy-import', 'task', ?1,
                         'work', 2, 'edit', 'historical edit without snapshots',
                         '{\"seq\":2,\"fields\":[\"title\"]}')",
                [task_id],
            )
            .unwrap();
    }
    let legacy = papertiger(&db.0, &["log", "--task", "2", "--json"]);
    assert_success(&legacy);
    let legacy: serde_json::Value = serde_json::from_slice(&legacy.stdout).unwrap();
    assert_eq!(
        legacy["events"][0]["task_definition_revision_state"],
        "legacy_without_snapshots"
    );
    assert_eq!(legacy["events"][0]["payload"]["fields"][0], "title");
    let audit = papertiger(&db.0, &["audit"]);
    assert_success(&audit);
    assert!(
        !String::from_utf8_lossy(&audit.stdout).contains("invalid_task_definition_revision"),
        "{}",
        String::from_utf8_lossy(&audit.stdout)
    );

    {
        let connection = rusqlite::Connection::open(&db.0).unwrap();
        let task_id: i64 = connection
            .query_row("SELECT task_id FROM tasks WHERE seq=2", [], |row| {
                row.get(0)
            })
            .unwrap();
        connection
            .execute(
                "INSERT INTO events
                 (at, actor, entity, entity_id, entity_plan, entity_seq, kind, why, payload)
                 VALUES ('2026-01-01T00:00:01.000Z', 'corrupt-import', 'task', ?1,
                         'work', 2, 'edit', 'malformed revision fixture',
                         '{\"seq\":2,\"revision_schema\":\"papertiger.task_definition_revision.v1\",\"fields\":[\"title\"]}')",
                [task_id],
            )
            .unwrap();
    }
    let invalid = papertiger(&db.0, &["log", "--task", "2", "--json"]);
    assert_success(&invalid);
    let invalid: serde_json::Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert_eq!(
        invalid["events"][0]["task_definition_revision_state"],
        "invalid"
    );
    let audit = papertiger(&db.0, &["audit"]);
    assert_success(&audit);
    assert!(
        String::from_utf8_lossy(&audit.stdout).contains("invalid_task_definition_revision"),
        "{}",
        String::from_utf8_lossy(&audit.stdout)
    );
}
