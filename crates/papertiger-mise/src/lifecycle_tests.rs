use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use ed25519_dalek::{Signer, SigningKey};
use rusqlite::Connection;
use tempfile::tempdir;

use super::*;
use crate::git_materialization::{git_run, reject_checkout_transform_rules};
use crate::path_identity::{canonical_or_pending_absolute, trial_path_identity};

#[test]
fn deterministic_runtime_refuses_paired_campaigns() {
    let mut manifest = crate::manifest::tests::valid_manifest();
    manifest.paired_analysis = Some(crate::statistics::tests::plan());
    let error = require_deterministic_runtime(&manifest)
        .expect_err("paired campaigns require the paired adapter runner");
    assert!(error.to_string().contains("cannot execute or adjudicate"));
}

#[test]
fn rust_trial_environment_is_fresh_offline_and_trial_scoped() {
    let root = tempdir().expect("workspace root");
    let mut manifest = crate::manifest::tests::valid_manifest();
    manifest.execution_limits.workspace_root_locator =
        canonical_or_pending_absolute(root.path()).expect("workspace path");
    manifest.execution_limits.runtime_root_locator =
        Some(canonical_or_pending_absolute(root.path()).expect("runtime root path"));
    let executable = manifest.evaluator.launcher_locator.clone();
    let executable_sha256 = manifest.evaluator.launcher_sha256.clone();
    manifest.evaluator.rust_build_environment = Some(crate::manifest::RustBuildEnvironment {
        cargo_executable_locator: executable.clone(),
        cargo_executable_sha256: executable_sha256.clone(),
        rustc_executable_locator: executable.clone(),
        rustc_executable_sha256: executable_sha256.clone(),
        toolchain: "1.95.0".to_owned(),
        lockfile_locator: "Cargo.lock".to_owned(),
        lockfile_sha256: crate::manifest::Sha256Digest("1".repeat(64)),
        cargo_config_locator: ".cargo/config.toml".to_owned(),
        cargo_config_sha256: crate::manifest::Sha256Digest("2".repeat(64)),
        vendored_sources_locator: "vendor".to_owned(),
        vendored_sources_tree: "3".repeat(40),
        linker: Some(crate::manifest::RustLinkerBinding {
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            executable_locator: executable,
            executable_sha256,
        }),
    });
    let environment = expected_trial_environment(&manifest, "campaign-a01", "trial-01")
        .expect("derived environment");
    let path_identity = trial_path_identity("campaign-a01", "trial-01");
    assert_eq!(
        environment.get("CARGO_NET_OFFLINE").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        environment.get("CARGO_INCREMENTAL").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        environment
            .get("PAPERTIGER_MISE_CARGO_EXECUTABLE")
            .map(String::as_str),
        manifest
            .evaluator
            .rust_build_environment
            .as_ref()
            .map(|rust| rust.cargo_executable_locator.as_str())
    );
    assert_eq!(
        environment
            .get("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER")
            .map(String::as_str),
        manifest
            .evaluator
            .rust_build_environment
            .as_ref()
            .and_then(|rust| rust.linker.as_ref())
            .map(|linker| linker.executable_locator.as_str())
    );
    let linker_directory = Path::new(
        manifest
            .evaluator
            .rust_build_environment
            .as_ref()
            .and_then(|rust| rust.linker.as_ref())
            .expect("linker binding")
            .executable_locator
            .as_str(),
    )
    .parent()
    .expect("linker directory");
    assert_eq!(
        std::env::split_paths(&environment["PATH"])
            .next()
            .as_deref(),
        Some(linker_directory)
    );
    assert!(
        environment["CARGO_HOME"]
            .replace('\\', "/")
            .ends_with(&format!("/rust/{path_identity}/cargo-home"))
    );
    assert_eq!(environment["TEMP"], environment["TMP"]);
    assert_eq!(environment["TMP"], environment["TMPDIR"]);
    assert!(
        environment["TMPDIR"]
            .replace('\\', "/")
            .ends_with(&format!("/rust/{path_identity}/temporary"))
    );
    prepare_trial_environment(&manifest, &environment).expect("fresh environment paths");
    assert!(Path::new(&environment["CARGO_HOME"]).is_dir());
    assert!(Path::new(&environment["TMPDIR"]).is_dir());
    let error = prepare_trial_environment(&manifest, &environment)
        .expect_err("reused mutable Rust environment must fail closed");
    assert!(error.to_string().contains("already exists"), "{error:#}");
}
use crate::budget::{BudgetRequest, BudgetResource, BudgetSettlement, reserve_budget};
use crate::candidate::{CandidateProposal, Hypothesis, bind_legacy_patch_candidate};
use crate::object::preserve_object;
use crate::store::{CampaignAdmission, admit_campaign, init};

fn prepared() -> (Connection, tempfile::TempDir, BoundCandidate) {
    prepared_with_maximum_output(8 * 1024)
}

fn prepared_with_maximum_output(
    maximum_output_bytes: u64,
) -> (Connection, tempfile::TempDir, BoundCandidate) {
    prepared_with_evaluator(maximum_output_bytes, "success")
}

fn prepared_with_evaluator(
    maximum_output_bytes: u64,
    evaluator_mode: &str,
) -> (Connection, tempfile::TempDir, BoundCandidate) {
    prepared_with_evaluator_and_request(maximum_output_bytes, 5_000, evaluator_mode, 0)
}

fn prepared_with_evaluator_and_request(
    maximum_output_bytes: u64,
    maximum_wall_time_ms: u64,
    evaluator_mode: &str,
    extra_objectives: usize,
) -> (Connection, tempfile::TempDir, BoundCandidate) {
    prepared_with_evaluator_request_and_frozen_rust_inputs(
        maximum_output_bytes,
        maximum_wall_time_ms,
        evaluator_mode,
        extra_objectives,
        false,
    )
}

fn prepared_with_evaluator_request_and_frozen_rust_inputs(
    maximum_output_bytes: u64,
    maximum_wall_time_ms: u64,
    evaluator_mode: &str,
    extra_objectives: usize,
    freeze_rust_inputs: bool,
) -> (Connection, tempfile::TempDir, BoundCandidate) {
    let objects = tempdir().expect("objects");
    let source = objects.path().join("source");
    let runs = objects.path().join("runs");
    std::fs::create_dir_all(source.join("src")).expect("source directory");
    std::fs::create_dir_all(source.join("fixtures/mise")).expect("evaluator directory");
    std::fs::create_dir_all(&runs).expect("run directory");
    git_run(&source, &["init"], None, None, None).expect("initialize source");
    git_run(
        &source,
        &["config", "user.name", "Mise Test"],
        None,
        None,
        None,
    )
    .expect("Git user");
    git_run(
        &source,
        &["config", "user.email", "mise-test@example.invalid"],
        None,
        None,
        None,
    )
    .expect("Git email");
    git_run(
        &source,
        &["config", "core.autocrlf", "false"],
        None,
        None,
        None,
    )
    .expect("Git line endings");
    std::fs::write(source.join("src/fixture.rs"), b"old\n").expect("source fixture");
    let evaluator = include_bytes!("../tests/fixtures/lifecycle_evaluator.rs");
    std::fs::write(source.join("fixtures/mise/evaluator.rs"), evaluator)
        .expect("evaluator fixture");
    std::fs::write(
        source.join("fixtures/mise/exploration.json"),
        b"{\"fixture\":\"exploration\"}",
    )
    .expect("exploration fixture");
    if freeze_rust_inputs {
        std::fs::create_dir_all(source.join(".cargo")).expect("Cargo config directory");
        std::fs::create_dir_all(source.join("vendor/fixture")).expect("vendor directory");
        std::fs::write(source.join("Cargo.lock"), b"# frozen lockfile\n").expect("frozen lockfile");
        std::fs::write(
            source.join(".cargo/config.toml"),
            b"[net]\noffline = true\n",
        )
        .expect("frozen Cargo config");
        std::fs::write(source.join("vendor/fixture/source.rs"), b"// vendored\n")
            .expect("vendored source");
    }
    git_run(&source, &["add", "."], None, None, None).expect("stage source");
    git_run(&source, &["commit", "-m", "frozen base"], None, None, None).expect("commit source");
    let connection = Connection::open_in_memory().expect("database");
    init(&connection).expect("schema");
    let mut manifest = crate::manifest::tests::valid_manifest();
    manifest.containment = crate::manifest::ContainmentGrade::WorkspaceOnly;
    manifest.containment_requirement = None;
    manifest.execution_limits.network = crate::manifest::NetworkPolicy::Unrestricted;
    let launcher_directory = objects.path().join("launcher");
    std::fs::create_dir(&launcher_directory).expect("launcher directory");
    let launcher = launcher_directory.join(format!(
        "lifecycle-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(compiled_lifecycle_evaluator(), &launcher)
        .expect("copy lifecycle evaluator launcher");
    let launcher = std::fs::canonicalize(launcher).expect("canonical evaluator launcher");
    manifest.evaluator.launcher_locator =
        canonical_or_pending_absolute(&launcher).expect("evaluator locator");
    manifest.evaluator.launcher_sha256 = crate::manifest::Sha256Digest(sha256(
        &std::fs::read(&launcher).expect("evaluator launcher"),
    ));
    manifest.evaluator.argv[0] = manifest.evaluator.launcher_locator.clone();
    manifest.evaluator.argv = vec![
        manifest.evaluator.launcher_locator.clone(),
        "fixtures/mise/evaluator.rs".to_owned(),
    ];
    manifest.evaluator.evaluator_locator = "fixtures/mise/evaluator.rs".to_owned();
    manifest.evaluator.environment.insert(
        "PAPERTIGER_MISE_LIFECYCLE_FIXTURE_MODE".to_owned(),
        evaluator_mode.to_owned(),
    );
    manifest.execution_limits.maximum_trial_output_bytes = maximum_output_bytes;
    manifest.execution_limits.maximum_trial_wall_time_ms = maximum_wall_time_ms;
    for index in 0..extra_objectives {
        manifest.objectives.push(crate::manifest::ObjectiveSpec {
            key: format!("request-padding-{index}"),
            role: crate::manifest::ObjectiveRole::Diagnostic,
            direction: crate::manifest::ObjectiveDirection::Minimize,
            unit: "padding".to_owned(),
            minimum_practical_change: 0.0,
            regression_tolerance: 0.0,
            acceptance_threshold: None,
            target_value: None,
        });
    }
    for key in ["SystemRoot", "WINDIR", "ComSpec", "PATH", "PATHEXT"] {
        if let Ok(value) = std::env::var(key) {
            manifest.evaluator.environment.insert(key.to_owned(), value);
        }
    }
    manifest
        .holdouts
        .tiers
        .retain(|tier| tier.kind != crate::manifest::HoldoutTierKind::Confirmation);
    manifest.holdouts.disclosure_cap = manifest
        .holdouts
        .tiers
        .iter()
        .map(|tier| tier.maximum_disclosures)
        .sum();
    manifest.source.repository_locator =
        canonical_or_pending_absolute(&source).expect("source locator");
    manifest.source.base_commit = git_text(&source, &["rev-parse", "HEAD^{commit}"])
        .expect("base commit")
        .trim()
        .to_owned();
    manifest.source.base_tree = git_text(&source, &["rev-parse", "HEAD^{tree}"])
        .expect("base tree")
        .trim()
        .to_owned();
    manifest.execution_limits.workspace_root_locator =
        canonical_or_pending_absolute(&runs).expect("run locator");
    manifest.execution_limits.runtime_root_locator =
        Some(canonical_or_pending_absolute(objects.path()).expect("runtime root locator"));
    manifest.evaluator.evaluator_sha256.0 = sha256(evaluator);
    if freeze_rust_inputs {
        manifest.mutation_scope.protected_paths.extend([
            "Cargo.lock".to_owned(),
            ".cargo".to_owned(),
            "vendor".to_owned(),
        ]);
        let toolchain_directory = objects.path().join("toolchain");
        std::fs::create_dir(&toolchain_directory).expect("toolchain directory");
        let cargo =
            toolchain_directory.join(format!("cargo-fixture{}", std::env::consts::EXE_SUFFIX));
        let rustc =
            toolchain_directory.join(format!("rustc-fixture{}", std::env::consts::EXE_SUFFIX));
        let linker =
            toolchain_directory.join(format!("linker-fixture{}", std::env::consts::EXE_SUFFIX));
        for path in [&cargo, &rustc, &linker] {
            std::fs::copy(&launcher, path).expect("copy frozen toolchain executable");
        }
        let cargo = std::fs::canonicalize(cargo).expect("canonical Cargo fixture");
        let rustc = std::fs::canonicalize(rustc).expect("canonical rustc fixture");
        let linker = std::fs::canonicalize(linker).expect("canonical linker fixture");
        manifest.evaluator.rust_build_environment = Some(crate::manifest::RustBuildEnvironment {
            cargo_executable_locator: canonical_or_pending_absolute(&cargo).expect("Cargo locator"),
            cargo_executable_sha256: crate::manifest::Sha256Digest(sha256(
                &std::fs::read(&cargo).expect("Cargo bytes"),
            )),
            rustc_executable_locator: canonical_or_pending_absolute(&rustc).expect("rustc locator"),
            rustc_executable_sha256: crate::manifest::Sha256Digest(sha256(
                &std::fs::read(&rustc).expect("rustc bytes"),
            )),
            toolchain: "fixture-1.0.0".to_owned(),
            lockfile_locator: "Cargo.lock".to_owned(),
            lockfile_sha256: crate::manifest::Sha256Digest(sha256(
                &std::fs::read(source.join("Cargo.lock")).expect("lockfile bytes"),
            )),
            cargo_config_locator: ".cargo/config.toml".to_owned(),
            cargo_config_sha256: crate::manifest::Sha256Digest(sha256(
                &std::fs::read(source.join(".cargo/config.toml")).expect("config bytes"),
            )),
            vendored_sources_locator: "vendor".to_owned(),
            vendored_sources_tree: git_text(&source, &["rev-parse", "HEAD:vendor"])
                .expect("vendored tree")
                .trim()
                .to_owned(),
            linker: Some(crate::manifest::RustLinkerBinding {
                target_triple: "x86_64-pc-windows-msvc".to_owned(),
                executable_locator: canonical_or_pending_absolute(&linker).expect("linker locator"),
                executable_sha256: crate::manifest::Sha256Digest(sha256(
                    &std::fs::read(linker).expect("linker bytes"),
                )),
            }),
        });
    }
    manifest
        .holdouts
        .tiers
        .iter_mut()
        .find(|tier| tier.key == "exploration")
        .expect("exploration tier")
        .fixture_sha256
        .0 = sha256(
        &std::fs::read(source.join("fixtures/mise/exploration.json"))
            .expect("exploration fixture bytes"),
    );
    let admission = CampaignAdmission::from_manifest(&manifest).expect("admission");
    admit_campaign(&connection, "test", &admission).expect("campaign");
    let proposal = CandidateProposal {
        campaign_id: manifest.campaign_id.clone(),
        parent_candidate_ids: BTreeSet::new(),
        base_commit: manifest.source.base_commit.clone(),
        base_tree: manifest.source.base_tree.clone(),
        proposer: "fixture".to_owned(),
        proposal_policy_sha256: manifest.generation.proposal_policy_sha256.0,
        adapter_sha256: manifest.adapter.implementation_sha256.0,
        hypothesis: Hypothesis {
            mechanism: "bounded fixture change".to_owned(),
            expected_effects: vec!["lower work".to_owned()],
            possible_regressions: vec!["wrong answer".to_owned()],
            decisive_falsifiers: vec!["fixture differs".to_owned()],
        },
        changed_paths: BTreeSet::from(["src/fixture.rs".to_owned()]),
        changed_symbols: BTreeSet::from(["fixture".to_owned()]),
        semantic_class: "fixture-change".to_owned(),
        differentiator: None,
    };
    let candidate = bind_legacy_patch_candidate(
            proposal,
            b"diff --git a/src/fixture.rs b/src/fixture.rs\n--- a/src/fixture.rs\n+++ b/src/fixture.rs\n@@ -1 +1 @@\n-old\n+new\n".to_vec(),
        )
        .expect("candidate");
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "candidate-budget",
        &[
            BudgetRequest::new(BudgetResource::Candidates, 1).expect("candidate request"),
            BudgetRequest::new(
                BudgetResource::ArtifactBytes,
                u64::try_from(candidate.material_bytes.len()).expect("patch length"),
            )
            .expect("artifact request"),
        ],
    )
    .expect("candidate reservation");
    (connection, objects, candidate)
}

#[test]
fn public_candidate_record_refuses_legacy_patch_writes() {
    let (connection, objects, candidate) = prepared();
    let object = preserve_object(objects.path(), &candidate.material_bytes).expect("legacy patch");
    let error = record_candidate(
        &connection,
        "test",
        objects.path(),
        "candidate-budget",
        &candidate,
        &object,
    )
    .expect_err("public API must not mint new legacy patch candidates");
    assert!(
        error.to_string().contains("candidate build-material"),
        "{error:#}"
    );
}

struct CompiledLifecycleEvaluator {
    _directory: tempfile::TempDir,
    executable: PathBuf,
}

fn compiled_lifecycle_evaluator() -> &'static Path {
    static EVALUATOR: OnceLock<CompiledLifecycleEvaluator> = OnceLock::new();
    EVALUATOR
        .get_or_init(|| {
            let directory = tempdir().expect("compiled lifecycle evaluator directory");
            let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/lifecycle_evaluator.rs");
            let executable = directory.path().join(format!(
                "lifecycle-evaluator{}",
                std::env::consts::EXE_SUFFIX
            ));
            let output = Command::new("rustc")
                .arg(source)
                .args(["--edition", "2024", "-O", "-o"])
                .arg(&executable)
                .output()
                .expect("compile lifecycle evaluator");
            assert!(
                output.status.success(),
                "lifecycle evaluator compilation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            CompiledLifecycleEvaluator {
                _directory: directory,
                executable,
            }
        })
        .executable
        .as_path()
}

