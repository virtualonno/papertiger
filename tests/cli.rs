mod support;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "cli/schema_contracts.rs"]
mod schema_contracts;
#[path = "cli/task_outline.rs"]
mod task_outline;

#[test]
fn session_pickup_is_visible_advisory_and_requires_no_release() {
    let db = TestDatabase::new("session-pickup");
    let invoke = |session: &str, args: &[&str]| {
        support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .arg("--db")
            .arg(&db.0)
            .args(args)
            .env("PAPERTIGER_SESSION", session)
            .env("PAPERTIGER_ACTOR", "same-harness")
            .output()
            .unwrap()
    };
    assert_success(&invoke("a", &["init"]));
    assert_success(&invoke("a", &["plan", "add", "work", "Work"]));
    assert_success(&invoke(
        "a",
        &[
            "add", "--kind", "feature", "First", "--start", "--why", "begin",
        ],
    ));
    assert_success(&invoke("a", &["add", "--kind", "feature", "Second"]));
    let focus = invoke("b", &["focus", "--limit", "1", "--json"]);
    assert_success(&focus);
    let focus: serde_json::Value = serde_json::from_slice(&focus.stdout).unwrap();
    assert_eq!(focus["schema"], "papertiger.focus.v8");
    assert_eq!(focus["entries"][0]["task"]["seq"], 2);
    assert!(focus["entries"][0]["task"].get("intent").is_none());
    let all = invoke("b", &["focus", "--json"]);
    assert_success(&all);
    let all: serde_json::Value = serde_json::from_slice(&all.stdout).unwrap();
    assert_eq!(all["entries"][1]["pickup"]["session"], "a");
    assert_eq!(all["entries"][1]["readiness"], "picked_up_elsewhere");
    assert_eq!(all["entries"][1]["blockers"], serde_json::json!([]));
    let human = invoke("b", &["focus"]);
    assert_success(&human);
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("picked_up_elsewhere"));
    assert!(human.contains("pickup a"));
    assert!(
        focus["continuation_command"]
            .as_str()
            .unwrap()
            .contains("--session b")
    );
    let status = invoke("b", &["status", "--json"]);
    assert_success(&status);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(
        status["active_plans"][0]["in_progress"]["leaves"]["entries"][0]["pickup"]["session"],
        "a"
    );

    // Explicit session overrides the environment. The old session need not run
    // again, and no release/recovery ceremony is required.
    assert_success(&invoke("b", &["--session", "c", "start", "1"]));
    let current = invoke("c", &["show", "1", "--no-history", "--json"]);
    assert_success(&current);
    let current: serde_json::Value = serde_json::from_slice(&current.stdout).unwrap();
    assert_eq!(current["task"]["pickup"]["session"], "c");
    let retry = invoke("c", &["start", "1", "--json"]);
    assert_success(&retry);
    let retry: serde_json::Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(retry["changed"], false);
    assert_eq!(retry["events"], serde_json::json!([]));
    let invalid = invoke("bad session", &["start", "1"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("PAPERTIGER_SESSION"));
    assert_success(&invoke("different-session", &["done", "1"]));
    assert_success(&invoke("b", &["audit"]));
}

