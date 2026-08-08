use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use papertiger_mise::manifest::{
    AdapterBinding, CAMPAIGN_SCHEMA_V1, CalibrationRequirements, CampaignManifest,
    CandidateMaterialContract, ContainmentGrade, CumulativeBudgetCaps, EvaluatorBinding,
    ExecutionLimits, GenerationBinding, HoldoutDisclosure, HoldoutPolicy, HoldoutTier,
    HoldoutTierKind, JudgeBuildBinding, KnownBadCalibration, MutationScope, NetworkPolicy,
    NoOpCalibration, ObjectiveDirection, ObjectiveRole, ObjectiveSpec, ParentCampaignBinding,
    Sha256Digest, StopRules,
};
use papertiger_mise::{
    AdmissionOutcome, BudgetLimit, BudgetRequest, BudgetResource, CandidateDisposition,
    CandidateMaterial, CandidateProposal, ColdRecoveryOutcome, FixtureBundleDescriptor,
    FixtureBundleEntry, GIT_CHANGE_SET_MEDIA_TYPE, GIT_CHANGE_SET_PROTOCOL_V1, GitChangeOperation,
    Hypothesis, NominationRecord, PromotionGateBinding, ReservationOutcome,
    SEALED_ATTESTATION_PROTOCOL_V2, SupervisedTrialSpec, TRUSTED_CONTAINMENT_POLICY_SCHEMA_V2,
    TrustedContainmentPolicy, adjudicate_deterministic_candidate, admit_verified_campaign,
    admit_verified_successor, bind_candidate, budget_balances, build_git_change_set_material,
    campaign, campaign_events, derive_candidate_planner_projection,
    derive_nomination_planner_projection, derive_parent_promotion_proof, derive_promotion_proof,
    execute_workspace_trial, init, inspect_source_binding, materialize_candidate,
    negative_fingerprint_candidates, open_existing, open_for_init, preserve_object,
    preserve_parent_promotion_proof, read_object, record_candidate, recover_workspace_trial,
    reserve_budget, successor_admission, trial, verify_campaign_admission,
    verify_nomination_integrity, verify_parent_promotion_gate, verify_successor_admission,
};
use tempfile::TempDir;

const ACTOR: &str = "deterministic-dogfood";
const CAMPAIGN_ID: &str = "papertiger-mise-deterministic-dogfood-a01";

#[test]
fn interrupted_trial_worker() {
    let Ok(database) = std::env::var("PAPERTIGER_MISE_DOGFOOD_WORKER_DB") else {
        return;
    };
    let connection = open_existing(&database).expect("open worker Mise authority");
    let object_root = PathBuf::from(
        std::env::var("PAPERTIGER_MISE_DOGFOOD_WORKER_OBJECTS").expect("worker object root"),
    );
    let spec: SupervisedTrialSpec = serde_json::from_str(
        &std::env::var("PAPERTIGER_MISE_DOGFOOD_WORKER_SPEC").expect("worker trial spec"),
    )
    .expect("parse worker trial spec");
    execute_workspace_trial(&connection, "interrupted-worker", &object_root, &spec)
        .expect("worker trial should be interrupted by its parent");
}

