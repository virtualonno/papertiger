//! One-call decomposition: children, sibling and existing dependencies and
//! optional starts are created atomically; any invalid entry refuses all of it.
use super::{TestDatabase, assert_success, papertiger, papertiger_with_stdin};
use serde_json::{Value, json};
use std::process::Output;

fn outline(children: Value) -> String {
    json!({"schema": "papertiger.task_outline.v1", "children": children}).to_string()
}

fn decompose(db: &TestDatabase, parent: &str, outline: &str, extra: &[&str]) -> Output {
    let mut args = vec!["decompose", parent, "--outline-file", "-"];
    args.extend_from_slice(extra);
    papertiger_with_stdin(&db.0, &args, outline)
}

fn json_of(output: &Output) -> Value {
    assert_success(output);
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The receipt's task create events, which come one per child in outline order.
fn created(receipt: &Value) -> Vec<(i64, String)> {
    receipt["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["event"]["entity"] == "task" && event["event"]["kind"] == "create")
        .map(|event| {
            (
                event["task"]["seq"].as_i64().unwrap(),
                event["task"]["title"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn status(db: &TestDatabase, seq: i64) -> String {
    json_of(&papertiger(&db.0, &["show", &seq.to_string(), "--json"]))["task"]["status"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn log_head(db: &TestDatabase) -> Value {
    json_of(&papertiger(&db.0, &["log", "--json"]))["head"].clone()
}

fn task_count(db: &TestDatabase) -> usize {
    json_of(&papertiger(
        &db.0,
        &["list", "--all-plans", "--status", "unfinished", "--json"],
    ))["total"]
        .as_u64()
        .unwrap() as usize
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn decompose_creates_children_edges_and_ready_starts_in_one_receipt() {
    let db = TestDatabase::new("decompose-success");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(&db.0, &["add", "Finished prerequisite"]));
    assert_success(&papertiger(&db.0, &["done", "1"]));
    assert_success(&papertiger(&db.0, &["add", "Open prerequisite"]));
    assert_success(&papertiger(&db.0, &["add", "Records outcome"]));
    let children = outline(json!([
        {"key": "persist", "title": "Persist records", "intent": "Records survive restarts",
         "intent_source": "agent", "why": "Storage comes first", "deps": [1],
         "tags": ["storage"], "priority": 3},
        {"key": "serve", "title": "Serve records", "why": "Clients read records",
         "deps": ["persist"]},
        {"key": "measure", "title": "Measure record reads", "kind": "probe",
         "deps": [2, "serve"]},
        {"key": "announce", "title": "Announce records", "why": "Readers need the change"}
    ]));
    let receipt = json_of(&decompose(
        &db,
        "3",
        &children,
        &["--start-ready", "--json"],
    ));
    assert_eq!(receipt["schema"], "papertiger.mutation.v1");
    assert_eq!(receipt["changed"], true);
    assert_eq!(
        created(&receipt),
        [
            (4, "Persist records".to_owned()),
            (5, "Serve records".to_owned()),
            (6, "Measure record reads".to_owned()),
            (7, "Announce records".to_owned()),
        ]
    );
    let events = receipt["events"].as_array().unwrap();
    let kinds = events
        .iter()
        .map(|event| {
            format!(
                "{}/{}#{}",
                event["event"]["entity"].as_str().unwrap(),
                event["event"]["kind"].as_str().unwrap(),
                event["task"]["seq"]
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "task/create#4",
            "task/create#5",
            "task/create#6",
            "task/create#7",
            "dep/add#4",
            "dep/add#5",
            "dep/add#6",
            "dep/add#6",
            "task/status#4",
            "task/status#7",
        ]
    );
    assert_eq!(events[0]["event"]["why"], "Storage comes first");
    assert_eq!(events[5]["event"]["payload"]["on"], 4);
    assert_eq!(
        [4, 5, 6, 7].map(|seq| status(&db, seq)),
        ["in_progress", "proposed", "proposed", "in_progress"]
    );

    let persisted = json_of(&papertiger(&db.0, &["show", "4", "--json"]));
    assert_eq!(persisted["parent"]["seq"], 3);
    assert_eq!(persisted["tags"], json!(["storage"]));
    assert_eq!(persisted["task"]["priority"], 3);
    assert_eq!(persisted["task"]["intent_source"], "agent");
    let measured = json_of(&papertiger(&db.0, &["show", "6", "--json"]));
    assert_eq!(measured["task"]["kind"], "probe");
    let mut dependencies = measured["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|dependency| dependency["seq"].as_i64().unwrap())
        .collect::<Vec<_>>();
    dependencies.sort();
    assert_eq!(dependencies, [2, 5]);

    let export = papertiger(&db.0, &["export"]);
    assert_success(&export);
    let export = String::from_utf8(export.stdout).unwrap();
    for key in ["persist", "serve", "measure", "announce"] {
        assert!(
            !export.contains(&format!("\"{key}\"")),
            "batch-local key {key} was stored"
        );
    }

    let plain = decompose(
        &db,
        "3",
        &outline(json!([{"key": "follow", "title": "Follow up on records"}])),
        &[],
    );
    assert_success(&plain);
    let plain = String::from_utf8(plain.stdout).unwrap();
    assert!(plain.contains("#3 decomposed into 1 child task(s):"));
    assert!(plain.contains("follow -> #8"));
    assert_success(&papertiger(&db.0, &["audit"]));
}

#[test]
fn decompose_refuses_the_whole_outline_and_names_every_problem() {
    let db = TestDatabase::new("decompose-refusal");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "other", "Other"]));
    assert_success(&papertiger(
        &db.0,
        &["add", "Retired prerequisite", "--plan", "work"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["retire", "1", "--why", "no longer needed"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["add", "Grandparent", "--plan", "work"],
    ));
    assert_success(&papertiger(
        &db.0,
        &["add", "Parent", "--plan", "work", "--parent", "2"],
    ));
    assert_success(&papertiger(&db.0, &["add", "Foreign", "--plan", "other"]));
    assert_success(&papertiger(
        &db.0,
        &["add", "Downstream", "--plan", "work", "--dep", "3"],
    ));
    let head = log_head(&db);
    let children = outline(json!([
        {"key": "a", "title": "First"},
        {"key": "a", "title": "Second"},
        {"key": "1bad", "title": "Third"},
        {"key": "c", "title": " first "},
        {"key": "d", "title": "Fourth", "kind": "chore"},
        {"key": "e", "title": "Fifth", "intent_source": "agent"},
        {"key": "f", "title": "Sixth", "deps": ["missing", "#1", 1, 4, 3, 2, 5, "f", 99, true]},
        {"key": "g", "title": "Seventh", "why": "  "},
        {"key": "h", "title": "Eighth", "deps": ["i"]},
        {"key": "i", "title": "Ninth", "deps": ["j"]},
        {"key": "j", "title": "Tenth", "deps": ["h"]},
        {"key": "k", "title": ""},
        {"key": "l", "title": "Eleventh", "intent": "Has an intent", "intent_source": "bot"},
        {"key": "m", "title": "Twelfth", "tags": ["", "t".repeat(65), "dup", " dup "]},
        {"key": "n", "title": "Thirteenth", "deps": ["h", "h", 99, 99.0]},
        {"key": "o", "title": "   "}
    ]));
    let refused = decompose(&db, "3", &children, &["--json"]);
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    let message = stderr(&refused);
    for expected in [
        "task outline refused; nothing was created",
        "child 2 (\"a\"): key repeats child 1 (\"a\")",
        "child 3 (\"1bad\"): key must be 1-64 ASCII letters",
        "child 4 (\"c\"): title repeats child 1 (\"a\")",
        "child 5 (\"d\"): unknown task kind 'chore'",
        "child 6 (\"e\"): intent_source requires a nonblank intent",
        "child 7 (\"f\"): dependency \"missing\" names no key in this outline",
        "child 7 (\"f\"): dependency \"#1\" is a string; write an existing task as the integer 1",
        "child 7 (\"f\"): dependency #1 is retired",
        "child 7 (\"f\"): dependency #4 belongs to a different plan than parent #3",
        "child 7 (\"f\"): dependency #3 finishes only after parent #3, which waits for this child",
        "child 7 (\"f\"): dependency #2 finishes only after parent #3, which waits for this child",
        "child 7 (\"f\"): dependency #5 finishes only after parent #3, which waits for this child (#5 depends on #3); depend on a task that does not wait for #3, or first remove a dependency in that chain with `papertiger dep remove 5 3 --why <reason>`",
        "child 7 (\"f\"): dependency #2 finishes only after parent #3, which waits for this child (#2 waits for unfinished child #3); depend on a task that does not wait for #3
",
        "child 7 (\"f\"): cannot depend on itself",
        "child 7 (\"f\"): dependency #99 does not exist",
        "child 7 (\"f\"): dependency true must be a sibling key string or an existing task number",
        "child 8 (\"g\"): why must be nonblank",
        "sibling dependencies form a cycle: \"h\" -> \"i\" -> \"j\" -> \"h\"",
        "child 12 (\"k\"): task title must not be blank",
        "child 13 (\"l\"): unknown meaning source 'bot' (expected user|agent|external)",
        "child 14 (\"m\"): tag must not be blank",
        "child 14 (\"m\"): tag has 65 characters; shorten it to at most 64 characters",
        "child 14 (\"m\"): repeats tag \"dup\"; list each tag once",
        "child 15 (\"n\"): repeats dependency \"h\"",
        "child 15 (\"n\"): repeats dependency #99",
        "child 16 (\"o\"): task title must not be blank",
    ] {
        assert!(
            message.contains(expected),
            "missing {expected:?} in:\n{message}"
        );
    }
    assert_eq!(log_head(&db), head);
    assert_eq!(task_count(&db), 4);

    for (document, expected) in [
        (
            json!({"schema": "papertiger.task_outline.v2", "children": []}).to_string(),
            "set \"schema\": \"papertiger.task_outline.v1\"",
        ),
        (
            outline(json!([{"key": "a", "title": "A", "depends_on": ["b"]}])),
            "unknown field `depends_on`",
        ),
        (outline(json!([])), "task outline has no children"),
        ("not json".to_owned(), "task outline is not valid JSON"),
        (
            r#"{"schema":"papertiger.task_outline.v1","children":[{"key":"a","title":"A"}],"children":[{"key":"b","title":"B"}]}"#.to_owned(),
            "task outline repeats key \"children\" in one object; write each key once",
        ),
        (
            r#"{"schema":"papertiger.task_outline.v1","children":[{"key":"a","title":"A","title":"B"}]}"#.to_owned(),
            "task outline repeats key \"title\" in one object",
        ),
        (
            outline(Value::Array(
                (0..257)
                    .map(|index| json!({"key": format!("k{index}"), "title": format!("Child {index}")}))
                    .collect(),
            )),
            "task outline has 257 children, more than the limit of 256; group them under intermediate children",
        ),
        (
            " ".repeat(1024 * 1024 + 1),
            "task outline exceeds the 1048576-byte limit; split it into several decompose calls",
        ),
    ] {
        let refused = decompose(&db, "3", &document, &[]);
        assert!(!refused.status.success());
        let message = stderr(&refused);
        assert!(
            message.contains(expected),
            "missing {expected:?} in:\n{message}"
        );
    }
    let finished_parent = decompose(&db, "1", &outline(json!([{"key": "a", "title": "A"}])), &[]);
    assert!(!finished_parent.status.success());
    assert!(stderr(&finished_parent).contains(
        "parent #1 is retired; reopen it with `papertiger reopen 1 --why <reason>` before adding live children"
    ));
    assert_eq!(log_head(&db), head);
}

#[test]
fn decompose_names_the_plan_step_first_and_reads_outline_files() {
    let db = TestDatabase::new("decompose-plan-and-file");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "closed", "Closed"]));
    assert_success(&papertiger(&db.0, &["add", "Shipped parent"]));
    assert_success(&papertiger(&db.0, &["done", "1"]));
    assert_success(&papertiger(
        &db.0,
        &["plan", "set", "closed", "done", "--why", "shipped"],
    ));
    let children = outline(json!([{"key": "follow", "title": "Follow up", "priority": 2.0}]));
    let head = log_head(&db);
    let refused = decompose(&db, "1", &children, &[]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains(
            "plan 'closed' is done; reactivate it with `papertiger plan set closed active --why <reason>` and then parent #1 with `papertiger reopen 1 --why <reason>` before adding tasks"
        ),
        "{}",
        stderr(&refused)
    );
    assert_eq!(log_head(&db), head);

    assert_success(&papertiger(
        &db.0,
        &[
            "plan",
            "set",
            "closed",
            "active",
            "--why",
            "follow-up found",
        ],
    ));
    assert_success(&papertiger(
        &db.0,
        &["reopen", "1", "--why", "follow-up found"],
    ));
    let path = db.0.with_extension("outline.json");
    std::fs::write(&path, &children).unwrap();
    let applied = json_of(&papertiger(
        &db.0,
        &[
            "decompose",
            "1",
            "--outline-file",
            path.to_str().unwrap(),
            "--json",
        ],
    ));
    std::fs::remove_file(&path).unwrap();
    assert_eq!(created(&applied), [(2, "Follow up".to_owned())]);
    let child = json_of(&papertiger(&db.0, &["show", "2", "--json"]));
    assert_eq!(child["task"]["priority"], 2);
    assert_eq!(child["parent"]["seq"], 1);
}

#[test]
fn decompose_replays_refuse_while_children_are_live_and_starts_require_inputs() {
    let db = TestDatabase::new("decompose-replay");
    assert_success(&papertiger(&db.0, &["init"]));
    assert_success(&papertiger(&db.0, &["plan", "add", "work", "Work"]));
    assert_success(&papertiger(&db.0, &["add", "Parent"]));
    let children = outline(json!([
        {"key": "model", "title": "Model the data"},
        {"key": "check", "title": "Check the data", "deps": ["model"]}
    ]));
    let applied = json_of(&decompose(&db, "1", &children, &["--json"]));
    assert_eq!(created(&applied)[1].0, 3);
    let head = log_head(&db);
    let replay = decompose(&db, "1", &children, &["--json"]);
    assert!(!replay.status.success());
    assert!(replay.stdout.is_empty());
    let message = stderr(&replay);
    assert!(message.contains("title duplicates live child #2 of #1"));
    assert!(message.contains("title duplicates live child #3 of #1"));
    assert!(message.contains("inspect `papertiger show 1` instead of replaying it"));
    assert_eq!(log_head(&db), head);

    let missing_why = decompose(
        &db,
        "1",
        &outline(json!([{"key": "ship", "title": "Ship the data"}])),
        &["--start-ready"],
    );
    assert!(!missing_why.status.success());
    assert!(stderr(&missing_why).contains("--start-ready would start it, so it needs a \"why\""));
    assert_success(&papertiger(
        &db.0,
        &[
            "plan",
            "set",
            "work",
            "paused",
            "--why",
            "waiting on review",
        ],
    ));
    let head = log_head(&db);
    let paused = decompose(
        &db,
        "1",
        &outline(json!([{"key": "ship", "title": "Ship the data", "why": "Ready now"}])),
        &["--start-ready"],
    );
    assert!(!paused.status.success());
    assert!(stderr(&paused).contains("--start-ready needs an active plan, but the plan is paused"));
    assert_eq!(log_head(&db), head);
    let proposed = json_of(&decompose(
        &db,
        "1",
        &outline(json!([{"key": "ship", "title": "Ship the data"}])),
        &["--json"],
    ));
    assert_eq!(created(&proposed), [(4, "Ship the data".to_owned())]);
    assert_eq!(status(&db, 4), "proposed");

    // Titles are compared only with live children, so once the earlier
    // children are finished the same outline creates new ones.
    assert_success(&papertiger(
        &db.0,
        &["plan", "set", "work", "active", "--why", "review done"],
    ));
    for seq in ["2", "3"] {
        assert_success(&papertiger(&db.0, &["done", seq]));
    }
    let again = json_of(&decompose(&db, "1", &children, &["--json"]));
    assert_eq!(
        created(&again),
        [
            (5, "Model the data".to_owned()),
            (6, "Check the data".to_owned()),
        ]
    );
}