#[test]
fn progressive_reads_preserve_full_context_and_all_plan_inventory() {
    let project = TestDirectory::new("progressive-reads");
    let invoke = |args: &[&str]| {
        support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .args(args)
            .current_dir(&project.0)
            .env_remove("PAPERTIGER_DB")
            .env("PAPERTIGER_ACTOR", "inventory-test")
            .output()
            .unwrap()
    };
    let root = project.0.to_str().unwrap();
    assert_success(&invoke(&["--project-root", root, "init"]));
    for plan in ["active", "paused", "closed", "empty", "retired"] {
        assert_success(&invoke(&[
            "plan",
            "add",
            plan,
            plan,
            "--intent",
            "Full orientation",
        ]));
    }
    for i in 0..68 {
        let plan = if i < 65 {
            "active"
        } else if i < 67 {
            "paused"
        } else {
            "closed"
        };
        assert_success(&invoke(&[
            "add",
            "--kind",
            "feature",
            &format!("discovery {i}"),
            "--plan",
            plan,
            "--intent",
            &"lengthy context ".repeat(100),
            "--tag",
            "cohort",
        ]));
    }
    assert_success(&invoke(&["start", "1", "--why", "work begins"]));
    assert_success(&invoke(&["done", "68"]));
    assert_success(&invoke(&[
        "plan", "set", "closed", "done", "--why", "complete",
    ]));
    assert_success(&invoke(&[
        "plan", "set", "paused", "paused", "--why", "deferred",
    ]));
    assert_success(&invoke(&[
        "plan",
        "set",
        "retired",
        "retired",
        "--why",
        "unused plan",
    ]));
    let read = |args: &[&str]| {
        let output = invoke(args);
        assert_success(&output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let plans = read(&["plan", "list", "--json"]);
    assert_eq!(plans["total"], 5);
    let plan = read(&["plan", "list", "--plan", "paused", "--json"]);
    assert_eq!(plan["plans"][0]["intent"], "Full orientation");
    assert_eq!(plan["plans"][0]["status"], "paused");
    let first = read(&[
        "list",
        "--all-plans",
        "--status",
        "unfinished",
        "--limit",
        "65",
        "--json",
    ]);
    assert_eq!(first["total"], 67);
    assert_eq!(first["remaining"], 2);
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    assert!(cursor.starts_with("inventory-v1:65:"), "{cursor}");
    assert_eq!(
        first["continuation_command"],
        format!(
            "papertiger list --all-plans --status unfinished --limit 65 --after-cursor {cursor} --json"
        )
    );
    let next_args = [
        "list",
        "--all-plans",
        "--status",
        "unfinished",
        "--after-cursor",
        &cursor,
        "--json",
    ];
    let next = read(&next_args);
    assert_eq!(next["schema"], "papertiger.task_inventory.v3");
    assert_eq!(next["remaining"], 0);
    assert!(next["next_cursor"].is_null());
    assert!(next["continuation_command"].is_null());
    let other_filters = invoke(&["list", "--all-plans", "--after-cursor", &cursor]);
    assert!(
        String::from_utf8_lossy(&other_filters.stderr)
            .contains("different --status/--tag/--kind/--unreviewed filters")
    );
    assert_eq!(next["tasks"].as_array().unwrap().len(), 2);
    assert!(
        next["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["plan"]["status"] == "paused")
    );
    assert_eq!(
        read(&["list", "--all-plans", "--tag", "missing", "--json"])["total"],
        0
    );
    assert_eq!(
        read(&["list", "--all-plans", "--status", "done", "--json"])["total"],
        1
    );
    let full = read(&["search", "discovery", "--limit", "2", "--json"]);
    let compact = read(&["search", "discovery", "--limit", "2", "--compact", "--json"]);
    assert_eq!(full["schema"], "papertiger.search.v3");
    assert_eq!(compact["schema"], "papertiger.search_compact.v3");
    assert_eq!(compact["total_matches"], full["total_matches"]);
    assert_eq!(compact["truncated"], true);
    let total = compact["total_matches"].as_u64().unwrap();
    assert_eq!(
        compact["continuation_command"],
        format!(
            "papertiger search \"discovery\" --compact --limit {}",
            total.min(200)
        )
    );
    let default_compact = read(&["search", "discovery", "--compact"]);
    assert_eq!(
        default_compact["results"].as_array().unwrap().len() as u64,
        total.min(5)
    );
    let widened = read(&["search", "discovery", "--compact", "--limit", "200"]);
    assert!(widened["continuation_command"].is_null());
    assert_eq!(
        read(&["search", "discovery", "--json"])["results"]
            .as_array()
            .unwrap()
            .len() as u64,
        total.min(20)
    );
    for (a, b) in full["results"]
        .as_array()
        .unwrap()
        .iter()
        .zip(compact["results"].as_array().unwrap())
    {
        for key in ["plan", "score", "matched_fields", "excerpt"] {
            assert_eq!(a[key], b[key]);
        }
        assert_eq!(a["task"]["seq"], b["task"]["seq"]);
        assert!(b["task"].get("intent").is_none());
    }
    let full = read(&["show", "1", "--json"]);
    let current = read(&["show", "1", "--no-history", "--json"]);
    assert_eq!(current["task"], full["task"]);
    assert_eq!(current["schema"], "papertiger.task_current.v4");
    assert!(current.get("recent_events").is_none());
    assert_eq!(current["history_command"], "papertiger log --task 1 --json");
    assert_eq!(read(&next_args)["tasks"], next["tasks"]);
    assert_success(&invoke(&[
        "note",
        "--text",
        "authority changed",
        "--task",
        "1",
    ]));
    let stale = invoke(&next_args);
    assert!(!stale.status.success());
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains(
            "inventory authority changed since this cursor; restart with `papertiger list --all-plans --status unfinished --limit 100 --json`"
        )
    );
    assert_success(&invoke(&[
        "add",
        "--kind",
        "feature",
        "discarded design",
        "--plan",
        "active",
    ]));
    assert_success(&invoke(&[
        "reject",
        "69",
        "--why",
        "Rejected because zephyr duplicates the authority",
    ]));
    let rejected = read(&[
        "search",
        "zephyr",
        "--status",
        "rejected",
        "--compact",
        "--json",
    ]);
    assert_eq!(rejected["results"][0]["task"]["seq"], 69);
    assert_eq!(rejected["results"][0]["task"]["status"], "rejected");
    assert!(
        rejected["results"][0]["matched_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "rationale")
    );
    let history = read(&["log", "--task", "69", "--json"]);
    assert!(history["events"].as_array().unwrap().iter().any(|event| {
        event["why"]
            .as_str()
            .is_some_and(|why| why.contains("zephyr"))
    }));
    for args in [
        vec!["list", "--all-plans", "--limit", "0"],
        vec!["list", "--after-cursor", "inventory-v1:1:0:empty"],
        vec!["list", "--all-plans", "--after-cursor", "event-v1:1:0"],
        vec!["list", "--all-plans", "--plan", "active"],
        vec!["list", "--all-plans", "--sort", "activity"],
    ] {
        assert!(!invoke(&args).status.success(), "{args:?}");
    }
    // JSON-only projections select JSON without a separate --json.
    for (args, schema) in [
        (
            vec!["search", "discovery", "--compact"],
            "papertiger.search_compact.v3",
        ),
        (
            vec!["show", "1", "--no-history"],
            "papertiger.task_current.v4",
        ),
    ] {
        let output = invoke(&args);
        assert!(output.status.success(), "{args:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema"], schema, "{args:?}");
    }
}

#[test]
fn mutation_receipts_model_attribution_and_task_moves_are_cli_usable() {
    let db = TestDatabase::new("mutation-receipts");
    assert_success(&papertiger(&db.0, &["init"]));
    for plan in ["old", "new"] {
        let output = papertiger(&db.0, &["plan", "add", plan, plan, "--json"]);
        assert_success(&output);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema"], "papertiger.mutation.v2");
        assert_eq!(value["events"][0]["plan"]["slug"], plan);
    }
    let output = papertiger(
        &db.0,
        &[
            "add",
            "--kind",
            "feature",
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
            "--kind",
            "feature",
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

#[test]
fn init_refuses_json_and_names_its_plain_text_report() {
    let db = TestDatabase::new("init-json-refusal");
    let refused = papertiger(&db.0, &["init", "--json"]);
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        error.contains("init reports in plain text only") && error.contains("omit --json"),
        "{error}"
    );
    assert!(
        !db.0.exists(),
        "a refused init must not create the authority"
    );

    let initialized = papertiger(&db.0, &["init"]);
    assert_success(&initialized);
    assert!(String::from_utf8_lossy(&initialized.stdout).starts_with("initialized "));
}

struct TestDatabase(PathBuf);

#[test]
fn tag_and_dependency_edits_take_an_optional_reason_and_transitions_keep_theirs() {
    let db = TestDatabase::new("optional-reasons");
    let run = |args: &[&str]| papertiger(&db.0, args);
    assert_success(&run(&["init"]));
    assert_success(&run(&["plan", "add", "work", "Work"]));
    assert_success(&run(&["add", "--kind", "feature", "First"]));
    assert_success(&run(&["add", "--kind", "feature", "Second"]));
    assert_success(&run(&["tag", "add", "1", "ui"]));
    assert_success(&run(&["dep", "add", "2", "1"]));
    assert_success(&run(&["dep", "remove", "2", "1"]));
    assert_success(&run(&["tag", "remove", "1", "ui"]));
    let log = run(&["log", "--task", "1", "--json"]);
    assert_success(&log);
    let log: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
    let tag_events: Vec<_> = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "tag_add" || event["kind"] == "tag_remove")
        .collect();
    assert_eq!(tag_events.len(), 2, "{log}");
    assert!(
        tag_events.iter().all(|event| event["why"].is_null()),
        "{log}"
    );

    let blank = run(&["tag", "add", "1", "ui", "--why", "  "]);
    assert!(!blank.status.success());
    let retire = run(&["retire", "2"]);
    assert!(
        !retire.status.success(),
        "bare retirement still requires a reason"
    );
}

#[test]
fn low_stakes_corrections_take_an_optional_reason_and_dispositions_keep_theirs() {
    let db = TestDatabase::new("optional-correction-reasons");
    let run = |args: &[&str]| papertiger(&db.0, args);
    let oid = "0123456789abcdef0123456789abcdef01234567";
    for setup in [
        &["init"][..],
        &["plan", "add", "work", "Work"],
        &["add", "--kind", "feature", "Canonical"],
        &["add", "--kind", "feature", "Movable"],
        &["add", "--kind", "feature", "Duplicate"],
        &["add", "--kind", "feature", "Kept"],
        &["plan", "add", "other", "Other"],
        &[
            "gate",
            "add",
            "1",
            "proof",
            "--kind",
            "test",
            "--requirement",
            "Tests pass",
        ],
        &["gate", "waive", "1", "proof", "--why", "covered elsewhere"],
        &["commit", "add", "1", oid],
    ] {
        assert_success(&run(setup));
    }
    for relaxed in [
        &["edit", "1", "--title", "Canonical outcome"][..],
        &["plan", "edit", "work", "--title", "Work edited"],
        &["plan", "set", "other", "paused"],
        &["plan", "set", "other", "active"],
        &["move-plan", "2", "--plan", "other"],
        &["gate", "reopen", "1", "proof"],
        &["commit", "remove", "1", oid],
        &["retire", "3", "--into", "1"],
        &["reopen", "3"],
    ] {
        assert_success(&run(relaxed));
    }
    let log = run(&["log", "--json"]);
    assert_success(&log);
    let log: serde_json::Value = serde_json::from_slice(&log.stdout).unwrap();
    let events = log["events"].as_array().unwrap();
    // Setup recorded no reason except the waiver; every relaxed command recorded none.
    let reasons: Vec<_> = events
        .iter()
        .filter(|event| !event["why"].is_null())
        .collect();
    assert_eq!(reasons.len(), 1, "{log}");
    assert_eq!(reasons[0]["why"], "covered elsewhere", "{log}");

    let retire = run(&["retire", "4"]);
    assert!(!retire.status.success());
    assert!(
        String::from_utf8_lossy(&retire.stderr).contains(
            "retire without --into requires --why <reason>; or name the replacement with --into <task>"
        ),
        "{}",
        String::from_utf8_lossy(&retire.stderr)
    );
    let reject = run(&["reject", "4"]);
    assert!(!reject.status.success());
    assert!(
        String::from_utf8_lossy(&reject.stderr).contains("reject requires a reason"),
        "{}",
        String::from_utf8_lossy(&reject.stderr)
    );
    let shown = run(&["show", "4", "--json"]);
    assert_success(&shown);
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["task"]["status"], "proposed");
}

#[test]
fn a_goal_held_by_a_disputed_result_says_why_and_how_to_clear_it() {
    let db = TestDatabase::new("held-goal");
    let as_session = |session: &str, args: &[&str]| {
        support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .arg("--db")
            .arg(&db.0)
            .args(args)
            .env("PAPERTIGER_SESSION", session)
            .output()
            .expect("run papertiger")
    };
    let ok = |output: Output| {
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };
    ok(as_session("author", &["init"]));
    ok(as_session("author", &["plan", "add", "app", "App"]));
    ok(as_session(
        "author",
        &["add", "--kind", "goal", "Ship export"],
    ));
    ok(as_session(
        "author",
        &[
            "add",
            "--kind",
            "feature",
            "--parent",
            "G-1",
            "Write the exporter",
        ],
    ));
    ok(as_session(
        "author",
        &["done", "F-2", "--result", "Exporter writes JSON"],
    ));
    ok(as_session(
        "reviewer",
        &[
            "review",
            "F-2",
            "--result",
            "--verdict",
            "disputed",
            "--why",
            "Empty tables crash",
        ],
    ));

    let refused = as_session("author", &["done", "G-1", "--result", "Shipped"]);
    assert!(!refused.status.success());
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        error.contains("G-1 cannot be completed yet: F-2's result is disputed")
            && error.contains("papertiger review F-2 --result --verdict confirmed")
            && error.contains("papertiger reopen F-2"),
        "{error}"
    );
    let focus = ok(as_session("author", &["focus"]));
    assert!(
        focus.contains("nothing actionable; 1 blocked task(s) hidden")
            && focus.contains("--include-blocked"),
        "{focus}"
    );
    let audit = ok(as_session("author", &["audit"]));
    assert!(!audit.contains("parent_lagging"), "{audit}");
    let brief = ok(as_session("author", &["brief", "G-1"]));
    assert!(
        brief.contains("F-2 Write the exporter [result disputed · human]: Exporter writes JSON"),
        "{brief}"
    );
    let brief: serde_json::Value =
        serde_json::from_str(&ok(as_session("author", &["brief", "G-1", "--json"]))).unwrap();
    assert_eq!(
        brief["outcomes"]["entries"][0]["review"]["state"],
        "disputed"
    );

    let missing = db.0.with_extension("missing.sqlite");
    let import = papertiger(&missing, &["import", "dump.json"]);
    assert!(!import.status.success());
    assert!(
        String::from_utf8_lossy(&import.stderr).contains("running `init` and then `import <dump>`"),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
}

#[test]
fn claims_show_their_review_state_and_a_disputed_result_holds_its_dependents() {
    let db = TestDatabase::new("reviews");
    let as_session = |session: &str, model: &str, args: &[&str]| {
        let output = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .arg("--db")
            .arg(&db.0)
            .args(args)
            .env("PAPERTIGER_SESSION", session)
            .env("PAPERTIGER_ACTOR", session)
            .env("PAPERTIGER_MODEL", model)
            .output()
            .expect("run papertiger");
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let fleet = |args: &[&str]| as_session("fleet-1", "deepseek-v4.1-flash", args);
    let strong = |args: &[&str]| as_session("strong-1", "claude-opus-5-5", args);
    let third = |args: &[&str]| as_session("third-1", "claude-opus-5.5", args);
    let ok = |(success, stdout, stderr): (bool, String, String)| {
        assert!(success, "stdout:\n{stdout}\nstderr:\n{stderr}");
        stdout
    };
    ok(strong(&["init"]));
    ok(strong(&["plan", "add", "work", "Work"]));
    ok(fleet(&["add", "--kind", "feature", "Cache plan reads"]));
    ok(fleet(&["add", "--kind", "bug", "Crash on empty plan"]));
    ok(strong(&[
        "add",
        "--kind",
        "feature",
        "--dep",
        "F-1",
        "Warm the cache at startup",
    ]));

    let list = ok(strong(&["list"]));
    assert!(
        list.contains("F-1 Cache plan reads\n") && !list.contains("unreviewed"),
        "review is optional, so an unreviewed claim carries no marker: {list}"
    );
    let unreviewed = ok(strong(&["list", "--unreviewed"]));
    assert!(
        unreviewed.contains("F-1 Cache plan reads [unreviewed · deepseek-v4.1-flash]")
            && unreviewed.contains("B-2 Crash on empty plan [unreviewed · deepseek-v4.1-flash]"),
        "the triage view labels each claim with its state and model: {unreviewed}"
    );
    let triage = ok(strong(&["focus", "--unreviewed"]));
    assert!(
        triage.contains("[unreviewed · deepseek-v4.1-flash]"),
        "{triage}"
    );
    let (refused, _, stderr) = fleet(&[
        "review",
        "B-2",
        "--verdict",
        "confirmed",
        "--why",
        "I filed it",
    ]);
    assert!(
        !refused && stderr.contains("a review must come from a different session"),
        "{stderr}"
    );
    ok(strong(&[
        "review",
        "B-2",
        "--verdict",
        "disputed",
        "--why",
        "Empty plans render fine",
    ]));
    let list = ok(strong(&["list"]));
    assert!(
        list.contains("B-2 Crash on empty plan [disputed · deepseek-v4.1-flash]"),
        "{list}"
    );

    ok(strong(&[
        "start",
        "F-1",
        "--why",
        "checked the cache design",
    ]));
    let show: serde_json::Value =
        serde_json::from_str(&ok(strong(&["show", "F-1", "--json"]))).unwrap();
    assert_eq!(show["review"]["filing"]["state"], "engaged");
    assert_eq!(
        show["review"]["filing"]["reviewer_model"],
        "claude-opus-5-5"
    );
    let unreviewed: serde_json::Value =
        serde_json::from_str(&ok(strong(&["focus", "--unreviewed", "--json"]))).unwrap();
    let handles: Vec<_> = unreviewed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["task"]["handle"].clone())
        .collect();
    assert_eq!(handles, vec![serde_json::json!("B-2")], "{unreviewed}");

    ok(strong(&["done", "F-1", "--result", "Reads hit the cache"]));
    let focus = ok(fleet(&["focus"]));
    assert!(
        focus.contains("F-3 Warm the cache at startup") && !focus.contains("unreviewed"),
        "an unreviewed result neither holds nor marks its dependent: {focus}"
    );
    ok(third(&[
        "review",
        "F-1",
        "--result",
        "--verdict",
        "disputed",
        "--why",
        "Cold reads still miss",
    ]));
    let focus = ok(fleet(&["focus"]));
    assert!(
        !focus.contains("F-3"),
        "a disputed result holds its dependent: {focus}"
    );
    let blocked = ok(fleet(&["focus", "--include-blocked"]));
    assert!(blocked.contains("disputed-result:F-1"), "{blocked}");
    ok(third(&[
        "review",
        "F-1",
        "--result",
        "--verdict",
        "confirmed",
        "--why",
        "Cold reads fixed upstream",
    ]));
    let focus = ok(fleet(&["focus"]));
    assert!(
        !focus.contains("disputed-result")
            && focus.contains(
                "F-3 Warm the cache at startup [priority next unlocks 0 downstream 0 gates 0]
"
            ),
        "{focus}"
    );
    let show = ok(fleet(&["show", "F-1"]));
    assert!(
        show.contains("result review: confirmed (by claude-opus-5-5); last checked by claude-opus-5.5, the same model"),
        "{show}"
    );
}

#[test]
fn focus_reports_a_pickup_owner_that_provably_ended() {
    let db = TestDatabase::new("owner-liveness");
    #[cfg(windows)]
    let mut harness = Command::new("cmd")
        .args(["/C", "ping -n 60 127.0.0.1 > NUL"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    #[cfg(not(windows))]
    let mut harness = Command::new("sleep").arg("60").spawn().unwrap();
    let pid = harness.id().to_string();
    let run = |args: &[&str]| {
        let output = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .arg("--db")
            .arg(&db.0)
            .args(args)
            .env_remove("PAPERTIGER_MODEL")
            .env("CLAUDE_CODE_SESSION_ID", "liveness-session-1")
            .env("CLAUDE_PID", &pid)
            .output()
            .expect("run papertiger");
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["init"]);
    run(&["plan", "add", "work", "Work", "--model", "claude-opus-5-5"]);
    run(&[
        "add",
        "--kind",
        "feature",
        "Owned work",
        "--model",
        "claude-opus-5-5",
    ]);
    run(&["start", "1", "--why", "begin", "--model", "claude-opus-5-5"]);
    let focus = run(&["focus"]);
    assert!(focus.contains("owner running"), "{focus}");
    let show = run(&["show", "1"]);
    assert!(
        show.contains("pickup: last by liveness-session-1 at ")
            && show.contains("; owner running; advisory, never a lock")
            && !show.contains("liveness unknown"),
        "{show}"
    );
    harness.kill().unwrap();
    harness.wait().unwrap();
    let focus = run(&["focus"]);
    assert!(focus.contains("owner ended"), "{focus}");
    assert!(run(&["show", "1"]).contains("; owner ended; advisory, never a lock"));
    // Outside a harness no owner is recorded, so its state is unknown.
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "Unowned work"],
    ));
    assert_success(&papertiger(&db.0, &["start", "2"]));
    let unowned = papertiger(&db.0, &["show", "2"]);
    assert_success(&unowned);
    let unowned = String::from_utf8(unowned.stdout).unwrap();
    assert!(
        unowned.contains("; owner state unknown; advisory, never a lock"),
        "{unowned}"
    );
    let show: serde_json::Value = serde_json::from_str(&run(&["show", "1", "--json"])).unwrap();
    assert_eq!(show["owner_state"], "ended");
    assert_eq!(
        show["task"]["pickup"]["owner"]["pid"],
        pid.parse::<u32>().unwrap()
    );
}

#[test]
fn every_write_records_a_reported_model_and_the_harness_supplies_the_rest() {
    let db = TestDatabase::new("authorship");
    let command = |args: &[&str], env: &[(&str, &str)]| {
        let mut command = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"));
        command
            .env_remove("PAPERTIGER_MODEL")
            .arg("--db")
            .arg(&db.0)
            .args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        command.output().expect("run papertiger")
    };
    let operator = [("PAPERTIGER_MODEL", "human")];
    assert_success(&command(&["init"], &[]));
    assert_success(&command(&["plan", "add", "work", "Work"], &operator));

    let refused = command(&["add", "--kind", "feature", "Unattributed"], &[]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("pass --model <model-id>") && stderr.contains("PAPERTIGER_MODEL=human"),
        "{stderr}"
    );
    assert_success(&command(&["status"], &[]));

    let claude = [
        ("CLAUDE_CODE_SESSION_ID", "abcdef1234-5678"),
        ("CLAUDE_EFFORT", "high"),
    ];
    let human_in_harness = command(
        &["add", "--kind", "feature", "Pretend operator"],
        &[claude[0], claude[1], ("PAPERTIGER_MODEL", "human")],
    );
    assert!(!human_in_harness.status.success());
    assert!(
        String::from_utf8_lossy(&human_in_harness.stderr).contains("runs inside Claude Code"),
        "{}",
        String::from_utf8_lossy(&human_in_harness.stderr)
    );
    let runtime = command(
        &[
            "add",
            "--kind",
            "feature",
            "Pretend runtime",
            "--model",
            "runtime",
        ],
        &claude,
    );
    assert!(!runtime.status.success());

    let added = command(
        &[
            "add",
            "--kind",
            "feature",
            "Attributed",
            "--model",
            "Claude-Opus-5-5",
            "--json",
        ],
        &claude,
    );
    assert_success(&added);
    let receipt: serde_json::Value = serde_json::from_slice(&added.stdout).unwrap();
    let event = &receipt["events"][0]["event"];
    assert_eq!(event["actor"], "claude-abcdef12");
    assert_eq!(event["model"], "claude-opus-5-5");
    assert_eq!(event["model_source"], "flag");
    assert_eq!(event["harness"], "Claude Code");
    assert_eq!(event["session"], "abcdef1234-5678");
    assert_eq!(event["reasoning_effort"], "high");

    let plan_event = command(&["log", "--json"], &[]);
    let log: serde_json::Value = serde_json::from_slice(&plan_event.stdout).unwrap();
    let plan_create = log["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["entity"] == "plan")
        .unwrap();
    assert_eq!(plan_create["actor"], "operator");
    assert_eq!(plan_create["model"], "human");
    assert_eq!(plan_create["model_source"], "environment");
}

#[test]
fn starting_and_reading_ask_for_nothing_extra() {
    let db = TestDatabase::new("add-start-no-why");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "Quick fix", "--start"],
    ));
    let show = papertiger(&db.0, &["show", "F-1", "--json"]);
    assert_success(&show);
    let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show["task"]["status"], "in_progress");
    // Kinds read in any letter case, and a missing kind names the choices.
    assert_success(&papertiger(&db.0, &["add", "--kind", "Bug", "Crash"]));
    assert!(
        String::from_utf8_lossy(&papertiger(&db.0, &["list", "--kind", "BUG"]).stdout)
            .contains("B-2 Crash")
    );
    let missing = papertiger(&db.0, &["add", "No kind"]);
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .contains("--kind <goal|feature|change|bug|probe|decision|release>"),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    // Lifecycle commands take their result or reason as a second word.
    assert_success(&papertiger(
        &db.0,
        &["done", "B-2", "Cause: empty input; guarded"],
    ));
    let done = String::from_utf8(papertiger(&db.0, &["show", "B-2", "--json"]).stdout).unwrap();
    assert!(done.contains("Cause: empty input; guarded"), "{done}");
    assert_success(&papertiger(&db.0, &["reopen", "B-2", "regressed"]));
    let twice = papertiger(&db.0, &["reject", "B-2", "duplicate", "--why", "again"]);
    assert!(String::from_utf8_lossy(&twice.stderr).contains("give the why once"));
    let bare = papertiger(&db.0, &["reject", "B-2"]);
    assert!(String::from_utf8_lossy(&bare.stderr).contains("reject requires a reason"));
    assert_success(&papertiger(&db.0, &["reject", "B-2", "duplicate of F-1"]));
    // A note reads its text, or a task and then its text, positionally.
    assert_success(&papertiger(&db.0, &["note", "F-1", "Found the cause"]));
    assert_success(&papertiger(&db.0, &["note", "Plan-level thought"]));
    let log = String::from_utf8(papertiger(&db.0, &["log", "--limit", "2"]).stdout).unwrap();
    assert!(
        log.contains("plan/note - Plan-level thought")
            && log.contains("F-1 task/note - Found the cause"),
        "{log}"
    );
    let twice = papertiger(&db.0, &["note", "F-1", "text", "--text", "again"]);
    assert!(String::from_utf8_lossy(&twice.stderr).contains("give the note text once"));
    // A read has nothing to attribute, so authorship flags are ignored.
    for read in [&["status"][..], &["show", "F-1"], &["init"]] {
        let mut args = read.to_vec();
        args.extend(["--model", "claude-opus-5-5", "--reasoning-effort", "high"]);
        assert_success(&papertiger(&db.0, &args));
    }
}