fn fake_materialization(
    connection: &Connection,
    objects: &Path,
    candidate: &BoundCandidate,
    reservation_id: &str,
) -> MaterializationRecord {
    let manifest =
        campaign_manifest(connection, &candidate.proposal.campaign_id).expect("manifest");
    let worktree = objects
        .join("runs")
        .join(format!("worktree-{}", candidate.candidate_id));
    git_run(
        Path::new(&manifest.source.repository_locator),
        &["worktree", "add", "--detach"],
        Some(&worktree),
        Some(&manifest.source.base_commit),
        None,
    )
    .expect("fixture worktree");
    if !candidate.material_bytes.is_empty() {
        git_run(
            &worktree,
            &["apply", "--index", "--whitespace=nowarn", "-"],
            None,
            None,
            Some(&candidate.material_bytes),
        )
        .expect("fixture patch");
    }
    let result_tree = git_text(&worktree, &["write-tree"])
        .expect("fixture result tree")
        .trim()
        .to_owned();
    let receipt = MaterializationReceipt {
        schema: "papertiger-mise.materialization.v1".to_owned(),
        campaign_id: candidate.proposal.campaign_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        base_commit: manifest.source.base_commit,
        base_tree: manifest.source.base_tree,
        patch_sha256: Some(candidate.material_sha256.clone()),
        material_sha256: None,
        result_tree: result_tree.clone(),
        worktree_locator: canonical_or_pending_absolute(&worktree).expect("worktree locator"),
        adapter_sha256: candidate.proposal.adapter_sha256.clone(),
    };
    let bytes = serde_json::to_vec(&receipt).expect("receipt");
    let object = preserve_object(objects, &bytes).expect("materialization receipt");
    record_artifact_in(connection, &object, "application/json").expect("receipt artifact");
    connection
            .execute(
                "INSERT INTO materializations
                 (candidate_id, reservation_id, receipt_sha256, result_tree, worktree_locator, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    candidate.candidate_id,
                    reservation_id,
                    object.sha256,
                    result_tree,
                    receipt.worktree_locator,
                    now()
                ],
            )
            .expect("fake materialization");
    connection
        .execute(
            "UPDATE candidates SET disposition='materialized', updated_at=?2 WHERE candidate_id=?1",
            params![candidate.candidate_id, now()],
        )
        .expect("materialized disposition");
    materialization_in(connection, &candidate.candidate_id)
        .expect("query materialization")
        .expect("durable materialization")
}

fn prepared_trial() -> (
    Connection,
    tempfile::TempDir,
    BoundCandidate,
    MaterializationRecord,
    MaterializationRecord,
) {
    prepared_trial_with_maximum_output(8 * 1024)
}

fn prepared_trial_with_maximum_output(
    maximum_output_bytes: u64,
) -> (
    Connection,
    tempfile::TempDir,
    BoundCandidate,
    MaterializationRecord,
    MaterializationRecord,
) {
    prepared_trial_with_evaluator(maximum_output_bytes, "success")
}

fn prepared_trial_with_evaluator(
    maximum_output_bytes: u64,
    evaluator_mode: &str,
) -> (
    Connection,
    tempfile::TempDir,
    BoundCandidate,
    MaterializationRecord,
    MaterializationRecord,
) {
    prepared_trial_with_evaluator_and_request(maximum_output_bytes, 5_000, evaluator_mode, 0)
}

fn prepared_trial_with_evaluator_and_request(
    maximum_output_bytes: u64,
    maximum_wall_time_ms: u64,
    evaluator_mode: &str,
    extra_objectives: usize,
) -> (
    Connection,
    tempfile::TempDir,
    BoundCandidate,
    MaterializationRecord,
    MaterializationRecord,
) {
    prepared_trial_with_evaluator_request_and_frozen_rust_inputs(
        maximum_output_bytes,
        maximum_wall_time_ms,
        evaluator_mode,
        extra_objectives,
        false,
    )
}

fn prepared_trial_with_evaluator_request_and_frozen_rust_inputs(
    maximum_output_bytes: u64,
    maximum_wall_time_ms: u64,
    evaluator_mode: &str,
    extra_objectives: usize,
    freeze_rust_inputs: bool,
) -> (
    Connection,
    tempfile::TempDir,
    BoundCandidate,
    MaterializationRecord,
    MaterializationRecord,
) {
    let (connection, objects, candidate) = prepared_with_evaluator_request_and_frozen_rust_inputs(
        maximum_output_bytes,
        maximum_wall_time_ms,
        evaluator_mode,
        extra_objectives,
        freeze_rust_inputs,
    );
    let patch = preserve_object(objects.path(), &candidate.material_bytes).expect("patch object");
    record_legacy_candidate_for_test(
        &connection,
        "test",
        objects.path(),
        "candidate-budget",
        &candidate,
        &patch,
    )
    .expect("candidate");
    let candidate_materialization = fake_materialization(
        &connection,
        objects.path(),
        &candidate,
        "fake-candidate-mat",
    );
    let manifest =
        campaign_manifest(&connection, &candidate.proposal.campaign_id).expect("manifest");
    let no_op = bind_legacy_patch_candidate(
        proposal_for(&manifest, "calibration-no-op", BTreeSet::new()),
        Vec::new(),
    )
    .expect("no-op");
    reserve_budget(
        &connection,
        "test",
        &manifest.campaign_id,
        "baseline-candidate-budget",
        &[
            BudgetRequest::new(BudgetResource::Candidates, 1).expect("candidate"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 1).expect("artifact"),
        ],
    )
    .expect("baseline reserve");
    let no_op_object = preserve_object(objects.path(), &[]).expect("no-op object");
    record_legacy_candidate_for_test(
        &connection,
        "test",
        objects.path(),
        "baseline-candidate-budget",
        &no_op,
        &no_op_object,
    )
    .expect("baseline candidate");
    let baseline_materialization =
        fake_materialization(&connection, objects.path(), &no_op, "fake-baseline-mat");
    (
        connection,
        objects,
        candidate,
        candidate_materialization,
        baseline_materialization,
    )
}

fn reserve_fixture_trial_budget(connection: &Connection, campaign_id: &str, reservation_id: &str) {
    reserve_budget(
        connection,
        "test",
        campaign_id,
        reservation_id,
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 5_000).expect("wall"),
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 8 * 1024).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 16 * 1024).expect("artifact"),
        ],
    )
    .expect("trial reservation");
}