#[test]
fn deterministic_public_api_campaign_preserves_every_outcome() {
    let fixture = DogfoodFixture::new();
    let connection = open_existing(&fixture.database).expect("open Mise authority");

    let no_op = fixture.record_and_materialize(&connection, "no-op", "calibration-no-op", b"");
    let known_bad = fixture.record_and_materialize(
        &connection,
        "known-bad",
        "calibration-known-bad",
        &replace_score_patch("10", "known-bad"),
    );
    let incorrect = fixture.record_and_materialize(
        &connection,
        "incorrect",
        "hard-constraint-regression",
        &replace_score_patch("10", "incorrect"),
    );
    let flat = fixture.record_and_materialize(
        &connection,
        "flat",
        "non-improving-note",
        &replace_note_patch("base", "flat"),
    );
    let improved = fixture.record_and_materialize(
        &connection,
        "improved",
        "lower-score",
        &replace_score_patch("10", "8"),
    );
    let crashed = fixture.record_and_materialize(
        &connection,
        "crashed",
        "evaluator-crash",
        &replace_score_patch("10", "crash"),
    );
    let interrupted = fixture.record_and_materialize(
        &connection,
        "interrupted",
        "supervisor-interruption",
        &replace_score_patch("10", "sleep"),
    );

    fixture
        .run_success(
            &connection,
            &no_op.candidate_id,
            "noop-1",
            "calibration.no_op",
        )
        .expect("first no-op calibration");
    fixture
        .run_success(
            &connection,
            &no_op.candidate_id,
            "noop-2",
            "calibration.no_op",
        )
        .expect("second no-op calibration");
    fixture
        .run_success(
            &connection,
            &known_bad.candidate_id,
            "known-bad-1",
            "calibration.known_bad",
        )
        .expect("known-bad calibration");

    fixture
        .run_success(
            &connection,
            &incorrect.candidate_id,
            "incorrect-1",
            "exploration",
        )
        .expect("incorrect candidate trial");
    assert!(
        adjudicate_deterministic_candidate(&connection, ACTOR, &incorrect.candidate_id)
            .expect("adjudicate incorrect candidate")
            .is_none()
    );
    assert_eq!(
        papertiger_mise::candidate(&connection, &incorrect.candidate_id)
            .expect("incorrect candidate query")
            .expect("incorrect candidate")
            .disposition,
        CandidateDisposition::Rejected
    );
    assert_eq!(
        negative_fingerprint_candidates(&connection, CAMPAIGN_ID, &incorrect.negative_fingerprint,)
            .expect("negative evidence query"),
        vec![incorrect.candidate_id.clone()]
    );
    let rejected_projection =
        derive_candidate_planner_projection(&connection, &fixture.objects, &incorrect.candidate_id)
            .expect("derive rejected planner projection from retained evidence");
    assert_eq!(rejected_projection.disposition.as_str(), "rejected");
    assert_eq!(
        rejected_projection.candidate_material_sha256,
        incorrect.material_sha256
    );

    fixture
        .run_success(&connection, &flat.candidate_id, "flat-1", "exploration")
        .expect("flat candidate trial");
    assert!(
        adjudicate_deterministic_candidate(&connection, ACTOR, &flat.candidate_id)
            .expect("adjudicate flat candidate")
            .is_none()
    );
    assert_eq!(
        papertiger_mise::candidate(&connection, &flat.candidate_id)
            .expect("flat candidate query")
            .expect("flat candidate")
            .disposition,
        CandidateDisposition::Inconclusive
    );

    let improved_spec = fixture.reserve_trial(
        &connection,
        &improved.candidate_id,
        "improved-1",
        "exploration",
    );
    let first = execute_workspace_trial(&connection, ACTOR, &fixture.objects, &improved_spec)
        .expect("improved candidate trial");
    let events_before = campaign_events(&connection, CAMPAIGN_ID).expect("events before replay");
    let replay = execute_workspace_trial(&connection, ACTOR, &fixture.objects, &improved_spec)
        .expect("successful trial replay");
    assert_eq!(replay.receipt, first.receipt);
    assert_eq!(
        campaign_events(&connection, CAMPAIGN_ID)
            .expect("events after replay")
            .len(),
        events_before.len(),
        "successful replay must not execute or append lifecycle events"
    );
    let nomination = adjudicate_deterministic_candidate(&connection, ACTOR, &improved.candidate_id)
        .expect("adjudicate improved candidate")
        .expect("qualified nomination");
    let verified =
        verify_nomination_integrity(&connection, &fixture.objects, &nomination.nomination_id)
            .expect("re-derive nomination from CAS");
    assert_eq!(verified.nomination, nomination);
    assert!(
        verified
            .relied_upon_trial_ids
            .contains(&"improved-1".to_owned())
    );
    let projection = derive_nomination_planner_projection(
        &connection,
        &fixture.objects,
        &nomination.nomination_id,
    )
    .expect("derive nominated planner projection from retained evidence");
    assert_eq!(projection.disposition.as_str(), "nominated");
    assert_eq!(
        projection.nomination_id.as_deref(),
        Some(nomination.nomination_id.as_str())
    );
    assert!(
        projection
            .limitations
            .iter()
            .any(|value| value == "nomination-is-evidence-not-integration-or-promotion")
    );

    let policy = trusted_policy();
    let error = derive_promotion_proof(
        &fixture.database,
        &fixture.objects,
        &policy,
        &nomination.nomination_id,
    )
    .expect_err("WorkspaceOnly evidence must never produce a promotion proof");
    assert!(
        error.to_string().contains("requires a sealed campaign"),
        "{error:#}"
    );

    let duplicate_patch = preserve_object(&fixture.objects, &improved.material_bytes)
        .expect("duplicate patch object");
    assert!(
        !record_candidate(
            &connection,
            ACTOR,
            &fixture.objects,
            "candidate-improved",
            &improved,
            &duplicate_patch,
        )
        .expect("exact candidate replay")
    );

    let crash_spec = fixture.reserve_trial(
        &connection,
        &crashed.candidate_id,
        "crashed-1",
        "exploration",
    );
    let error = execute_workspace_trial(&connection, ACTOR, &fixture.objects, &crash_spec)
        .expect_err("crashing evaluator must fail");
    assert!(
        error.to_string().contains("process-exit-failed"),
        "{error:#}"
    );
    let crashed_trial = trial(&connection, "crashed-1")
        .expect("crash trial query")
        .expect("crash trial");
    assert_eq!(
        crashed_trial.status,
        papertiger_mise::TrialStatus::InfrastructureFailed
    );
    let crash_evidence: papertiger_mise::PreservedObject = serde_json::from_value(
        crashed_trial
            .outcome
            .expect("crash outcome")
            .pointer("/absence_proof/evidence")
            .cloned()
            .expect("crash evidence pointer"),
    )
    .expect("crash evidence object");
    let crash_body = read_object(&fixture.objects, &crash_evidence).expect("crash evidence bytes");
    assert!(
        String::from_utf8(crash_body)
            .expect("crash evidence UTF-8")
            .contains("process-exit-failed")
    );

    let interrupted_spec = fixture.reserve_trial(
        &connection,
        &interrupted.candidate_id,
        "interrupted-1",
        "exploration",
    );
    fixture.interrupt_and_recover(&connection, &interrupted_spec);

    let balances_after_recovery =
        budget_balances(&connection, CAMPAIGN_ID).expect("budget balances");
    let failure_spend = balances_after_recovery
        .iter()
        .find(|balance| balance.resource == BudgetResource::Failures)
        .expect("failure balance")
        .spent_amount;
    assert_eq!(
        failure_spend, 2,
        "crash and interruption charge exactly once"
    );
    assert_eq!(
        recover_workspace_trial(&connection, ACTOR, &fixture.objects, "interrupted-1")
            .expect("second cold recovery"),
        ColdRecoveryOutcome::AlreadyReconciled
    );
    assert_eq!(
        budget_balances(&connection, CAMPAIGN_ID).expect("replayed balances"),
        balances_after_recovery,
        "cold-recovery replay must not change any budget balance"
    );

    let error = reserve_budget(
        &connection,
        ACTOR,
        CAMPAIGN_ID,
        "over-budget",
        &[BudgetRequest::new(BudgetResource::Candidates, 1_000).expect("request")],
    )
    .expect_err("cumulative budget cap must be enforced");
    assert!(error.to_string().contains("exceeds campaign"), "{error:#}");

    fixture.prove_successor_lineage(&connection, &nomination, &improved);
}

struct DogfoodFixture {
    _temporary: TempDir,
    database: PathBuf,
    objects: PathBuf,
    runs: PathBuf,
    manifest: CampaignManifest,
}