#[test]
fn text_reads_cover_every_active_plan_when_none_is_named() {
    let db = TestDatabase::new("focus-all-plans");
    let run = |args: &[&str]| {
        let output = papertiger(&db.0, args);
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["init"]);
    run(&["plan", "add", "alpha", "Alpha"]);
    run(&["plan", "add", "beta", "Beta"]);
    for index in 0..7 {
        run(&[
            "add",
            "--kind",
            "feature",
            &format!("Alpha {index}"),
            "--plan",
            "alpha",
        ]);
    }
    run(&["add", "--kind", "bug", "Beta crash", "--plan", "beta"]);

    let text = run(&["focus"]);
    assert!(
        text.contains(
            "focus alpha
"
        ) && text.contains(
            "focus beta
"
        ),
        "{text}"
    );
    assert_eq!(
        text.matches("  F-").count(),
        5,
        "five per plan by default: {text}"
    );
    assert!(
        text.contains(
            "... 2 actionable task(s) omitted; run `papertiger focus --plan alpha --limit 7`"
        ),
        "a text continuation stays text: {text}"
    );
    assert!(run(&["focus", "--limit", "7"]).matches("  F-").count() == 7);

    let list = run(&["list"]);
    assert!(
        list.contains(
            "alpha
· F-1 Alpha 0"
        ) && list.contains(
            "beta
· B-8 Beta crash"
        ),
        "{list}"
    );
    let tree = run(&["tree"]);
    assert!(
        tree.contains(
            "alpha
"
        ) && tree.contains(
            "beta
"
        ),
        "{tree}"
    );

    let json = papertiger(&db.0, &["focus", "--json"]);
    assert!(!json.status.success(), "JSON stays one plan per document");
    assert!(String::from_utf8_lossy(&json.stderr).contains("pass --plan"));
}