fn record_owned_fixture_trial(
    connection: &Connection,
    candidate: &BoundCandidate,
    materialization: &MaterializationRecord,
    baseline: &MaterializationRecord,
    trial_id: &str,
    reservation_id: &str,
) -> TrialIntent {
    let manifest = campaign_manifest(connection, &candidate.proposal.campaign_id)
        .expect("fixture campaign manifest");
    let intent = TrialIntent {
        trial_id: trial_id.to_owned(),
        campaign_id: candidate.proposal.campaign_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        materialization_receipt_sha256: materialization.receipt_sha256.clone(),
        baseline_materialization_receipt_sha256: baseline.receipt_sha256.clone(),
        result_tree: materialization.result_tree.clone(),
        working_directory: materialization.worktree_locator.clone(),
        reservation_id: reservation_id.to_owned(),
        tier: "exploration".to_owned(),
        argv: manifest.evaluator.argv.clone(),
        environment: serde_json::to_value(
            expected_trial_environment(&manifest, &candidate.proposal.campaign_id, trial_id)
                .expect("frozen trial environment"),
        )
        .expect("environment"),
        owner_uuid: format!("{trial_id}-owner"),
        supervisor_identity: format!("{trial_id}-supervisor"),
    };
    record_trial_intent(connection, "test", &intent).expect("trial intent");
    intent
}

fn fixture_integrity_failure(objects: &Path, manifest: &CampaignManifest) -> IntegrityFailure {
    let expected_fixture_sha256 = manifest
        .holdouts
        .tiers
        .iter()
        .find(|tier| tier.key == "exploration")
        .expect("exploration tier")
        .fixture_sha256
        .0
        .clone();
    IntegrityFailure {
        reason_code: "evaluator-digest-mismatch".to_owned(),
        expected_outer_judge_sha256: manifest.generation.outer_judge_executable_sha256.0.clone(),
        observed_outer_judge_sha256: manifest.generation.outer_judge_executable_sha256.0.clone(),
        expected_launcher_sha256: manifest.evaluator.launcher_sha256.0.clone(),
        observed_launcher_sha256: manifest.evaluator.launcher_sha256.0.clone(),
        expected_evaluator_sha256: manifest.evaluator.evaluator_sha256.0.clone(),
        observed_evaluator_sha256: "0".repeat(64),
        expected_fixture_sha256: expected_fixture_sha256.clone(),
        observed_fixture_sha256: expected_fixture_sha256,
        frozen_input_mismatches: Vec::new(),
        evidence: preserve_object(objects, br#"{"evaluator":"drifted","expected":"frozen"}"#)
            .expect("integrity evidence"),
    }
}

fn prepared_git_materialization() -> (
    Connection,
    tempfile::TempDir,
    tempfile::TempDir,
    CampaignManifest,
    BoundCandidate,
) {
    let repository_owner = tempdir().expect("repository owner");
    let repository = repository_owner.path().join("source");
    std::fs::create_dir_all(repository.join("src")).expect("source directory");
    git_run(&repository, &["init"], None, None, None).expect("initialize source repository");
    git_run(
        &repository,
        &["config", "user.name", "Mise Test"],
        None,
        None,
        None,
    )
    .expect("configure Git name");
    git_run(
        &repository,
        &["config", "user.email", "mise-test@example.invalid"],
        None,
        None,
        None,
    )
    .expect("configure Git email");
    git_run(
        &repository,
        &["config", "core.autocrlf", "false"],
        None,
        None,
        None,
    )
    .expect("disable line-ending conversion");
    std::fs::write(repository.join("src/fixture.rs"), b"old\n").expect("base fixture");
    std::fs::write(repository.join(".gitignore"), b"*.cache\n").expect("base ignores");
    git_run(&repository, &["add", "."], None, None, None).expect("stage base fixture");
    git_run(
        &repository,
        &["commit", "-m", "frozen base"],
        None,
        None,
        None,
    )
    .expect("commit frozen base");

    let workspace_root = repository_owner.path().join("runs");
    std::fs::create_dir_all(&workspace_root).expect("workspace root");
    let base_commit = git_text(&repository, &["rev-parse", "HEAD^{commit}"])
        .expect("base commit")
        .trim()
        .to_owned();
    let base_tree = git_text(&repository, &["rev-parse", "HEAD^{tree}"])
        .expect("base tree")
        .trim()
        .to_owned();

    let mut manifest = crate::manifest::tests::valid_manifest();
    manifest.source.repository_locator =
        canonical_or_pending_absolute(&repository).expect("repository locator");
    manifest.source.base_commit = base_commit;
    manifest.source.base_tree = base_tree;
    manifest.execution_limits.workspace_root_locator =
        canonical_or_pending_absolute(&workspace_root).expect("workspace locator");
    manifest.validate().expect("real Git manifest");

    let connection = Connection::open_in_memory().expect("database");
    init(&connection).expect("schema");
    let admission = CampaignAdmission::from_manifest(&manifest).expect("admission");
    admit_campaign(&connection, "test", &admission).expect("campaign");

    let candidate = bind_legacy_patch_candidate(
            proposal_for(
                &manifest,
                "fixture-change",
                BTreeSet::from(["src/fixture.rs".to_owned()]),
            ),
            b"diff --git a/src/fixture.rs b/src/fixture.rs\n--- a/src/fixture.rs\n+++ b/src/fixture.rs\n@@ -1 +1 @@\n-old\n+new\n"
                .to_vec(),
        )
        .expect("candidate");
    reserve_budget(
        &connection,
        "test",
        &manifest.campaign_id,
        "candidate-budget",
        &[
            BudgetRequest::new(BudgetResource::Candidates, 1).expect("candidate request"),
            BudgetRequest::new(
                BudgetResource::ArtifactBytes,
                u64::try_from(candidate.material_bytes.len()).expect("patch length"),
            )
            .expect("patch request"),
        ],
    )
    .expect("candidate reservation");
    let objects = tempdir().expect("objects");
    let patch = preserve_object(objects.path(), &candidate.material_bytes).expect("patch object");
    record_legacy_candidate_for_test(
        &connection,
        "test",
        objects.path(),
        "candidate-budget",
        &candidate,
        &patch,
    )
    .expect("record candidate");
    reserve_budget(
        &connection,
        "test",
        &manifest.campaign_id,
        "materialization-budget",
        &[
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 128 * 1024).expect("disk request"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 8_192).expect("receipt request"),
        ],
    )
    .expect("materialization reservation");
    (connection, repository_owner, objects, manifest, candidate)
}

#[test]
fn candidate_identity_and_budget_use_are_exactly_replayable() {
    let (connection, objects, candidate) = prepared();
    let patch = preserve_object(objects.path(), &candidate.material_bytes).expect("patch object");
    assert!(
        record_legacy_candidate_for_test(
            &connection,
            "test",
            objects.path(),
            "candidate-budget",
            &candidate,
            &patch,
        )
        .expect("record candidate")
    );
    assert!(
        !record_legacy_candidate_for_test(
            &connection,
            "test",
            objects.path(),
            "candidate-budget",
            &candidate,
            &patch,
        )
        .expect("record replay")
    );
    assert_eq!(
        crate::lifecycle::candidate(&connection, &candidate.candidate_id)
            .expect("candidate query")
            .expect("durable candidate")
            .disposition,
        CandidateDisposition::Proposed
    );
}

#[test]
fn caller_constructed_candidate_fields_cannot_bypass_canonical_binding() {
    let (connection, objects, mut candidate) = prepared();
    let patch = preserve_object(objects.path(), &candidate.material_bytes).expect("patch object");
    candidate.candidate_id = "f".repeat(64);
    let error = record_legacy_candidate_for_test(
        &connection,
        "test",
        objects.path(),
        "candidate-budget",
        &candidate,
        &patch,
    )
    .expect_err("forged public candidate fields must be refused");
    assert!(
        error.to_string().contains("canonical identity"),
        "{error:#}"
    );
    assert!(
        super::candidate(&connection, &candidate.candidate_id)
            .expect("candidate query")
            .is_none()
    );
}

#[test]
fn candidate_mode_transitions_are_refused_before_materialization() {
    let (connection, objects, candidate) = prepared();
    let mode_patch = b"diff --git a/src/fixture.rs b/src/fixture.rs\nold mode 100644\nnew mode 120000\n--- a/src/fixture.rs\n+++ b/src/fixture.rs\n@@ -1 +1 @@\n-old\n+outside\n"
            .to_vec();
    let mode_candidate =
        bind_legacy_patch_candidate(candidate.proposal, mode_patch).expect("mode candidate");
    reserve_budget(
        &connection,
        "test",
        &mode_candidate.proposal.campaign_id,
        "mode-candidate-budget",
        &[
            BudgetRequest::new(BudgetResource::Candidates, 1).expect("candidate"),
            BudgetRequest::new(
                BudgetResource::ArtifactBytes,
                u64::try_from(mode_candidate.material_bytes.len()).expect("patch size"),
            )
            .expect("artifact"),
        ],
    )
    .expect("mode reservation");
    let object = preserve_object(objects.path(), &mode_candidate.material_bytes).expect("patch");
    let error = record_legacy_candidate_for_test(
        &connection,
        "test",
        objects.path(),
        "mode-candidate-budget",
        &mode_candidate,
        &object,
    )
    .expect_err("mode transition must be refused");
    assert!(error.to_string().contains("mode records"), "{error:#}");
}

#[test]
fn v1_patch_grammar_refuses_new_file_and_deletion_markers_directly() {
    let (_, _, candidate) = prepared();
    for (label, patch) in [
        (
            "new-file",
            b"diff --git a/src/fixture.rs b/src/fixture.rs\n--- /dev/null\n+++ b/src/fixture.rs\n@@ -0,0 +1 @@\n+new\n"
                .as_slice(),
        ),
        (
            "deletion",
            b"diff --git a/src/fixture.rs b/src/fixture.rs\n--- a/src/fixture.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n"
                .as_slice(),
        ),
    ] {
        let candidate =
            bind_legacy_patch_candidate(candidate.proposal.clone(), patch.to_vec()).expect("candidate binding");
        let error = exact_patch_paths(&candidate).expect_err("v1 /dev/null marker must fail");
        assert!(error.to_string().contains("/dev/null"), "{label}: {error:#}");
    }
}

#[test]
fn workspace_supervisor_owns_real_process_and_completion() {
    let (connection, objects, candidate, _, _) = prepared_trial();
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "supervised-trial-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 5_000).expect("wall"),
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 8 * 1024).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 16 * 1024).expect("artifact"),
        ],
    )
    .expect("supervisor reservation");
    let spec = SupervisedTrialSpec {
        trial_id: "supervised-exploration".to_owned(),
        campaign_id: candidate.proposal.campaign_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        reservation_id: "supervised-trial-budget".to_owned(),
        tier: "exploration".to_owned(),
    };
    let outcome =
        execute_workspace_trial(&connection, "workspace-supervisor", objects.path(), &spec)
            .expect("execute supervised evaluator");
    assert_eq!(
        outcome.classification.disposition,
        CandidateDisposition::Inconclusive
    );
    assert_eq!(
        trial(&connection, "supervised-exploration")
            .expect("trial query")
            .expect("supervised trial")
            .status,
        TrialStatus::Succeeded
    );
    let replay =
        execute_workspace_trial(&connection, "workspace-supervisor", objects.path(), &spec)
            .expect("replay without execution");
    assert_eq!(replay.receipt, outcome.receipt);
}

#[test]
fn trial_success_and_budget_settlement_are_one_atomic_transition() {
    let (connection, objects, candidate, _, _) = prepared_trial();
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "atomic-trial-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 5_000).expect("wall"),
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 8 * 1024).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 16 * 1024).expect("artifact"),
        ],
    )
    .expect("atomic reservation");
    connection
        .execute_batch(
            "CREATE TRIGGER test_refuse_measured_settlement
                 BEFORE UPDATE ON budget_reservations
                 WHEN NEW.status='settled'
                 BEGIN SELECT RAISE(ABORT, 'injected settlement refusal'); END;",
        )
        .expect("settlement refusal trigger");
    let error = execute_workspace_trial(
        &connection,
        "workspace-supervisor",
        objects.path(),
        &SupervisedTrialSpec {
            trial_id: "atomic-trial".to_owned(),
            campaign_id: candidate.proposal.campaign_id,
            candidate_id: candidate.candidate_id,
            reservation_id: "atomic-trial-budget".to_owned(),
            tier: "exploration".to_owned(),
        },
    )
    .expect_err("injected settlement failure must refuse trial success");
    assert!(
        error
            .to_string()
            .contains("complete supervised deterministic trial"),
        "{error:#}"
    );
    let durable = trial(&connection, "atomic-trial")
        .expect("trial query")
        .expect("atomic trial");
    assert_eq!(durable.status, TrialStatus::InfrastructureFailed);
    let states = connection
        .prepare(
            "SELECT DISTINCT status FROM budget_reservations
                 WHERE reservation_id='atomic-trial-budget'",
        )
        .expect("reservation states")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("state rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("states");
    assert_eq!(states, vec!["charged"]);
}