impl DogfoodFixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("dogfood root");
        let source = temporary.path().join("source");
        let control = temporary.path().join("control");
        let runs = temporary.path().join("runs");
        let objects = temporary.path().join("objects");
        std::fs::create_dir_all(source.join("src")).expect("source directory");
        std::fs::create_dir_all(source.join("fixtures/mise")).expect("fixture directory");
        std::fs::create_dir_all(&control).expect("control directory");
        std::fs::create_dir_all(&runs).expect("run directory");

        std::fs::write(source.join("src/score.txt"), b"10\n").expect("score fixture");
        std::fs::write(source.join("src/note.txt"), b"base\n").expect("note fixture");
        let evaluator_source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/deterministic_evaluator.rs");
        let evaluator = std::fs::read(&evaluator_source).expect("fixture evaluator source");
        std::fs::write(
            source.join("fixtures/mise/deterministic_evaluator.rs"),
            &evaluator,
        )
        .expect("evaluator implementation");
        let adapter = br#"{"schema":"papertiger-mise.dogfood-adapter.v1"}"#;
        std::fs::write(source.join("fixtures/mise/adapter.json"), adapter).expect("adapter");
        let no_op_fixture = br#"{"calibration":"no-op"}"#;
        let known_bad_fixture = br#"{"calibration":"known-bad"}"#;
        let exploration_fixture = br#"{"holdout":"exploration"}"#;
        std::fs::write(
            source.join("fixtures/mise/calibration-no-op.json"),
            no_op_fixture,
        )
        .expect("no-op fixture");
        std::fs::write(
            source.join("fixtures/mise/calibration-known-bad.json"),
            known_bad_fixture,
        )
        .expect("known-bad fixture");
        std::fs::write(
            source.join("fixtures/mise/exploration.json"),
            exploration_fixture,
        )
        .expect("exploration fixture");

        let bundle = FixtureBundleDescriptor {
            schema: papertiger_mise::FIXTURE_BUNDLE_SCHEMA_V1.to_owned(),
            fixtures: vec![
                fixture_entry(
                    "calibration.known_bad",
                    "fixtures/mise/calibration-known-bad.json",
                    known_bad_fixture,
                ),
                fixture_entry(
                    "calibration.no_op",
                    "fixtures/mise/calibration-no-op.json",
                    no_op_fixture,
                ),
                fixture_entry(
                    "holdout.exploration",
                    "fixtures/mise/exploration.json",
                    exploration_fixture,
                ),
            ],
        };
        let bundle_bytes = bundle.canonical_bytes().expect("canonical fixture bundle");
        std::fs::write(source.join("fixtures/mise/bundle.json"), &bundle_bytes)
            .expect("fixture bundle");
        init_git_repository(&source);
        let source_binding = inspect_source_binding(&source).expect("source binding");

        let policy_bytes =
            br#"{"schema":"papertiger-mise.proposal-policy.v1","maximum_candidates":12}"#;
        std::fs::write(control.join("policy.json"), policy_bytes).expect("proposal policy");
        let launcher_directory = temporary.path().join("launcher");
        std::fs::create_dir(&launcher_directory).expect("launcher directory");
        let launcher = launcher_directory.join(format!(
            "deterministic-evaluator{}",
            std::env::consts::EXE_SUFFIX
        ));
        let compile = Command::new("rustc")
            .arg(&evaluator_source)
            .args(["--edition", "2024", "-O", "-o"])
            .arg(&launcher)
            .output()
            .expect("compile deterministic evaluator fixture");
        assert!(
            compile.status.success(),
            "fixture evaluator compilation failed: {}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let launcher = canonical_absolute(launcher);
        let outer_judge = canonical_absolute(std::env::current_exe().expect("test executable"));
        let launcher_locator = portable_absolute(&launcher);
        let launcher_sha256 = file_digest(&launcher);
        let outer_judge_locator = portable_absolute(&outer_judge);
        let known_bad_patch = replace_score_patch("10", "known-bad");
        let authoring = temporary.path().join("material-authoring");
        std::fs::create_dir(&authoring).expect("material authoring root");
        let no_op_material = material_from_patch(
            &source,
            &source_binding.base_commit,
            &source_binding.base_tree,
            &authoring.join("binding-no-op"),
            b"",
        );
        let known_bad_material = material_from_patch(
            &source,
            &source_binding.base_commit,
            &source_binding.base_tree,
            &authoring.join("binding-known-bad"),
            &known_bad_patch,
        );
        let manifest = CampaignManifest {
            schema: CAMPAIGN_SCHEMA_V1.to_owned(),
            campaign_id: CAMPAIGN_ID.to_owned(),
            source: source_binding,
            mutation_scope: MutationScope {
                allowlist: vec!["src".to_owned()],
                protected_paths: vec!["fixtures/mise".to_owned()],
            },
            candidate_material: Some(CandidateMaterialContract {
                kind: "git_change_set".to_owned(),
                protocol: GIT_CHANGE_SET_PROTOCOL_V1.to_owned(),
                media_type: GIT_CHANGE_SET_MEDIA_TYPE.to_owned(),
            }),
            adapter: AdapterBinding {
                name: "deterministic-dogfood".to_owned(),
                protocol: "papertiger-mise.adapter.v1".to_owned(),
                implementation_locator: "fixtures/mise/adapter.json".to_owned(),
                implementation_sha256: digest(adapter),
            },
            evaluator: EvaluatorBinding {
                argv: vec![
                    launcher_locator.clone(),
                    "fixtures/mise/deterministic_evaluator.rs".to_owned(),
                ],
                working_directory: ".".to_owned(),
                environment: BTreeMap::from([
                    ("MISE_PROTOCOL".to_owned(), "v1".to_owned()),
                    ("MISE_JUDGE_BUILD_TOOL".to_owned(), launcher_locator.clone()),
                ]),
                launcher_locator,
                launcher_sha256: launcher_sha256.clone(),
                evaluator_locator: "fixtures/mise/deterministic_evaluator.rs".to_owned(),
                evaluator_sha256: digest(&evaluator),
                fixture_bundle_locator: "fixtures/mise/bundle.json".to_owned(),
                fixture_bundle_sha256: digest(&bundle_bytes),
                protocol: "papertiger-mise.measurement.v1".to_owned(),
                rust_build_environment: None,
                judge_build: Some(JudgeBuildBinding {
                    argv: vec![
                        portable_absolute(&launcher),
                        "fixture-judge-build-v1".to_owned(),
                    ],
                    toolchain_name: "fixture-builder".to_owned(),
                    toolchain_version: "1".to_owned(),
                    toolchain_executable_locator: portable_absolute(&launcher),
                    toolchain_executable_sha256: launcher_sha256,
                    executable_locator: "judge.bin".to_owned(),
                    maximum_executable_bytes: 1024,
                }),
            },
            execution_limits: ExecutionLimits {
                workspace_root_locator: portable_absolute(&runs),
                runtime_root_locator: Some(portable_absolute(temporary.path())),
                maximum_trial_wall_time_ms: 5_000,
                maximum_trial_output_bytes: 8 * 1024,
                network: NetworkPolicy::Unrestricted,
            },
            objectives: vec![
                ObjectiveSpec {
                    key: "latency-ms".to_owned(),
                    role: ObjectiveRole::Primary,
                    direction: ObjectiveDirection::Minimize,
                    unit: "ms".to_owned(),
                    minimum_practical_change: 1.0,
                    regression_tolerance: 0.0,
                    acceptance_threshold: None,
                    target_value: None,
                },
                ObjectiveSpec {
                    key: "tests-pass".to_owned(),
                    role: ObjectiveRole::HardConstraint,
                    direction: ObjectiveDirection::Maximize,
                    unit: "boolean".to_owned(),
                    minimum_practical_change: 0.0,
                    regression_tolerance: 0.0,
                    acceptance_threshold: Some(1.0),
                    target_value: None,
                },
            ],
            paired_analysis: None,
            budgets: CumulativeBudgetCaps {
                caps: vec![
                    budget_limit(BudgetResource::Candidates, 12),
                    budget_limit(BudgetResource::Trials, 12),
                    budget_limit(BudgetResource::Failures, 4),
                    budget_limit(BudgetResource::HoldoutDisclosures, 8),
                    budget_limit(BudgetResource::WallTimeMilliseconds, 60_000),
                    budget_limit(BudgetResource::DiskBytesWritten, 16 * 1024 * 1024),
                    budget_limit(BudgetResource::ArtifactBytes, 2 * 1024 * 1024),
                ],
            },
            stop_rules: StopRules {
                not_before_unix_ms: 1,
                deadline_unix_ms: 4_000_000_000_000,
                max_consecutive_infrastructure_failures: 3,
                max_trials_without_qualified_improvement: 8,
                stop_on_evaluator_integrity_failure: true,
            },
            containment: ContainmentGrade::WorkspaceOnly,
            containment_requirement: None,
            holdouts: HoldoutPolicy {
                disclosure_cap: 8,
                tiers: vec![HoldoutTier {
                    key: "exploration".to_owned(),
                    kind: HoldoutTierKind::Exploration,
                    fixture_locator: "fixtures/mise/exploration.json".to_owned(),
                    fixture_sha256: digest(exploration_fixture),
                    maximum_disclosures: 8,
                    minimum_repetitions: 1,
                    disclosure: HoldoutDisclosure::Aggregate,
                }],
            },
            generation: GenerationBinding {
                runtime_generation: 1,
                recursion_depth: 0,
                maximum_recursion_depth: 3,
                outer_judge_executable_sha256: file_digest(&outer_judge),
                outer_judge_executable_locator: outer_judge_locator,
                proposal_policy_sha256: digest(policy_bytes),
                proposal_policy_locator: "policy.json".to_owned(),
                parent_campaign: None,
            },
            calibration: CalibrationRequirements {
                no_op: NoOpCalibration {
                    candidate_patch_sha256: None,
                    candidate_material_sha256: Some(digest(&no_op_material)),
                    fixture_locator: "fixtures/mise/calibration-no-op.json".to_owned(),
                    fixture_sha256: digest(no_op_fixture),
                    minimum_repetitions: 2,
                },
                known_bad: KnownBadCalibration {
                    candidate_patch_sha256: None,
                    candidate_material_sha256: Some(digest(&known_bad_material)),
                    fixture_locator: "fixtures/mise/calibration-known-bad.json".to_owned(),
                    fixture_sha256: digest(known_bad_fixture),
                    minimum_repetitions: 1,
                    expected_rejection_code: "known-regression".to_owned(),
                },
            },
        };
        manifest.validate().expect("dogfood manifest");
        let manifest_path = control.join("campaign.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).expect("manifest JSON"),
        )
        .expect("manifest file");
        init_git_repository(&control);

        let database = temporary.path().join("mise.sqlite");
        let connection = open_for_init(&database).expect("new Mise authority");
        init(&connection).expect("Mise schema");
        let verified = verify_campaign_admission(&manifest_path).expect("verified admission");
        admit_verified_campaign(&connection, ACTOR, &verified).expect("admit campaign");
        drop(connection);

        Self {
            _temporary: temporary,
            database,
            objects,
            runs,
            manifest,
        }
    }

    fn record_and_materialize(
        &self,
        connection: &rusqlite::Connection,
        key: &str,
        semantic_class: &str,
        patch: &[u8],
    ) -> papertiger_mise::BoundCandidate {
        let paths = if patch.is_empty() {
            BTreeSet::new()
        } else if patch
            .windows("src/note.txt".len())
            .any(|part| part == b"src/note.txt")
        {
            BTreeSet::from(["src/note.txt".to_owned()])
        } else {
            BTreeSet::from(["src/score.txt".to_owned()])
        };
        let proposal = CandidateProposal {
            campaign_id: CAMPAIGN_ID.to_owned(),
            parent_candidate_ids: BTreeSet::new(),
            base_commit: self.manifest.source.base_commit.clone(),
            base_tree: self.manifest.source.base_tree.clone(),
            proposer: "deterministic-dogfood-policy".to_owned(),
            proposal_policy_sha256: self.manifest.generation.proposal_policy_sha256.0.clone(),
            adapter_sha256: self.manifest.adapter.implementation_sha256.0.clone(),
            hypothesis: Hypothesis {
                mechanism: format!("exercise deterministic outcome {key}"),
                expected_effects: vec![format!("produce the {key} fixture outcome")],
                possible_regressions: vec!["objective or infrastructure failure".to_owned()],
                decisive_falsifiers: vec![
                    "frozen evaluator reports a different outcome".to_owned(),
                ],
            },
            changed_paths: paths.clone(),
            changed_symbols: paths,
            semantic_class: semantic_class.to_owned(),
            differentiator: None,
        };
        let material = material_from_patch(
            Path::new(&self.manifest.source.repository_locator),
            &self.manifest.source.base_commit,
            &self.manifest.source.base_tree,
            &self._temporary.path().join("material-authoring").join(key),
            patch,
        );
        let candidate = bind_candidate(proposal, material.clone()).expect("bind candidate");
        let reservation = format!("candidate-{key}");
        assert_eq!(
            reserve_budget(
                connection,
                ACTOR,
                CAMPAIGN_ID,
                &reservation,
                &[
                    budget_request(BudgetResource::Candidates, 1),
                    budget_request(
                        BudgetResource::ArtifactBytes,
                        u64::try_from(material.len()).expect("material length"),
                    ),
                ],
            )
            .expect("candidate reservation"),
            ReservationOutcome::Reserved
        );
        let object = preserve_object(&self.objects, &material).expect("candidate material object");
        assert!(
            record_candidate(
                connection,
                ACTOR,
                &self.objects,
                &reservation,
                &candidate,
                &object,
            )
            .expect("record candidate")
        );
        let materialization_reservation = format!("materialization-{key}");
        reserve_budget(
            connection,
            ACTOR,
            CAMPAIGN_ID,
            &materialization_reservation,
            &[
                budget_request(BudgetResource::DiskBytesWritten, 1024 * 1024),
                budget_request(BudgetResource::ArtifactBytes, 8 * 1024),
            ],
        )
        .expect("materialization reservation");
        materialize_candidate(
            connection,
            ACTOR,
            &self.objects,
            &materialization_reservation,
            &candidate.candidate_id,
            &self.runs.join(key),
        )
        .expect("materialize candidate");
        candidate
    }

    fn reserve_trial(
        &self,
        connection: &rusqlite::Connection,
        candidate_id: &str,
        trial_id: &str,
        tier: &str,
    ) -> SupervisedTrialSpec {
        let reservation_id = format!("trial-{trial_id}");
        let mut resources = vec![
            budget_request(BudgetResource::Trials, 1),
            budget_request(BudgetResource::Failures, 1),
            budget_request(BudgetResource::WallTimeMilliseconds, 5_000),
            budget_request(BudgetResource::DiskBytesWritten, 8 * 1024),
            budget_request(BudgetResource::ArtifactBytes, 32 * 1024),
        ];
        if !tier.starts_with("calibration.") {
            resources.push(budget_request(BudgetResource::HoldoutDisclosures, 1));
        }
        reserve_budget(connection, ACTOR, CAMPAIGN_ID, &reservation_id, &resources)
            .expect("trial reservation");
        SupervisedTrialSpec {
            trial_id: trial_id.to_owned(),
            campaign_id: CAMPAIGN_ID.to_owned(),
            candidate_id: candidate_id.to_owned(),
            reservation_id,
            tier: tier.to_owned(),
        }
    }

    fn run_success(
        &self,
        connection: &rusqlite::Connection,
        candidate_id: &str,
        trial_id: &str,
        tier: &str,
    ) -> anyhow::Result<papertiger_mise::SupervisedTrialOutcome> {
        let spec = self.reserve_trial(connection, candidate_id, trial_id, tier);
        match execute_workspace_trial(connection, ACTOR, &self.objects, &spec) {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let evidence_bytes = trial(connection, trial_id)
                    .ok()
                    .flatten()
                    .and_then(|trial| trial.outcome)
                    .and_then(|outcome| outcome.pointer("/absence_proof/evidence").cloned())
                    .and_then(|value| {
                        serde_json::from_value::<papertiger_mise::PreservedObject>(value).ok()
                    })
                    .and_then(|object| read_object(&self.objects, &object).ok())
                    .unwrap_or_default();
                let stderr = serde_json::from_slice::<serde_json::Value>(&evidence_bytes)
                    .ok()
                    .and_then(|body| body.get("stderr").cloned())
                    .and_then(|value| {
                        serde_json::from_value::<papertiger_mise::PreservedObject>(value).ok()
                    })
                    .and_then(|object| read_object(&self.objects, &object).ok())
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_else(|| "no retained stderr".to_owned());
                anyhow::bail!(
                    "{error:#}; retained evidence: {}; retained stderr: {stderr}",
                    String::from_utf8_lossy(&evidence_bytes)
                )
            }
        }
    }

    fn prove_successor_lineage(
        &self,
        connection: &rusqlite::Connection,
        nomination: &NominationRecord,
        promoted: &papertiger_mise::BoundCandidate,
    ) {
        const SUCCESSOR_ID: &str = "papertiger-mise-dogfood-successor-a01";
        let child_source = self._temporary.path().join("successor").join("source");
        std::fs::create_dir_all(child_source.parent().expect("successor source parent"))
            .expect("successor source parent directory");
        let clone = Command::new("git")
            .args(["-c", "core.autocrlf=false", "clone", "--no-hardlinks"])
            .arg(&self.manifest.source.repository_locator)
            .arg(&child_source)
            .output()
            .expect("clone parent source for successor");
        assert!(
            clone.status.success(),
            "successor source clone failed: {}",
            String::from_utf8_lossy(&clone.stderr)
        );
        git(&child_source, &["config", "user.name", "Mise Dogfood"]);
        git(
            &child_source,
            &["config", "user.email", "mise-dogfood@example.invalid"],
        );
        git(&child_source, &["config", "core.autocrlf", "false"]);
        apply_material_to_repository(&child_source, &promoted.material_bytes);
        git(
            &child_source,
            &["commit", "-m", "integrate nominated parent candidate"],
        );
        let source_binding =
            inspect_source_binding(&child_source).expect("successor source binding");
        assert_eq!(
            source_binding.base_tree,
            git_text(&self.runs.join("improved"), &["write-tree"]),
            "integrated successor tree must equal the nominated materialization tree"
        );

        let child_runs = self._temporary.path().join("successor-runs");
        std::fs::create_dir_all(&child_runs).expect("successor run root");
        let mut successor = self.manifest.clone();
        successor.campaign_id = SUCCESSOR_ID.to_owned();
        successor.source = source_binding;
        successor.execution_limits.workspace_root_locator = portable_absolute(&child_runs);
        successor.budgets = CumulativeBudgetCaps {
            caps: vec![
                budget_limit(BudgetResource::Candidates, 3),
                budget_limit(BudgetResource::Trials, 4),
                budget_limit(BudgetResource::Failures, 2),
                budget_limit(BudgetResource::HoldoutDisclosures, 1),
                budget_limit(BudgetResource::WallTimeMilliseconds, 20_000),
                budget_limit(BudgetResource::DiskBytesWritten, 2 * 1024 * 1024),
                budget_limit(BudgetResource::ArtifactBytes, 256 * 1024),
            ],
        };
        successor.holdouts.disclosure_cap = 1;
        successor.holdouts.tiers[0].maximum_disclosures = 1;
        successor
            .stop_rules
            .max_trials_without_qualified_improvement = 4;
        successor.generation.runtime_generation = 2;
        successor.generation.recursion_depth = 1;
        successor.generation.parent_campaign = Some(ParentCampaignBinding {
            campaign_id: self.manifest.campaign_id.clone(),
            manifest_sha256: Sha256Digest(self.manifest.sha256().expect("parent manifest digest")),
            runtime_generation: 1,
            recursion_depth: 0,
        });
        let improved_trial = trial(connection, "improved-1")
            .expect("improved trial query")
            .expect("improved trial");
        let built_judge = PathBuf::from(
            improved_trial
                .environment
                .get("PAPERTIGER_MISE_JUDGE_BUILD_ROOT")
                .and_then(serde_json::Value::as_str)
                .expect("runtime-owned judge-build root"),
        )
        .join("judge.bin");
        successor.generation.outer_judge_executable_locator = portable_absolute(&built_judge);
        successor.generation.outer_judge_executable_sha256 = file_digest(&built_judge);
        successor.validate().expect("successor manifest");

        let mut wrong_judge = successor.clone();
        wrong_judge.generation.outer_judge_executable_sha256 = digest(b"operator rebuild");
        let error = derive_parent_promotion_proof(
            connection,
            &self.objects,
            &nomination.nomination_id,
            &wrong_judge,
        )
        .expect_err("operator-selected successor judge must not replace the parent build");
        assert!(
            error
                .to_string()
                .contains("successor outer judge differs from parent candidate trial"),
            "{error:#}"
        );

        let trial_receipt_object: papertiger_mise::PreservedObject = serde_json::from_value(
            improved_trial
                .outcome
                .expect("improved outcome")
                .get("receipt")
                .cloned()
                .expect("trial receipt pointer"),
        )
        .expect("typed trial receipt pointer");
        let trial_receipt: papertiger_mise::TrialReceipt = serde_json::from_slice(
            &read_object(&self.objects, &trial_receipt_object).expect("trial receipt CAS"),
        )
        .expect("typed trial receipt");
        let built_executable = trial_receipt
            .judge_build
            .expect("v3 trial judge build")
            .executable;
        let built_bytes = read_object(&self.objects, &built_executable).expect("built judge CAS");
        std::fs::remove_file(self.objects.join(&built_executable.locator))
            .expect("remove scratch judge CAS object");
        let error = derive_parent_promotion_proof(
            connection,
            &self.objects,
            &nomination.nomination_id,
            &successor,
        )
        .expect_err("missing parent-built judge object must fail closed");
        assert!(
            format!("{error:#}").contains("judge executable CAS object failed exact identity"),
            "{error:#}"
        );
        assert_eq!(
            preserve_object(&self.objects, &built_bytes).expect("restore scratch judge CAS object"),
            built_executable
        );

        let child_control = self._temporary.path().join("successor-control");
        std::fs::create_dir_all(&child_control).expect("successor control root");
        std::fs::write(
            child_control.join("policy.json"),
            br#"{"schema":"papertiger-mise.proposal-policy.v1","maximum_candidates":12}"#,
        )
        .expect("successor proposal policy");
        let manifest_path = child_control.join("campaign.json");
        let manifest_bytes =
            serde_json::to_vec_pretty(&successor).expect("successor manifest JSON");
        std::fs::write(&manifest_path, &manifest_bytes).expect("successor manifest file");
        init_git_repository(&child_control);
        verify_campaign_admission(&manifest_path).expect("successor admission preflight");

        let mut wrong_tree = successor.clone();
        wrong_tree.source.base_tree = self.manifest.source.base_tree.clone();
        let error = derive_parent_promotion_proof(
            connection,
            &self.objects,
            &nomination.nomination_id,
            &wrong_tree,
        )
        .expect_err("wrong integrated tree must fail");
        assert!(
            error.to_string().contains("exact nominated tree"),
            "{error:#}"
        );

        let mut depth_drift = successor.clone();
        depth_drift.generation.recursion_depth = 2;
        let error = derive_parent_promotion_proof(
            connection,
            &self.objects,
            &nomination.nomination_id,
            &depth_drift,
        )
        .expect_err("depth drift must fail");
        assert!(error.to_string().contains("exactly one less"), "{error:#}");
        let mut generation_drift = successor.clone();
        generation_drift.generation.runtime_generation = 3;
        let error = derive_parent_promotion_proof(
            connection,
            &self.objects,
            &nomination.nomination_id,
            &generation_drift,
        )
        .expect_err("generation drift must fail");
        assert!(error.to_string().contains("exactly one after"), "{error:#}");

        let proof = derive_parent_promotion_proof(
            connection,
            &self.objects,
            &nomination.nomination_id,
            &successor,
        )
        .expect("derive parent proof");
        let binding = PromotionGateBinding {
            task_seq: 1,
            gate_name: "successor-admission".to_owned(),
            evidence_locator: proof.evidence_locator().expect("proof locator"),
            evidence_sha256: proof.sha256().expect("proof digest"),
        };
        let missing_error = verify_parent_promotion_gate(
            connection,
            &self.objects,
            &self
                ._temporary
                .path()
                .join("not-yet-created-papertiger.sqlite"),
            &nomination.nomination_id,
            &successor,
            &binding,
        )
        .expect_err("missing proof object must fail before gate inspection");
        assert!(
            missing_error
                .to_string()
                .contains("promotion derive-parent"),
            "{missing_error:#}"
        );

        let preserved = preserve_parent_promotion_proof(
            connection,
            &self.objects,
            &nomination.nomination_id,
            &successor,
        )
        .expect("preserve parent proof");
        assert_eq!(preserved.proof, proof);
        let proof_path = self.objects.join(&preserved.object.locator);
        let exact_proof_bytes = std::fs::read(&proof_path).expect("retained proof bytes");
        let mut corrupted = exact_proof_bytes.clone();
        corrupted[0] ^= 1;
        std::fs::write(&proof_path, &corrupted).expect("inject scratch CAS corruption");
        let error = verify_parent_promotion_gate(
            connection,
            &self.objects,
            &self
                ._temporary
                .path()
                .join("not-yet-created-papertiger.sqlite"),
            &nomination.nomination_id,
            &successor,
            &binding,
        )
        .expect_err("corrupt proof object must fail exact identity");
        assert!(format!("{error:#}").contains("exact identity"), "{error:#}");
        std::fs::write(&proof_path, &exact_proof_bytes).expect("restore scratch proof object");

        let planner_path = self._temporary.path().join("papertiger.sqlite");
        let planner = papertiger::open_for_init(
            planner_path
                .to_str()
                .expect("portable test planner database path"),
        )
        .expect("open planner authority");
        papertiger::init(&planner).expect("planner schema");
        let plan = papertiger::add_plan(
            &planner,
            "independent-operator",
            "lineage-fixture",
            "Lineage fixture",
            "Independent successor authority",
        )
        .expect("planner plan");
        let task = papertiger::add_task(
            &planner,
            "independent-operator",
            plan,
            "Authorize exact successor",
            "Review one exact parent promotion proof",
            None,
            &[],
            &[],
            0,
            None,
            None,
        )
        .expect("planner task");
        papertiger::add_gate(
            &planner,
            "independent-operator",
            task,
            "successor-admission",
            "other",
            "Authorize the exact parent promotion proof",
        )
        .expect("planner gate");
        papertiger::start_task(&planner, "independent-operator", task, None)
            .expect("start planner task");
        papertiger::close_gate(
            &planner,
            "independent-operator",
            task,
            "successor-admission",
            &preserved.evidence_locator,
            Some(&preserved.object.sha256),
            Some("independently reviewed fixture proof"),
        )
        .expect("close exact planner gate");
        drop(planner);
        let exact_binding = PromotionGateBinding {
            task_seq: task,
            gate_name: "successor-admission".to_owned(),
            evidence_locator: preserved.evidence_locator.clone(),
            evidence_sha256: preserved.object.sha256.clone(),
        };

        let mut mismatched_binding = exact_binding.clone();
        mismatched_binding.evidence_sha256 = "9".repeat(64);
        let error = verify_parent_promotion_gate(
            connection,
            &self.objects,
            &planner_path,
            &nomination.nomination_id,
            &successor,
            &mismatched_binding,
        )
        .expect_err("mismatched independent gate must fail");
        assert!(error.to_string().contains("exact re-derived"), "{error:#}");
        verify_parent_promotion_gate(
            connection,
            &self.objects,
            &planner_path,
            &nomination.nomination_id,
            &successor,
            &exact_binding,
        )
        .expect("verify exact independent gate");

        let verified = verify_successor_admission(
            connection,
            &manifest_path,
            &self.objects,
            &planner_path,
            &nomination.nomination_id,
            &exact_binding,
        )
        .expect("verified successor admission");
        let balances_before = budget_balances(connection, CAMPAIGN_ID).expect("parent balances");
        let mut dirty_manifest = manifest_bytes.clone();
        dirty_manifest.push(b' ');
        std::fs::write(&manifest_path, &dirty_manifest).expect("inject manifest TOCTOU drift");
        let error = admit_verified_successor(connection, ACTOR, &verified)
            .expect_err("manifest drift after preflight must fail");
        assert!(error.to_string().contains("clean"), "{error:#}");
        std::fs::write(&manifest_path, &manifest_bytes).expect("restore successor manifest");
        assert!(
            campaign(connection, SUCCESSOR_ID)
                .expect("child campaign query")
                .is_none()
        );
        assert_eq!(
            budget_balances(connection, CAMPAIGN_ID).expect("unchanged parent balances"),
            balances_before
        );

        let planner = papertiger::open_existing(
            planner_path
                .to_str()
                .expect("portable test planner database path"),
        )
        .expect("reopen planner authority");
        papertiger::reopen_gate(
            &planner,
            "independent-operator",
            task,
            "successor-admission",
            "exercise gate TOCTOU refusal",
        )
        .expect("reopen gate");
        let error = admit_verified_successor(connection, ACTOR, &verified)
            .expect_err("gate reopened after preflight must fail");
        assert!(
            error.to_string().contains("does not exactly authorize"),
            "{error:#}"
        );
        assert_eq!(
            budget_balances(connection, CAMPAIGN_ID).expect("unchanged parent balances"),
            balances_before
        );
        papertiger::close_gate(
            &planner,
            "independent-operator",
            task,
            "successor-admission",
            &preserved.evidence_locator,
            Some(&preserved.object.sha256),
            Some("reclosed after explicit TOCTOU test"),
        )
        .expect("reclose exact gate");
        drop(planner);

        let fresh = verify_successor_admission(
            connection,
            &manifest_path,
            &self.objects,
            &planner_path,
            &nomination.nomination_id,
            &exact_binding,
        )
        .expect("fresh verified successor admission");
        assert_eq!(
            admit_verified_successor(connection, ACTOR, &fresh).expect("admit successor"),
            AdmissionOutcome::Admitted
        );
        let record = successor_admission(connection, SUCCESSOR_ID)
            .expect("show successor")
            .expect("durable successor receipt");
        assert_eq!(record.parent_nomination_id, nomination.nomination_id);
        assert_eq!(
            record.parent_promotion_proof_sha256,
            preserved.object.sha256
        );
        let balances_after = budget_balances(connection, CAMPAIGN_ID).expect("debited balances");
        assert_eq!(
            admit_verified_successor(connection, ACTOR, &fresh).expect("successor replay"),
            AdmissionOutcome::Existing
        );
        assert_eq!(
            budget_balances(connection, CAMPAIGN_ID).expect("replayed balances"),
            balances_after,
            "successor replay must not debit the parent twice"
        );

        let planner = papertiger::open_existing(
            planner_path
                .to_str()
                .expect("portable test planner database path"),
        )
        .expect("reopen planner for divergent replay");
        papertiger::reopen_gate(
            &planner,
            "independent-operator",
            task,
            "successor-admission",
            "prove divergent gate replay is refused",
        )
        .expect("reopen admitted gate");
        std::thread::sleep(Duration::from_millis(2));
        papertiger::close_gate(
            &planner,
            "independent-operator",
            task,
            "successor-admission",
            &preserved.evidence_locator,
            Some(&preserved.object.sha256),
            Some("same proof, different gate closure identity"),
        )
        .expect("close divergent replay gate");
        drop(planner);
        let divergent = verify_successor_admission(
            connection,
            &manifest_path,
            &self.objects,
            &planner_path,
            &nomination.nomination_id,
            &exact_binding,
        )
        .expect("verify divergent successor replay");
        let error = admit_verified_successor(connection, ACTOR, &divergent)
            .expect_err("changed gate closure must not rewrite an admitted successor");
        assert!(
            error.to_string().contains("different immutable successor"),
            "{error:#}"
        );
        assert_eq!(
            budget_balances(connection, CAMPAIGN_ID).expect("divergent replay balances"),
            balances_after,
            "divergent replay must not debit the parent"
        );
    }

    fn interrupt_and_recover(&self, connection: &rusqlite::Connection, spec: &SupervisedTrialSpec) {
        let mut supervisor = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "interrupted_trial_worker", "--nocapture"])
            .env("PAPERTIGER_MISE_DOGFOOD_WORKER_DB", &self.database)
            .env("PAPERTIGER_MISE_DOGFOOD_WORKER_OBJECTS", &self.objects)
            .env(
                "PAPERTIGER_MISE_DOGFOOD_WORKER_SPEC",
                serde_json::to_string(spec).expect("worker spec JSON"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn nested trial supervisor");
        let durable = (0..400).find_map(|_| {
            let current = trial(connection, &spec.trial_id).ok().flatten();
            if current
                .as_ref()
                .is_some_and(|trial| trial.status == papertiger_mise::TrialStatus::Launched)
            {
                current
            } else {
                std::thread::sleep(Duration::from_millis(25));
                None
            }
        });
        let durable = match durable {
            Some(durable) => durable,
            None => {
                let _ = supervisor.kill();
                let output = supervisor
                    .wait_with_output()
                    .expect("collect failed worker diagnostics");
                panic!(
                    "worker must durably launch evaluator; status={}; stdout={}; stderr={}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        };
        let expected_birth = durable
            .process_birth_identity
            .clone()
            .expect("durable evaluator birth identity");
        supervisor.kill().expect("kill trial supervisor");
        supervisor
            .wait_with_output()
            .expect("reap trial supervisor");
        let outcome =
            match recover_workspace_trial(connection, ACTOR, &self.objects, &spec.trial_id) {
                Ok(outcome) => Some(outcome),
                Err(error) if error.to_string().contains("still owns its exact live") => {
                    // Windows Job Objects kill the family when the abruptly stopped
                    // supervisor closes its last handle. Unix process groups remain
                    // recoverable by identity until the controlled fixture exits.
                    // Both paths must converge on the same durable absence proof.
                    signal_fixture_exit(&durable.working_directory);
                    (0..200).find_map(|_| {
                        match recover_workspace_trial(
                            connection,
                            ACTOR,
                            &self.objects,
                            &spec.trial_id,
                        ) {
                            Ok(outcome) => Some(outcome),
                            Err(error)
                                if error.to_string().contains("still owns its exact live") =>
                            {
                                std::thread::sleep(Duration::from_millis(25));
                                None
                            }
                            Err(error) => panic!("unexpected cold-recovery error: {error:#}"),
                        }
                    })
                }
                Err(error) => panic!("unexpected cold-recovery error: {error:#}"),
            };
        assert_eq!(outcome, Some(ColdRecoveryOutcome::Reconciled));
        let recovered = trial(connection, &spec.trial_id)
            .expect("recovered trial query")
            .expect("recovered trial");
        let absence_object: papertiger_mise::PreservedObject = serde_json::from_value(
            recovered
                .outcome
                .expect("recovered outcome")
                .pointer("/absence_proof/evidence")
                .cloned()
                .expect("absence evidence pointer"),
        )
        .expect("absence evidence object");
        let absence: serde_json::Value = serde_json::from_slice(
            &read_object(&self.objects, &absence_object).expect("absence evidence bytes"),
        )
        .expect("absence evidence JSON");
        assert_eq!(
            absence["schema"],
            "papertiger-mise.process-absence-evidence.v1"
        );
        assert_eq!(absence["expected_process_birth_identity"], expected_birth);
        assert!(matches!(
            absence
                .pointer("/observation/state")
                .and_then(serde_json::Value::as_str),
            Some("absent" | "exited")
        ));
    }
}

fn replace_score_patch(old: &str, new: &str) -> Vec<u8> {
    format!(
        "diff --git a/src/score.txt b/src/score.txt\n--- a/src/score.txt\n+++ b/src/score.txt\n@@ -1 +1 @@\n-{old}\n+{new}\n"
    )
    .into_bytes()
}

fn replace_note_patch(old: &str, new: &str) -> Vec<u8> {
    format!(
        "diff --git a/src/note.txt b/src/note.txt\n--- a/src/note.txt\n+++ b/src/note.txt\n@@ -1 +1 @@\n-{old}\n+{new}\n"
    )
    .into_bytes()
}

fn fixture_entry(key: &str, locator: &str, bytes: &[u8]) -> FixtureBundleEntry {
    FixtureBundleEntry {
        key: key.to_owned(),
        locator: locator.to_owned(),
        sha256: digest(bytes),
    }
}

fn budget_limit(resource: BudgetResource, hard_limit: u64) -> BudgetLimit {
    BudgetLimit::new(resource, hard_limit).expect("budget limit")
}

fn budget_request(resource: BudgetResource, amount: u64) -> BudgetRequest {
    BudgetRequest::new(resource, amount).expect("budget request")
}

fn digest(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest(papertiger_mise::sha256(bytes))
}

fn file_digest(path: &Path) -> Sha256Digest {
    digest(&std::fs::read(path).expect("artifact bytes"))
}

fn portable_absolute(path: &Path) -> String {
    let canonical = canonical_absolute(path);
    let text = canonical.to_string_lossy();
    text.strip_prefix(r"\\?\")
        .unwrap_or(&text)
        .replace('\\', "/")
}

fn canonical_absolute(path: impl AsRef<Path>) -> PathBuf {
    std::fs::canonicalize(path.as_ref()).expect("canonical path")
}

fn init_git_repository(repository: &Path) {
    git(repository, &["init"]);
    git(repository, &["config", "user.name", "Mise Dogfood"]);
    git(
        repository,
        &["config", "user.email", "mise-dogfood@example.invalid"],
    );
    git(repository, &["config", "core.autocrlf", "false"]);
    git(repository, &["add", "."]);
    git(repository, &["commit", "-m", "frozen dogfood fixture"]);
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("launch Git");
    assert!(
        output.status.success(),
        "Git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_apply(repository: &Path, patch: &[u8]) {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["apply", "--index", "--whitespace=nowarn", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch Git apply");
    child
        .stdin
        .take()
        .expect("Git apply stdin")
        .write_all(patch)
        .expect("write Git patch");
    let output = child.wait_with_output().expect("wait for Git apply");
    assert!(
        output.status.success(),
        "Git apply failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn material_from_patch(
    repository: &Path,
    base_commit: &str,
    base_tree: &str,
    worktree: &Path,
    patch: &[u8],
) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["worktree", "add", "--detach"])
        .arg(worktree)
        .arg(base_commit)
        .output()
        .expect("create material authoring worktree");
    assert!(
        output.status.success(),
        "material authoring worktree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    if !patch.is_empty() {
        git_apply(worktree, patch);
    }
    let result_tree = git_text(worktree, &["write-tree"]);
    build_git_change_set_material(repository, base_tree, &result_tree)
        .expect("build typed candidate material")
}

fn apply_material_to_repository(repository: &Path, bytes: &[u8]) {
    let material = CandidateMaterial::parse_canonical(bytes).expect("typed candidate material");
    for change in material.change_set.changes {
        let path = repository.join(&change.path);
        match change.operation {
            GitChangeOperation::Delete => {
                std::fs::remove_file(&path).expect("delete material path")
            }
            GitChangeOperation::Add | GitChangeOperation::Modify => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("candidate material parent");
                }
                std::fs::write(
                    &path,
                    change
                        .new
                        .expect("new material state")
                        .bytes()
                        .expect("material bytes"),
                )
                .expect("write material path");
            }
        }
    }
    git(repository, &["add", "-A"]);
}

fn git_text(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("launch Git query");
    assert!(
        output.status.success(),
        "Git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output UTF-8")
        .trim()
        .to_owned()
}

fn trusted_policy() -> TrustedContainmentPolicy {
    let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    TrustedContainmentPolicy {
        schema: TRUSTED_CONTAINMENT_POLICY_SCHEMA_V2.to_owned(),
        protocol: SEALED_ATTESTATION_PROTOCOL_V2.to_owned(),
        issuer_identity: "dogfood-operator".to_owned(),
        public_key_ed25519: lower_hex(&signing_key.verifying_key().to_bytes()),
        executor_sha256: "a".repeat(64),
        profile_sha256: "b".repeat(64),
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("hex");
    }
    output
}

fn signal_fixture_exit(working_directory: &str) {
    std::fs::write(
        Path::new(working_directory).join(".papertiger-mise-test-exit"),
        b"exit\n",
    )
    .expect("signal controlled evaluator fixture to exit");
}
