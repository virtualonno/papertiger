//! Validate actual CLI responses, including refusal controls, against the
//! published schema. Runs inside the ordinary workspace suite without Python.
use super::{TestDatabase, assert_success};
use serde_json::{Value, json};
use std::process::Command;

#[test]
fn emitted_contracts_match_schema_and_reject_malformed_records() {
    let db = TestDatabase::new("schema-contracts");
    let invoke = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_papertiger"))
            .arg("--db")
            .arg(&db.0)
            .args(args)
            .env("PAPERTIGER_ACTOR", "schema-fixture")
            .env("PAPERTIGER_SESSION", "schema-fixture")
            .env_remove("PAPERTIGER_DB")
            .env_remove("PAPERTIGER_MODEL")
            .env_remove("PAPERTIGER_REASONING_EFFORT")
            .output()
            .unwrap();
        assert_success(&output);
        output.stdout
    };
    let schema: Value = serde_json::from_slice(&invoke(&["schema"])).unwrap();
    jsonschema::draft202012::meta::validate(&schema).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let run = |args: &[&str]| {
        let value: Value = serde_json::from_slice(&invoke(args)).unwrap();
        validator
            .validate(&value)
            .unwrap_or_else(|error| panic!("response for {args:?} violates its schema: {error}"));
        value
    };
    invoke(&["init"]);
    run(&["focus", "--json"]);
    run(&["plan", "add", "source", "Source", "--json"]);
    run(&["plan", "add", "destination", "Destination", "--json"]);
    run(&[
        "add",
        "Selected work",
        "--plan",
        "source",
        "--model",
        "fixture-author",
        "--reasoning-effort",
        "high",
        "--start",
        "--why",
        "verify structured entry",
        "--json",
    ]);
    run(&[
        "add",
        "Related work",
        "--plan",
        "source",
        "--dep",
        "1",
        "--json",
    ]);
    run(&[
        "reference",
        "add",
        "1",
        "https://example.test/review",
        "--kind",
        "review",
        "--json",
    ]);
    run(&[
        "gate",
        "add",
        "1",
        "proof",
        "--kind",
        "test",
        "--requirement",
        "fixture proof",
        "--json",
    ]);
    let context = run(&["show", "1", "--json"]);
    assert_eq!(
        context["activity"]["created_event"]["reasoning_effort"],
        "high"
    );
    let selection = run(&["focus", "--plan", "source", "--all", "--json"]);
    assert_eq!(selection["schema"], "papertiger.focus.v7");
    assert_eq!(
        selection["entries"][0]["pickup"]["session"],
        "schema-fixture"
    );
    assert_eq!(selection["entries"][0]["readiness"], "mine");
    assert_eq!(selection["entries"][1]["blockers"], json!(["dep:#1"]));
    let inventory = run(&["list", "--all-plans", "--status", "unfinished", "--json"]);
    assert_eq!(inventory["tasks"][0]["task"]["seq"], 1);
    assert_eq!(inventory["tasks"][0]["plan"]["slug"], "source");
    run(&["status", "--json"]);
    run(&["list", "--plan", "source", "--json"]);
    run(&["log", "--json"]);
    run(&["export"]);
    run(&[
        "move-plan",
        "1",
        "2",
        "--plan",
        "destination",
        "--why",
        "relocate complete set",
        "--json",
    ]);
    run(&["export", "--plan", "destination"]);
    run(&[
        "gate",
        "waive",
        "1",
        "proof",
        "--why",
        "test waiver",
        "--json",
    ]);
    run(&[
        "done",
        "1",
        "--result",
        "fixture outcome",
        "--model",
        "fixture-reviewer",
        "--reasoning-effort",
        "medium",
        "--json",
    ]);
    let completed = run(&["show", "1", "--json"]);
    assert_eq!(
        completed["activity"]["completed_event"]["reasoning_effort"],
        "medium"
    );

    for (path, invalid) in [
        ("/task/status", json!("duplicate")),
        ("/task/seq", json!("1")),
        ("/activity/created_event/model", json!("invalid model")),
        ("/activity/created_event/reasoning_effort", json!(17)),
        (
            "/activity/created_event/reasoning_effort",
            json!("very high"),
        ),
    ] {
        let mut corrupt = context.clone();
        *corrupt.pointer_mut(path).unwrap() = invalid;
        assert!(!validator.is_valid(&corrupt), "accepted invalid {path}");
    }
    let mut corrupt = context.clone();
    corrupt["dependents"][0]["intent"] = json!("must be a summary");
    assert!(!validator.is_valid(&corrupt));
    let mut missing_pickup = selection.clone();
    missing_pickup["entries"][0]
        .as_object_mut()
        .unwrap()
        .remove("pickup");
    assert!(!validator.is_valid(&missing_pickup));
    let mut full_task = selection.clone();
    full_task["entries"][0]["task"]["intent"] = json!("belongs in show");
    assert!(!validator.is_valid(&full_task));
    let mut full_plan = selection.clone();
    full_plan["plan"]["intent"] = json!("belongs in plan context");
    assert!(!validator.is_valid(&full_plan));
}