#[test]
fn cold_recovery_heals_legacy_succeeded_unsettled_trial_without_execution() {
    let (connection, objects, candidate, _, _) = prepared_trial();
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "legacy-trial-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 5_000).expect("wall"),
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 8 * 1024).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 16 * 1024).expect("artifact"),
        ],
    )
    .expect("legacy reservation");
    let spec = SupervisedTrialSpec {
        trial_id: "legacy-unsettled".to_owned(),
        campaign_id: candidate.proposal.campaign_id.clone(),
        candidate_id: candidate.candidate_id,
        reservation_id: "legacy-trial-budget".to_owned(),
        tier: "exploration".to_owned(),
    };
    execute_workspace_trial(&connection, "workspace-supervisor", objects.path(), &spec)
        .expect("initial successful trial");
    let settled_balances =
        crate::budget::budget_balances(&connection, &spec.campaign_id).expect("settled balances");

    connection
        .execute_batch(
            "DROP TRIGGER budget_reservation_transition_guard;
                 UPDATE budget_balances
                    SET reserved_amount=reserved_amount +
                          (SELECT reserved_amount FROM budget_reservations r
                            WHERE r.campaign_id=budget_balances.campaign_id
                              AND r.resource=budget_balances.resource
                              AND r.reservation_id='legacy-trial-budget'),
                        spent_amount=spent_amount -
                          (SELECT settled_amount FROM budget_reservations r
                            WHERE r.campaign_id=budget_balances.campaign_id
                              AND r.resource=budget_balances.resource
                              AND r.reservation_id='legacy-trial-budget')
                  WHERE campaign_id='fixture-rsi-1'
                    AND resource IN (SELECT resource FROM budget_reservations
                                      WHERE reservation_id='legacy-trial-budget');
                 UPDATE budget_reservations
                    SET settled_amount=NULL, status='reserved', settled_at=NULL, note=NULL
                  WHERE reservation_id='legacy-trial-budget';
                 CREATE TRIGGER budget_reservation_transition_guard
                 BEFORE UPDATE ON budget_reservations
                 WHEN NOT (
                   OLD.status='reserved' AND NEW.status IN ('settled','charged') AND
                   NEW.settled_amount IS NOT NULL AND NEW.settled_at IS NOT NULL
                 )
                 BEGIN SELECT RAISE(ABORT, 'invalid budget reservation transition'); END;",
        )
        .expect("simulate legacy split-commit window");

    assert_eq!(
        recover_workspace_trial(&connection, "recovery", objects.path(), &spec.trial_id,)
            .expect("recover settlement"),
        ColdRecoveryOutcome::Reconciled
    );
    assert_eq!(
        crate::budget::budget_balances(&connection, &spec.campaign_id).expect("recovered balances"),
        settled_balances
    );
    assert_eq!(
        recover_workspace_trial(&connection, "recovery", objects.path(), &spec.trial_id,)
            .expect("idempotent recovery"),
        ColdRecoveryOutcome::AlreadyReconciled
    );
}

#[test]
fn workspace_supervisor_hard_caps_retained_output() {
    let (connection, objects, candidate, _, _) = prepared_trial_with_maximum_output(32);
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "bounded-output-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 5_000).expect("wall"),
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 8 * 1024).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 16 * 1024).expect("artifact"),
        ],
    )
    .expect("bounded output reservation");
    let error = execute_workspace_trial(
        &connection,
        "workspace-supervisor",
        objects.path(),
        &SupervisedTrialSpec {
            trial_id: "bounded-output".to_owned(),
            campaign_id: candidate.proposal.campaign_id,
            candidate_id: candidate.candidate_id,
            reservation_id: "bounded-output-budget".to_owned(),
            tier: "exploration".to_owned(),
        },
    )
    .expect_err("oversized evaluator output must fail");
    assert!(
        error.to_string().contains("output-limit-exceeded"),
        "{error:#}"
    );
    assert_eq!(
        trial(&connection, "bounded-output")
            .expect("trial query")
            .expect("bounded trial")
            .status,
        TrialStatus::InfrastructureFailed
    );
}

#[test]
fn workspace_supervisor_retains_successful_process_stderr_as_failure_evidence() {
    let (connection, objects, candidate, _, _) = prepared_trial_with_evaluator(8 * 1024, "stderr");
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "stderr-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 5_000).expect("wall"),
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 8 * 1024).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 16 * 1024).expect("artifact"),
        ],
    )
    .expect("stderr reservation");
    let error = execute_workspace_trial(
        &connection,
        "workspace-supervisor",
        objects.path(),
        &SupervisedTrialSpec {
            trial_id: "stderr-output".to_owned(),
            campaign_id: candidate.proposal.campaign_id,
            candidate_id: candidate.candidate_id,
            reservation_id: "stderr-budget".to_owned(),
            tier: "exploration".to_owned(),
        },
    )
    .expect_err("successful process stderr must become retained failure evidence");
    assert!(error.to_string().contains("wrote to stderr"), "{error:#}");
    let durable = trial(&connection, "stderr-output")
        .expect("trial query")
        .expect("stderr trial");
    assert_eq!(durable.status, TrialStatus::InfrastructureFailed);
    let evidence: PreservedObject = serde_json::from_value(
        durable
            .outcome
            .expect("failure outcome")
            .pointer("/absence_proof/evidence")
            .cloned()
            .expect("failure evidence"),
    )
    .expect("failure evidence object");
    let body: Value = serde_json::from_slice(
        &read_object(objects.path(), &evidence).expect("failure evidence body"),
    )
    .expect("failure evidence JSON");
    assert_eq!(body["reason"], "unexpected-evaluator-stderr");
    let stderr: PreservedObject =
        serde_json::from_value(body["stderr"].clone()).expect("stderr object");
    assert_eq!(
        read_object(objects.path(), &stderr).expect("stderr bytes"),
        b"warning that must not disappear"
    );
}

#[test]
fn workspace_supervisor_quiesces_descendants_holding_inherited_output_handles() {
    let (connection, objects, candidate, _, _) =
        prepared_trial_with_evaluator(8 * 1024, "inherited-handle");
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "inherited-handle-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 5_000).expect("wall"),
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 8 * 1024).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 16 * 1024).expect("artifact"),
        ],
    )
    .expect("inherited handle reservation");
    let outcome = execute_workspace_trial(
        &connection,
        "workspace-supervisor",
        objects.path(),
        &SupervisedTrialSpec {
            trial_id: "inherited-output-handle".to_owned(),
            campaign_id: candidate.proposal.campaign_id,
            candidate_id: candidate.candidate_id,
            reservation_id: "inherited-handle-budget".to_owned(),
            tier: "exploration".to_owned(),
        },
    )
    .expect("owned process family must close inherited output handles");
    assert_eq!(
        outcome.classification.disposition,
        CandidateDisposition::Inconclusive
    );
}

#[test]
fn workspace_supervisor_wall_bound_includes_blocked_stdin() {
    let (connection, objects, candidate, _, _) =
        prepared_trial_with_evaluator_and_request(8 * 1024, 150, "blocked-stdin", 2_000);
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "blocked-stdin-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 150).expect("wall"),
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 8 * 1024).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 16 * 1024).expect("artifact"),
        ],
    )
    .expect("blocked stdin reservation");
    let error = execute_workspace_trial(
        &connection,
        "workspace-supervisor",
        objects.path(),
        &SupervisedTrialSpec {
            trial_id: "blocked-stdin".to_owned(),
            campaign_id: candidate.proposal.campaign_id,
            candidate_id: candidate.candidate_id,
            reservation_id: "blocked-stdin-budget".to_owned(),
            tier: "exploration".to_owned(),
        },
    )
    .expect_err("non-reading evaluator must hit the wall bound");
    assert!(
        error.to_string().contains("wall-time-limit-exceeded"),
        "{error:#}"
    );
}

#[test]
fn workspace_supervisor_refuses_materialization_drift_before_intent() {
    let (connection, objects, candidate, materialization, _) = prepared_trial();
    std::fs::write(
        Path::new(&materialization.worktree_locator).join("src/fixture.rs"),
        b"drifted\n",
    )
    .expect("inject worktree drift");
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "drifted-trial-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 5_000).expect("wall"),
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 8 * 1024).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 16 * 1024).expect("artifact"),
        ],
    )
    .expect("reservation");
    let error = execute_workspace_trial(
        &connection,
        "workspace-supervisor",
        objects.path(),
        &SupervisedTrialSpec {
            trial_id: "drifted-exploration".to_owned(),
            campaign_id: candidate.proposal.campaign_id.clone(),
            candidate_id: candidate.candidate_id,
            reservation_id: "drifted-trial-budget".to_owned(),
            tier: "exploration".to_owned(),
        },
    )
    .expect_err("worktree drift must be refused");
    assert!(error.to_string().contains("unstaged drift"), "{error:#}");
    assert!(
        trial(&connection, "drifted-exploration")
            .expect("trial query")
            .is_none()
    );
}

#[test]
fn workspace_supervisor_records_launcher_drift_as_terminal_integrity() {
    let (connection, objects, candidate, _, _) = prepared_trial();
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "launcher-drift-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 5_000).expect("wall"),
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 8 * 1024).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 16 * 1024).expect("artifact"),
        ],
    )
    .expect("launcher drift reservation");
    let manifest =
        campaign_manifest(&connection, &candidate.proposal.campaign_id).expect("manifest");
    std::fs::write(&manifest.evaluator.launcher_locator, b"drifted launcher")
        .expect("replace launcher bytes");
    let error = execute_workspace_trial(
        &connection,
        "workspace-supervisor",
        objects.path(),
        &SupervisedTrialSpec {
            trial_id: "launcher-drift".to_owned(),
            campaign_id: candidate.proposal.campaign_id,
            candidate_id: candidate.candidate_id,
            reservation_id: "launcher-drift-budget".to_owned(),
            tier: "exploration".to_owned(),
        },
    )
    .expect_err("launcher drift must be terminal integrity evidence");
    assert!(
        error.to_string().contains("judge inputs drifted"),
        "{error:#}"
    );
    assert_eq!(
        trial(&connection, "launcher-drift")
            .expect("trial query")
            .expect("integrity trial")
            .status,
        TrialStatus::IntegrityFailed
    );
}

#[test]
fn workspace_supervisor_records_lockfile_drift_and_stops_campaign() {
    let (connection, objects, candidate, materialization, baseline) =
        prepared_trial_with_evaluator_request_and_frozen_rust_inputs(
            8 * 1024,
            5_000,
            "success",
            0,
            true,
        );
    reserve_fixture_trial_budget(
        &connection,
        &candidate.proposal.campaign_id,
        "lockfile-drift-budget",
    );
    let intent = record_owned_fixture_trial(
        &connection,
        &candidate,
        &materialization,
        &baseline,
        "lockfile-drift",
        "lockfile-drift-budget",
    );
    std::fs::write(
        Path::new(&materialization.worktree_locator).join("Cargo.lock"),
        b"# drifted after durable trial intent\n",
    )
    .expect("replace candidate lockfile bytes");
    let manifest =
        campaign_manifest(&connection, &candidate.proposal.campaign_id).expect("campaign manifest");
    let mismatch = verify_runtime_judge_inputs(
        &manifest,
        Path::new(&materialization.worktree_locator),
        "exploration",
    )
    .expect_err("lockfile drift must be detected");
    assert_eq!(mismatch.role, "rust-lockfile");
    record_runtime_integrity_failure(
        &connection,
        "workspace-supervisor",
        objects.path(),
        &intent,
        &manifest,
        &mismatch,
        None,
    )
    .expect("lockfile drift must be durable terminal integrity evidence");
    let trial = trial(&connection, "lockfile-drift")
        .expect("trial query")
        .expect("integrity trial");
    assert_eq!(trial.status, TrialStatus::IntegrityFailed);
    assert_eq!(
        trial.outcome.expect("integrity outcome")["failure"]["frozen_input_mismatches"][0]["role"],
        "rust-lockfile"
    );
    let stop = require_campaign_integrity(&connection, &manifest)
        .expect_err("integrity failure must trip the campaign stop rule");
    assert!(
        stop.to_string()
            .contains("campaign stopped after evaluator-integrity failure"),
        "{stop:#}"
    );
}

