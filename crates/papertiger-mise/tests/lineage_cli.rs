mod support;

#[test]
fn parent_promotion_preflight_contract_is_exposed() {
    let output = support::mise_command()
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
fn authorship_flags_are_global_and_name_their_environment() {
    let reserve = help(&["budget", "reserve"]);
    for (flag, environment) in [
        ("--model", "PAPERTIGER_MODEL"),
        ("--actor", "PAPERTIGER_ACTOR"),
        ("--session", "PAPERTIGER_SESSION"),
        ("--reasoning-effort", "PAPERTIGER_REASONING_EFFORT"),
    ] {
        assert!(reserve.contains(flag), "budget reserve help omitted {flag}");
        assert!(
            reserve.contains(environment),
            "budget reserve help omitted {environment}"
        );
    }
    let record = help(&["candidate", "record"]);
    assert!(record.contains("--authored-by <MODEL>"), "{record}");
}

#[test]
fn candidate_cli_exposes_only_typed_material_writes() {
    let record = support::mise_command()
        .args(["candidate", "record", "--help"])
        .output()
        .expect("launch candidate record help");
    assert!(record.status.success());
    let record_help = String::from_utf8(record.stdout).expect("record help is UTF-8");
    assert!(record_help.contains("--material"));
    assert!(!record_help.contains("--patch"));

    let build = support::mise_command()
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
    let output = support::mise_command()
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
    let output = support::mise_command()
        .arg("--db")
        .arg(&database)
        .args(["candidate", "record", "--proposal"])
        .arg(temporary.path().join("absent-proposal.json"))
        .arg("--material")
        .arg(temporary.path().join("absent-material.json"))
        .args(["--reservation", "candidate-reservation", "--objects"])
        .arg(&objects)
        .args(["--authored-by", "test-author"])
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
    let output = support::mise_command()
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
fn operator_surface_uses_canonical_command_names() {
    for command in [
        &["paired", "execute-next"][..],
        &["paired", "show-execution"],
        &["paired", "list-executions"],
        &["improvement", "verify-registry"],
        &["improvement", "verify-brief"],
        &["promotion", "rederive"],
        &["projection", "export"],
    ] {
        help(command);
    }
}

#[test]
fn unknown_paired_execution_is_named_the_same_by_every_command() {
    let root = tempfile::tempdir().expect("project root");
    let init = support::mise_command()
        .args(["--project-root"])
        .arg(root.path())
        .arg("init")
        .output()
        .expect("launch init");
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    for command in [
        &["paired", "show-execution", "nope"][..],
        &["paired", "recover", "nope"],
    ] {
        let output = support::mise_command()
            .args(["--project-root"])
            .arg(root.path())
            .args(command)
            .output()
            .expect("launch paired command");
        assert!(
            !output.status.success(),
            "{command:?} accepted an unknown execution"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("unknown paired execution 'nope'"),
            "{command:?}: {stderr}"
        );
    }

    let output = support::mise_command()
        .args(["--project-root"])
        .arg(root.path())
        .args(["paired", "cancel", "nope", "--why", "operator stop"])
        .output()
        .expect("launch paired cancel");
    assert!(
        !output.status.success(),
        "cancel accepted an unknown execution"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cancellation requires a launched execution"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("Error code"),
        "trigger refusal leaked SQLite noise: {stderr}"
    );
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
    }
}

#[test]
fn blank_rationale_refuses_before_opening_an_authority() {
    let root = std::env::temp_dir();
    let output = support::mise_command()
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

#[test]
fn writes_require_a_caller_model_while_commands_that_write_nothing_ignore_it() {
    let root = tempfile::tempdir().expect("project root");
    let mise = |args: &[&str]| {
        support::mise_command()
            .arg("--project-root")
            .arg(root.path())
            .args(args)
            .output()
            .expect("launch papertiger-mise")
    };
    // Commands that record no event ignore authorship flags, even a model no
    // write would accept.
    for args in [
        &["--model", "runtime", "init"][..],
        &["--model", "runtime", "status", "--json"],
        &["--model", "runtime", "promotion", "list"],
        &["--model", "two words", "execution-status"],
    ] {
        let output = mise(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let reserve = [
        "budget",
        "reserve",
        "campaign-a01",
        "reservation-1",
        "--amount",
        "candidates=1",
    ];
    let unattributed = support::mise_command()
        .env_remove("PAPERTIGER_MODEL")
        .arg("--project-root")
        .arg(root.path())
        .args(reserve)
        .output()
        .expect("launch unattributed write");
    assert!(!unattributed.status.success());
    let stderr = String::from_utf8_lossy(&unattributed.stderr);
    assert!(stderr.contains("--model <model-id>"), "{stderr}");
    assert!(stderr.contains("PAPERTIGER_MODEL=human"), "{stderr}");

    // A caller can never claim the runtime's own transitions.
    let output = mise(&[&["--model", "runtime"][..], &reserve].concat());
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reserved for transitions a runtime performs itself"),
        "{stderr}"
    );

    // The operator's manual model is refused inside a harness.
    let output = support::mise_command()
        .env("PAPERTIGER_MODEL", "human")
        .env("CODEX_THREAD_ID", "thread-1")
        .arg("--project-root")
        .arg(root.path())
        .args(reserve)
        .output()
        .expect("launch harnessed write");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("runs inside Codex"), "{stderr}");
}

#[test]
fn candidate_record_requires_its_authoring_model_before_creating_cas() {
    let root = tempfile::tempdir().expect("project root");
    let init = support::mise_command()
        .arg("--project-root")
        .arg(root.path())
        .arg("init")
        .output()
        .expect("launch init");
    assert!(init.status.success());
    let objects = root.path().join("objects");
    let record = |authored_by: Option<&str>| {
        let mut command = support::mise_command();
        command
            .arg("--project-root")
            .arg(root.path())
            .args([
                "candidate",
                "record",
                "--proposal",
                "absent-proposal.json",
                "--material",
                "absent-material.json",
                "--reservation",
                "candidate-reservation",
                "--objects",
            ])
            .arg(&objects);
        if let Some(model) = authored_by {
            command.args(["--authored-by", model]);
        }
        command.output().expect("launch candidate record")
    };
    let missing = record(None);
    assert!(!missing.status.success());
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("--authored-by <MODEL>"), "{stderr}");
    let runtime = record(Some("runtime"));
    assert!(!runtime.status.success());
    let stderr = String::from_utf8_lossy(&runtime.stderr);
    assert!(
        stderr.contains("candidate record --authored-by <model-id>"),
        "{stderr}"
    );
    assert!(!objects.exists(), "a refused author created a stray CAS");
}