#[test]
fn unreviewed_triage_spans_every_plan_with_honest_paging() {
    let db = TestDatabase::new("triage-all-plans");
    let as_session = |session: &str, model: &str, args: &[&str]| {
        let output = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .arg("--db")
            .arg(&db.0)
            .args(args)
            .env("PAPERTIGER_SESSION", session)
            .env("PAPERTIGER_MODEL", model)
            .output()
            .expect("run papertiger");
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };
    let fleet = |args: &[&str]| as_session("fleet-1", "deepseek-v4.1-flash", args);
    let strong = |args: &[&str]| as_session("strong-1", "claude-opus-5-5", args);
    strong(&["init"]);
    strong(&["plan", "add", "alpha", "Alpha"]);
    strong(&["plan", "add", "beta", "Beta"]);
    fleet(&["add", "--kind", "bug", "Crash in alpha", "--plan", "alpha"]);
    fleet(&["add", "--kind", "feature", "Beta export", "--plan", "beta"]);
    fleet(&["add", "--kind", "feature", "Alpha cache", "--plan", "alpha"]);
    strong(&[
        "review",
        "F-3",
        "--verdict",
        "confirmed",
        "--why",
        "Cache misses measured",
    ]);
    strong(&["plan", "set", "beta", "paused"]);

    let text = strong(&["list", "--all-plans", "--unreviewed"]);
    assert!(
        text.contains("B-1 [alpha; active] Crash in alpha [unreviewed · deepseek-v4.1-flash]")
            && text.contains("F-2 [beta; paused] Beta export [unreviewed · deepseek-v4.1-flash]")
            && !text.contains("F-3"),
        "triage reaches paused plans and skips confirmed claims: {text}"
    );
    let first: serde_json::Value = serde_json::from_str(&strong(&[
        "list",
        "--all-plans",
        "--unreviewed",
        "--limit",
        "1",
        "--json",
    ]))
    .unwrap();
    assert_eq!(first["unreviewed"], true);
    assert_eq!(
        (first["total"].as_u64(), first["remaining"].as_u64()),
        (Some(2), Some(1))
    );
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    assert!(
        first["continuation_command"]
            .as_str()
            .unwrap()
            .contains("--unreviewed --limit 1 --after-cursor"),
        "{first}"
    );
    let second: serde_json::Value = serde_json::from_str(&strong(&[
        "list",
        "--all-plans",
        "--unreviewed",
        "--limit",
        "1",
        "--after-cursor",
        &cursor,
        "--json",
    ]))
    .unwrap();
    assert_eq!(second["tasks"][0]["task"]["handle"], "F-2");
    assert_eq!(second["remaining"], 0);
    let crossed = papertiger(
        &db.0,
        &["list", "--all-plans", "--after-cursor", &cursor, "--json"],
    );
    assert!(
        !crossed.status.success(),
        "a triage cursor cannot page an unfiltered listing"
    );
}

#[test]
fn brief_bounds_a_long_intent_and_points_at_show() {
    let db = TestDatabase::new("brief-long-intent");
    let run = |args: &[&str]| {
        let output = papertiger(&db.0, args);
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["init"]);
    run(&["plan", "add", "work", "Work"]);
    let paragraph =
        "Re-review after an engine change re-authors work whose facts did not change. ".repeat(12);
    run(&[
        "add",
        "--kind",
        "goal",
        "Long intent",
        "--intent",
        &paragraph,
    ]);
    run(&[
        "add",
        "--kind",
        "goal",
        "Short intent",
        "--intent",
        "One line",
    ]);
    let text = run(&["brief", "G-1"]);
    let intent = text
        .lines()
        .find(|line| line.starts_with("  intent: "))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        intent.ends_with('…') && intent.chars().count() < 640,
        "{intent}"
    );
    assert!(
        text.contains("  ... intent continues; run `papertiger show G-1`"),
        "{text}"
    );
    let short = run(&["brief", "G-2"]);
    assert!(
        short.contains(
            "  intent: One line
"
        ) && !short.contains("intent continues"),
        "{short}"
    );
}