#[test]
fn workspace_supervisor_records_toolchain_drift_as_terminal_integrity() {
    let (connection, objects, candidate, _, _) =
        prepared_trial_with_evaluator_request_and_frozen_rust_inputs(
            8 * 1024,
            5_000,
            "success",
            0,
            true,
        );
    reserve_fixture_trial_budget(
        &connection,
        &candidate.proposal.campaign_id,
        "toolchain-drift-budget",
    );
    let manifest =
        campaign_manifest(&connection, &candidate.proposal.campaign_id).expect("campaign manifest");
    let cargo = &manifest
        .evaluator
        .rust_build_environment
        .as_ref()
        .expect("frozen Rust environment")
        .cargo_executable_locator;
    std::fs::write(cargo, b"drifted Cargo executable")
        .expect("replace frozen Cargo executable bytes");
    let error = execute_workspace_trial(
        &connection,
        "workspace-supervisor",
        objects.path(),
        &SupervisedTrialSpec {
            trial_id: "toolchain-drift".to_owned(),
            campaign_id: candidate.proposal.campaign_id,
            candidate_id: candidate.candidate_id,
            reservation_id: "toolchain-drift-budget".to_owned(),
            tier: "exploration".to_owned(),
        },
    )
    .expect_err("toolchain drift must be terminal integrity evidence");
    assert!(
        error.to_string().contains("judge inputs drifted"),
        "{error:#}"
    );
    let trial = trial(&connection, "toolchain-drift")
        .expect("trial query")
        .expect("integrity trial");
    assert_eq!(trial.status, TrialStatus::IntegrityFailed);
    assert_eq!(
        trial.outcome.expect("integrity outcome")["failure"]["frozen_input_mismatches"][0]["role"],
        "cargo-executable"
    );
}

#[test]
fn real_git_materialization_is_exact_and_idempotently_replayable() {
    let (connection, repository_owner, objects, manifest, candidate) =
        prepared_git_materialization();
    let worktree = repository_owner.path().join("runs/candidate");
    let expected_tree =
        expected_result_tree(&manifest, &candidate.material_bytes).expect("expected result tree");

    let first = materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "materialization-budget",
        &candidate.candidate_id,
        &worktree,
    )
    .expect("materialize candidate");
    assert_eq!(first.result_tree, expected_tree);
    assert_eq!(
        git_text(&worktree, &["write-tree"])
            .expect("materialized tree")
            .trim(),
        expected_tree
    );
    assert_eq!(
        std::fs::read(worktree.join("src/fixture.rs")).expect("materialized fixture"),
        b"new\n"
    );

    let replay = materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "materialization-budget",
        &candidate.candidate_id,
        &worktree,
    )
    .expect("replay materialization");
    assert_eq!(replay, first);
    assert_eq!(
        super::candidate(&connection, &candidate.candidate_id)
            .expect("candidate query")
            .expect("durable candidate")
            .disposition,
        CandidateDisposition::Materialized
    );
}

#[test]
fn relative_pending_materialization_path_is_resolved_once_to_absolute_identity() {
    let current = std::env::current_dir().expect("current directory");
    std::fs::create_dir_all(current.join("target")).expect("target directory");
    let relative = PathBuf::from("target").join(format!(
        "mise-relative-materialization-{}",
        std::process::id()
    ));
    let (locator, resolved) = resolved_materialization_worktree(&relative).expect("resolve path");
    assert!(resolved.is_absolute());
    assert_eq!(resolved, PathBuf::from(locator));
    assert_eq!(
        canonical_or_pending_absolute(resolved.parent().expect("resolved parent"))
            .expect("portable resolved parent"),
        canonical_or_pending_absolute(&current.join("target")).expect("portable target")
    );
}

#[test]
fn active_materialization_binding_requires_typed_abandonment_before_retry() {
    let (connection, repository_owner, objects, manifest, candidate) =
        prepared_git_materialization();
    let transaction = begin_mutation(&connection).expect("bind interrupted attempt");
    bind_or_require_reservation_use(
        &transaction,
        &manifest.campaign_id,
        "materialization-budget",
        "materialization",
        &candidate.candidate_id,
    )
    .expect("materialization binding");
    transaction.commit().expect("durable binding");
    reserve_budget(
        &connection,
        "test",
        &manifest.campaign_id,
        "materialization-retry-budget",
        &[
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 128 * 1024).expect("retry disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 8_192).expect("retry artifact"),
        ],
    )
    .expect("retry reservation");

    let retry_worktree = repository_owner.path().join("runs/retry-after-abandonment");
    let error = materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "materialization-retry-budget",
        &candidate.candidate_id,
        &retry_worktree,
    )
    .expect_err("active prior binding must refuse retry");
    assert!(
        error
            .to_string()
            .contains("candidate abandon-materialization"),
        "{error:#}"
    );
    let error = crate::budget::settle_budget(
        &connection,
        "test",
        &manifest.campaign_id,
        "materialization-budget",
        SettlementMode::ChargeReservation,
        &[],
        Some("attempted-bypass"),
    )
    .expect_err("generic settlement must not bypass lifecycle recovery");
    assert!(error.to_string().contains("lifecycle-bound"), "{error:#}");

    assert_eq!(
        abandon_materialization_attempt(
            &connection,
            "operator",
            &candidate.candidate_id,
            "materialization-budget",
            "operator verified interrupted caller",
        )
        .expect("typed abandonment"),
        SettlementOutcome::Settled
    );
    assert_eq!(
        abandon_materialization_attempt(
            &connection,
            "operator",
            &candidate.candidate_id,
            "materialization-budget",
            "operator verified interrupted caller",
        )
        .expect("typed abandonment replay"),
        SettlementOutcome::Existing
    );
    let record = materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "materialization-retry-budget",
        &candidate.candidate_id,
        &retry_worktree,
    )
    .expect("retry after typed abandonment");
    assert_eq!(record.reservation_id, "materialization-retry-budget");
    let events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM events
             WHERE entity='candidate' AND entity_key=?1 AND kind='materialization-abandoned'",
            params![candidate.candidate_id],
            |row| row.get(0),
        )
        .expect("abandonment events");
    assert_eq!(events, 1, "replay must not duplicate abandonment evidence");
}

#[test]
fn terminally_charged_materialization_attempt_can_retry_with_a_fresh_reservation() {
    let (connection, repository_owner, objects, manifest, candidate) =
        prepared_git_materialization();
    let repository = Path::new(&manifest.source.repository_locator);
    let failed_worktree = repository_owner.path().join("runs/failed-attempt");
    git_run(
        repository,
        &["worktree", "add", "--detach"],
        Some(&failed_worktree),
        Some(&manifest.source.base_commit),
        None,
    )
    .expect("create adversarial worktree");
    std::fs::write(failed_worktree.join("src/fixture.rs"), b"attacker\n")
        .expect("adversarial fixture");
    git_run(
        &failed_worktree,
        &["add", "src/fixture.rs"],
        None,
        None,
        None,
    )
    .expect("stage adversarial fixture");

    materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "materialization-budget",
        &candidate.candidate_id,
        &failed_worktree,
    )
    .expect_err("first materialization must fail after binding its budget");
    git_run(
        repository,
        &["worktree", "remove", "--force"],
        Some(&failed_worktree),
        None,
        None,
    )
    .expect("remove failed worktree");

    reserve_budget(
        &connection,
        "test",
        &manifest.campaign_id,
        "materialization-retry-budget",
        &[
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 128 * 1024).expect("retry disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 8_192).expect("retry artifact"),
        ],
    )
    .expect("retry reservation");
    let retry_worktree = repository_owner.path().join("runs/retry-attempt");
    let record = materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "materialization-retry-budget",
        &candidate.candidate_id,
        &retry_worktree,
    )
    .expect("retry materialization");
    assert_eq!(record.reservation_id, "materialization-retry-budget");
    let bindings: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM budget_reservation_uses
             WHERE campaign_id=?1 AND use_kind='materialization' AND entity_key=?2",
            params![manifest.campaign_id, candidate.candidate_id],
            |row| row.get(0),
        )
        .expect("retry bindings");
    assert_eq!(bindings, 2, "both charged attempts remain durable");
    let retry_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM events
             WHERE entity='candidate' AND entity_key=?1 AND kind='materialization-retry-bound'",
            params![candidate.candidate_id],
            |row| row.get(0),
        )
        .expect("retry event");
    assert_eq!(retry_events, 1);
}

#[test]
fn underreserved_materialization_is_refused_before_checkout_creation() {
    let (connection, repository_owner, objects, manifest, candidate) =
        prepared_git_materialization();
    reserve_budget(
        &connection,
        "test",
        &manifest.campaign_id,
        "underreserved-materialization",
        &[
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 1).expect("disk"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 8_192).expect("artifact"),
        ],
    )
    .expect("underreservation");
    let worktree = repository_owner.path().join("runs/underreserved");
    let objects_before = git_text(
        Path::new(&manifest.source.repository_locator),
        &["count-objects", "-v"],
    )
    .expect("object count before refusal");
    let error = materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "underreserved-materialization",
        &candidate.candidate_id,
        &worktree,
    )
    .expect_err("one-byte checkout reservation must fail");
    assert!(
        error.to_string().contains("logical-write bound"),
        "{error:#}"
    );
    assert!(!worktree.exists(), "refusal must precede checkout creation");
    assert_eq!(
        git_text(
            Path::new(&manifest.source.repository_locator),
            &["count-objects", "-v"],
        )
        .expect("object count after refusal"),
        objects_before,
        "refusal must precede isolated-index object writes"
    );
}

#[test]
fn ignored_untracked_files_are_exactness_drift() {
    let (connection, repository_owner, objects, _, candidate) = prepared_git_materialization();
    let worktree = repository_owner.path().join("runs/ignored-drift");
    materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "materialization-budget",
        &candidate.candidate_id,
        &worktree,
    )
    .expect("materialize candidate");
    std::fs::write(worktree.join("compiler.cache"), b"can affect evaluation\n")
        .expect("ignored cache drift");
    let error = materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "materialization-budget",
        &candidate.candidate_id,
        &worktree,
    )
    .expect_err("ignored untracked files must invalidate exact replay");
    assert!(error.to_string().contains("untracked drift"), "{error:#}");
}

#[test]
fn checkout_transform_attributes_are_refused() {
    for rule in [
        "*.rs filter=unbounded\n",
        "*.txt working-tree-encoding=UTF-16\n",
        "*.json ident\n",
        "[attr]expand filter=unbounded\n*.rs expand\n",
    ] {
        let error = reject_checkout_transform_rules(rule, ".gitattributes")
            .expect_err("checkout transform must be refused");
        assert!(
            error.to_string().contains("checkout transform attribute"),
            "{error:#}"
        );
    }
}

#[test]
fn repository_info_attributes_are_refused_before_checkout_creation() {
    let (connection, repository_owner, objects, manifest, candidate) =
        prepared_git_materialization();
    let repository = Path::new(&manifest.source.repository_locator);
    let info_attributes = git_text(repository, &["rev-parse", "--git-path", "info/attributes"])
        .expect("info attributes locator");
    let info_attributes = PathBuf::from(info_attributes.trim());
    let info_attributes = if info_attributes.is_absolute() {
        info_attributes
    } else {
        repository.join(info_attributes)
    };
    std::fs::create_dir_all(info_attributes.parent().expect("info directory"))
        .expect("create info directory");
    std::fs::write(&info_attributes, b"*.rs filter=unbounded\n")
        .expect("write adversarial info attributes");
    let worktree = repository_owner.path().join("runs/info-attributes");
    let error = materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "materialization-budget",
        &candidate.candidate_id,
        &worktree,
    )
    .expect_err("repository-local attributes must be refused");
    assert!(error.to_string().contains("info/attributes"), "{error:#}");
    assert!(!worktree.exists(), "refusal must precede checkout creation");
}

