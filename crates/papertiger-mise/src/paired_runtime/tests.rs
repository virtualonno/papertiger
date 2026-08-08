use super::*;
use std::collections::BTreeSet;

use crate::digest::sha256;

use crate::budget::BudgetResource;
use crate::git_materialization::{git_run, git_text};
use crate::lifecycle::record_artifact_in;
use crate::path_identity::canonical_or_pending_absolute;
use crate::statistics::{paired_schedule_sha256, tests as statistic_fixtures};
use crate::store::{CampaignAdmission, admit_campaign, init};
use tempfile::tempdir;

const ACTOR: &str = "paired-runtime-test";
const NO_OP_SEED: &[u8] = b"mise-test-no-op-order-seed-000001";
const KNOWN_BAD_SEED: &[u8] = b"mise-test-known-bad-seed-0000001";
const RESEARCH_SEED: &[u8] = b"mise-test-research-order-seed-0001";

#[test]
fn local_runtime_refuses_a_sealed_campaign() {
    let fixture = Fixture::new_with_containment(ContainmentGrade::Sealed);
    let spec = fixture.spec(
        "paired-sealed-no-op",
        &fixture.baseline_id,
        PairedCohort::NoOpCalibration,
        NO_OP_SEED,
        "paired-sealed-budget",
    );
    let error = prepare_paired_cohort(&fixture.connection, ACTOR, fixture.objects.path(), &spec)
        .expect_err("local execution cannot impersonate a sealed worker");
    assert!(error.to_string().contains("attested-worker"), "{error:#}");
}