#[test]
fn brief_resumes_a_goal_with_its_whole_subtree_in_bounded_sections() {
    let db = TestDatabase::new("brief");
    let run = |args: &[&str]| {
        let output = papertiger(&db.0, args);
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["init"]);
    run(&["plan", "add", "work", "Work"]);
    run(&[
        "add",
        "--kind",
        "goal",
        "Ship templates",
        "--intent",
        "Consumers start from templates",
    ]);
    run(&[
        "add",
        "--kind",
        "decision",
        "--parent",
        "G-1",
        "Choose the format",
    ]);
    run(&[
        "add",
        "--kind",
        "feature",
        "--parent",
        "G-1",
        "--dep",
        "D-2",
        "Render templates",
    ]);
    run(&[
        "add",
        "--kind",
        "change",
        "--parent",
        "F-3",
        "Split the renderer",
    ]);
    run(&["add", "--kind", "feature", "Unrelated work"]);
    run(&[
        "gate",
        "add",
        "F-3",
        "vendor",
        "--kind",
        "external",
        "--requirement",
        "parser 2.0 released",
    ]);
    run(&[
        "note",
        "--task",
        "F-3",
        "--text",
        "Renderer waits on the parser",
    ]);
    let long_result = format!(
        "Markdown with front matter
{}",
        "because templates stay diffable ".repeat(10).trim_end()
    );
    run(&["done", "D-2", "--result", &long_result]);
    for index in 0..12 {
        run(&[
            "note",
            "--task",
            "C-4",
            "--text",
            &format!("progress {index}"),
        ]);
    }

    let brief: serde_json::Value = serde_json::from_str(&run(&["brief", "G-1", "--json"])).unwrap();
    assert_eq!(brief["progress"]["tasks"], 4);
    assert_eq!(brief["progress"]["done"], 1);
    assert_eq!(brief["progress"]["open"], 3);
    let handles = |section: &str| -> Vec<String> {
        brief[section]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                entry.get("task").unwrap_or(entry)["handle"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    };
    assert_eq!(
        handles("open"),
        ["F-3", "C-4"],
        "the root and unrelated work are not open entries"
    );
    assert_eq!(handles("next"), ["C-4"], "F-3 waits on its external gate");
    assert_eq!(brief["obligations"]["entries"][0]["gate"], "vendor");
    assert_eq!(
        brief["outcomes"]["entries"][0]["result"],
        long_result.as_str()
    );
    assert_eq!(brief["notes"]["entries"].as_array().unwrap().len(), 10);
    assert_eq!(brief["notes"]["omitted"], 3);
    assert_eq!(
        brief["open"]["entries"][1]["ancestry"],
        serde_json::json!(["G-1", "F-3"])
    );

    let text = run(&["brief", "G-1"]);
    assert!(
        text.contains("subtree: 4 task(s): 1 done, 0 retired, 0 rejected, 3 open"),
        "{text}"
    );
    assert!(
        text.contains("F-3 vendor (external): parser 2.0 released"),
        "{text}"
    );
    let outcome = text
        .lines()
        .find(|line| line.contains("D-2 Choose the format: Markdown with front matter because"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        outcome.ends_with('…') && outcome.chars().count() < 200,
        "a long result is one capped line in text: {outcome}"
    );
    assert!(text.contains("  ... 3 more"), "{text}");
}

#[test]
fn breadcrumbs_place_each_task_and_a_goal_surfaces_for_closing() {
    let db = TestDatabase::new("breadcrumbs");
    let run = |args: &[&str]| {
        let output = papertiger(&db.0, args);
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["init"]);
    run(&["plan", "add", "work", "Work"]);
    run(&["add", "--kind", "goal", "Ship reliable plans"]);
    run(&[
        "add",
        "--kind",
        "feature",
        "--parent",
        "G-1",
        "Plan templates",
    ]);
    run(&[
        "add",
        "--kind",
        "decision",
        "--parent",
        "F-2",
        "Choose the template format",
    ]);

    let show = run(&["show", "D-3"]);
    assert!(show.contains("  path: G-1 › F-2"), "{show}");
    let show: serde_json::Value = serde_json::from_str(&run(&["show", "3", "--json"])).unwrap();
    assert_eq!(show["ancestry"], serde_json::json!(["G-1", "F-2"]));
    let root: serde_json::Value = serde_json::from_str(&run(&["show", "1", "--json"])).unwrap();
    assert_eq!(root["ancestry"], serde_json::json!([]));

    let focus = run(&["focus"]);
    assert!(
        focus.contains("D-3 Choose the template format [in G-1 › F-2]"),
        "{focus}"
    );
    let focus: serde_json::Value = serde_json::from_str(&run(&["focus", "--json"])).unwrap();
    assert_eq!(
        focus["entries"][0]["ancestry"],
        serde_json::json!(["G-1", "F-2"])
    );

    run(&["done", "3", "--result", "Markdown with front matter"]);
    run(&["done", "2"]);
    let focus = run(&["focus"]);
    assert!(
        focus.contains("G-1 Ship reliable plans")
            && focus.contains("[close: all 1 children closed]"),
        "{focus}"
    );
}

#[test]
fn kind_filters_select_one_kind_and_status_counts_open_work_by_kind() {
    let db = TestDatabase::new("kind-filters");
    let run = |args: &[&str]| {
        let output = papertiger(&db.0, args);
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["init"]);
    run(&["plan", "add", "work", "Work"]);
    run(&["add", "--kind", "goal", "Ship reliable plans"]);
    run(&[
        "add",
        "--kind",
        "bug",
        "--parent",
        "1",
        "Crash on empty plan",
    ]);
    run(&[
        "add",
        "--kind",
        "feature",
        "--parent",
        "1",
        "Plan templates",
    ]);
    run(&["add", "--kind", "bug", "Wrong sort order"]);
    run(&["add", "--kind", "probe", "Measure plan reads"]);
    run(&["done", "4", "--result", "Ties were broken by title"]);

    let seqs = |json: &str, rows: &str| -> Vec<i64> {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        value[rows]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row.get("task").unwrap_or(row)["seq"].as_i64().unwrap())
            .collect()
    };
    assert_eq!(
        seqs(&run(&["list", "--kind", "bug", "--json"]), "tasks"),
        vec![2, 4]
    );
    assert_eq!(
        seqs(
            &run(&["list", "--kind", "bug", "--status", "proposed", "--json"]),
            "tasks"
        ),
        vec![2]
    );
    let inventory = run(&[
        "list",
        "--all-plans",
        "--kind",
        "bug",
        "--limit",
        "1",
        "--json",
    ]);
    assert_eq!(seqs(&inventory, "tasks"), vec![2]);
    let inventory: serde_json::Value = serde_json::from_str(&inventory).unwrap();
    assert_eq!(inventory["kind"], "bug");
    assert!(
        inventory["continuation_command"]
            .as_str()
            .unwrap()
            .contains("--kind bug"),
        "{inventory}"
    );
    assert_eq!(
        seqs(&run(&["focus", "--kind", "probe", "--json"]), "entries"),
        vec![5]
    );
    assert_eq!(
        seqs(
            &run(&["search", "plan", "--kind", "feature", "--json"]),
            "results"
        ),
        vec![3]
    );
    let tree = run(&["tree", "--kind", "feature"]);
    assert!(tree.contains("G-1 Ship reliable plans"), "{tree}");
    assert!(tree.contains("F-3 Plan templates"), "{tree}");
    assert!(!tree.contains("B-2") && !tree.contains("P-5"), "{tree}");
    let status = run(&["status"]);
    assert!(
        status.contains("open by kind: G 1, F 1, B 1, P 1"),
        "{status}"
    );
    let status: serde_json::Value = serde_json::from_str(&run(&["status", "--json"])).unwrap();
    assert_eq!(
        status["active_plans"][0]["open_by_kind"],
        serde_json::json!([
            {"kind": "goal", "count": 1},
            {"kind": "feature", "count": 1},
            {"kind": "bug", "count": 1},
            {"kind": "probe", "count": 1},
        ])
    );
    let refused = papertiger(&db.0, &["list", "--kind", "chore"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("unknown task kind 'chore'"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    for args in [
        &["list", "--status", "all"][..],
        &["list", "--status", "all", "--unreviewed"],
        &["list", "--status", "all", "--sort", "activity"],
    ] {
        let refused = papertiger(&db.0, args);
        assert!(!refused.status.success(), "{args:?} must refuse");
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("unknown task status 'all'"),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
}

#[test]
fn authority_refusals_print_paths_without_windows_verbatim_prefixes() {
    let project = std::env::temp_dir().join(format!(
        "papertiger-plain-path-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(project.join("state")).unwrap();
    let authority = project.join("state/papertiger.sqlite");
    assert_success(&papertiger(&authority, &["init"]));
    let raw = rusqlite::Connection::open(&authority).unwrap();
    raw.create_scalar_function(
        "papertiger_write_requires_public_api",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        |_| Ok(1_i64),
    )
    .unwrap();
    raw.execute("UPDATE meta SET value='13' WHERE key='schema_version'", [])
        .unwrap();
    drop(raw);
    let refused = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
        .arg("--project-root")
        .arg(&project)
        .arg("status")
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("init` explicitly to upgrade to v14"),
        "{stderr}"
    );
    assert!(!stderr.contains(r"\?\"), "{stderr}");
    assert!(stderr.contains("/state/papertiger.sqlite"), "{stderr}");
    std::fs::remove_dir_all(project).unwrap();
}

#[test]
fn handles_name_the_kind_and_resolve_like_numbers_even_when_stale() {
    let db = TestDatabase::new("handles");
    let run = |args: &[&str]| papertiger(&db.0, args);
    assert_success(&run(&["init"]));
    assert_success(&run(&["plan", "add", "work", "Work"]));
    let added = run(&["add", "--kind", "bug", "Crash on empty plan"]);
    assert_success(&added);
    assert_eq!(
        String::from_utf8_lossy(&added.stdout).trim(),
        "B-1 added to work"
    );
    let show = run(&["show", "B-1", "--json"]);
    assert_success(&show);
    let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show["task"]["handle"], "B-1");
    assert_success(&run(&[
        "tag",
        "add",
        "b-1",
        "triage",
        "--why",
        "lowercase letters resolve",
    ]));
    let retyped = run(&[
        "edit",
        "1",
        "--kind",
        "feature",
        "--why",
        "it was a missing feature",
    ]);
    assert_success(&retyped);
    assert_eq!(
        String::from_utf8_lossy(&retyped.stdout).trim(),
        "F-1 updated (kind)",
        "a receipt names the task by its handle after the mutation"
    );
    let stale = run(&["show", "B-1"]);
    assert_success(&stale);
    assert!(
        String::from_utf8_lossy(&stale.stdout).starts_with("· F-1 "),
        "a stale handle resolves and output shows the current one: {}",
        String::from_utf8_lossy(&stale.stdout)
    );
    let outline = serde_json::json!({
        "schema": "papertiger.task_outline.v2",
        "children": [{"key": "f-2", "title": "Child", "kind": "change"}],
    });
    let refused = papertiger_with_stdin(
        &db.0,
        &["decompose", "F-1", "--outline-file", "-"],
        &outline.to_string(),
    );
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("key \"f-2\" reads as a task handle"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

#[test]
fn closed_children_count_as_closed_and_their_parent_surfaces_for_closing() {
    let db = TestDatabase::new("closed-children");
    let run = |args: &[&str]| {
        let output = papertiger(&db.0, args);
        assert_success(&output);
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["init"]);
    run(&["plan", "add", "work", "Work"]);
    run(&["add", "--kind", "feature", "Parent outcome"]);
    run(&["add", "--kind", "feature", "Built child", "--parent", "1"]);
    run(&[
        "add",
        "--kind",
        "feature",
        "Superseded child",
        "--parent",
        "1",
    ]);
    run(&[
        "add",
        "--kind",
        "feature",
        "Wrong approach",
        "--parent",
        "1",
    ]);
    run(&["add", "--kind", "feature", "Other parent"]);
    run(&[
        "add",
        "--kind",
        "feature",
        "Finished child",
        "--parent",
        "5",
    ]);
    run(&["done", "6"]);
    let focus_entries = |focus: &str| -> Vec<(i64, i64)> {
        let focus: serde_json::Value = serde_json::from_str(focus).unwrap();
        focus["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["task"]["seq"].as_i64().unwrap(),
                    entry["closed_child_count"].as_i64().unwrap(),
                )
            })
            .collect()
    };
    assert_eq!(
        focus_entries(&run(&["focus", "--json"])),
        vec![(2, 0), (3, 0), (4, 0), (5, 1)],
        "a parent with open children is not actionable; one whose children are all closed is"
    );

    run(&["done", "2"]);
    run(&["retire", "3", "--why", "Superseded by the built child"]);
    run(&["reject", "4", "--why", "Approach does not hold"]);
    let tree = run(&["tree"]);
    assert!(
        tree.contains("F-1 Parent outcome (3/3 closed: 1 done, 1 retired, 1 rejected)"),
        "{tree}"
    );
    assert!(tree.contains("F-5 Other parent (1/1)"), "{tree}");
    assert_eq!(
        focus_entries(&run(&["focus", "--json"])),
        vec![(1, 3), (5, 1)]
    );
    assert!(
        run(&["focus"]).contains("[close: all 3 children closed]"),
        "text focus names the closing step"
    );
}

#[test]
fn a_reader_that_closes_the_pipe_ends_output_without_failure() {
    let db = TestDatabase::new("closed-pipe");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "Parent outcome"],
    ));
    let children: Vec<_> = (0..256)
        .map(|index| {
            serde_json::json!({
                "key": format!("c{index}"),
                "title": format!("Child outcome {index} with a title long enough to fill the pipe buffer quickly"),
                "kind": "feature",
            })
        })
        .collect();
    let outline = serde_json::json!({
        "schema": "papertiger.task_outline.v2",
        "children": children,
    });
    assert_success(&papertiger_with_stdin(
        &db.0,
        &["decompose", "1", "--outline-file", "-"],
        &outline.to_string(),
    ));

    let mut child = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
        .arg("--db")
        .arg(&db.0)
        .arg("list")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn papertiger");
    let mut stdout = child.stdout.take().expect("papertiger stdout");
    let mut first = [0u8; 8];
    std::io::Read::read_exact(&mut stdout, &mut first).expect("read the first bytes");
    drop(stdout);
    let output = child.wait_with_output().expect("wait for papertiger");
    assert!(
        output.status.success(),
        "a closed reader must end output quietly, got {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn orientation_reads_leave_the_authority_bytes_unchanged() {
    let db = TestDatabase::new("orientation-reads");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "--kind",
            "feature",
            "Parent outcome",
            "--plan",
            "work",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "--kind",
            "feature",
            "Child outcome",
            "--plan",
            "work",
            "--parent",
            "1",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "--kind",
            "feature",
            "Later outcome",
            "--plan",
            "work",
            "--dep",
            "2",
        ],
    ));
    let before = std::fs::read(&db.0).unwrap();
    for args in [
        &["status"][..],
        &["status", "--json"],
        &["focus", "--plan", "work", "--json"],
        &["list", "--plan", "work", "--json"],
        &["tree", "--plan", "work"],
        &["show", "2"],
        &["show", "2", "--json"],
        &["search", "outcome", "--json"],
        &["log", "--json"],
        &["audit"],
    ] {
        assert_success(&papertiger(&db.0, args));
        assert!(
            std::fs::read(&db.0).unwrap() == before,
            "`papertiger {}` changed the authority",
            args.join(" ")
        );
    }
}

#[test]
fn reasoning_effort_uses_environment_and_explicit_flags_without_inference() {
    let db = TestDatabase::new("reasoning-effort");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    let run = |args: &[&str], model: Option<&str>, effort: Option<&str>| {
        let mut command = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"));
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
        &["add", "--kind", "feature", "precise identity", "--json"],
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
        &["add", "--kind", "feature", "unknown effort", "--json"],
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
            &[
                "add",
                "--kind",
                "feature",
                "orphan",
                "--reasoning-effort",
                "high",
                "--json",
            ],
            None,
            None,
        ),
        run(
            &["add", "--kind", "feature", "invalid", "--json"],
            Some("gpt-6-astra"),
            Some("very high"),
        ),
    ] {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let after = run(&["export"], None, None);
    assert_success(&after);
    assert_eq!(before.stdout, after.stdout);
    // A read ignores authorship flags, and inherited metadata never affects it.
    assert_success(&run(
        &["show", &seq, "--reasoning-effort", "high", "--json"],
        None,
        None,
    ));
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
        vec![
            "add",
            "--kind",
            "feature",
            "historical outcome",
            "--plan",
            "work",
        ],
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
    // Explicit API admission for intentional disposable-fixture construction.
    papertiger::begin_mutation(&writer)
        .unwrap()
        .commit()
        .unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        DROP VIEW canonical_events; DROP TABLE event_quarantines; DROP TABLE external_references;
        ALTER TABLE tasks DROP COLUMN pickup_at;
ALTER TABLE tasks DROP COLUMN pickup_session;
ALTER TABLE tasks DROP COLUMN pickup_process;
ALTER TABLE tasks DROP COLUMN pickup_host;
ALTER TABLE tasks DROP COLUMN pickup_pid;
CREATE TABLE task_blockers (
  blocker_id INTEGER PRIMARY KEY,
  task_id INTEGER NOT NULL REFERENCES tasks(task_id),
  name TEXT NOT NULL,
  reason TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','waived')),
  evidence_locator TEXT,
  evidence_sha256 TEXT,
  note TEXT,
  resolved_at TEXT,
  UNIQUE (task_id, name)
); ALTER TABLE gates RENAME COLUMN resolved_at TO closed_at;
UPDATE meta SET value='8' WHERE key='schema_version';
        UPDATE gates SET status='resolved', evidence_locator='commit:ca9ff90';
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
                // Explicit API admission for intentional disposable-fixture construction.
                papertiger::begin_mutation(&conn).unwrap().commit().unwrap();
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
fn backup_records_no_events_even_when_given_a_model() {
    let directory = TestDirectory::new("backup-events");
    let source = directory.0.join("source.sqlite");
    let output = directory.0.join("recovery.sqlite");
    assert_success(&papertiger(&source, &["init"]));
    let before = papertiger(&source, &["export"]);
    assert_success(&before);
    assert_success(&papertiger(
        &source,
        &[
            "backup",
            "--output",
            output.to_str().unwrap(),
            "--json",
            "--model",
            "invented",
        ],
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

#[test]
fn add_start_is_atomic_and_rolls_back_every_refusal() {
    let db = TestDatabase::new("atomic-add-start");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "--kind",
            "feature",
            "open dependency",
            "--plan",
            "work",
        ],
    ));

    let before_refusal = papertiger(&db.0, &["log", "--json"]);
    assert_success(&before_refusal);
    let before_refusal: serde_json::Value = serde_json::from_slice(&before_refusal.stdout).unwrap();

    let blocked = papertiger(
        &db.0,
        &[
            "add",
            "--kind",
            "feature",
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
    assert!(blocked_stderr.contains("resolve dep:F-1"));
    assert!(!blocked_stderr.contains("F-2 is not ready"));
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
            "--kind",
            "feature",
            "atomic task",
            "--plan",
            "work",
            "--intent",
            "implement one durable outcome",
            "--start",
            "--why",
            "the outcome is ready",
        ],
    ));

    let show = papertiger(&db.0, &["show", "2", "--json"]);
    assert_success(&show);
    let show: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show["task"]["status"], "in_progress");
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
fn dependencies_that_finish_only_after_their_dependent_are_refused() {
    let db = TestDatabase::new("wait-for-deadlock");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "Grandparent"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "Parent", "--parent", "1"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "Downstream", "--dep", "2"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "Child", "--parent", "2"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "Independent"],
    ));
    let head = |db: &TestDatabase| {
        let log = papertiger(&db.0, &["log", "--json"]);
        assert_success(&log);
        serde_json::from_slice::<serde_json::Value>(&log.stdout).unwrap()["head"].clone()
    };
    let before = head(&db);
    for (args, expected) in [
        (
            vec!["add", "--kind", "feature", "New child", "--parent", "2", "--dep", "1"],
            "dependency F-6 -> F-1 would deadlock: F-1 finishes only after F-6",
        ),
        (
            vec!["add", "--kind", "feature", "New child", "--parent", "2", "--dep", "3"],
            "dependency F-6 -> F-3 would deadlock: F-3 finishes only after F-6",
        ),
        (
            vec!["add", "--kind", "feature", "New child", "--parent", "2", "--dep", "2"],
            "dependency F-6 -> F-2 would deadlock: F-2 finishes only after F-6",
        ),
        (
            vec!["dep", "add", "4", "1", "--why", "ancestor"],
            "dependency F-4 -> F-1 would deadlock: F-1 finishes only after F-4 (F-1 waits for unfinished child F-2, which waits for unfinished child F-4); choose a prerequisite that does not wait for F-4
",
        ),
        (
            vec!["dep", "add", "4", "3", "--why", "downstream"],
            "dependency F-4 -> F-3 would deadlock: F-3 finishes only after F-4 (F-3 depends on F-2, which waits for unfinished child F-4); choose a prerequisite that does not wait for F-4, or first remove a dependency in that chain with `papertiger dep remove 3 2 --why <reason>`",
        ),
        (
            vec!["dep", "add", "4", "2", "--why", "direct parent"],
            "dependency F-4 -> F-2 would deadlock: F-2 finishes only after F-4",
        ),
        (
            vec!["edit", "3", "--parent", "4", "--why", "nest downstream"],
            "parent change would deadlock: F-3 finishes only after its new parent F-4 (F-3 depends on F-2, which waits for unfinished child F-4); choose another parent, or first remove a dependency in that chain with `papertiger dep remove 3 2 --why <reason>`",
        ),
    ] {
        let refused = papertiger(&db.0, &args);
        assert!(!refused.status.success(), "{args:?} was accepted");
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
    }
    assert_eq!(head(&db), before);
    assert_success(&papertiger(
        &db.0,
        &["dep", "add", "4", "5", "--why", "independent prerequisite"],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "dep",
            "add",
            "2",
            "4",
            "--why",
            "parent already waits for its child",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "add", "--kind", "feature", "Sibling", "--parent", "2", "--dep", "4",
        ],
    ));

    // A finished task gains no wait edge when moved, but reopening it makes
    // its new parent wait for it again.
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "Moved later", "--dep", "3"],
    ));
    assert_success(&papertiger(&db.0, &["retire", "7", "--why", "set aside"]));
    assert_success(&papertiger(
        &db.0,
        &[
            "edit",
            "7",
            "--parent",
            "2",
            "--why",
            "file it under the parent",
        ],
    ));
    let before = head(&db);
    let refused = papertiger(&db.0, &["reopen", "7", "--why", "needed again"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains(
            "reopening F-7 would deadlock: F-7 finishes only after its parent F-2 (F-7 depends on F-3, which depends on F-2), which would wait for it again; move it first with `papertiger edit 7 --parent <task>` or `papertiger edit 7 --clear-parent`, or first remove a dependency in that chain with `papertiger dep remove 7 3 --why <reason>` or `papertiger dep remove 3 2 --why <reason>`"
        ),
        "{stderr}"
    );
    assert_eq!(head(&db), before);
    assert_success(&papertiger(
        &db.0,
        &["dep", "remove", "7", "3", "--why", "no longer downstream"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["reopen", "7", "--why", "needed again"],
    ));
}

#[test]
fn entry_bounds_tag_identity_and_focus_projection_are_explicit() {
    let db = TestDatabase::new("entry-bounds-focus");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));

    let long_title = "x".repeat(papertiger::MAX_TASK_TITLE_CHARS + 1);
    let refused = papertiger(
        &db.0,
        &["add", "--kind", "feature", &long_title, "--plan", "work"],
    );
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("shorten it to at most 160 characters"),
        "{stderr}"
    );

    let blank_tag = papertiger(
        &db.0,
        &[
            "add", "--kind", "feature", "tagged", "--plan", "work", "--tag", "a,,  b",
        ],
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
            &[
                "add", "--kind", "feature", title, "--plan", "work", "--tag", " alpha ",
            ],
        ));
    }
    let focus = papertiger(
        &db.0,
        &["focus", "--plan", "work", "--limit", "1", "--json"],
    );
    assert_success(&focus);
    let focus: serde_json::Value = serde_json::from_slice(&focus.stdout).unwrap();
    assert_eq!(focus["schema"], "papertiger.focus.v8");
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
        &[
            "add",
            "--kind",
            "feature",
            "verified task",
            "--plan",
            "work",
        ],
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
            "resolve",
            "1",
            "proof",
            "--evidence",
            "file:docs/evidence/proof.json",
            "--sha256",
            &digest,
            "--project-root",
            root.to_str().unwrap(),
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
            "--classification",
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
    let resolve_arguments =
        drifted_json["projection"]["bindings"][0]["corrective_commands"][1]["arguments"]
            .as_array()
            .unwrap();
    assert_eq!(
        &resolve_arguments[..7],
        serde_json::json!([
            "gate",
            "resolve",
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
fn new_file_evidence_resolves_against_the_discovered_project_from_a_subdirectory() {
    let project = TestDirectory::new("file-evidence-discovered");
    std::fs::create_dir_all(project.0.join("docs")).unwrap();
    std::fs::write(project.0.join("docs/proof.txt"), b"proof\n").unwrap();
    let invoke = |args: &[&str]| {
        support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .args(args)
            .current_dir(project.0.join("docs"))
            .env_remove("PAPERTIGER_DB")
            .env("PAPERTIGER_ACTOR", "evidence-test")
            .output()
            .unwrap()
    };
    let root = project.0.to_str().unwrap();
    assert_success(&invoke(&["--project-root", root, "init"]));
    assert_success(&invoke(&["plan", "add", "work", "Work"]));
    assert_success(&invoke(&[
        "add", "--kind", "feature", "bound", "--plan", "work",
    ]));
    assert_success(&invoke(&[
        "gate",
        "add",
        "1",
        "proof",
        "--kind",
        "review",
        "--requirement",
        "retained evidence",
    ]));

    let backslash = invoke(&[
        "gate",
        "resolve",
        "1",
        "proof",
        "--evidence",
        "file:docs\\proof.txt",
    ]);
    assert!(!backslash.status.success());
    let stderr = String::from_utf8_lossy(&backslash.stderr);
    assert!(stderr.contains("(invalid_path)"), "{stderr}");
    assert!(
        stderr.contains("pass --evidence file:docs/proof.txt"),
        "{stderr}"
    );

    let missing = invoke(&[
        "gate",
        "resolve",
        "1",
        "proof",
        "--evidence",
        "file:proof.txt",
    ]);
    assert!(!missing.status.success());
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("note --text-file <path> --task <task>"),
        "{stderr}"
    );
    assert!(
        stderr.contains("gate waive <task> <name> --why <reason>"),
        "{stderr}"
    );

    assert_success(&invoke(&[
        "gate",
        "resolve",
        "1",
        "proof",
        "--evidence",
        "file:docs/proof.txt",
    ]));
}

#[test]
fn new_file_evidence_must_name_an_existing_file_beneath_the_project_root() {
    let root = TestDirectory::new("file-evidence-write");
    std::fs::create_dir_all(root.0.join("state")).unwrap();
    std::fs::create_dir_all(root.0.join("docs")).unwrap();
    std::fs::write(
        root.0.join("docs/proof.txt"),
        b"proof
",
    )
    .unwrap();
    let db = TestDatabase(root.0.join("state/papertiger.sqlite"));
    let root_text = root.0.to_str().unwrap();
    let in_project = |args: &[&str]| {
        support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .args(["--project-root", root_text])
            .args(args)
            .output()
            .expect("run papertiger")
    };
    let refusal = |output: &Output| {
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "bound", "--plan", "work"],
    ));
    for gate in ["proof", "commit"] {
        assert_success(&papertiger(
            &db.0,
            &[
                "gate",
                "add",
                "1",
                gate,
                "--kind",
                "review",
                "--requirement",
                "retained evidence",
            ],
        ));
    }
    assert_success(&papertiger(
        &db.0,
        &[
            "gate",
            "add",
            "1",
            "input",
            "--kind",
            "external",
            "--requirement",
            "input exists",
        ],
    ));

    let unrooted = refusal(&papertiger(
        &db.0,
        &[
            "gate",
            "resolve",
            "1",
            "proof",
            "--evidence",
            "file:docs/proof.txt",
        ],
    ));
    assert!(
        unrooted.contains("was selected without a project root")
            && unrooted.contains("--project-root <project-root>")
            && unrooted.contains("note --text-file"),
        "{unrooted}"
    );
    let absolute = format!("file:{}", root.0.join("docs/proof.txt").display());
    let absolute = refusal(&in_project(&[
        "gate",
        "resolve",
        "1",
        "proof",
        "--evidence",
        &absolute,
    ]));
    assert!(absolute.contains("(invalid_path)"), "{absolute}");
    let escape = refusal(&in_project(&[
        "gate",
        "resolve",
        "1",
        "proof",
        "--evidence",
        "file:../outside.txt",
    ]));
    assert!(escape.contains("(path_escape)"), "{escape}");
    let missing = refusal(&in_project(&[
        "gate",
        "resolve",
        "1",
        "proof",
        "--evidence",
        "file:target/missing.txt",
    ]));
    assert!(
        missing.contains("(missing)")
            && missing.contains("with a path relative to project root")
            && missing.contains("--evidence note:<summary>"),
        "{missing}"
    );
    let blocker_missing = refusal(&in_project(&[
        "gate",
        "resolve",
        "1",
        "input",
        "--evidence",
        "file:docs/input.txt",
    ]));
    assert!(blocker_missing.contains("(missing)"), "{blocker_missing}");
    let gates = in_project(&["gate", "list", "1"]);
    assert_success(&gates);
    assert!(!String::from_utf8_lossy(&gates.stdout).contains("[resolved]"));

    let actual = papertiger::sha256(
        b"proof
",
    );
    let wrong = "a".repeat(64);
    let mismatch = refusal(&in_project(&[
        "gate",
        "resolve",
        "1",
        "proof",
        "--evidence",
        "file:docs/proof.txt",
        "--sha256",
        &wrong,
    ]));
    assert!(
        mismatch.contains("(digest_mismatch)")
            && mismatch.contains(&format!("the file's SHA-256 is {actual}, not {wrong}"))
            && mismatch.contains(&format!("pass --sha256 {actual}, or omit --sha256")),
        "{mismatch}"
    );
    let blocker_mismatch = refusal(&in_project(&[
        "gate",
        "resolve",
        "1",
        "input",
        "--evidence",
        "file:docs/proof.txt",
        "--sha256",
        &wrong,
    ]));
    assert!(
        blocker_mismatch.contains("(digest_mismatch)"),
        "{blocker_mismatch}"
    );
    assert_success(&in_project(&[
        "gate",
        "resolve",
        "1",
        "proof",
        "--evidence",
        "file:docs/proof.txt",
        "--sha256",
        &actual,
    ]));
    let verified = in_project(&[
        "evidence",
        "verify",
        "--task",
        "1",
        "--classification",
        "all",
        "--json",
    ]);
    let verified: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    let proof = verified["projection"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|binding| binding["name"] == "proof")
        .expect("proof binding");
    assert_eq!(proof["status"], "verified");
    assert_success(&papertiger(
        &db.0,
        &[
            "gate",
            "resolve",
            "1",
            "input",
            "--evidence",
            "file:./docs/proof.txt",
            "--project-root",
            root_text,
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "gate",
            "resolve",
            "1",
            "commit",
            "--evidence",
            "commit:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ],
    ));
    let mixed = papertiger(&db.0, &["--project-root", root_text, "status"]);
    assert!(
        String::from_utf8_lossy(&mixed.stderr)
            .contains("`evidence verify` and `gate resolve` alone")
    );
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
        &[
            "add",
            "--kind",
            "feature",
            "mixed evidence",
            "--plan",
            "work",
        ],
    ));
    let root_text = root.0.to_str().unwrap();
    // New file: bindings must name existing files; they go missing afterwards.
    std::fs::create_dir(root.0.join("docs")).unwrap();
    for missing in ["docs/missing-a.txt", "docs/missing-b.txt"] {
        std::fs::write(
            root.0.join(missing),
            b"removed after binding
",
        )
        .unwrap();
    }
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
            &[
                "gate",
                "resolve",
                "1",
                name,
                "--evidence",
                locator,
                "--project-root",
                root_text,
            ],
        ));
    }
    for missing in ["docs/missing-a.txt", "docs/missing-b.txt"] {
        std::fs::remove_file(root.0.join(missing)).unwrap();
    }

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
    assert_eq!(first["schema"], "papertiger.evidence_verification.v3");
    assert_eq!(first["summary"]["binding_count"], 3);
    assert_eq!(first["summary"]["failed_count"], 2);
    assert_eq!(first["summary"]["unsupported_count"], 1);
    assert_eq!(first["summary"]["status_counts"]["missing"], 2);
    assert_eq!(first["summary"]["unsupported_scheme_counts"]["commit"], 1);
    assert_eq!(first["projection"]["classification"], "incomplete");
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

    let second = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
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
            "--classification",
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
    support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .expect("run papertiger")
}

