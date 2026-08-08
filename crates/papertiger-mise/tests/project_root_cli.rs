use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

fn run(arguments: &[&str], current_directory: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
        .current_dir(current_directory)
        .args(arguments)
        .output()
        .expect("run papertiger-mise")
}

#[test]
fn project_root_binds_default_authority_from_an_unrelated_working_directory() {
    let fixture = tempdir().expect("fixture");
    let project = fixture.path().join("consumer");
    let caller = fixture.path().join("unrelated/nested");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::create_dir_all(&caller).expect("caller");
    let project_argument = project.to_str().expect("UTF-8 project path");

    let before = run(
        &["--project-root", project_argument, "status", "--json"],
        &caller,
    );
    assert!(
        before.status.success(),
        "{}",
        String::from_utf8_lossy(&before.stderr)
    );
    let before: Value = serde_json::from_slice(&before.stdout).expect("status JSON");
    assert_eq!(before["schema"], "papertiger-mise.project-status.v1");
    assert_eq!(before["initialized"], false);
    for field in ["project_root", "database", "object_store"] {
        let rendered = before[field].as_str().expect("portable path field");
        assert!(
            !rendered.starts_with("//?/"),
            "{field} leaked a Windows verbatim prefix: {rendered}"
        );
        assert!(
            !rendered.contains('\\'),
            "{field} contains a platform-specific separator: {rendered}"
        );
    }
    let corrective = before["corrective_command"]
        .as_str()
        .expect("corrective command");
    assert!(!corrective.contains("//?/"));
    assert!(!corrective.contains('\\'));
    assert!(!project.join("state").exists(), "status must be read-only");
    assert!(
        !caller.join("state").exists(),
        "caller must remain untouched"
    );

    let initialized = run(&["--project-root", project_argument, "init"], &caller);
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    assert!(project.join("state/papertiger-mise.sqlite").is_file());
    assert!(!caller.join("state").exists());

    let after = run(
        &["--project-root", project_argument, "status", "--json"],
        &caller,
    );
    assert!(
        after.status.success(),
        "{}",
        String::from_utf8_lossy(&after.stderr)
    );
    let after: Value = serde_json::from_slice(&after.stdout).expect("status JSON");
    assert_eq!(after["initialized"], true);
    assert_eq!(after["authority"]["campaign_count"], 0);
    assert_eq!(after["authority"]["open_reservation_count"], 0);
}

#[test]
fn project_root_refuses_a_missing_directory_without_creating_it() {
    let fixture = tempdir().expect("fixture");
    let missing = fixture.path().join("missing-project");
    let missing_argument = missing.to_str().expect("UTF-8 project path");
    let output = run(
        &["--project-root", missing_argument, "status", "--json"],
        fixture.path(),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("pass an existing consuming project"));
    assert!(!missing.exists());
}

#[test]
fn improvement_inputs_are_mise_owned_and_do_not_create_authority() {
    let fixture = tempdir().expect("fixture");
    let project = fixture.path().join("consumer");
    std::fs::create_dir(&project).expect("project");
    let brief = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/mise_profiles/example-project.runtime-readiness.brief.json");
    let brief_bytes = std::fs::read(&brief).expect("read synthetic example brief");
    let approval = fixture.path().join("approval.json");
    let output = fixture.path().join("draft.json");
    std::fs::write(
        &approval,
        serde_json::to_vec(&serde_json::json!({
            "schema": "papertiger.project-improvement-brief-approval.v1",
            "brief_sha256": papertiger::sha256(&brief_bytes),
            "approved_by": "test-operator",
            "approved_at": "2026-08-08T00:00:00Z",
            "decision": "compile_draft"
        }))
        .expect("serialize approval"),
    )
    .expect("write approval");

    let project_argument = project.to_str().expect("UTF-8 project path");
    let paradigms = run(
        &[
            "--project-root",
            project_argument,
            "improvement",
            "paradigms",
            "--json",
        ],
        fixture.path(),
    );
    assert!(
        paradigms.status.success(),
        "{}",
        String::from_utf8_lossy(&paradigms.stderr)
    );
    assert!(!project.join("state").exists());

    let compile = run(
        &[
            "--project-root",
            project_argument,
            "improvement",
            "compile",
            "--brief",
            brief.to_str().expect("UTF-8 brief path"),
            "--approval",
            approval.to_str().expect("UTF-8 approval path"),
            "--output",
            output.to_str().expect("UTF-8 output path"),
        ],
        fixture.path(),
    );
    assert!(!compile.status.success());
    assert!(String::from_utf8_lossy(&compile.stderr).contains("dirty observed project revision"));
    assert!(!output.exists());
    assert!(!project.join("state").exists());
}