#[test]
fn preexisting_staged_same_path_cannot_impersonate_retained_patch() {
    let (connection, repository_owner, objects, manifest, candidate) =
        prepared_git_materialization();
    let repository = Path::new(&manifest.source.repository_locator);
    let worktree = repository_owner.path().join("runs/adversarial");
    git_run(
        repository,
        &["worktree", "add", "--detach"],
        Some(&worktree),
        Some(&manifest.source.base_commit),
        None,
    )
    .expect("create adversarial worktree");
    std::fs::write(worktree.join("src/fixture.rs"), b"attacker\n").expect("adversarial fixture");
    git_run(&worktree, &["add", "src/fixture.rs"], None, None, None)
        .expect("stage adversarial fixture");

    let error = materialize_candidate(
        &connection,
        "test",
        objects.path(),
        "materialization-budget",
        &candidate.candidate_id,
        &worktree,
    )
    .expect_err("different staged bytes must be refused");
    assert!(
        error
            .to_string()
            .contains("not the exact frozen base plus retained material"),
        "unexpected error: {error:#}"
    );
    assert!(
        materialization_in(&connection, &candidate.candidate_id)
            .expect("materialization query")
            .is_none()
    );
    assert_eq!(
        super::candidate(&connection, &candidate.candidate_id)
            .expect("candidate query")
            .expect("durable candidate")
            .disposition,
        CandidateDisposition::Proposed
    );
}

#[test]
fn owned_trial_abandonment_is_typed_conservative_and_replayable() {
    let (connection, _objects, candidate, materialization, baseline) = prepared_trial();
    reserve_fixture_trial_budget(
        &connection,
        &candidate.proposal.campaign_id,
        "owned-abandon-budget",
    );
    record_owned_fixture_trial(
        &connection,
        &candidate,
        &materialization,
        &baseline,
        "owned-abandon-trial",
        "owned-abandon-budget",
    );
    let error = crate::budget::settle_budget(
        &connection,
        "operator",
        &candidate.proposal.campaign_id,
        "owned-abandon-budget",
        SettlementMode::ChargeReservation,
        &[],
        Some("attempted-bypass"),
    )
    .expect_err("generic settlement must not bypass the trial lifecycle");
    assert!(error.to_string().contains("lifecycle-bound"), "{error:#}");

    assert_eq!(
        abandon_owned_trial(
            &connection,
            "operator",
            "owned-abandon-trial",
            "supervisor exited before durable launch ownership",
        )
        .expect("typed trial abandonment"),
        SettlementOutcome::Settled
    );
    assert_eq!(
        abandon_owned_trial(
            &connection,
            "operator",
            "owned-abandon-trial",
            "supervisor exited before durable launch ownership",
        )
        .expect("typed trial abandonment replay"),
        SettlementOutcome::Existing
    );
    let durable = trial(&connection, "owned-abandon-trial")
        .expect("trial query")
        .expect("durable abandoned trial");
    assert_eq!(durable.status, TrialStatus::InfrastructureFailed);
    assert_eq!(
        durable
            .outcome
            .as_ref()
            .and_then(|value| value.get("process_absence_claimed"))
            .and_then(Value::as_bool),
        Some(false)
    );
    let events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM events
             WHERE entity='trial' AND entity_key='owned-abandon-trial'
               AND kind='abandoned-before-launch'",
            [],
            |row| row.get(0),
        )
        .expect("abandonment event count");
    assert_eq!(events, 1, "replay must not duplicate abandonment evidence");

    reserve_fixture_trial_budget(
        &connection,
        &candidate.proposal.campaign_id,
        "launched-abandon-budget",
    );
    let launched = record_owned_fixture_trial(
        &connection,
        &candidate,
        &materialization,
        &baseline,
        "launched-abandon-trial",
        "launched-abandon-budget",
    );
    mark_trial_launched(
        &connection,
        "test",
        &launched.trial_id,
        &TrialOwnership {
            owner_uuid: launched.owner_uuid,
            pid: 4242,
            process_birth_identity: "synthetic-test-birth-identity".to_owned(),
            supervisor_identity: launched.supervisor_identity,
        },
    )
    .expect("mark launched");
    let error = abandon_owned_trial(
        &connection,
        "operator",
        "launched-abandon-trial",
        "must not apply to launched work",
    )
    .expect_err("launched trial needs process recovery");
    assert!(error.to_string().contains("trial recover"), "{error:#}");
}

#[test]
fn absence_reconciliation_rolls_back_budget_when_trial_transition_fails() {
    let (connection, objects, candidate, materialization, baseline) = prepared_trial();
    reserve_fixture_trial_budget(
        &connection,
        &candidate.proposal.campaign_id,
        "absence-atomic-budget",
    );
    let intent = record_owned_fixture_trial(
        &connection,
        &candidate,
        &materialization,
        &baseline,
        "absence-atomic-trial",
        "absence-atomic-budget",
    );
    let evidence = preserve_object(
        objects.path(),
        br#"{"reason":"injected-supervisor-failure"}"#,
    )
    .expect("absence evidence");
    let proof = AbsenceProof {
        verifier: "papertiger-mise.workspace-supervisor.v1".to_owned(),
        observed_at: now(),
        supervisor_identity: intent.supervisor_identity,
        process_birth_identity: None,
        evidence: evidence.clone(),
    };
    connection
        .execute_batch(
            "CREATE TEMP TRIGGER inject_absence_transition_failure
             BEFORE UPDATE OF status ON trials
             WHEN NEW.status='infrastructure_failed'
             BEGIN SELECT RAISE(ABORT, 'injected absence transition failure'); END;",
        )
        .expect("inject transition failure");
    let error = reconcile_lost_trial(
        &connection,
        "test",
        objects.path(),
        "absence-atomic-trial",
        &proof,
    )
    .expect_err("injected transition failure must roll back everything");
    assert!(error.to_string().contains("injected absence"), "{error:#}");
    assert_eq!(
        trial(&connection, "absence-atomic-trial")
            .expect("trial query")
            .expect("durable trial")
            .status,
        TrialStatus::Owned
    );
    let statuses = connection
        .prepare(
            "SELECT DISTINCT status FROM budget_reservations
             WHERE reservation_id='absence-atomic-budget' ORDER BY status",
        )
        .expect("status query")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("status rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("statuses");
    assert_eq!(statuses, vec!["reserved"]);
    let artifact_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM artifacts WHERE sha256=?1",
            params![evidence.sha256],
            |row| row.get(0),
        )
        .expect("artifact count");
    assert_eq!(artifact_count, 0);

    connection
        .execute_batch("DROP TRIGGER inject_absence_transition_failure")
        .expect("remove injected failure");
    reconcile_lost_trial(
        &connection,
        "test",
        objects.path(),
        "absence-atomic-trial",
        &proof,
    )
    .expect("atomic reconciliation retry");
    assert_eq!(
        trial(&connection, "absence-atomic-trial")
            .expect("trial query")
            .expect("durable trial")
            .status,
        TrialStatus::InfrastructureFailed
    );
}

#[test]
fn cold_recovery_refuses_live_process_then_reconciles_exact_exit_once() {
    let (connection, objects, candidate, materialization, baseline) = prepared_trial();
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "trial-1-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial request"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure request"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure request"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 1_000).expect("wall request"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 4_096).expect("artifact request"),
        ],
    )
    .expect("reserve");
    let intent = TrialIntent {
        trial_id: "trial-1".to_owned(),
        campaign_id: candidate.proposal.campaign_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        materialization_receipt_sha256: materialization.receipt_sha256.clone(),
        baseline_materialization_receipt_sha256: baseline.receipt_sha256,
        result_tree: materialization.result_tree,
        working_directory: materialization.worktree_locator,
        reservation_id: "trial-1-budget".to_owned(),
        tier: "exploration".to_owned(),
        argv: campaign_manifest(&connection, &candidate.proposal.campaign_id)
            .expect("manifest")
            .evaluator
            .argv,
        environment: serde_json::to_value(
            &campaign_manifest(&connection, &candidate.proposal.campaign_id)
                .expect("manifest")
                .evaluator
                .environment,
        )
        .expect("environment"),
        owner_uuid: "owner-1".to_owned(),
        supervisor_identity: "job-1".to_owned(),
    };
    record_trial_intent(&connection, "test", &intent).expect("trial intent");
    let error = recover_workspace_trial(&connection, "recovery", objects.path(), "trial-1")
        .expect_err("ambiguous pre-launch ownership must remain locked");
    assert!(
        error
            .to_string()
            .contains("only an OS-bound launched process is decidable"),
        "{error:#}"
    );
    let manifest =
        campaign_manifest(&connection, &candidate.proposal.campaign_id).expect("manifest");
    let mut child = std::process::Command::new(&manifest.evaluator.argv[0])
        .env("PAPERTIGER_MISE_LIFECYCLE_FIXTURE_DESCENDANT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn recoverable process");
    let process_birth_identity = match observe_process(child.id()).expect("process identity") {
        ProcessObservation::Active {
            process_birth_identity,
        } => process_birth_identity,
        observation => panic!("expected live child, observed {observation:?}"),
    };
    mark_trial_launched(
        &connection,
        "test",
        "trial-1",
        &TrialOwnership {
            owner_uuid: "owner-1".to_owned(),
            pid: child.id(),
            process_birth_identity,
            supervisor_identity: "job-1".to_owned(),
        },
    )
    .expect("launch");
    let error = recover_workspace_trial(&connection, "recovery", objects.path(), "trial-1")
        .expect_err("live exact process must block recovery");
    assert!(error.to_string().contains("still owns its exact live"));
    child.kill().expect("kill recoverable process");
    child.wait().expect("reap recoverable process");
    assert_eq!(
        recover_workspace_trial(&connection, "recovery", objects.path(), "trial-1")
            .expect("reconcile"),
        ColdRecoveryOutcome::Reconciled
    );
    assert_eq!(
        recover_workspace_trial(&connection, "recovery", objects.path(), "trial-1")
            .expect("reconcile replay"),
        ColdRecoveryOutcome::AlreadyReconciled
    );
    assert_eq!(
        trial(&connection, "trial-1")
            .expect("trial query")
            .expect("durable trial")
            .status,
        TrialStatus::InfrastructureFailed
    );
}

#[test]
fn integrity_failure_rolls_back_budget_when_trial_transition_fails() {
    let (connection, objects, candidate, materialization, baseline) = prepared_trial();
    reserve_fixture_trial_budget(
        &connection,
        &candidate.proposal.campaign_id,
        "integrity-atomic-budget",
    );
    record_owned_fixture_trial(
        &connection,
        &candidate,
        &materialization,
        &baseline,
        "integrity-atomic-trial",
        "integrity-atomic-budget",
    );
    let manifest =
        campaign_manifest(&connection, &candidate.proposal.campaign_id).expect("manifest");
    let failure = fixture_integrity_failure(objects.path(), &manifest);
    connection
        .execute_batch(
            "CREATE TEMP TRIGGER inject_integrity_transition_failure
             BEFORE UPDATE OF status ON trials
             WHEN NEW.status='integrity_failed'
             BEGIN SELECT RAISE(ABORT, 'injected integrity transition failure'); END;",
        )
        .expect("inject transition failure");
    let error = record_integrity_failure(
        &connection,
        "test",
        objects.path(),
        "integrity-atomic-trial",
        &failure,
    )
    .expect_err("injected transition failure must roll back everything");
    assert!(
        error.to_string().contains("injected integrity"),
        "{error:#}"
    );
    assert_eq!(
        trial(&connection, "integrity-atomic-trial")
            .expect("trial query")
            .expect("durable trial")
            .status,
        TrialStatus::Owned
    );
    let statuses = connection
        .prepare(
            "SELECT DISTINCT status FROM budget_reservations
             WHERE reservation_id='integrity-atomic-budget' ORDER BY status",
        )
        .expect("status query")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("status rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("statuses");
    assert_eq!(statuses, vec!["reserved"]);
    let artifact_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM artifacts WHERE sha256=?1",
            params![failure.evidence.sha256],
            |row| row.get(0),
        )
        .expect("artifact count");
    assert_eq!(artifact_count, 0);

    connection
        .execute_batch("DROP TRIGGER inject_integrity_transition_failure")
        .expect("remove injected failure");
    record_integrity_failure(
        &connection,
        "test",
        objects.path(),
        "integrity-atomic-trial",
        &failure,
    )
    .expect("atomic integrity retry");
    assert_eq!(
        trial(&connection, "integrity-atomic-trial")
            .expect("trial query")
            .expect("durable trial")
            .status,
        TrialStatus::IntegrityFailed
    );
    let error = require_campaign_integrity(&connection, &manifest)
        .expect_err("durable integrity evidence must stop the campaign");
    assert!(error.to_string().contains("integrity failure"), "{error:#}");
}

