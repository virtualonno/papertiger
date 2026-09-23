use std::process::Command;

#[test]
fn parent_promotion_preflight_contract_is_exposed() {
    let output = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
        .args(["promotion", "verify-parent", "--help"])
        .output()
        .expect("launch papertiger-mise parent-promotion preflight help");
    assert!(
        output.status.success(),
        "parent-promotion preflight help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8(output.stdout).expect("preflight help is UTF-8");
    for required in [
        "--papertiger-db",
        "--nomination",
        "--successor-manifest",
        "--task",
        "--gate",
        "--evidence",
        "--sha256",
        "--objects",
    ] {
        assert!(
            help.contains(required),
            "parent-promotion preflight help omitted {required}"
        );
    }
}

#[test]
fn actor_attribution_environment_is_exposed() {
    let output = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
        .arg("--help")
        .output()
        .expect("launch papertiger-mise help");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    assert!(
        help.contains("[env: PAPERTIGER_ACTOR="),
        "top-level actor option must honor the shared attribution environment"
    );
}

#[test]
fn candidate_cli_exposes_only_typed_material_writes() {
    let record = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
        .args(["candidate", "record", "--help"])
        .output()
        .expect("launch candidate record help");
    assert!(record.status.success());
    let record_help = String::from_utf8(record.stdout).expect("record help is UTF-8");
    assert!(record_help.contains("--material"));
    assert!(!record_help.contains("--patch"));

    let build = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
        .args(["candidate", "build-material", "--help"])
        .output()
        .expect("launch candidate build-material help");
    assert!(build.status.success());
    let build_help = String::from_utf8(build.stdout).expect("build help is UTF-8");
    for required in ["--repository", "--base-tree", "--result-tree", "--output"] {
        assert!(
            build_help.contains(required),
            "material builder omitted {required}"
        );
    }
}

#[test]
fn campaign_preflight_failure_reports_json_without_creating_a_database() {
    let temporary = tempfile::tempdir().expect("temporary preflight root");
    let database = temporary.path().join("absent-authority.sqlite");
    let manifest = temporary.path().join("absent-manifest.json");
    let output = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
        .arg("--db")
        .arg(&database)
        .args(["campaign", "preflight"])
        .arg(&manifest)
        .output()
        .expect("launch campaign preflight");

    assert!(
        !output.status.success(),
        "missing manifest must fail closed"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("preflight stdout is one JSON report");
    assert_eq!(report["schema"], "papertiger-mise.campaign_preflight.v2");
    assert_eq!(report["ready"], false);
    assert_eq!(report["defects"][0]["check"], "manifest.path");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("campaign preflight found 1 defect(s)"),
        "stderr omitted bounded refusal: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!database.exists(), "preflight created a Mise authority");
}

#[test]
fn candidate_record_validates_authority_before_creating_cas() {
    let temporary = tempfile::tempdir().expect("temporary candidate root");
    let database = temporary.path().join("absent-authority.sqlite");
    let objects = temporary.path().join("objects");
    let output = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
        .arg("--db")
        .arg(&database)
        .args(["candidate", "record", "--proposal"])
        .arg(temporary.path().join("absent-proposal.json"))
        .arg("--material")
        .arg(temporary.path().join("absent-material.json"))
        .args(["--reservation", "candidate-reservation", "--objects"])
        .arg(&objects)
        .output()
        .expect("launch candidate record");

    assert!(
        !output.status.success(),
        "absent authority must fail closed"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no papertiger-mise authority exists at"),
        "candidate record did not validate authority first: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("papertiger-mise --db"),
        "missing-authority refusal omitted its corrective command: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!database.exists(), "candidate record created an authority");
    assert!(!objects.exists(), "candidate record created a stray CAS");
}

fn help(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
        .args(args)
        .arg("--help")
        .output()
        .expect("launch papertiger-mise help");
    assert!(
        output.status.success(),
        "{args:?} --help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("help is UTF-8")
}

#[test]
fn paired_surface_names_executions_and_retires_run_commands() {
    for command in ["execute-next", "show-execution", "list-executions"] {
        help(&["paired", command]);
    }
    for retired in ["run-next", "show-run", "list-runs"] {
        let output = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
            .args(["paired", retired, "--help"])
            .output()
            .expect("launch retired paired command");
        assert!(!output.status.success(), "paired {retired} must not exist");
    }
    for (group, retired, current) in [
        ("improvement", "verify", "verify-registry"),
        ("improvement", "brief-verify", "verify-brief"),
        ("promotion", "inspect", "rederive"),
        ("projection", "inspect", "export"),
    ] {
        help(&[group, current]);
        let output = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
            .args([group, retired, "--help"])
            .output()
            .expect("launch retired command");
        assert!(!output.status.success(), "{group} {retired} must not exist");
    }
}

#[test]
fn rationale_flags_are_why_pairs() {
    for command in [
        &["budget", "release"][..],
        &["candidate", "abandon-materialization"],
        &["trial", "cancel"],
        &["trial", "abandon"],
        &["paired", "cancel"],
    ] {
        let text = help(command);
        assert!(text.contains("--why <TEXT>"), "{command:?} lacks --why");
        assert!(
            text.contains("--why-file <PATH|->"),
            "{command:?} lacks --why-file"
        );
        assert!(
            !text.contains("--reason"),
            "{command:?} still exposes --reason"
        );
    }
}

#[test]
fn blank_rationale_refuses_before_opening_an_authority() {
    let root = std::env::temp_dir();
    let output = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"))
        .args(["--project-root", root.to_str().unwrap()])
        .args(["--db", "absent-mise-authority.sqlite"])
        .args([
            "budget",
            "release",
            "campaign-a01",
            "reservation",
            "--why",
            "  ",
        ])
        .output()
        .expect("launch budget release");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--why requires nonblank text"),
        "unexpected refusal: {stderr}"
    );
    assert!(!root.join("absent-mise-authority.sqlite").exists());
}

#[test]
fn promotion_help_is_honest_and_planner_database_defaults_agree() {
    for command in ["derive", "verify"] {
        let text = help(&["promotion", command]);
        assert!(
            text.contains("Unavailable until sealed confirmation attestation lands"),
            "promotion {command} help must state that it always refuses"
        );
    }
    for command in [
        &["campaign", "admit-successor"][..],
        &["promotion", "verify-parent"],
        &["promotion", "verify"],
    ] {
        let text = help(command);
        assert!(
            text.contains("[default: state/papertiger.sqlite]"),
            "{command:?} must default --papertiger-db to state/papertiger.sqlite"
        );
    }
    assert!(help(&["trial"]).contains("cancel"));
    assert!(help(&["object"]).contains("Reverify and print one content-addressed"));
}