#[test]
fn persisted_calibrations_and_research_adjudicate_only_from_reopened_receipts() {
    let fixture = Fixture::new();
    let no_op = fixture.spec(
        "paired-no-op",
        &fixture.baseline_id,
        PairedCohort::NoOpCalibration,
        NO_OP_SEED,
        "paired-no-op-budget",
    );
    prepare_paired_cohort(&fixture.connection, ACTOR, fixture.objects.path(), &no_op)
        .expect("prepare no-op cohort");
    let no_op_runs = paired_runs(&fixture.connection, &no_op.cohort_id).unwrap();
    assert_eq!(no_op_runs.len(), 16);
    assert!(
        no_op_runs
            .iter()
            .all(|run| run.status == PairedRunStatus::Prepared)
    );

    let first = &no_op_runs[0];
    let active_identity = match observe_process(std::process::id()).expect("current identity") {
        ProcessObservation::Active {
            process_birth_identity,
        }
        | ProcessObservation::Exited {
            process_birth_identity,
        } => process_birth_identity,
        ProcessObservation::Absent => panic!("test process cannot be absent"),
    };
    mark_paired_run_launched(
        &fixture.connection,
        ACTOR,
        &no_op.cohort_id,
        &first.execution_id,
        std::process::id(),
        &active_identity,
    )
    .expect("bind simulated interrupted run");
    let replay = execute_next_paired_run(
        &fixture.connection,
        ACTOR,
        fixture.objects.path(),
        &no_op.cohort_id,
    )
    .expect_err("launched run cannot replay");
    assert!(replay.to_string().contains("paired recover"));
    let recovery = recover_paired_run(
        &fixture.connection,
        ACTOR,
        fixture.objects.path(),
        &first.execution_id,
    )
    .expect_err("live exact process cannot be recovered absent");
    assert!(recovery.to_string().contains("still owns a live process"));

    fixture.complete_all(&no_op.cohort_id, CalibrationMode::NoOp);
    let (record, adjudication) = adjudicate_paired_cohort(
        &fixture.connection,
        ACTOR,
        fixture.objects.path(),
        &no_op.cohort_id,
    )
    .expect("adjudicate no-op from CAS");
    assert_eq!(record.status, PairedCohortStatus::Calibrated);
    assert!(matches!(
        adjudication,
        PairedCohortAdjudication::NoOpCalibration(NoOpCalibrationResult { passed: true, .. })
    ));

    let known_bad = fixture.spec(
        "paired-known-bad",
        &fixture.known_bad_id,
        PairedCohort::KnownBadCalibration,
        KNOWN_BAD_SEED,
        "paired-known-bad-budget",
    );
    prepare_paired_cohort(
        &fixture.connection,
        ACTOR,
        fixture.objects.path(),
        &known_bad,
    )
    .expect("prepare known-bad cohort");
    fixture.complete_all(&known_bad.cohort_id, CalibrationMode::KnownBad);
    let (record, adjudication) = adjudicate_paired_cohort(
        &fixture.connection,
        ACTOR,
        fixture.objects.path(),
        &known_bad.cohort_id,
    )
    .expect("adjudicate known-bad from CAS");
    assert_eq!(record.status, PairedCohortStatus::Rejected);
    assert!(matches!(
        adjudication,
        PairedCohortAdjudication::KnownBadCalibration(PairedClassification {
            disposition: PairedDisposition::Rejected,
            ..
        })
    ));

    fixture.reserve_research_slot();
    let research = fixture.spec(
        "paired-research",
        &fixture.research_id,
        PairedCohort::Research {
            candidate_analysis_slot: 1,
        },
        RESEARCH_SEED,
        "paired-research-budget",
    );
    prepare_paired_cohort(
        &fixture.connection,
        ACTOR,
        fixture.objects.path(),
        &research,
    )
    .expect("prepare research cohort");
    fixture.complete_all(&research.cohort_id, CalibrationMode::Research);
    let (record, adjudication) = adjudicate_paired_cohort(
        &fixture.connection,
        ACTOR,
        fixture.objects.path(),
        &research.cohort_id,
    )
    .expect("adjudicate research from CAS");
    assert_eq!(record.status, PairedCohortStatus::Qualified);
    assert!(matches!(
        adjudication,
        PairedCohortAdjudication::Research(PairedClassification {
            disposition: PairedDisposition::Qualified,
            ..
        })
    ));
    let nomination_spec = DerivePairedNominationSpec {
        research_cohort_id: research.cohort_id.clone(),
        no_op_cohort_id: no_op.cohort_id.clone(),
        known_bad_cohort_id: known_bad.cohort_id.clone(),
    };
    let nomination = derive_paired_nomination(
        &fixture.connection,
        ACTOR,
        fixture.objects.path(),
        &nomination_spec,
    )
    .expect("derive paired nomination from exact reopened cohorts");
    let verified = crate::lifecycle::verify_nomination_integrity(
        &fixture.connection,
        fixture.objects.path(),
        &nomination.nomination_id,
    )
    .expect("rederive paired nomination from CAS");
    assert_eq!(
        verified.evidence_grade,
        EvidenceGrade::WorkspaceOnlyDevelopment
    );
    assert!(verified.relied_upon_trial_ids.is_empty());
    assert_eq!(
        verified.relied_upon_paired_cohort_ids,
        vec![
            no_op.cohort_id.clone(),
            known_bad.cohort_id.clone(),
            research.cohort_id.clone(),
        ]
    );
    assert_eq!(
        derive_paired_nomination(
            &fixture.connection,
            ACTOR,
            fixture.objects.path(),
            &nomination_spec,
        )
        .expect("paired nomination derivation is idempotent"),
        nomination
    );
    let wrong_calibration = derive_paired_nomination(
        &fixture.connection,
        ACTOR,
        fixture.objects.path(),
        &DerivePairedNominationSpec {
            research_cohort_id: research.cohort_id.clone(),
            no_op_cohort_id: no_op.cohort_id.clone(),
            known_bad_cohort_id: no_op.cohort_id.clone(),
        },
    )
    .expect_err("a no-op cohort cannot impersonate known-bad authority");
    assert!(
        wrong_calibration
            .to_string()
            .contains("known-bad calibration"),
        "{wrong_calibration:#}"
    );

    let domain_receipts: i64 = fixture
        .connection
        .query_row(
            "SELECT COUNT(*) FROM paired_live_domain_receipts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(domain_receipts, 48);
    assert_eq!(
        paired_cohorts(&fixture.connection, &fixture.manifest.campaign_id)
            .unwrap()
            .len(),
        3
    );
    let balances =
        crate::budget::budget_balances(&fixture.connection, &fixture.manifest.campaign_id).unwrap();
    for balance in &balances {
        assert_eq!(balance.reserved_amount, 0, "{:?}", balance.resource);
    }
    assert_eq!(
        balances
            .iter()
            .find(|balance| balance.resource == BudgetResource::Trials)
            .unwrap()
            .spent_amount,
        48
    );
    assert_eq!(
        balances
            .iter()
            .find(|balance| balance.resource == BudgetResource::HoldoutDisclosures)
            .unwrap()
            .spent_amount,
        16
    );
    assert_eq!(
        balances
            .iter()
            .find(|balance| balance.resource == BudgetResource::Failures)
            .unwrap()
            .spent_amount,
        0
    );
}

#[test]
fn complete_cohort_with_missing_cas_result_fails_terminally_without_a_wedge() {
    let fixture = Fixture::new();
    let spec = fixture.spec(
        "paired-corrupt-no-op",
        &fixture.baseline_id,
        PairedCohort::NoOpCalibration,
        NO_OP_SEED,
        "paired-corrupt-budget",
    );
    prepare_paired_cohort(&fixture.connection, ACTOR, fixture.objects.path(), &spec)
        .expect("prepare corruption fixture");
    fixture.complete_all(&spec.cohort_id, CalibrationMode::NoOp);
    let runs = paired_runs(&fixture.connection, &spec.cohort_id).unwrap();
    let result = indexed_object(
        &fixture.connection,
        runs[0]
            .adapter_result_sha256
            .as_deref()
            .expect("successful run result"),
    )
    .unwrap();
    std::fs::remove_file(fixture.objects.path().join(&result.locator))
        .expect("simulate missing CAS bytes");

    let error = adjudicate_paired_cohort(
        &fixture.connection,
        ACTOR,
        fixture.objects.path(),
        &spec.cohort_id,
    )
    .expect_err("missing result cannot remain a running cohort");
    assert!(error.to_string().contains("integrity_failed"), "{error:#}");
    let cohort = paired_cohort(&fixture.connection, &spec.cohort_id)
        .unwrap()
        .unwrap();
    assert_eq!(cohort.status, PairedCohortStatus::IntegrityFailed);
    assert_eq!(
        cohort.reason_code.as_deref(),
        Some(PairedCohortReasonCode::AdjudicationEvidenceInvalid.as_str())
    );
    let balances =
        crate::budget::budget_balances(&fixture.connection, &fixture.manifest.campaign_id).unwrap();
    assert!(balances.iter().all(|balance| balance.reserved_amount == 0));
    assert_eq!(
        balances
            .iter()
            .find(|balance| balance.resource == BudgetResource::Failures)
            .unwrap()
            .spent_amount,
        1
    );
}

#[derive(Clone, Copy)]
enum CalibrationMode {
    NoOp,
    KnownBad,
    Research,
}

struct Fixture {
    connection: Connection,
    _repository_owner: tempfile::TempDir,
    objects: tempfile::TempDir,
    manifest: CampaignManifest,
    baseline_id: String,
    known_bad_id: String,
    research_id: String,
}

impl Fixture {
    fn new() -> Self {
        Self::new_with_containment(ContainmentGrade::WorkspaceOnly)
    }

    fn new_with_containment(containment: ContainmentGrade) -> Self {
        let connection = Connection::open_in_memory().expect("database");
        init(&connection).expect("schema");
        let repository_owner = tempdir().expect("repository owner");
        let source = repository_owner.path().join("source");
        let runs = repository_owner.path().join("runs");
        std::fs::create_dir_all(source.join("src")).expect("source directory");
        std::fs::create_dir_all(&runs).expect("workspace root");
        git_run(&source, &["init"], None, None, None).expect("initialize source repository");
        git_run(
            &source,
            &["config", "user.name", "Mise Paired Test"],
            None,
            None,
            None,
        )
        .expect("configure Git name");
        git_run(
            &source,
            &["config", "user.email", "mise-paired@example.invalid"],
            None,
            None,
            None,
        )
        .expect("configure Git email");
        git_run(
            &source,
            &["config", "core.autocrlf", "false"],
            None,
            None,
            None,
        )
        .expect("disable line-ending conversion");
        std::fs::write(source.join("src/fixture.rs"), b"old\n").expect("base fixture");
        git_run(&source, &["add", "."], None, None, None).expect("stage base fixture");
        git_run(
            &source,
            &["commit", "-m", "frozen paired base"],
            None,
            None,
            None,
        )
        .expect("commit paired base");

        const KNOWN_BAD_PATCH: &[u8] = b"diff --git a/src/fixture.rs b/src/fixture.rs\n--- a/src/fixture.rs\n+++ b/src/fixture.rs\n@@ -1 +1 @@\n-old\n+known-bad\n";
        const RESEARCH_PATCH: &[u8] = b"diff --git a/src/fixture.rs b/src/fixture.rs\n--- a/src/fixture.rs\n+++ b/src/fixture.rs\n@@ -1 +1 @@\n-old\n+improved\n";
        let mut manifest = crate::manifest::tests::valid_manifest();
        manifest.campaign_id = match containment {
            ContainmentGrade::WorkspaceOnly => "paired-runtime-campaign",
            ContainmentGrade::Sealed => "paired-runtime-sealed-campaign",
        }
        .to_owned();
        manifest.source.repository_locator =
            canonical_or_pending_absolute(&source).expect("source repository locator");
        manifest.source.base_commit = git_text(&source, &["rev-parse", "HEAD^{commit}"])
            .expect("source commit")
            .trim()
            .to_owned();
        manifest.source.base_tree = git_text(&source, &["rev-parse", "HEAD^{tree}"])
            .expect("source tree")
            .trim()
            .to_owned();
        manifest.execution_limits.workspace_root_locator =
            canonical_or_pending_absolute(&runs).expect("workspace root locator");
        manifest
            .calibration
            .known_bad
            .candidate_patch_sha256
            .as_mut()
            .expect("legacy known-bad patch")
            .0 = sha256(KNOWN_BAD_PATCH);
        let evidence_kind = match containment {
            ContainmentGrade::WorkspaceOnly => {
                manifest.containment = ContainmentGrade::WorkspaceOnly;
                manifest.containment_requirement = None;
                manifest.execution_limits.network = crate::manifest::NetworkPolicy::Unrestricted;
                manifest
                    .holdouts
                    .tiers
                    .retain(|tier| tier.kind == crate::manifest::HoldoutTierKind::Exploration);
                crate::manifest::HoldoutTierKind::Exploration
            }
            ContainmentGrade::Sealed => crate::manifest::HoldoutTierKind::Confirmation,
        };
        manifest.objectives = statistic_fixtures::objectives();
        manifest.evaluator.protocol = crate::statistics::PAIRED_MEASUREMENT_PROTOCOL_V1.to_owned();
        manifest.calibration.no_op.minimum_repetitions = 16;
        manifest.calibration.known_bad.minimum_repetitions = 16;
        let evidence_tier = manifest
            .holdouts
            .tiers
            .iter_mut()
            .find(|tier| tier.kind == evidence_kind)
            .expect("paired evidence tier");
        evidence_tier.minimum_repetitions = 16;
        evidence_tier.maximum_disclosures = 16;
        manifest.holdouts.disclosure_cap = if containment == ContainmentGrade::Sealed {
            21
        } else {
            16
        };
        manifest.stop_rules.max_trials_without_qualified_improvement = 32;
        let mut plan = statistic_fixtures::plan();
        plan.calibration_fixtures.no_op.locator =
            manifest.calibration.no_op.fixture_locator.clone();
        plan.calibration_fixtures.no_op.sha256 = manifest.calibration.no_op.fixture_sha256.clone();
        plan.calibration_fixtures.known_bad.locator =
            manifest.calibration.known_bad.fixture_locator.clone();
        plan.calibration_fixtures.known_bad.sha256 =
            manifest.calibration.known_bad.fixture_sha256.clone();
        let evidence_tier = manifest
            .holdouts
            .tiers
            .iter()
            .find(|tier| tier.kind == evidence_kind)
            .expect("paired evidence tier");
        for block in &mut plan.blocks {
            block.fixture_locator = evidence_tier.fixture_locator.clone();
            block.fixture_sha256 = evidence_tier.fixture_sha256.clone();
        }
        manifest.paired_analysis = Some(plan);
        for limit in &mut manifest.budgets.caps {
            match limit.resource {
                BudgetResource::Trials => limit.hard_limit = 64,
                BudgetResource::WallTimeMilliseconds => limit.hard_limit = 100_000,
                BudgetResource::HoldoutDisclosures => {
                    limit.hard_limit = manifest.holdouts.disclosure_cap
                }
                _ => {}
            }
        }
        manifest.validate().expect("paired manifest");
        let admission = CampaignAdmission::from_manifest(&manifest).unwrap();
        admit_campaign(&connection, ACTOR, &admission).expect("admit campaign");
        let objects = tempdir().expect("objects");
        let baseline = seed_candidate(
            &connection,
            objects.path(),
            &manifest,
            "calibration-no-op",
            Vec::new(),
            &runs.join("baseline"),
        );
        let known_bad = seed_candidate(
            &connection,
            objects.path(),
            &manifest,
            "calibration-known-bad",
            KNOWN_BAD_PATCH.to_vec(),
            &runs.join("known-bad"),
        );
        let research = seed_candidate(
            &connection,
            objects.path(),
            &manifest,
            "paired-nomination",
            RESEARCH_PATCH.to_vec(),
            &runs.join("research"),
        );
        Self {
            connection,
            _repository_owner: repository_owner,
            objects,
            manifest,
            baseline_id: baseline.candidate_id,
            known_bad_id: known_bad.candidate_id,
            research_id: research.candidate_id,
        }
    }

    fn spec(
        &self,
        cohort_id: &str,
        candidate_id: &str,
        cohort: PairedCohort,
        seed: &[u8],
        reservation_id: &str,
    ) -> PreparePairedCohortSpec {
        let candidate_tree = candidate_materialization(&self.connection, candidate_id).unwrap();
        let baseline_tree = candidate_materialization(&self.connection, &self.baseline_id).unwrap();
        PreparePairedCohortSpec {
            cohort_id: cohort_id.to_owned(),
            campaign_id: self.manifest.campaign_id.clone(),
            candidate_id: candidate_id.to_owned(),
            cohort,
            revealed_order_seed: seed.to_vec(),
            participants: PairedExecutionParticipants {
                baseline: crate::adapter::PairedExecutionParticipant {
                    identity_sha256: Sha256Digest(self.baseline_id.clone()),
                    revision: baseline_tree,
                },
                candidate: crate::adapter::PairedExecutionParticipant {
                    identity_sha256: Sha256Digest(candidate_id.to_owned()),
                    revision: candidate_tree,
                },
            },
            reservation_id: reservation_id.to_owned(),
        }
    }

    fn reserve_research_slot(&self) {
        let plan = self.manifest.paired_analysis.as_ref().unwrap();
        let identity = Sha256Digest(self.research_id.clone());
        let context = PairedCandidateContext {
            cohort: PairedCohort::Research {
                candidate_analysis_slot: 1,
            },
            candidate_identity_sha256: &identity,
            revealed_order_seed: RESEARCH_SEED,
        };
        let schedule = paired_schedule_sha256(plan, &self.manifest.objectives, &context).unwrap();
        self.connection
            .execute(
                "INSERT INTO paired_analysis_slots
                     (campaign_id, slot, candidate_id, schedule_sha256,
                      order_seed_reveal, reserved_at)
                     VALUES (?1, 1, ?2, ?3, ?4, ?5)",
                params![
                    self.manifest.campaign_id,
                    self.research_id,
                    schedule.0,
                    RESEARCH_SEED,
                    now(),
                ],
            )
            .unwrap();
    }

    fn complete_all(&self, cohort_id: &str, mode: CalibrationMode) {
        let runs = paired_runs(&self.connection, cohort_id).unwrap();
        let binding = self
            .manifest
            .paired_analysis
            .as_ref()
            .unwrap()
            .trial_adapter
            .as_ref()
            .unwrap();
        for run in runs {
            let request_object = indexed_object(&self.connection, &run.request_sha256).unwrap();
            let request_bytes = read_object(self.objects.path(), &request_object).unwrap();
            let request: PairedTrialRequest = serde_json::from_slice(&request_bytes).unwrap();
            if run.status == PairedRunStatus::Prepared {
                mark_paired_run_launched(
                    &self.connection,
                    ACTOR,
                    cohort_id,
                    &run.execution_id,
                    std::process::id(),
                    &format!("fixture-birth-{}", run.execution_id),
                )
                .unwrap();
            }
            let (correct, frame_ms) = match (mode, request.participant_role) {
                (CalibrationMode::KnownBad, PairedParticipantRole::Candidate) => (0, 20_000),
                (CalibrationMode::Research, PairedParticipantRole::Candidate) => (1, 8_000),
                _ => (1, 10_000),
            };
            let result = DomainTrialResult {
                schema: binding.result_schema.clone(),
                execution_id: request.execution_id.clone(),
                request_sha256: Sha256Digest(sha256(&request_bytes)),
                adapter_executable_sha256: binding.executable_sha256.clone(),
                participant_identity_sha256: request.participant.identity_sha256.clone(),
                domain_trial_receipt: json!({
                    "execution_id": request.execution_id,
                    "fixture": "paired-runtime",
                }),
                domain_authority: json!({"fixture": true}),
                measurements: vec![
                    crate::adapter::DomainTrialMeasurement {
                        objective: "correct".to_owned(),
                        units: correct,
                    },
                    crate::adapter::DomainTrialMeasurement {
                        objective: "frame-ms".to_owned(),
                        units: frame_ms,
                    },
                    crate::adapter::DomainTrialMeasurement {
                        objective: "memory-mib".to_owned(),
                        units: 100_000,
                    },
                ],
            };
            let result_bytes = serde_json::to_vec(&serde_json::to_value(&result).unwrap()).unwrap();
            let birth_identity = format!("fixture-birth-{}", run.execution_id);
            let capabilities = ExecutionCapabilities {
                portable_contract: Some(
                    crate::executor::PORTABLE_LOCAL_SUPERVISION_CONTRACT_V1.to_owned(),
                ),
                platform: std::env::consts::OS.to_owned(),
                process_family: "fixture-diagnostic".to_owned(),
                aggregate_process_limit: false,
                aggregate_memory_limit: false,
            };
            complete_paired_run(
                &self.connection,
                ACTOR,
                self.objects.path(),
                &paired_cohort(&self.connection, cohort_id).unwrap().unwrap(),
                &run,
                &CompletedRunEvidence {
                    request: &request_object,
                    result_bytes: &result_bytes,
                    result: &result,
                    elapsed_ms: 1,
                    process_birth_identity: &birth_identity,
                    capabilities: &capabilities,
                },
            )
            .unwrap();
        }
    }
}

fn seed_candidate(
    connection: &Connection,
    objects: &Path,
    manifest: &CampaignManifest,
    semantic_class: &str,
    patch_bytes: Vec<u8>,
    worktree: &Path,
) -> crate::candidate::BoundCandidate {
    let changed_paths = if patch_bytes.is_empty() {
        BTreeSet::new()
    } else {
        BTreeSet::from(["src/fixture.rs".to_owned()])
    };
    let candidate = crate::candidate::bind_legacy_patch_candidate(
        crate::candidate::CandidateProposal {
            campaign_id: manifest.campaign_id.clone(),
            parent_candidate_ids: BTreeSet::new(),
            base_commit: manifest.source.base_commit.clone(),
            base_tree: manifest.source.base_tree.clone(),
            proposer: "paired-runtime-fixture".to_owned(),
            proposal_policy_sha256: manifest.generation.proposal_policy_sha256.0.clone(),
            adapter_sha256: manifest.adapter.implementation_sha256.0.clone(),
            hypothesis: crate::candidate::Hypothesis {
                mechanism: semantic_class.to_owned(),
                expected_effects: vec!["bounded paired effect".to_owned()],
                possible_regressions: vec!["paired calibration rejection".to_owned()],
                decisive_falsifiers: vec!["reopened CAS evidence differs".to_owned()],
            },
            changed_paths,
            changed_symbols: BTreeSet::from([semantic_class.to_owned()]),
            semantic_class: semantic_class.to_owned(),
            differentiator: None,
        },
        patch_bytes,
    )
    .expect("bind fixture candidate");
    let patch_object = preserve_object(objects, &candidate.material_bytes).expect("patch object");
    record_artifact_in(connection, &patch_object, "text/x-diff; charset=utf-8")
        .expect("index patch object");
    let proposal_json = serde_json::to_string(&candidate.proposal).expect("proposal JSON");
    connection
        .execute(
            "INSERT INTO candidates
                 (candidate_id, campaign_id, proposal_json, patch_sha256,
                  negative_fingerprint, disposition, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'materialized', ?6, ?6)",
            params![
                candidate.candidate_id,
                manifest.campaign_id,
                proposal_json,
                candidate.material_sha256,
                candidate.negative_fingerprint,
                now()
            ],
        )
        .expect("record fixture candidate");
    connection
        .execute(
            "INSERT INTO candidate_artifacts (candidate_id, role, sha256)
                 VALUES (?1, 'patch', ?2)",
            params![candidate.candidate_id, candidate.material_sha256],
        )
        .expect("bind patch artifact");

    git_run(
        Path::new(&manifest.source.repository_locator),
        &["worktree", "add", "--detach"],
        Some(worktree),
        Some(&manifest.source.base_commit),
        None,
    )
    .expect("fixture worktree");
    if !candidate.material_bytes.is_empty() {
        git_run(
            worktree,
            &["apply", "--index", "--whitespace=nowarn", "-"],
            None,
            None,
            Some(&candidate.material_bytes),
        )
        .expect("apply fixture patch");
    }
    let result_tree = git_text(worktree, &["write-tree"])
        .expect("fixture result tree")
        .trim()
        .to_owned();
    let worktree_locator = canonical_or_pending_absolute(worktree).expect("worktree locator");
    let receipt = crate::lifecycle::MaterializationReceipt {
        schema: "papertiger-mise.materialization.v1".to_owned(),
        campaign_id: manifest.campaign_id.clone(),
        candidate_id: candidate.candidate_id.clone(),
        base_commit: manifest.source.base_commit.clone(),
        base_tree: manifest.source.base_tree.clone(),
        patch_sha256: Some(candidate.material_sha256.clone()),
        material_sha256: None,
        result_tree: result_tree.clone(),
        worktree_locator: worktree_locator.clone(),
        adapter_sha256: candidate.proposal.adapter_sha256.clone(),
    };
    let receipt_bytes = serde_json::to_vec(&receipt).expect("materialization receipt");
    let receipt_object =
        preserve_object(objects, &receipt_bytes).expect("materialization receipt object");
    record_artifact_in(connection, &receipt_object, "application/json")
        .expect("index materialization receipt");
    connection
        .execute(
            "INSERT INTO materializations
                 (candidate_id, reservation_id, receipt_sha256, result_tree,
                  worktree_locator, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                candidate.candidate_id,
                format!("materialize-{}", candidate.candidate_id),
                receipt_object.sha256,
                result_tree,
                worktree_locator,
                now(),
            ],
        )
        .expect("record fixture materialization");
    connection
        .execute(
            "INSERT INTO candidate_artifacts (candidate_id, role, sha256)
                 VALUES (?1, 'materialization-receipt', ?2)",
            params![candidate.candidate_id, receipt_object.sha256],
        )
        .expect("bind materialization receipt");
    candidate
}