#[test]
fn evaluator_drift_is_terminal_integrity_evidence_and_stops_campaign() {
    let (connection, objects, candidate, materialization, baseline) = prepared_trial();
    reserve_budget(
        &connection,
        "test",
        &candidate.proposal.campaign_id,
        "integrity-budget",
        &[
            BudgetRequest::new(BudgetResource::Trials, 1).expect("trial"),
            BudgetRequest::new(BudgetResource::Failures, 1).expect("failure"),
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 100).expect("wall"),
            BudgetRequest::new(BudgetResource::ArtifactBytes, 512).expect("artifact"),
        ],
    )
    .expect("reserve");
    let manifest =
        campaign_manifest(&connection, &candidate.proposal.campaign_id).expect("manifest");
    let intent = TrialIntent {
        trial_id: "integrity-trial".to_owned(),
        campaign_id: candidate.proposal.campaign_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        materialization_receipt_sha256: materialization.receipt_sha256.clone(),
        baseline_materialization_receipt_sha256: baseline.receipt_sha256,
        result_tree: materialization.result_tree,
        working_directory: materialization.worktree_locator,
        reservation_id: "integrity-budget".to_owned(),
        tier: "exploration".to_owned(),
        argv: manifest.evaluator.argv.clone(),
        environment: serde_json::to_value(&manifest.evaluator.environment).expect("environment"),
        owner_uuid: "integrity-owner".to_owned(),
        supervisor_identity: "integrity-supervisor".to_owned(),
    };
    record_trial_intent(&connection, "test", &intent).expect("intent");
    let evidence = preserve_object(
        objects.path(),
        br#"{"evaluator":"drifted","expected":"frozen"}"#,
    )
    .expect("integrity evidence");
    let failure = IntegrityFailure {
        reason_code: "evaluator-digest-mismatch".to_owned(),
        expected_outer_judge_sha256: manifest.generation.outer_judge_executable_sha256.0.clone(),
        observed_outer_judge_sha256: manifest.generation.outer_judge_executable_sha256.0.clone(),
        expected_launcher_sha256: manifest.evaluator.launcher_sha256.0.clone(),
        observed_launcher_sha256: manifest.evaluator.launcher_sha256.0.clone(),
        expected_evaluator_sha256: manifest.evaluator.evaluator_sha256.0,
        observed_evaluator_sha256: "0".repeat(64),
        expected_fixture_sha256: manifest
            .holdouts
            .tiers
            .iter()
            .find(|tier| tier.key == "exploration")
            .expect("exploration")
            .fixture_sha256
            .0
            .clone(),
        observed_fixture_sha256: manifest
            .holdouts
            .tiers
            .iter()
            .find(|tier| tier.key == "exploration")
            .expect("exploration")
            .fixture_sha256
            .0
            .clone(),
        frozen_input_mismatches: Vec::new(),
        evidence,
    };
    record_integrity_failure(
        &connection,
        "test",
        objects.path(),
        "integrity-trial",
        &failure,
    )
    .expect("integrity failure");
    record_integrity_failure(
        &connection,
        "test",
        objects.path(),
        "integrity-trial",
        &failure,
    )
    .expect("integrity replay");
    assert_eq!(
        trial(&connection, "integrity-trial")
            .expect("query")
            .expect("trial")
            .status,
        TrialStatus::IntegrityFailed
    );
}

fn proposal_for(
    manifest: &CampaignManifest,
    semantic_class: &str,
    changed_paths: BTreeSet<String>,
) -> CandidateProposal {
    CandidateProposal {
        campaign_id: manifest.campaign_id.clone(),
        parent_candidate_ids: BTreeSet::new(),
        base_commit: manifest.source.base_commit.clone(),
        base_tree: manifest.source.base_tree.clone(),
        proposer: "fixture".to_owned(),
        proposal_policy_sha256: manifest.generation.proposal_policy_sha256.0.clone(),
        adapter_sha256: manifest.adapter.implementation_sha256.0.clone(),
        hypothesis: Hypothesis {
            mechanism: semantic_class.to_owned(),
            expected_effects: vec!["bounded deterministic effect".to_owned()],
            possible_regressions: vec!["fixture rejection".to_owned()],
            decisive_falsifiers: vec!["frozen objective failure".to_owned()],
        },
        changed_paths,
        changed_symbols: BTreeSet::from([semantic_class.to_owned()]),
        semantic_class: semantic_class.to_owned(),
        differentiator: None,
    }
}

fn record_fixture_candidate(
    connection: &Connection,
    objects: &Path,
    manifest: &CampaignManifest,
    reservation_id: &str,
    semantic_class: &str,
    patch_bytes: Vec<u8>,
) -> BoundCandidate {
    let changed_paths = if patch_bytes.is_empty() {
        BTreeSet::new()
    } else {
        BTreeSet::from(["src/fixture.rs".to_owned()])
    };
    let candidate = bind_legacy_patch_candidate(
        proposal_for(manifest, semantic_class, changed_paths),
        patch_bytes,
    )
    .expect("bind fixture candidate");
    reserve_budget(
        connection,
        "test",
        &manifest.campaign_id,
        reservation_id,
        &[
            BudgetRequest::new(BudgetResource::Candidates, 1).expect("candidate request"),
            BudgetRequest::new(
                BudgetResource::ArtifactBytes,
                u64::try_from(candidate.material_bytes.len())
                    .expect("patch size")
                    .max(1),
            )
            .expect("patch artifact request"),
        ],
    )
    .expect("reserve fixture candidate");
    let object = preserve_object(objects, &candidate.material_bytes).expect("patch object");
    record_legacy_candidate_for_test(
        connection,
        "test",
        objects,
        reservation_id,
        &candidate,
        &object,
    )
    .expect("record fixture candidate");
    fake_materialization(
        connection,
        objects,
        &candidate,
        &format!("fake-materialization-{reservation_id}"),
    );
    candidate
}

fn complete_fixture_trial(
    connection: &Connection,
    objects: &Path,
    manifest: &CampaignManifest,
    candidate: &BoundCandidate,
    trial_id: &str,
    tier: &str,
    outcome: (Vec<DeterministicObservation>, Option<&str>),
) {
    let (mut observations, reason_code) = outcome;
    observations.sort_by(|left, right| left.objective.cmp(&right.objective));
    let reservation_id = format!("{trial_id}-budget");
    let calibration = tier.starts_with("calibration.");
    let mut requests = vec![
        BudgetRequest::new(BudgetResource::Trials, 1).expect("trial request"),
        BudgetRequest::new(BudgetResource::Failures, 1).expect("failure request"),
        BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 100).expect("wall request"),
        BudgetRequest::new(BudgetResource::ArtifactBytes, 4_096).expect("artifact request"),
    ];
    if !calibration {
        requests.push(
            BudgetRequest::new(BudgetResource::HoldoutDisclosures, 1).expect("disclosure request"),
        );
    }
    reserve_budget(
        connection,
        "test",
        &manifest.campaign_id,
        &reservation_id,
        &requests,
    )
    .expect("reserve trial");
    let materialization = materialization_in(connection, &candidate.candidate_id)
        .expect("materialization query")
        .expect("candidate materialization");
    let baseline_candidate_id: String = connection
        .query_row(
            "SELECT candidate_id FROM candidates WHERE campaign_id=?1 AND patch_sha256=?2",
            params![
                manifest.campaign_id,
                manifest.calibration.no_op_material_sha256().0
            ],
            |row| row.get(0),
        )
        .expect("baseline candidate");
    let baseline = materialization_in(connection, &baseline_candidate_id)
        .expect("baseline query")
        .expect("baseline materialization");
    let intent = TrialIntent {
        trial_id: trial_id.to_owned(),
        campaign_id: manifest.campaign_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        materialization_receipt_sha256: materialization.receipt_sha256.clone(),
        baseline_materialization_receipt_sha256: baseline.receipt_sha256.clone(),
        result_tree: materialization.result_tree.clone(),
        working_directory: materialization.worktree_locator.clone(),
        reservation_id,
        tier: tier.to_owned(),
        argv: manifest.evaluator.argv.clone(),
        environment: serde_json::to_value(&manifest.evaluator.environment).expect("environment"),
        owner_uuid: format!("owner-{trial_id}"),
        supervisor_identity: format!("supervisor-{trial_id}"),
    };
    record_trial_intent(connection, "test", &intent).expect("trial intent");
    mark_trial_launched(
        connection,
        "test",
        trial_id,
        &TrialOwnership {
            owner_uuid: intent.owner_uuid,
            pid: 42,
            process_birth_identity: format!("birth-{trial_id}"),
            supervisor_identity: intent.supervisor_identity,
        },
    )
    .expect("trial launch");
    heartbeat_trial(
        connection,
        "test",
        trial_id,
        &format!("owner-{trial_id}"),
        &format!("supervisor-{trial_id}"),
    )
    .expect("trial heartbeat");
    let fixture_sha256 = if tier == "calibration.no_op" {
        manifest.calibration.no_op.fixture_sha256.0.clone()
    } else if tier == "calibration.known_bad" {
        manifest.calibration.known_bad.fixture_sha256.0.clone()
    } else {
        manifest
            .holdouts
            .tiers
            .iter()
            .find(|declared| declared.key == tier)
            .expect("holdout tier")
            .fixture_sha256
            .0
            .clone()
    };
    let mut measured_usage = vec![
        BudgetSettlement {
            resource: BudgetResource::Trials,
            actual_amount: 1,
        },
        BudgetSettlement {
            resource: BudgetResource::Failures,
            actual_amount: 0,
        },
        BudgetSettlement {
            resource: BudgetResource::WallTimeMilliseconds,
            actual_amount: 5,
        },
    ];
    if !calibration {
        measured_usage.push(BudgetSettlement {
            resource: BudgetResource::HoldoutDisclosures,
            actual_amount: 1,
        });
    }
    let receipt = TrialReceipt {
        schema: "papertiger-mise.trial-receipt.v1".to_owned(),
        environment_sha256: None,
        judge_build: None,
        trial_id: trial_id.to_owned(),
        campaign_id: manifest.campaign_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        materialization_receipt_sha256: materialization.receipt_sha256,
        baseline_materialization_receipt_sha256: baseline.receipt_sha256,
        result_tree: materialization.result_tree,
        working_directory: materialization.worktree_locator,
        tier: tier.to_owned(),
        owner_uuid: format!("owner-{trial_id}"),
        supervisor_identity: format!("supervisor-{trial_id}"),
        process_birth_identity: format!("birth-{trial_id}"),
        launcher_sha256: manifest.evaluator.launcher_sha256.0.clone(),
        evaluator_sha256: manifest.evaluator.evaluator_sha256.0.clone(),
        fixture_sha256,
        protocol: manifest.evaluator.protocol.clone(),
        observations,
        reason_code: reason_code.map(str::to_owned),
        measured_usage,
        execution_capabilities: None,
    };
    let receipt_bytes = serde_json::to_vec(&receipt).expect("receipt bytes");
    let receipt_object = preserve_object(objects, &receipt_bytes).expect("trial receipt");
    complete_deterministic_trial(
        connection,
        "test",
        objects,
        trial_id,
        &TrialCompletion {
            receipt: receipt_object,
        },
        &TrialCompletionCredential {
            owner_uuid: format!("owner-{trial_id}"),
            supervisor_identity: format!("supervisor-{trial_id}"),
            process_birth_identity: format!("birth-{trial_id}"),
        },
    )
    .expect("complete trial");
}

fn trusted_policy(signing_key: &SigningKey) -> crate::attestation::TrustedContainmentPolicy {
    crate::attestation::TrustedContainmentPolicy {
        schema: crate::attestation::TRUSTED_CONTAINMENT_POLICY_SCHEMA_V2.to_owned(),
        protocol: crate::attestation::SEALED_ATTESTATION_PROTOCOL_V2.to_owned(),
        issuer_identity: "independent-fixture-issuer".to_owned(),
        public_key_ed25519: bytes_hex(&signing_key.verifying_key().to_bytes()),
        executor_sha256: "4".repeat(64),
        profile_sha256: "3".repeat(64),
    }
}