fn papertiger_with_stdin(db: &Path, args: &[&str], input: &str) -> Output {
    let mut child = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
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
    let output = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
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
fn installer_commands_are_gone() {
    for command in [
        "setup-project",
        "uninstall-project",
        "setup-user",
        "uninstall-user",
    ] {
        let output = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .arg(command)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{command} must not exist");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn explicit_project_root_selects_one_authority_from_another_project() {
    let sandbox = TestDirectory::new("explicit-project-root");
    let canonical = sandbox.0.join("canonical");
    let foreign = sandbox.0.join("foreign");
    std::fs::create_dir(&canonical).expect("create canonical project");
    std::fs::create_dir(&foreign).expect("create foreign project");

    for project in [&canonical, &foreign] {
        let init = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .arg("--project-root")
            .arg(project)
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
        let plan = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .args(["plan", "add", slug, "Consumer plan"])
            .current_dir(project)
            .env_remove("PAPERTIGER_DB")
            .env_remove("PAPERTIGER_ACTOR")
            .output()
            .expect("add consumer plan");
        assert_success(&plan);
    }

    let foreign_before = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
        .args(["status", "--json"])
        .current_dir(&foreign)
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("read foreign authority before cross-project mutation");
    assert_success(&foreign_before);
    let foreign_before: serde_json::Value =
        serde_json::from_slice(&foreign_before.stdout).expect("parse foreign status");

    let add = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
        .arg("--project-root")
        .arg(&canonical)
        .args([
            "--actor",
            "cross-project-test",
            "add", "--kind", "feature",
            "Canonical cross-project outcome",
            "--plan",
            "canonical",
            "--start",
            "--intent",
            "One outcome remains in its initiating authority while implementation enters another repository.",
            "--why",
            "Exercise explicit authority selection from inside another project.",
        ])
        .current_dir(&foreign)
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_ACTOR")
        .output()
        .expect("add canonical task from foreign project");
    assert_success(&add);

    let canonical_task = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
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

    let foreign_after = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
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
fn explicit_project_root_init_creates_the_project_authority_and_its_ignore_rules() {
    let sandbox = TestDirectory::new("explicit-project-root-init");
    let repository = sandbox.0.join("repository");
    let plain = sandbox.0.join("plain");
    std::fs::create_dir_all(repository.join(".git")).expect("create repository");
    std::fs::write(repository.join(".gitignore"), "target/\n").expect("write ignore rules");
    std::fs::create_dir(&plain).expect("create plain directory");
    let init = |project: &std::path::Path| {
        support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .arg("--project-root")
            .arg(project)
            .arg("init")
            .current_dir(&sandbox.0)
            .env_remove("PAPERTIGER_DB")
            .output()
            .expect("initialize through the explicit project root")
    };
    let created = init(&repository);
    assert_success(&created);
    assert!(
        String::from_utf8_lossy(&created.stdout).contains("added Papertiger's ignore rules"),
        "{}",
        String::from_utf8_lossy(&created.stdout)
    );
    assert!(repository.join("state/papertiger.sqlite").is_file());
    assert!(!sandbox.0.join("state").exists());
    let ignore = std::fs::read_to_string(repository.join(".gitignore")).unwrap();
    assert!(
        ignore.starts_with("target/\n\n# Papertiger")
            && ignore.contains("/state/papertiger.sqlite-wal"),
        "{ignore}"
    );
    let again = init(&repository);
    assert_success(&again);
    assert!(String::from_utf8_lossy(&again.stdout).contains("nothing changed"));
    assert_eq!(
        std::fs::read_to_string(repository.join(".gitignore")).unwrap(),
        ignore
    );

    assert_success(&init(&plain));
    assert!(plain.join("state/papertiger.sqlite").is_file());
    assert!(
        !plain.join(".gitignore").exists(),
        "only a Git root gets ignore rules"
    );
}

#[test]
fn explicit_project_root_selects_the_existing_project_authority() {
    let sandbox = TestDirectory::new("project-root-default-authority");
    let project = sandbox.0.join("uninstalled");
    let elsewhere = sandbox.0.join("elsewhere");
    std::fs::create_dir_all(project.join("state")).expect("create project state directory");
    std::fs::create_dir(&elsewhere).expect("create unrelated working directory");
    let authority = project.join("state/papertiger.sqlite");
    let invoke = |args: &[&str]| {
        support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .args(args)
            .current_dir(&elsewhere)
            .env_remove("PAPERTIGER_DB")
            .env("PAPERTIGER_ACTOR", "project-root-test")
            .output()
            .expect("run papertiger")
    };
    let authority_arg = authority.to_str().expect("UTF-8 test path");
    assert_success(&invoke(&["--db", authority_arg, "init"]));
    assert_success(&invoke(&[
        "--db",
        authority_arg,
        "plan",
        "add",
        "work",
        "Work",
    ]));

    let root = project.to_str().expect("UTF-8 test path");
    assert_success(&invoke(&[
        "--project-root",
        root,
        "add",
        "--kind",
        "feature",
        "Selected by project root",
        "--plan",
        "work",
    ]));
    let status = invoke(&["--project-root", root, "status", "--json"]);
    assert_success(&status);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).expect("parse status");
    assert_eq!(status["active_plans"][0]["plan"]["slug"], "work");
    let task = invoke(&["--db", authority_arg, "show", "1", "--json"]);
    assert_success(&task);
    let task: serde_json::Value = serde_json::from_slice(&task.stdout).expect("parse task");
    assert_eq!(task["task"]["title"], "Selected by project root");
    assert!(!elsewhere.join("state").exists());
    assert!(!project.join("tools").exists());
}

#[test]
fn authority_selection_walks_up_and_refuses_missing_or_ambiguous_selection() {
    let sandbox = TestDirectory::new("authority-selection");
    let project = sandbox.0.join("project");
    let nested = project.join("src/deep");
    let outside = sandbox.0.join("outside");
    std::fs::create_dir_all(&nested).expect("create nested project directory");
    std::fs::create_dir(&outside).expect("create unrelated directory");
    let run = |cwd: &std::path::Path, args: &[&str]| {
        support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
            .args(args)
            .current_dir(cwd)
            .env_remove("PAPERTIGER_DB")
            .output()
            .expect("run papertiger")
    };

    let missing = run(&outside, &["status"]);
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(
        error.contains("no Papertiger authority (state/papertiger.sqlite) was found")
            && error.contains("--project-root <project-root> init"),
        "{error}"
    );
    assert!(!outside.join("state").exists());

    // Creating an authority names its root; a bare init nests nothing.
    let refused = run(&nested, &["init"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--project-root <project-root> init"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!nested.join("state").exists() && !project.join("state").exists());
    assert_success(&run(
        &nested,
        &["--project-root", project.to_str().unwrap(), "init"],
    ));
    assert_success(&run(&nested, &["plan", "add", "work", "Work"]));
    let status = run(&nested, &["status", "--json"]);
    assert_success(&status);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["active_plans"][0]["plan"]["slug"], "work");
    assert!(
        !nested.join("state").exists(),
        "a subdirectory finds the project authority"
    );
    let current = run(&nested, &["init"]);
    assert_success(&current);
    assert!(String::from_utf8_lossy(&current.stdout).contains("nothing changed"));

    let exact = run(
        &outside,
        &["--project-root", nested.to_str().unwrap(), "status"],
    );
    assert!(
        !exact.status.success(),
        "--project-root names one exact root"
    );
    assert!(
        String::from_utf8_lossy(&exact.stderr).contains("no Papertiger authority exists"),
        "{}",
        String::from_utf8_lossy(&exact.stderr)
    );

    std::fs::create_dir_all(outside.join("state/papertiger.sqlite")).unwrap();
    let occupied = run(&outside, &["status"]);
    assert!(!occupied.status.success());
    assert!(
        String::from_utf8_lossy(&occupied.stderr).contains("is not a regular file"),
        "{}",
        String::from_utf8_lossy(&occupied.stderr)
    );

    let override_db = sandbox.0.join("override.sqlite");
    let explicit_override = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
        .arg("--db")
        .arg(&override_db)
        .arg("--project-root")
        .arg(&project)
        .arg("status")
        .env_remove("PAPERTIGER_DB")
        .output()
        .expect("run ambiguous explicit database selection");
    assert!(!explicit_override.status.success());
    let error = String::from_utf8_lossy(&explicit_override.stderr);
    assert!(error.contains("one canonical authority"), "{error}");
    let environment_override = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
        .arg("--project-root")
        .arg(&project)
        .arg("status")
        .env("PAPERTIGER_DB", &override_db)
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
        root.contains("Project root whose state/papertiger.sqlite is the authority")
            && root.contains("every write requires one")
            && root.contains("else the harness and session")
            && root.contains("else the harness session"),
        "{root}"
    );
    assert!(!root.contains("invalid with"), "{root}");

    let plan = command_help(&["plan"]);
    for description in [
        "Create a plan",
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
        "Resolve an open gate with an evidence locator",
        "Waive an open gate with a rationale",
        "Reopen a resolved or waived gate",
        "Remove an open gate",
        "List every gate on one task",
    ] {
        assert!(
            gate.contains(description),
            "missing {description:?}:\n{gate}"
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
            && task_add.contains("Parent task: N, #N or its handle (e.g. G-N)")
            && task_add.contains("Scheduling level within readiness order"),
        "{task_add}"
    );
    let gate_add = command_help(&["gate", "add"]);
    assert!(
        gate_add.contains("Task (N, #N or handle) that owns the gate")
            && gate_add.contains("Proof required to resolve the gate"),
        "{gate_add}"
    );
    let commit_add = command_help(&["commit", "add"]);
    assert!(
        commit_add.contains("Stable repository label")
            && commit_add.contains("Optional context explaining why this snapshot is useful"),
        "{commit_add}"
    );
    let mise_record = command_help(&["mise", "record"]);
    assert!(
        mise_record.contains("Task (N, #N or handle) that owns the projection")
            && mise_record.contains("Mise inspector pipeline from stdin"),
        "{mise_record}"
    );
    let show = command_help(&["show"]);
    assert!(
        show.contains("Task: N, #N or its handle (e.g. F-N)"),
        "{show}"
    );
    assert!(!show.contains("shell-portable"), "{show}");
    let reject = command_help(&["reject"]);
    assert!(
        reject.contains("`reopen` can revisit it") && !reject.contains("re-litigation"),
        "{reject}"
    );
    let export = command_help(&["export"]);
    assert!(
        export.contains("events and Mise projections as papertiger.dump.v11 JSON"),
        "{export}"
    );
    let focus = command_help(&["focus"]);
    assert!(focus.contains("--include-blocked"), "{focus}");
    let reference_add = command_help(&["reference", "add"]);
    assert!(
        reference_add.contains("pull_request|issue|review|adr|input|other")
            && reference_add.contains("Locator of the external item"),
        "{reference_add}"
    );
    let inspect = command_help(&["history", "inspect"]);
    assert!(inspect.contains("Stored event id"), "{inspect}");
    let evidence = command_help(&["evidence", "verify"]);
    assert!(evidence.contains("--classification"), "{evidence}");
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
    assert_eq!(value["schema"], "papertiger.focus.v8");
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
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "task", "--plan", "campaign"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["note", "--text", "historical timestamp", "--task", "1"],
    ));
    let connection = rusqlite::Connection::open(&db.0).expect("open test authority");
    // Explicit API admission for intentional disposable-fixture construction.
    papertiger::begin_mutation(&connection)
        .unwrap()
        .commit()
        .unwrap();
    let guard: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='events_append_only_update'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Model history written before schema v11 refused malformed events.
    connection
        .execute_batch("DROP TRIGGER events_append_only_update;")
        .expect("model a pre-v11 authority");
    connection
        .execute("UPDATE events SET at='é'", [])
        .expect("simulate a malformed historical import");
    connection.execute_batch(&guard).unwrap();
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
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "first", "--plan", "work"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "second", "--plan", "work"],
    ));
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
    assert_eq!(value["schema"], "papertiger.task_context.v9");
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
    assert!(text.lines().next().unwrap().contains("F-1 first"));

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
    assert_success(&papertiger(&db.0, &["note", "--text", "inline note"]));
    assert_success(&papertiger(&db.0, &["note", "positional note"]));
    let missing_text = papertiger(&db.0, &["note", "--task", "1"]);
    assert!(!missing_text.status.success());
    assert!(String::from_utf8_lossy(&missing_text.stderr).contains("note needs its text"));
    let both = papertiger(
        &db.0,
        &["note", "--text", "inline", "--text-file", &note_path],
    );
    assert!(!both.status.success());
    assert!(String::from_utf8_lossy(&both.stderr).contains("cannot be used with"));
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
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "first", "--plan", "work"],
    ));

    let ambiguous = papertiger_with_stdin(
        &db.0,
        &[
            "add",
            "--kind",
            "feature",
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
            "--kind",
            "feature",
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
            "--kind",
            "feature",
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
    let add = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
        .args(["add", "--help"])
        .output()
        .unwrap();
    assert_success(&add);
    let add = String::from_utf8(add.stdout).unwrap();
    assert!(add.contains("--intent-file <PATH|->"), "{add}");
    assert!(
        add.contains("Read the intent as UTF-8 from PATH, or stdin with '-'"),
        "{add}"
    );

    let note = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
        .args(["note", "--help"])
        .output()
        .unwrap();
    assert_success(&note);
    let note = String::from_utf8(note.stdout).unwrap();
    assert!(note.contains("--text <TEXT>"), "{note}");
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
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "duplicate", "--plan", "work"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["add", "--kind", "feature", "canonical", "--plan", "work"],
    ));

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
        "F-1 retired into F-2"
    );

    let text = papertiger(&db.0, &["show", "1"]);
    assert_success(&text);
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.lines().next().unwrap().contains("F-1 duplicate"));
    assert!(
        text.contains("replacement ") && text.contains("F-2 canonical"),
        "{text}"
    );

    let json = papertiger(&db.0, &["show", "1", "--json"]);
    assert_success(&json);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["schema"], "papertiger.task_context.v9");
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
    assert!(
        log.contains("F-1 task/status from=proposed replacement_seq=2 to=retired"),
        "{log}"
    );
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
        restored_log.contains("replacement_seq=2")
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
        &[
            "add",
            "--kind",
            "feature",
            "final canonical",
            "--plan",
            "work",
        ],
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
            "--kind",
            "feature",
            "Object store recovery",
            "--plan",
            "work",
            "--intent",
            "preserve exact evidence",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "--kind",
            "feature",
            "Continue implementation",
            "--plan",
            "work",
        ],
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
            "--text",
            "continued from live state",
            "--task",
            "2",
        ],
    ));

    let status = papertiger(&db.0, &["status", "--json"]);
    assert_success(&status);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["schema"], "papertiger.status.v4");
    assert_eq!(
        status["authority"]["schema_version"],
        papertiger::SCHEMA_VERSION
    );
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
    assert_eq!(list["schema"], "papertiger.task_list.v2");
    assert_eq!(list["tasks"].as_array().map(Vec::len), Some(2));
    assert_no_internal_identity_keys(&list);

    let search = papertiger(&db.0, &["search", "object store", "--json"]);
    assert_success(&search);
    let search: serde_json::Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["schema"], "papertiger.search.v3");
    assert_eq!(search["results"][0]["task"]["seq"], 1);
    assert_eq!(search["results"][0]["plan"], "work");
    assert_eq!(search["results"][0]["excerpt"]["field"], "title");
    assert_no_internal_identity_keys(&search);

    let latest = papertiger(&db.0, &["log", "--limit", "2", "--json"]);
    assert_success(&latest);
    let latest: serde_json::Value = serde_json::from_slice(&latest.stdout).unwrap();
    assert_eq!(latest["schema"], "papertiger.event_log.v2");
    assert_eq!(latest["events"].as_array().map(Vec::len), Some(2));
    assert_eq!(latest["truncated"], true);
    let older_cursor = latest["continuation"]["token"].as_str().unwrap();
    let older = papertiger(&db.0, &["log", "--before-cursor", older_cursor, "--json"]);
    assert_success(&older);
    let head_cursor = latest["head"]["token"].as_str().unwrap().to_owned();
    assert_success(&papertiger(
        &db.0,
        &["note", "--text", "event after cursor", "--task", "1"],
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
    assert_eq!(receipt["dump_schema"], "papertiger.dump.v11");
    let dump: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&export_path).unwrap()).unwrap();
    assert_eq!(dump["schema"], "papertiger.dump.v11");
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
            &["add", "--kind", "feature", &parent_title, "--plan", "work"],
        ));
        assert_success(&papertiger(&db.0, &["start", &parent_seq]));
        assert_success(&papertiger(
            &db.0,
            &[
                "add",
                "--kind",
                "feature",
                &leaf_title,
                "--plan",
                "work",
                "--parent",
                &parent_seq,
            ],
        ));
        assert_success(&papertiger(&db.0, &["start", &leaf_seq]));
    }
    for (index, priority) in ["now", "now", "next", "next", "later", "later"]
        .into_iter()
        .enumerate()
    {
        let title = format!("Ready {}", index + 1);
        assert_success(&papertiger(
            &db.0,
            &[
                "add",
                "--kind",
                "feature",
                &title,
                "--plan",
                "work",
                "--priority",
                priority,
            ],
        ));
    }
    for index in 1..=4 {
        let note = format!("note {index}");
        assert_success(&papertiger(&db.0, &["note", "--text", &note]));
    }

    let status = papertiger(&db.0, &["status", "--json"]);
    assert_success(&status);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["schema"], "papertiger.status.v4");
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
    assert_eq!(work["ready"]["entries"][0]["task"]["title"], "Ready 1");

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
    // Text shows the eight most recent pickups and counts the rest.
    assert!(
        human.contains("> leaf F-10 Active leaf 5")
            && human.contains("> parent F-3 Canonical parent 2"),
        "{human}"
    );
    assert!(!human.contains("F-1 Canonical parent 1"), "{human}");
    assert!(
        human.contains(
            "... 2 more in progress; run `papertiger list --plan work --status in_progress --sort activity`"
        ),
        "{human}"
    );
    assert!(
        human.contains("1 ready task(s) omitted; run `papertiger focus --plan work`"),
        "{human}"
    );
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
            "--kind",
            "feature",
            "Parent original",
            "--plan",
            "work",
            "--intent",
            "parent intent",
            "--priority",
            "now",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &[
            "add",
            "--kind",
            "feature",
            "Child original",
            "--plan",
            "work",
            "--intent",
            "child intent",
            "--priority",
            "later",
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
            "next",
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
        "papertiger.task_definition_revision.v2"
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
    assert_eq!(edit["payload"]["changes"]["priority"]["before"], "later");
    assert_eq!(edit["payload"]["changes"]["priority"]["after"], "next");
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
            "next",
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
        // Explicit API admission for intentional disposable-fixture construction.
        papertiger::begin_mutation(&connection)
            .unwrap()
            .commit()
            .unwrap();
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
        // Explicit API admission for intentional disposable-fixture construction.
        papertiger::begin_mutation(&connection)
            .unwrap()
            .commit()
            .unwrap();
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