fn sign_trial_attestation(
    connection: &Connection,
    objects: &Path,
    manifest: &CampaignManifest,
    trial_id: &str,
    signing_key: &SigningKey,
    policy: &crate::attestation::TrustedContainmentPolicy,
) -> Result<bool> {
    let durable = trial(connection, trial_id)
        .expect("trial query")
        .expect("attested trial");
    let manifest_sha256 = manifest.sha256().expect("manifest digest");
    let trial_receipt_sha256 = durable
        .outcome
        .as_ref()
        .and_then(|value| value.pointer("/receipt/sha256"))
        .and_then(Value::as_str)
        .expect("trial receipt digest")
        .to_owned();
    let baseline_result_tree: String = connection
        .query_row(
            "SELECT result_tree FROM materializations WHERE receipt_sha256=?1",
            params![durable.baseline_materialization_receipt_sha256],
            |row| row.get(0),
        )
        .expect("baseline tree");
    let invocation_sha256 = sha256(
        &serde_json::to_vec(&json!({
            "argv": durable.argv,
            "environment": durable.environment,
            "working_directory": durable.working_directory,
            "candidate_result_tree": durable.result_tree,
            "baseline_result_tree": baseline_result_tree,
        }))
        .expect("invocation"),
    );
    let execution_limits_sha256 =
        sha256(&serde_json::to_vec(&manifest.execution_limits).expect("limits"));
    let fixture_sha256 =
        expected_fixture_sha256(manifest, &durable.tier).expect("fixture identity");
    let verdict_only_disclosure = manifest
        .holdouts
        .tiers
        .iter()
        .find(|tier| tier.key == durable.tier)
        .is_some_and(|tier| {
            tier.kind == crate::manifest::HoldoutTierKind::Confirmation
                && matches!(
                    tier.disclosure,
                    crate::manifest::HoldoutDisclosure::VerdictOnly
                )
        });
    let payload = crate::attestation::SealedAttestationPayload {
        schema: crate::attestation::SEALED_ATTESTATION_SCHEMA_V2.to_owned(),
        campaign_id: durable.campaign_id.clone(),
        manifest_sha256,
        candidate_id: durable.candidate_id.clone(),
        trial_id: durable.trial_id.clone(),
        tier: durable.tier.clone(),
        trial_receipt_sha256,
        materialization_receipt_sha256: durable.materialization_receipt_sha256.clone(),
        baseline_materialization_receipt_sha256: durable
            .baseline_materialization_receipt_sha256
            .clone(),
        candidate_tree_before: durable.result_tree.clone(),
        candidate_tree_after: durable.result_tree.clone(),
        baseline_tree_before: baseline_result_tree.clone(),
        baseline_tree_after: baseline_result_tree,
        launcher_sha256: manifest.evaluator.launcher_sha256.0.clone(),
        evaluator_sha256: manifest.evaluator.evaluator_sha256.0.clone(),
        fixture_locator: if durable.tier == "calibration.no_op" {
            manifest.calibration.no_op.fixture_locator.clone()
        } else if durable.tier == "calibration.known_bad" {
            manifest.calibration.known_bad.fixture_locator.clone()
        } else {
            manifest
                .holdouts
                .tiers
                .iter()
                .find(|tier| tier.key == durable.tier)
                .expect("fixture tier")
                .fixture_locator
                .clone()
        },
        fixture_sha256,
        executor_sha256: policy.executor_sha256.clone(),
        profile_sha256: policy.profile_sha256.clone(),
        trusted_policy_sha256: policy.sha256().expect("policy digest"),
        issuer_identity: policy.issuer_identity.clone(),
        invocation_sha256,
        execution_limits_sha256,
        actual_grade: "sealed".to_owned(),
        network_denied: true,
        read_only_evaluator_inputs: true,
        workspace_isolated: true,
        resource_ceilings_enforced: true,
        controller_loss_cleanup_enforced: true,
        maximum_processes: manifest
            .containment_requirement
            .as_ref()
            .expect("containment requirement")
            .maximum_processes,
        maximum_memory_bytes: manifest
            .containment_requirement
            .as_ref()
            .expect("containment requirement")
            .maximum_memory_bytes,
        verdict_only_disclosure,
        supervisor_identity: durable.supervisor_identity.expect("supervisor"),
        process_birth_identity: durable.process_birth_identity.expect("birth identity"),
        boundary_identity: format!("fixture-boundary-{trial_id}"),
        execution_started_at: "2026-07-31T12:00:00Z".to_owned(),
        execution_finished_at: "2026-07-31T12:00:01Z".to_owned(),
    };
    let signature = signing_key.sign(&serde_json::to_vec(&payload).expect("payload"));
    let signed = crate::attestation::SignedSealedAttestation {
        payload,
        signature_ed25519: bytes_hex(&signature.to_bytes()),
    };
    let object = preserve_object(
        objects,
        &serde_json::to_vec(&signed).expect("signed attestation"),
    )
    .expect("attestation object");
    crate::attestation::record_sealed_attestation(
        connection,
        "independent-attestor",
        objects,
        trial_id,
        &object,
        policy,
    )
}

fn bytes_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("format hex");
    }
    output
}

#[test]
fn nomination_is_derived_from_calibrated_bound_trials_only() {
    let objects = tempdir().expect("objects");
    let mise_path = objects.path().join("mise.sqlite");
    let connection = Connection::open(&mise_path).expect("database");
    init(&connection).expect("schema");
    let known_bad_patch = b"diff --git a/src/fixture.rs b/src/fixture.rs\n--- a/src/fixture.rs\n+++ b/src/fixture.rs\n@@ -1 +1 @@\n-old\n+known-bad\n".to_vec();
    let improved_patch = b"diff --git a/src/fixture.rs b/src/fixture.rs\n--- a/src/fixture.rs\n+++ b/src/fixture.rs\n@@ -1 +1 @@\n-old\n+improved\n".to_vec();
    let mut manifest = crate::manifest::tests::valid_manifest();
    let source = objects.path().join("source");
    let runs = objects.path().join("runs");
    std::fs::create_dir_all(source.join("src")).expect("source directory");
    std::fs::create_dir_all(&runs).expect("runs directory");
    git_run(&source, &["init"], None, None, None).expect("initialize source");
    git_run(
        &source,
        &["config", "user.name", "Mise Test"],
        None,
        None,
        None,
    )
    .expect("Git user");
    git_run(
        &source,
        &["config", "user.email", "mise-test@example.invalid"],
        None,
        None,
        None,
    )
    .expect("Git email");
    git_run(
        &source,
        &["config", "core.autocrlf", "false"],
        None,
        None,
        None,
    )
    .expect("Git line endings");
    std::fs::write(source.join("src/fixture.rs"), b"old\n").expect("fixture source");
    git_run(&source, &["add", "src/fixture.rs"], None, None, None).expect("stage source");
    git_run(&source, &["commit", "-m", "frozen base"], None, None, None).expect("commit source");
    manifest.source.repository_locator =
        canonical_or_pending_absolute(&source).expect("source locator");
    manifest.source.base_commit = git_text(&source, &["rev-parse", "HEAD^{commit}"])
        .expect("base commit")
        .trim()
        .to_owned();
    manifest.source.base_tree = git_text(&source, &["rev-parse", "HEAD^{tree}"])
        .expect("base tree")
        .trim()
        .to_owned();
    manifest.execution_limits.workspace_root_locator =
        canonical_or_pending_absolute(&runs).expect("runs locator");
    manifest.calibration.no_op.minimum_repetitions = 2;
    manifest.calibration.known_bad.minimum_repetitions = 1;
    manifest
        .calibration
        .known_bad
        .candidate_patch_sha256
        .as_mut()
        .expect("legacy known-bad patch")
        .0 = sha256(&known_bad_patch);
    let admission = CampaignAdmission::from_manifest(&manifest).expect("admission");
    admit_campaign(&connection, "test", &admission).expect("campaign");

    let no_op = record_fixture_candidate(
        &connection,
        objects.path(),
        &manifest,
        "candidate-no-op",
        "calibration-no-op",
        Vec::new(),
    );
    let known_bad = record_fixture_candidate(
        &connection,
        objects.path(),
        &manifest,
        "candidate-known-bad",
        "calibration-known-bad",
        known_bad_patch,
    );
    let improved = record_fixture_candidate(
        &connection,
        objects.path(),
        &manifest,
        "candidate-improved",
        "bounded-improvement",
        improved_patch,
    );
    let flat = vec![
        DeterministicObservation {
            objective: "tests-pass".to_owned(),
            baseline: 1.0,
            candidate: 1.0,
        },
        DeterministicObservation {
            objective: "latency-ms".to_owned(),
            baseline: 10.0,
            candidate: 10.0,
        },
    ];
    complete_fixture_trial(
        &connection,
        objects.path(),
        &manifest,
        &no_op,
        "calibration-no-op-1",
        "calibration.no_op",
        (flat.clone(), None),
    );
    complete_fixture_trial(
        &connection,
        objects.path(),
        &manifest,
        &no_op,
        "calibration-no-op-2",
        "calibration.no_op",
        (flat, None),
    );
    complete_fixture_trial(
        &connection,
        objects.path(),
        &manifest,
        &known_bad,
        "calibration-known-bad-1",
        "calibration.known_bad",
        (
            vec![
                DeterministicObservation {
                    objective: "tests-pass".to_owned(),
                    baseline: 1.0,
                    candidate: 0.0,
                },
                DeterministicObservation {
                    objective: "latency-ms".to_owned(),
                    baseline: 10.0,
                    candidate: 8.0,
                },
            ],
            Some("known-regression"),
        ),
    );
    let improved_observations = vec![
        DeterministicObservation {
            objective: "tests-pass".to_owned(),
            baseline: 1.0,
            candidate: 1.0,
        },
        DeterministicObservation {
            objective: "latency-ms".to_owned(),
            baseline: 10.0,
            candidate: 8.0,
        },
    ];
    complete_fixture_trial(
        &connection,
        objects.path(),
        &manifest,
        &improved,
        "improved-exploration",
        "exploration",
        (improved_observations.clone(), None),
    );
    assert!(
        adjudicate_deterministic_candidate(&connection, "test", &improved.candidate_id).is_err()
    );
    complete_fixture_trial(
        &connection,
        objects.path(),
        &manifest,
        &improved,
        "improved-confirmation",
        "confirmation",
        (improved_observations, None),
    );
    let nomination =
        adjudicate_deterministic_candidate(&connection, "test", &improved.candidate_id)
            .expect("adjudicate")
            .expect("nomination");
    assert_eq!(nomination.candidate_id, improved.candidate_id);
    assert_eq!(nomination.receipt_sha256, nomination.nomination_id);
    let verified =
        verify_nomination_integrity(&connection, objects.path(), &nomination.nomination_id)
            .expect("re-derive nomination from CAS");
    assert_eq!(verified.nomination, nomination);
    assert_eq!(verified.relied_upon_trial_ids.len(), 5);
    assert_eq!(
        nominations(&connection, None).expect("list nominations"),
        vec![nomination.clone()]
    );
    assert_eq!(
        nominations(&connection, Some(&manifest.campaign_id)).expect("list campaign nominations"),
        vec![nomination.clone()]
    );
    assert!(
        nominations(&connection, Some("another-campaign"))
            .expect("empty campaign nomination list")
            .is_empty()
    );

    let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    let policy = trusted_policy(&signing_key);
    assert!(
        crate::promotion::derive_promotion_proof(
            &mise_path,
            objects.path(),
            &policy,
            &nomination.nomination_id,
        )
        .is_err(),
        "a nomination without independent attestations must not promote"
    );
    for trial_id in &verified.relied_upon_trial_ids {
        let result = sign_trial_attestation(
            &connection,
            objects.path(),
            &manifest,
            trial_id,
            &signing_key,
            &policy,
        );
        if trial_id == "improved-confirmation" {
            let error = result
                .expect_err("raw-observation confirmation cannot claim verdict-only attestation");
            assert!(
                error.to_string().contains("genuinely verdict-only"),
                "{error:#}"
            );
        } else {
            result.expect("record non-confirmation sealed attestation");
        }
    }
    assert!(
        crate::promotion::derive_promotion_proof(
            &mise_path,
            objects.path(),
            &policy,
            &nomination.nomination_id,
        )
        .is_err(),
        "current receipt schema must fail closed before promotion"
    );
}
