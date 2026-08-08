use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::budget::{
    BoundReservation, BudgetResource, BudgetSettlement, SettlementMode, SettlementOutcome,
    settle_bound_budget, settle_bound_budget_in,
};
use crate::candidate::{
    BoundCandidate, CandidateDisposition, CandidateMaterial, CandidateMaterialFormat,
    bind_legacy_patch_candidate,
};
use crate::classification::{Classification, DeterministicObservation, classify_deterministic};
use crate::digest::sha256;
use crate::executor::{
    ExecutionCapabilities, SupervisedExecutionOutcome, SupervisionHooks, execute_supervised,
};
use crate::git_materialization::{
    apply_candidate_material, expected_result_tree, git_text, git_worktree_add_without_hooks,
    projected_materialization_disk_bytes, require_confined_worktree,
    verify_materialization_receipt, verify_materialized_worktree, verify_no_checkout_transforms,
    verify_no_repository_info_attributes, verify_tree_has_only_regular_files,
};
use crate::manifest::CampaignManifest;
use crate::object::{PreservedObject, read_object, verify_object};
pub(crate) use crate::object::{
    indexed_object as artifact_object, record_indexed_object as record_artifact_in,
};
use crate::path_identity::{canonical_or_pending_absolute, portable_absolute, trial_path_identity};
use crate::process_identity::{ProcessObservation, observe_process};
use crate::state::{BudgetReservationStatus, EvidenceGrade, TrialStatus};
use crate::store::{begin_mutation, now, record_event_in_mutation, require_campaign};
use crate::validation::validate_nonblank;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateRecord {
    pub candidate_id: String,
    pub campaign_id: String,
    pub material_sha256: String,
    pub negative_fingerprint: String,
    pub disposition: CandidateDisposition,
    pub result: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NominationRecord {
    pub nomination_id: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub receipt_sha256: String,
    pub receipt_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedNominationEvidence {
    pub nomination: NominationRecord,
    pub manifest: CampaignManifest,
    pub manifest_sha256: String,
    pub candidate_result: Value,
    pub relied_upon_trial_ids: Vec<String>,
    #[serde(default)]
    pub candidate_trial_ids: Vec<String>,
    #[serde(default)]
    pub relied_upon_paired_cohort_ids: Vec<String>,
    pub evidence_grade: EvidenceGrade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedCandidateEvidence {
    pub candidate: CandidateRecord,
    pub manifest: CampaignManifest,
    pub manifest_sha256: String,
    pub candidate_material: CandidateMaterial,
    pub candidate_material_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationReceipt {
    pub schema: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub base_commit: String,
    pub base_tree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material_sha256: Option<String>,
    pub result_tree: String,
    pub worktree_locator: String,
    pub adapter_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializationRecord {
    pub candidate_id: String,
    pub reservation_id: String,
    pub receipt_sha256: String,
    pub result_tree: String,
    pub worktree_locator: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialIntent {
    pub trial_id: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub materialization_receipt_sha256: String,
    pub baseline_materialization_receipt_sha256: String,
    pub result_tree: String,
    pub working_directory: String,
    pub reservation_id: String,
    pub tier: String,
    pub argv: Vec<String>,
    pub environment: Value,
    pub owner_uuid: String,
    /// Identity of an already-active supervisor boundary. The child process
    /// may only be spawned after this intent commits.
    pub supervisor_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialOwnership {
    pub owner_uuid: String,
    pub pid: u32,
    pub process_birth_identity: String,
    pub supervisor_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbsenceProof {
    pub verifier: String,
    pub observed_at: String,
    pub supervisor_identity: String,
    pub process_birth_identity: Option<String>,
    pub evidence: PreservedObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrityFailure {
    pub reason_code: String,
    pub expected_outer_judge_sha256: String,
    pub observed_outer_judge_sha256: String,
    pub expected_launcher_sha256: String,
    pub observed_launcher_sha256: String,
    pub expected_evaluator_sha256: String,
    pub observed_evaluator_sha256: String,
    pub expected_fixture_sha256: String,
    pub observed_fixture_sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frozen_input_mismatches: Vec<FrozenJudgeInputMismatch>,
    pub evidence: PreservedObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenJudgeInputMismatch {
    pub role: String,
    pub locator: String,
    pub expected_identity: String,
    pub observed_identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenJudgeInputKind {
    CurrentExecutable,
    AbsoluteFile,
    CandidateFile,
    CandidateGitTree,
    UnsupportedWorkspaceFixture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenJudgeInputBinding {
    role: &'static str,
    locator: String,
    expected_identity: String,
    kind: FrozenJudgeInputKind,
}

fn absolute_file_identity(locator: &str, sha256: &str) -> String {
    format!("plain-file;canonical:{locator};sha256:{sha256}")
}

fn candidate_file_identity(locator: &str, sha256: &str) -> String {
    format!("plain-file;relative:{locator};sha256:{sha256}")
}

fn frozen_judge_input_bindings(
    manifest: &CampaignManifest,
    tier: &str,
) -> Result<Vec<FrozenJudgeInputBinding>> {
    let mut bindings = vec![
        FrozenJudgeInputBinding {
            role: "outer-judge-executable",
            locator: manifest.generation.outer_judge_executable_locator.clone(),
            expected_identity: absolute_file_identity(
                &manifest.generation.outer_judge_executable_locator,
                &manifest.generation.outer_judge_executable_sha256.0,
            ),
            kind: FrozenJudgeInputKind::CurrentExecutable,
        },
        FrozenJudgeInputBinding {
            role: "evaluator-launcher",
            locator: manifest.evaluator.launcher_locator.clone(),
            expected_identity: absolute_file_identity(
                &manifest.evaluator.launcher_locator,
                &manifest.evaluator.launcher_sha256.0,
            ),
            kind: FrozenJudgeInputKind::AbsoluteFile,
        },
        FrozenJudgeInputBinding {
            role: "evaluator",
            locator: manifest.evaluator.evaluator_locator.clone(),
            expected_identity: candidate_file_identity(
                &manifest.evaluator.evaluator_locator,
                &manifest.evaluator.evaluator_sha256.0,
            ),
            kind: FrozenJudgeInputKind::CandidateFile,
        },
    ];
    let (fixture_locator, fixture_sha256) = expected_fixture_binding(manifest, tier)?;
    bindings.push(FrozenJudgeInputBinding {
        role: "fixture",
        expected_identity: if fixture_locator.contains("://") {
            format!("tracked-local-file:{fixture_locator}")
        } else {
            candidate_file_identity(&fixture_locator, &fixture_sha256)
        },
        kind: if fixture_locator.contains("://") {
            FrozenJudgeInputKind::UnsupportedWorkspaceFixture
        } else {
            FrozenJudgeInputKind::CandidateFile
        },
        locator: fixture_locator,
    });
    if let Some(rust) = &manifest.evaluator.rust_build_environment {
        bindings.extend([
            FrozenJudgeInputBinding {
                role: "cargo-executable",
                locator: rust.cargo_executable_locator.clone(),
                expected_identity: absolute_file_identity(
                    &rust.cargo_executable_locator,
                    &rust.cargo_executable_sha256.0,
                ),
                kind: FrozenJudgeInputKind::AbsoluteFile,
            },
            FrozenJudgeInputBinding {
                role: "rustc-executable",
                locator: rust.rustc_executable_locator.clone(),
                expected_identity: absolute_file_identity(
                    &rust.rustc_executable_locator,
                    &rust.rustc_executable_sha256.0,
                ),
                kind: FrozenJudgeInputKind::AbsoluteFile,
            },
            FrozenJudgeInputBinding {
                role: "rust-lockfile",
                locator: rust.lockfile_locator.clone(),
                expected_identity: candidate_file_identity(
                    &rust.lockfile_locator,
                    &rust.lockfile_sha256.0,
                ),
                kind: FrozenJudgeInputKind::CandidateFile,
            },
            FrozenJudgeInputBinding {
                role: "cargo-config",
                locator: rust.cargo_config_locator.clone(),
                expected_identity: candidate_file_identity(
                    &rust.cargo_config_locator,
                    &rust.cargo_config_sha256.0,
                ),
                kind: FrozenJudgeInputKind::CandidateFile,
            },
            FrozenJudgeInputBinding {
                role: "vendored-rust-sources",
                locator: rust.vendored_sources_locator.clone(),
                expected_identity: format!("git-tree:{}", rust.vendored_sources_tree),
                kind: FrozenJudgeInputKind::CandidateGitTree,
            },
        ]);
        if let Some(linker) = &rust.linker {
            bindings.push(FrozenJudgeInputBinding {
                role: "rust-target-linker",
                locator: linker.executable_locator.clone(),
                expected_identity: absolute_file_identity(
                    &linker.executable_locator,
                    &linker.executable_sha256.0,
                ),
                kind: FrozenJudgeInputKind::AbsoluteFile,
            });
        }
    }
    if let Some(build) = &manifest.evaluator.judge_build {
        bindings.push(FrozenJudgeInputBinding {
            role: "judge-build-toolchain-executable",
            locator: build.toolchain_executable_locator.clone(),
            expected_identity: absolute_file_identity(
                &build.toolchain_executable_locator,
                &build.toolchain_executable_sha256.0,
            ),
            kind: FrozenJudgeInputKind::AbsoluteFile,
        });
    }
    Ok(bindings)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrialRecord {
    pub trial_id: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub materialization_receipt_sha256: String,
    pub baseline_materialization_receipt_sha256: String,
    pub result_tree: String,
    pub working_directory: String,
    pub reservation_id: String,
    pub tier: String,
    pub argv: Vec<String>,
    pub environment: Value,
    pub status: TrialStatus,
    pub owner_uuid: Option<String>,
    pub pid: Option<u32>,
    pub process_birth_identity: Option<String>,
    pub supervisor_identity: Option<String>,
    pub heartbeat_at: Option<String>,
    pub outcome: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialCompletion {
    pub receipt: PreservedObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialReceipt {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_build: Option<JudgeBuildReceipt>,
    pub trial_id: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub materialization_receipt_sha256: String,
    pub baseline_materialization_receipt_sha256: String,
    pub result_tree: String,
    pub working_directory: String,
    pub tier: String,
    pub owner_uuid: String,
    pub supervisor_identity: String,
    pub process_birth_identity: String,
    pub launcher_sha256: String,
    pub evaluator_sha256: String,
    pub fixture_sha256: String,
    pub protocol: String,
    pub observations: Vec<DeterministicObservation>,
    pub reason_code: Option<String>,
    pub measured_usage: Vec<BudgetSettlement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_capabilities: Option<ExecutionCapabilities>,
}

mod trial_runtime;
pub use trial_runtime::{
    DeterministicEvaluatorOutput, DeterministicEvaluatorRequest, EvaluatorJudgeBuild,
    JudgeBuildReceipt, SupervisedTrialOutcome, SupervisedTrialSpec, execute_workspace_trial,
};
use trial_runtime::{expected_trial_environment, require_trial_reservation};
#[cfg(test)]
use trial_runtime::{
    prepare_trial_environment, record_runtime_integrity_failure, verify_runtime_judge_inputs,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColdRecoveryOutcome {
    Reconciled,
    AlreadyReconciled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProcessAbsenceEvidence<'a> {
    schema: &'static str,
    pid: u32,
    expected_process_birth_identity: &'a str,
    observation: &'a ProcessObservation,
    platform: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialCompletionCredential {
    pub owner_uuid: String,
    pub supervisor_identity: String,
    pub process_birth_identity: String,
}

pub fn record_candidate(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    reservation_id: &str,
    candidate: &BoundCandidate,
    material_object: &PreservedObject,
) -> Result<bool> {
    if candidate.material_format != CandidateMaterialFormat::GitChangeSetV1 {
        bail!(
            "new candidate writes require typed material; run `papertiger-mise candidate build-material --repository <repo> --base-tree <tree> --result-tree <tree> --output <file>`"
        );
    }
    record_candidate_in(
        connection,
        actor,
        object_root,
        reservation_id,
        candidate,
        material_object,
    )
}

#[cfg(test)]
fn record_legacy_candidate_for_test(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    reservation_id: &str,
    candidate: &BoundCandidate,
    material_object: &PreservedObject,
) -> Result<bool> {
    record_candidate_in(
        connection,
        actor,
        object_root,
        reservation_id,
        candidate,
        material_object,
    )
}

fn record_candidate_in(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    reservation_id: &str,
    candidate: &BoundCandidate,
    material_object: &PreservedObject,
) -> Result<bool> {
    validate_nonblank("actor", actor)?;
    validate_nonblank("reservation_id", reservation_id)?;
    verify_object(object_root, material_object)?;
    let rebound = match candidate.material_format {
        CandidateMaterialFormat::GitChangeSetV1 => crate::candidate::bind_candidate(
            candidate.proposal.clone(),
            candidate.material_bytes.clone(),
        )?,
        CandidateMaterialFormat::LegacyGitPatchV1 => bind_legacy_patch_candidate(
            candidate.proposal.clone(),
            candidate.material_bytes.clone(),
        )?,
    };
    if &rebound != candidate {
        bail!("candidate fields do not recompute their canonical identity");
    }
    if candidate.material_sha256 != material_object.sha256
        || u64::try_from(candidate.material_bytes.len())? != material_object.bytes
        || candidate.proposal.campaign_id.trim().is_empty()
    {
        bail!("candidate material object does not match its immutable candidate identity");
    }
    let manifest = campaign_manifest(connection, &candidate.proposal.campaign_id)?;
    validate_candidate_against_manifest(candidate, &manifest)?;
    match candidate.material_format {
        CandidateMaterialFormat::GitChangeSetV1 => {
            let material = CandidateMaterial::parse_canonical(&candidate.material_bytes)?;
            if material.change_set.changes.is_empty() {
                if candidate.material_sha256 != manifest.calibration.no_op_material_sha256().0
                    || candidate.proposal.semantic_class != "calibration-no-op"
                {
                    bail!("only the manifest-bound no-op calibration may use an empty change set");
                }
            } else {
                validate_mutation_scope(&material.scope.changed_paths, &manifest)?;
            }
        }
        CandidateMaterialFormat::LegacyGitPatchV1 => {
            if candidate.material_bytes.is_empty() {
                if candidate.material_sha256 != manifest.calibration.no_op_material_sha256().0
                    || candidate.proposal.semantic_class != "calibration-no-op"
                {
                    bail!("only the manifest-bound no-op calibration may use an empty patch");
                }
            } else {
                let actual_paths = exact_patch_paths(candidate)?;
                if actual_paths != candidate.proposal.changed_paths {
                    bail!("candidate declared changed paths differ from its exact patch headers");
                }
                validate_mutation_scope(&actual_paths, &manifest)?;
            }
        }
    }
    let proposal_json = serde_json::to_string(&candidate.proposal)?;
    let transaction = begin_mutation(connection)?;
    require_campaign(&transaction, &candidate.proposal.campaign_id)?;
    if let Some(existing) = candidate_in(&transaction, &candidate.candidate_id)? {
        let existing_proposal_json = proposal_json_for(&transaction, &candidate.candidate_id)?;
        let mut differing_fields = Vec::new();
        if existing.campaign_id != candidate.proposal.campaign_id {
            differing_fields.push("campaign_id");
        }
        if existing.material_sha256 != candidate.material_sha256 {
            differing_fields.push("material_sha256");
        }
        if existing.negative_fingerprint != candidate.negative_fingerprint {
            differing_fields.push("negative_fingerprint");
        }
        if existing_proposal_json != proposal_json {
            differing_fields.push("proposal");
        }
        if differing_fields.is_empty() {
            require_reservation_use(
                &transaction,
                &candidate.proposal.campaign_id,
                reservation_id,
                "candidate",
                &candidate.candidate_id,
            )?;
            transaction.commit()?;
            settle_bound_budget(
                connection,
                actor,
                BoundReservation {
                    campaign_id: &candidate.proposal.campaign_id,
                    reservation_id,
                    use_kind: "candidate",
                    entity_key: &candidate.candidate_id,
                },
                SettlementMode::ChargeReservation,
                &[],
                Some("candidate-recorded"),
            )?;
            return Ok(false);
        }
        bail!(
            "candidate identity '{}' conflicts with durable state; differing fields: {}",
            candidate.candidate_id,
            differing_fields.join(", ")
        );
    }
    require_campaign_running(&transaction, &manifest)?;
    for parent in &candidate.proposal.parent_candidate_ids {
        let durable_parent = candidate_in(&transaction, parent)?;
        if durable_parent.is_none() {
            bail!("candidate parent '{parent}' is not durable in this Mise authority");
        }
        if durable_parent.is_some_and(|parent| parent.campaign_id != candidate.proposal.campaign_id)
        {
            bail!("candidate parent '{parent}' belongs to a different campaign");
        }
    }
    if candidate.proposal.differentiator.is_none()
        && negative_fingerprint_exists(
            &transaction,
            &candidate.proposal.campaign_id,
            &candidate.negative_fingerprint,
        )?
    {
        bail!("candidate repeats durable negative evidence without an explicit differentiator");
    }
    require_candidate_reservation(
        &transaction,
        &candidate.proposal.campaign_id,
        reservation_id,
        u64::try_from(candidate.material_bytes.len())?,
    )?;
    bind_reservation_use(
        &transaction,
        &candidate.proposal.campaign_id,
        reservation_id,
        "candidate",
        &candidate.candidate_id,
    )?;
    let media_type = match candidate.material_format {
        CandidateMaterialFormat::GitChangeSetV1 => crate::candidate::GIT_CHANGE_SET_MEDIA_TYPE,
        CandidateMaterialFormat::LegacyGitPatchV1 => "text/x-diff; charset=utf-8",
    };
    record_artifact_in(&transaction, material_object, media_type)?;
    let timestamp = now();
    transaction.execute(
        "INSERT INTO candidates
         (candidate_id, campaign_id, proposal_json, patch_sha256, negative_fingerprint,
          disposition, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'proposed', ?6, ?6)",
        params![
            candidate.candidate_id,
            candidate.proposal.campaign_id,
            proposal_json,
            candidate.material_sha256,
            candidate.negative_fingerprint,
            timestamp
        ],
    )?;
    for parent in &candidate.proposal.parent_candidate_ids {
        transaction.execute(
            "INSERT INTO candidate_parents (candidate_id, parent_candidate_id) VALUES (?1, ?2)",
            params![candidate.candidate_id, parent],
        )?;
    }
    let artifact_role = match candidate.material_format {
        CandidateMaterialFormat::GitChangeSetV1 => "material",
        CandidateMaterialFormat::LegacyGitPatchV1 => "patch",
    };
    transaction.execute(
        "INSERT INTO candidate_artifacts (candidate_id, role, sha256) VALUES (?1, ?2, ?3)",
        params![
            candidate.candidate_id,
            artifact_role,
            candidate.material_sha256
        ],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "candidate",
        &candidate.candidate_id,
        "recorded",
        None,
        Some(&json!({
            "campaign_id": candidate.proposal.campaign_id,
            "material_sha256": candidate.material_sha256,
            "material_format": candidate.material_format,
            "negative_fingerprint": candidate.negative_fingerprint,
        })),
    )?;
    transaction.commit()?;
    settle_bound_budget(
        connection,
        actor,
        BoundReservation {
            campaign_id: &candidate.proposal.campaign_id,
            reservation_id,
            use_kind: "candidate",
            entity_key: &candidate.candidate_id,
        },
        SettlementMode::ChargeReservation,
        &[],
        Some("candidate-recorded"),
    )?;
    Ok(true)
}

pub fn candidate(connection: &Connection, candidate_id: &str) -> Result<Option<CandidateRecord>> {
    candidate_in(connection, candidate_id)
}

pub fn materialize_candidate(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    reservation_id: &str,
    candidate_id: &str,
    worktree: &Path,
) -> Result<MaterializationRecord> {
    validate_nonblank("actor", actor)?;
    validate_nonblank("reservation_id", reservation_id)?;
    let durable_candidate = candidate(connection, candidate_id)?
        .with_context(|| format!("unknown candidate '{candidate_id}'"))?;
    let manifest = campaign_manifest(connection, &durable_candidate.campaign_id)?;
    let proposal: crate::candidate::CandidateProposal =
        serde_json::from_str(&proposal_json_for(connection, candidate_id)?)?;
    let material_object = artifact_object(connection, &durable_candidate.material_sha256)?;
    let material_bytes = read_object(object_root, &material_object)?;
    verify_no_checkout_transforms(&manifest, &manifest.source.base_tree)?;
    verify_no_repository_info_attributes(&manifest)?;
    let minimum_disk_bytes = projected_materialization_disk_bytes(&manifest, &material_bytes)?;
    let (requested_locator, resolved_worktree) = resolved_materialization_worktree(worktree)?;
    let worktree = resolved_worktree.as_path();
    require_confined_worktree(&manifest, worktree)?;

    let transaction = begin_mutation(connection)?;
    if let Some(existing) = materialization_in(&transaction, candidate_id)? {
        if existing.reservation_id != reservation_id
            || existing.worktree_locator != requested_locator
        {
            bail!("candidate already has a different immutable materialization");
        }
        transaction.commit()?;
        verify_materialization_receipt(connection, object_root, &manifest, &proposal, &existing)?;
        verify_materialized_worktree(&manifest, &proposal, &existing, worktree)?;
        settle_bound_budget(
            connection,
            actor,
            BoundReservation {
                campaign_id: &durable_candidate.campaign_id,
                reservation_id,
                use_kind: "materialization",
                entity_key: candidate_id,
            },
            SettlementMode::ChargeReservation,
            &[],
            Some("candidate-materialized"),
        )?;
        return Ok(existing);
    }
    if durable_candidate.disposition != CandidateDisposition::Proposed {
        bail!("only a proposed candidate can create its first materialization");
    }
    require_campaign_running(&transaction, &manifest)?;
    require_materialization_reservation(
        &transaction,
        &durable_candidate.campaign_id,
        reservation_id,
        minimum_disk_bytes,
    )?;
    let prior_attempts = bind_or_require_reservation_use(
        &transaction,
        &durable_candidate.campaign_id,
        reservation_id,
        "materialization",
        candidate_id,
    )?;
    if !prior_attempts.is_empty() {
        record_event_in_mutation(
            &transaction,
            actor,
            "candidate",
            candidate_id,
            "materialization-retry-bound",
            Some("prior materialization attempts were terminally budget-settled"),
            Some(&json!({
                "prior_reservation_ids": prior_attempts,
                "retry_reservation_id": reservation_id,
                "worktree_locator": requested_locator.clone(),
            })),
        )?;
    }
    transaction.commit()?;

    let attempt = (|| -> Result<MaterializationRecord> {
        // Applying to an isolated index still writes blobs and a tree into the
        // repository object database, so even expected-tree derivation belongs
        // behind the already bound disk reservation.
        let expected_result_tree = expected_result_tree(&manifest, &material_bytes)?;
        if !worktree.exists() {
            if let Some(parent) = worktree.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            git_worktree_add_without_hooks(
                Path::new(&manifest.source.repository_locator),
                worktree,
                &manifest.source.base_commit,
            )?;
        }
        let head = git_text(worktree, &["rev-parse", "HEAD^{commit}"])?;
        if head.trim() != manifest.source.base_commit {
            bail!("candidate worktree is not detached at the frozen base commit");
        }
        let staged_before = git_text(worktree, &["diff", "--cached", "--name-only"])?;
        if staged_before.trim().is_empty() {
            apply_candidate_material(&manifest, &material_bytes, worktree)?;
        }
        let result_tree = git_text(worktree, &["write-tree"])?.trim().to_owned();
        if result_tree != expected_result_tree {
            bail!("materialized tree is not the exact frozen base plus retained material");
        }
        verify_tree_has_only_regular_files(&manifest, &result_tree)?;
        let receipt = MaterializationReceipt {
            schema: if manifest.candidate_material.is_some() {
                "papertiger-mise.materialization.v2"
            } else {
                "papertiger-mise.materialization.v1"
            }
            .to_owned(),
            campaign_id: durable_candidate.campaign_id.clone(),
            candidate_id: candidate_id.to_owned(),
            base_commit: manifest.source.base_commit.clone(),
            base_tree: manifest.source.base_tree.clone(),
            patch_sha256: manifest
                .candidate_material
                .is_none()
                .then(|| durable_candidate.material_sha256.clone()),
            material_sha256: manifest
                .candidate_material
                .is_some()
                .then(|| durable_candidate.material_sha256.clone()),
            result_tree: result_tree.clone(),
            worktree_locator: requested_locator.clone(),
            adapter_sha256: proposal.adapter_sha256.clone(),
        };
        let receipt_bytes = serde_json::to_vec(&receipt)?;
        let receipt_object = crate::object::preserve_object(object_root, &receipt_bytes)?;
        if receipt_object.bytes
            > reservation_rows_for_use(connection, &durable_candidate.campaign_id, reservation_id)?
                .get(&BudgetResource::ArtifactBytes)
                .map(|row| row.0)
                .unwrap_or(0)
        {
            bail!("materialization receipt exceeds its artifact reservation");
        }
        let record = MaterializationRecord {
            candidate_id: candidate_id.to_owned(),
            reservation_id: reservation_id.to_owned(),
            receipt_sha256: receipt_object.sha256.clone(),
            result_tree,
            worktree_locator: requested_locator,
        };
        verify_materialized_worktree(&manifest, &proposal, &record, worktree)?;

        let transaction = begin_mutation(connection)?;
        if let Some(existing) = materialization_in(&transaction, candidate_id)? {
            if existing != record {
                bail!("candidate materialization raced with different durable state");
            }
            transaction.commit()?;
        } else {
            require_reservation_use(
                &transaction,
                &durable_candidate.campaign_id,
                reservation_id,
                "materialization",
                candidate_id,
            )?;
            record_artifact_in(&transaction, &receipt_object, "application/json")?;
            transaction.execute(
                "INSERT INTO materializations
                 (candidate_id, reservation_id, receipt_sha256, result_tree, worktree_locator, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    record.candidate_id,
                    record.reservation_id,
                    record.receipt_sha256,
                    record.result_tree,
                    record.worktree_locator,
                    now()
                ],
            )?;
            transaction.execute(
                "INSERT INTO candidate_artifacts (candidate_id, role, sha256)
                 VALUES (?1, 'materialization-receipt', ?2)",
                params![candidate_id, receipt_object.sha256],
            )?;
            transaction.execute(
                "UPDATE candidates SET disposition='materialized', updated_at=?2 WHERE candidate_id=?1",
                params![candidate_id, now()],
            )?;
            record_event_in_mutation(
                &transaction,
                actor,
                "candidate",
                candidate_id,
                "materialized",
                None,
                Some(&serde_json::to_value(&record)?),
            )?;
            transaction.commit()?;
        }
        Ok(record)
    })();
    let settlement = settle_bound_budget(
        connection,
        actor,
        BoundReservation {
            campaign_id: &durable_candidate.campaign_id,
            reservation_id,
            use_kind: "materialization",
            entity_key: candidate_id,
        },
        SettlementMode::ChargeReservation,
        &[],
        Some(if attempt.is_ok() {
            "candidate-materialized"
        } else {
            "candidate-materialization-failed"
        }),
    );
    match (attempt, settlement) {
        (Ok(record), Ok(_)) => Ok(record),
        (Err(error), Ok(_)) => Err(error),
        (Ok(_), Err(error)) => Err(error).context("settle successful materialization budget"),
        (Err(error), Err(settlement_error)) => Err(error).context(format!(
            "materialization also failed to charge its reservation: {settlement_error:#}"
        )),
    }
}

/// Conservatively close a materialization attempt that bound its budget but
/// did not durably record a materialization. This is an operator decision: it
/// charges the full reservation and does not claim what filesystem work did or
/// did not occur before the interruption.
pub fn abandon_materialization_attempt(
    connection: &Connection,
    actor: &str,
    candidate_id: &str,
    reservation_id: &str,
    reason: &str,
) -> Result<SettlementOutcome> {
    validate_nonblank("actor", actor)?;
    validate_nonblank("candidate_id", candidate_id)?;
    validate_nonblank("reservation_id", reservation_id)?;
    validate_nonblank("reason", reason)?;
    let transaction = begin_mutation(connection)?;
    let candidate = candidate_in(&transaction, candidate_id)?
        .with_context(|| format!("unknown candidate '{candidate_id}'"))?;
    if materialization_in(&transaction, candidate_id)?.is_some() {
        bail!("candidate '{candidate_id}' already has a durable materialization");
    }
    if candidate.disposition != CandidateDisposition::Proposed {
        bail!("only a proposed candidate can abandon an interrupted materialization");
    }
    require_reservation_use(
        &transaction,
        &candidate.campaign_id,
        reservation_id,
        "materialization",
        candidate_id,
    )?;
    let outcome = settle_bound_budget_in(
        &transaction,
        actor,
        BoundReservation {
            campaign_id: &candidate.campaign_id,
            reservation_id,
            use_kind: "materialization",
            entity_key: candidate_id,
        },
        SettlementMode::ChargeReservation,
        &[],
        Some("materialization-abandoned"),
    )?;
    if outcome == SettlementOutcome::Settled {
        record_event_in_mutation(
            &transaction,
            actor,
            "candidate",
            candidate_id,
            "materialization-abandoned",
            Some(reason),
            Some(&json!({
                "reservation_id": reservation_id,
                "reservation_charged": true,
            })),
        )?;
    }
    transaction.commit()?;
    Ok(outcome)
}

fn resolved_materialization_worktree(worktree: &Path) -> Result<(String, PathBuf)> {
    let locator = canonical_or_pending_absolute(worktree)?;
    Ok((locator.clone(), PathBuf::from(locator)))
}

fn require_materialization_reservation(
    connection: &Connection,
    campaign_id: &str,
    reservation_id: &str,
    minimum_disk_bytes: u64,
) -> Result<()> {
    let rows = reservation_rows_for_use(connection, campaign_id, reservation_id)?;
    if rows.is_empty()
        || rows
            .values()
            .any(|(_, status)| *status != BudgetReservationStatus::Reserved)
    {
        bail!("materialization requires one active durable reservation");
    }
    for resource in [
        BudgetResource::DiskBytesWritten,
        BudgetResource::ArtifactBytes,
    ] {
        if rows.get(&resource).map(|row| row.0).unwrap_or(0) == 0 {
            bail!("materialization reservation requires nonzero {resource}");
        }
    }
    let reserved_disk = rows
        .get(&BudgetResource::DiskBytesWritten)
        .map(|row| row.0)
        .unwrap_or(0);
    if reserved_disk < minimum_disk_bytes {
        bail!(
            "materialization disk reservation {reserved_disk} is below the conservative logical-write bound {minimum_disk_bytes}"
        );
    }
    Ok(())
}

fn bind_or_require_reservation_use(
    connection: &Connection,
    campaign_id: &str,
    reservation_id: &str,
    use_kind: &str,
    entity_key: &str,
) -> Result<Vec<String>> {
    let existing = connection
        .query_row(
            "SELECT use_kind, entity_key FROM budget_reservation_uses
             WHERE campaign_id=?1 AND reservation_id=?2",
            params![campaign_id, reservation_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((kind, key)) = existing {
        if kind == use_kind && key == entity_key {
            return Ok(Vec::new());
        }
        bail!("budget reservation is already bound to a different lifecycle entity");
    }
    let prior_reservations = connection
        .prepare(
            "SELECT reservation_id FROM budget_reservation_uses
             WHERE campaign_id=?1 AND use_kind=?2 AND entity_key=?3
             ORDER BY bound_at, reservation_id",
        )?
        .query_map(params![campaign_id, use_kind, entity_key], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    for prior in &prior_reservations {
        let active: i64 = connection.query_row(
            "SELECT COUNT(*) FROM budget_reservations
             WHERE campaign_id=?1 AND reservation_id=?2 AND status='reserved'",
            params![campaign_id, prior],
            |row| row.get(0),
        )?;
        if active != 0 {
            bail!(
                "prior {use_kind} reservation '{prior}' remains active; run `papertiger-mise candidate abandon-materialization {entity_key} --reservation {prior} --reason <reason>` before retrying"
            );
        }
    }
    bind_reservation_use(
        connection,
        campaign_id,
        reservation_id,
        use_kind,
        entity_key,
    )?;
    Ok(prior_reservations)
}

pub fn negative_fingerprint_candidates(
    connection: &Connection,
    campaign_id: &str,
    fingerprint: &str,
) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT candidate_id FROM negative_evidence
         WHERE campaign_id=?1 AND negative_fingerprint=?2 ORDER BY recorded_at, candidate_id",
    )?;
    Ok(statement
        .query_map(params![campaign_id, fingerprint], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn campaign_manifest(connection: &Connection, campaign_id: &str) -> Result<CampaignManifest> {
    let manifest_json = connection
        .query_row(
            "SELECT manifest_json FROM campaigns WHERE campaign_id=?1",
            params![campaign_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .with_context(|| format!("unknown campaign '{campaign_id}'"))?;
    let manifest: CampaignManifest = serde_json::from_str(&manifest_json)
        .context("stored campaign manifest is not a typed Mise manifest")?;
    manifest.validate()?;
    Ok(manifest)
}

fn validate_candidate_against_manifest(
    candidate: &BoundCandidate,
    manifest: &CampaignManifest,
) -> Result<()> {
    match (&manifest.candidate_material, candidate.material_format) {
        (Some(_), CandidateMaterialFormat::GitChangeSetV1) => {}
        (None, CandidateMaterialFormat::LegacyGitPatchV1) => {}
        (Some(_), CandidateMaterialFormat::LegacyGitPatchV1) => {
            bail!("typed candidate-material campaign refuses legacy Git patch material")
        }
        (None, CandidateMaterialFormat::GitChangeSetV1) => {
            bail!("legacy campaign does not admit typed candidate material")
        }
    }
    if candidate.proposal.base_commit != manifest.source.base_commit
        || candidate.proposal.base_tree != manifest.source.base_tree
    {
        bail!("candidate base commit/tree differ from the frozen campaign source");
    }
    if candidate.proposal.proposal_policy_sha256 != manifest.generation.proposal_policy_sha256.0 {
        bail!("candidate proposal policy differs from the frozen campaign policy");
    }
    if candidate.proposal.adapter_sha256 != manifest.adapter.implementation_sha256.0 {
        bail!("candidate adapter differs from the frozen campaign adapter");
    }
    Ok(())
}

fn exact_patch_paths(candidate: &BoundCandidate) -> Result<BTreeSet<String>> {
    let patch =
        std::str::from_utf8(&candidate.material_bytes).context("candidate patch is not UTF-8")?;
    let mut paths = BTreeSet::new();
    let mut current: Option<(String, bool, bool)> = None;
    for line in patch.lines() {
        if let Some(header) = line.strip_prefix("diff --git ") {
            if current
                .as_ref()
                .is_some_and(|(_, old_seen, new_seen)| !old_seen || !new_seen)
            {
                bail!("candidate patch has an incomplete Git file header");
            }
            let mut fields = header.split(' ');
            let old = fields.next().context("patch has no old path")?;
            let new = fields.next().context("patch has no new path")?;
            if fields.next().is_some()
                || !old.starts_with("a/")
                || !new.starts_with("b/")
                || old[2..] != new[2..]
                || old.contains('"')
                || new.contains('"')
            {
                bail!("Mise v1 accepts only unquoted, non-renaming Git patch headers");
            }
            let path = old[2..].to_owned();
            paths.insert(path.clone());
            current = Some((path, false, false));
        } else if let Some(old) = line.strip_prefix("--- ") {
            let (path, old_seen, _) = current
                .as_mut()
                .context("patch old-file marker precedes a Git file header")?;
            if old == "/dev/null" {
                bail!("Mise v1 candidate patches do not admit /dev/null new-file markers");
            }
            if *old_seen || old != format!("a/{path}") {
                bail!("candidate patch old-file marker differs from its Git header");
            }
            *old_seen = true;
        } else if let Some(new) = line.strip_prefix("+++ ") {
            let (path, _, new_seen) = current
                .as_mut()
                .context("patch new-file marker precedes a Git file header")?;
            if new == "/dev/null" {
                bail!("Mise v1 candidate patches do not admit /dev/null deletion markers");
            }
            if *new_seen || new != format!("b/{path}") {
                bail!("candidate patch new-file marker differs from its Git header");
            }
            *new_seen = true;
        }
        if line.starts_with("rename from ")
            || line.starts_with("rename to ")
            || line.starts_with("copy from ")
            || line.starts_with("copy to ")
            || line.starts_with("old mode ")
            || line.starts_with("new mode ")
            || line.starts_with("new file mode ")
            || line.starts_with("deleted file mode ")
            || line.starts_with("similarity index ")
        {
            bail!("Mise v1 candidate patches do not admit rename/copy/mode records");
        }
        if line == "GIT binary patch" || line.starts_with("Binary files ") {
            bail!("Mise v1 candidate patches require inspectable textual file markers");
        }
    }
    if current
        .as_ref()
        .is_some_and(|(_, old_seen, new_seen)| !old_seen || !new_seen)
    {
        bail!("candidate patch has an incomplete Git file header");
    }
    if paths.is_empty() {
        bail!("candidate patch contains no canonical Git file headers");
    }
    Ok(paths)
}

fn validate_mutation_scope(paths: &BTreeSet<String>, manifest: &CampaignManifest) -> Result<()> {
    for path in paths {
        if !manifest
            .mutation_scope
            .allowlist
            .iter()
            .any(|prefix| path_matches_prefix(path, prefix))
        {
            bail!("candidate material path '{path}' is outside the campaign allowlist");
        }
        if manifest
            .mutation_scope
            .protected_paths
            .iter()
            .any(|prefix| path_matches_prefix(path, prefix))
        {
            bail!("candidate material path '{path}' enters protected judge scope");
        }
    }
    Ok(())
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    prefix == "."
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn negative_fingerprint_exists(
    connection: &Connection,
    campaign_id: &str,
    fingerprint: &str,
) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM negative_evidence
             WHERE campaign_id=?1 AND negative_fingerprint=?2 LIMIT 1",
            params![campaign_id, fingerprint],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false))
}

fn reservation_rows_for_use(
    connection: &Connection,
    campaign_id: &str,
    reservation_id: &str,
) -> Result<BTreeMap<BudgetResource, (u64, BudgetReservationStatus)>> {
    let mut statement = connection.prepare(
        "SELECT resource, reserved_amount, status FROM budget_reservations
         WHERE campaign_id=?1 AND reservation_id=?2 ORDER BY resource",
    )?;
    let rows = statement
        .query_map(params![campaign_id, reservation_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(resource, amount, status)| {
            Ok((
                resource.parse()?,
                (
                    u64::try_from(amount).context("negative reserved amount")?,
                    BudgetReservationStatus::parse_column("budget_reservations.status", &status)?,
                ),
            ))
        })
        .collect()
}

fn require_candidate_reservation(
    connection: &Connection,
    campaign_id: &str,
    reservation_id: &str,
    material_bytes: u64,
) -> Result<()> {
    let rows = reservation_rows_for_use(connection, campaign_id, reservation_id)?;
    if rows.is_empty()
        || rows
            .values()
            .any(|(_, status)| *status != BudgetReservationStatus::Reserved)
    {
        bail!(
            "candidate requires one active durable budget reservation; run `papertiger-mise budget reserve {} {} --amount candidates=1 --amount artifact_bytes=<material-bytes>` first",
            campaign_id,
            reservation_id
        );
    }
    if rows.get(&BudgetResource::Candidates).map(|row| row.0) != Some(1) {
        bail!("candidate reservation must contain exactly one candidates unit");
    }
    let required_artifact_bytes = material_bytes.max(1);
    if rows
        .get(&BudgetResource::ArtifactBytes)
        .map(|row| row.0)
        .unwrap_or(0)
        < required_artifact_bytes
    {
        bail!("candidate reservation does not cover exact material artifact bytes");
    }
    Ok(())
}

fn validate_trial_against_manifest(
    intent: &TrialIntent,
    manifest: &CampaignManifest,
    candidate: &CandidateRecord,
) -> Result<()> {
    if intent.argv != manifest.evaluator.argv {
        bail!("trial argv differs from the frozen evaluator command");
    }
    let expected_environment = serde_json::to_value(expected_trial_environment(
        manifest,
        &intent.campaign_id,
        &intent.trial_id,
    )?)?;
    if intent.environment != expected_environment {
        bail!("trial environment differs from the frozen evaluator environment");
    }
    let tier_is_bound =
        if candidate.material_sha256 == manifest.calibration.no_op_material_sha256().0 {
            intent.tier == "calibration.no_op"
        } else if candidate.material_sha256 == manifest.calibration.known_bad_material_sha256().0 {
            intent.tier == "calibration.known_bad"
        } else {
            manifest
                .holdouts
                .tiers
                .iter()
                .any(|tier| tier.key == intent.tier)
        };
    if !tier_is_bound {
        bail!(
            "trial tier '{}' is not declared by the campaign",
            intent.tier
        );
    }
    Ok(())
}

fn require_campaign_running(connection: &Connection, manifest: &CampaignManifest) -> Result<()> {
    let now_ms =
        u64::try_from(Utc::now().timestamp_millis()).context("system time precedes Unix epoch")?;
    if now_ms < manifest.stop_rules.not_before_unix_ms {
        bail!("campaign has not reached its immutable start time");
    }
    if now_ms >= manifest.stop_rules.deadline_unix_ms {
        bail!("campaign reached its immutable deadline");
    }
    require_campaign_integrity(connection, manifest)?;
    let mut statement = connection.prepare(
        "SELECT status FROM trials
         WHERE campaign_id=?1 AND finished_at IS NOT NULL
         ORDER BY finished_at DESC, trial_id DESC",
    )?;
    let statuses = statement
        .query_map(params![manifest.campaign_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|status| TrialStatus::parse_column("trials.status", &status))
        .collect::<Result<Vec<_>>>()?;
    let consecutive_failures = statuses
        .iter()
        .take_while(|status| **status == TrialStatus::InfrastructureFailed)
        .count();
    if consecutive_failures
        >= usize::try_from(manifest.stop_rules.max_consecutive_infrastructure_failures)?
    {
        bail!("campaign reached its consecutive infrastructure-failure stop rule");
    }
    let trials_without_improvement: i64 = connection.query_row(
        "SELECT COUNT(*) FROM trials t
         JOIN candidates c ON c.candidate_id=t.candidate_id
         WHERE t.campaign_id=?1 AND t.status='succeeded'
           AND c.patch_sha256 NOT IN (?2, ?3)
           AND t.created_at > COALESCE(
             (SELECT MAX(created_at) FROM nominations WHERE campaign_id=?1), '')",
        params![
            manifest.campaign_id,
            manifest.calibration.no_op_material_sha256().0,
            manifest.calibration.known_bad_material_sha256().0
        ],
        |row| row.get(0),
    )?;
    if u32::try_from(trials_without_improvement)?
        >= manifest.stop_rules.max_trials_without_qualified_improvement
    {
        bail!("campaign reached its no-qualified-improvement stop rule");
    }
    Ok(())
}

pub(crate) fn require_campaign_integrity(
    connection: &Connection,
    manifest: &CampaignManifest,
) -> Result<()> {
    let integrity_failures: i64 = connection.query_row(
        "SELECT COUNT(*) FROM trials WHERE campaign_id=?1 AND status='integrity_failed'",
        params![manifest.campaign_id],
        |row| row.get(0),
    )?;
    if integrity_failures > 0 && manifest.stop_rules.stop_on_evaluator_integrity_failure {
        bail!("campaign stopped after evaluator-integrity failure");
    }
    Ok(())
}

fn bind_reservation_use(
    connection: &Connection,
    campaign_id: &str,
    reservation_id: &str,
    use_kind: &str,
    entity_key: &str,
) -> Result<()> {
    connection.execute(
        "INSERT INTO budget_reservation_uses
         (campaign_id, reservation_id, use_kind, entity_key, bound_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![campaign_id, reservation_id, use_kind, entity_key, now()],
    )?;
    Ok(())
}

fn require_reservation_use(
    connection: &Connection,
    campaign_id: &str,
    reservation_id: &str,
    use_kind: &str,
    entity_key: &str,
) -> Result<()> {
    let durable = connection
        .query_row(
            "SELECT use_kind, entity_key FROM budget_reservation_uses
             WHERE campaign_id=?1 AND reservation_id=?2",
            params![campaign_id, reservation_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if durable
        .as_ref()
        .map(|(kind, key)| (kind.as_str(), key.as_str()))
        != Some((use_kind, entity_key))
    {
        bail!("budget reservation is not durably bound to this exact entity");
    }
    Ok(())
}

/// Execute one WorkspaceOnly deterministic trial through an owned process.
/// Raw ownership and completion transitions remain crate-private, so external
/// callers cannot turn self-asserted PID strings or fabricated receipt bytes
/// into nomination evidence.
pub(crate) fn record_trial_intent(
    connection: &Connection,
    actor: &str,
    intent: &TrialIntent,
) -> Result<bool> {
    validate_trial_intent(intent)?;
    let command_json = serde_json::to_string(&intent.argv)?;
    let environment_json = serde_json::to_string(&intent.environment)?;
    let transaction = begin_mutation(connection)?;
    let durable_candidate = candidate_in(&transaction, &intent.candidate_id)?
        .with_context(|| format!("unknown candidate '{}'", intent.candidate_id))?;
    if durable_candidate.campaign_id != intent.campaign_id {
        bail!("trial campaign does not match candidate campaign");
    }
    let manifest = campaign_manifest(&transaction, &intent.campaign_id)?;
    validate_trial_against_manifest(intent, &manifest, &durable_candidate)?;
    let materialization = materialization_in(&transaction, &intent.candidate_id)?
        .context("trial candidate has no durable materialization")?;
    if materialization.receipt_sha256 != intent.materialization_receipt_sha256
        || materialization.result_tree != intent.result_tree
    {
        bail!("trial does not bind the candidate's exact materialization receipt and tree");
    }
    let baseline = materialization_by_receipt(
        &transaction,
        &intent.baseline_materialization_receipt_sha256,
    )?
    .context("trial has no durable baseline materialization")?;
    let baseline_candidate = candidate_in(&transaction, &baseline.candidate_id)?
        .context("baseline materialization candidate disappeared")?;
    if baseline_candidate.campaign_id != intent.campaign_id
        || baseline_candidate.material_sha256 != manifest.calibration.no_op_material_sha256().0
    {
        bail!("trial baseline is not the campaign's exact no-op materialization");
    }
    let expected_working_directory = canonical_or_pending_absolute(
        &Path::new(&materialization.worktree_locator).join(&manifest.evaluator.working_directory),
    )?;
    if intent.working_directory != expected_working_directory {
        bail!("trial working directory differs from its materialized candidate tree");
    }
    verify_trial_worktrees_live(
        &transaction,
        &manifest,
        &materialization,
        &baseline,
        &intent.working_directory,
    )?;
    if let Some(existing) = trial_in(&transaction, &intent.trial_id)? {
        let mut differing_fields = Vec::new();
        if existing.campaign_id != intent.campaign_id {
            differing_fields.push("campaign_id");
        }
        if existing.candidate_id != intent.candidate_id {
            differing_fields.push("candidate_id");
        }
        if existing.materialization_receipt_sha256 != intent.materialization_receipt_sha256 {
            differing_fields.push("materialization_receipt_sha256");
        }
        if existing.baseline_materialization_receipt_sha256
            != intent.baseline_materialization_receipt_sha256
        {
            differing_fields.push("baseline_materialization_receipt_sha256");
        }
        if existing.result_tree != intent.result_tree {
            differing_fields.push("result_tree");
        }
        if existing.working_directory != intent.working_directory {
            differing_fields.push("working_directory");
        }
        if existing.reservation_id != intent.reservation_id {
            differing_fields.push("reservation_id");
        }
        if existing.tier != intent.tier {
            differing_fields.push("tier");
        }
        if existing.argv != intent.argv {
            differing_fields.push("argv");
        }
        if existing.environment != intent.environment {
            differing_fields.push("environment");
        }
        if existing.owner_uuid.as_deref() != Some(intent.owner_uuid.as_str()) {
            differing_fields.push("owner_uuid");
        }
        if existing.supervisor_identity.as_deref() != Some(intent.supervisor_identity.as_str()) {
            differing_fields.push("supervisor_identity");
        }
        if differing_fields.is_empty() {
            require_reservation_use(
                &transaction,
                &intent.campaign_id,
                &intent.reservation_id,
                "trial",
                &intent.trial_id,
            )?;
            transaction.commit()?;
            return Ok(false);
        }
        bail!(
            "trial identity '{}' conflicts with durable state; differing fields: {}",
            intent.trial_id,
            differing_fields.join(", ")
        );
    }
    if !matches!(
        durable_candidate.disposition,
        CandidateDisposition::Materialized | CandidateDisposition::Evaluating
    ) {
        bail!("terminal candidate cannot admit another trial");
    }
    require_campaign_running(&transaction, &manifest)?;
    if let Some(tier) = manifest
        .holdouts
        .tiers
        .iter()
        .find(|tier| tier.key == intent.tier)
    {
        let existing: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM trials WHERE campaign_id=?1 AND tier=?2",
            params![intent.campaign_id, intent.tier],
            |row| row.get(0),
        )?;
        if u64::try_from(existing)? >= tier.maximum_disclosures {
            bail!(
                "holdout tier '{}' exhausted its frozen disclosure cap",
                intent.tier
            );
        }
    }
    require_trial_reservation(&transaction, intent, &manifest)?;
    bind_reservation_use(
        &transaction,
        &intent.campaign_id,
        &intent.reservation_id,
        "trial",
        &intent.trial_id,
    )?;
    transaction.execute(
        "INSERT INTO trials
         (trial_id, campaign_id, candidate_id, materialization_receipt_sha256,
          baseline_materialization_receipt_sha256, result_tree, working_directory,
          reservation_id, tier, command_json, environment_json, status, owner_uuid,
          supervisor_identity, heartbeat_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                 'owned', ?12, ?13, ?14, ?14)",
        params![
            intent.trial_id,
            intent.campaign_id,
            intent.candidate_id,
            intent.materialization_receipt_sha256,
            intent.baseline_materialization_receipt_sha256,
            intent.result_tree,
            intent.working_directory,
            intent.reservation_id,
            intent.tier,
            command_json,
            environment_json,
            intent.owner_uuid,
            intent.supervisor_identity,
            now()
        ],
    )?;
    transaction.execute(
        "UPDATE candidates SET disposition='evaluating', updated_at=?2
         WHERE candidate_id=?1 AND disposition IN ('proposed','materialized','evaluating')",
        params![intent.candidate_id, now()],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "trial",
        &intent.trial_id,
        "owned-intent-recorded",
        None,
        Some(&json!({"reservation_id": intent.reservation_id})),
    )?;
    transaction.commit()?;
    Ok(true)
}

pub(crate) fn mark_trial_launched(
    connection: &Connection,
    actor: &str,
    trial_id: &str,
    ownership: &TrialOwnership,
) -> Result<()> {
    validate_nonblank("owner_uuid", &ownership.owner_uuid)?;
    validate_nonblank("process_birth_identity", &ownership.process_birth_identity)?;
    validate_nonblank("supervisor_identity", &ownership.supervisor_identity)?;
    let transaction = begin_mutation(connection)?;
    let trial =
        trial_in(&transaction, trial_id)?.with_context(|| format!("unknown trial '{trial_id}'"))?;
    let manifest = campaign_manifest(&transaction, &trial.campaign_id)?;
    let materialization =
        materialization_by_receipt(&transaction, &trial.materialization_receipt_sha256)?
            .context("launched trial candidate materialization disappeared")?;
    let baseline =
        materialization_by_receipt(&transaction, &trial.baseline_materialization_receipt_sha256)?
            .context("launched trial baseline materialization disappeared")?;
    verify_trial_worktrees_live(
        &transaction,
        &manifest,
        &materialization,
        &baseline,
        &trial.working_directory,
    )?;
    if trial.status == TrialStatus::Launched {
        if trial.owner_uuid.as_deref() == Some(&ownership.owner_uuid)
            && trial.pid == Some(ownership.pid)
            && trial.process_birth_identity.as_deref() == Some(&ownership.process_birth_identity)
            && trial.supervisor_identity.as_deref() == Some(&ownership.supervisor_identity)
        {
            transaction.commit()?;
            return Ok(());
        }
        bail!("trial '{trial_id}' was launched with different process ownership");
    }
    if trial.status != TrialStatus::Owned {
        bail!("trial '{trial_id}' is already {}", trial.status);
    }
    let heartbeat = now();
    transaction.execute(
        "UPDATE trials SET status='launched', owner_uuid=?2, pid=?3,
         process_birth_identity=?4, supervisor_identity=?5, heartbeat_at=?6
         WHERE trial_id=?1",
        params![
            trial_id,
            ownership.owner_uuid,
            i64::from(ownership.pid),
            ownership.process_birth_identity,
            ownership.supervisor_identity,
            heartbeat
        ],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "trial",
        trial_id,
        "launched",
        None,
        Some(&serde_json::to_value(ownership)?),
    )?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn heartbeat_trial(
    connection: &Connection,
    actor: &str,
    trial_id: &str,
    owner_uuid: &str,
    supervisor_identity: &str,
) -> Result<()> {
    validate_nonblank("actor", actor)?;
    validate_nonblank("owner_uuid", owner_uuid)?;
    validate_nonblank("supervisor_identity", supervisor_identity)?;
    let transaction = begin_mutation(connection)?;
    let trial =
        trial_in(&transaction, trial_id)?.with_context(|| format!("unknown trial '{trial_id}'"))?;
    if trial.status != TrialStatus::Launched
        || trial.owner_uuid.as_deref() != Some(owner_uuid)
        || trial.supervisor_identity.as_deref() != Some(supervisor_identity)
    {
        bail!("trial heartbeat does not match one exact launched process owner");
    }
    let heartbeat = now();
    transaction.execute(
        "UPDATE trials SET heartbeat_at=?2 WHERE trial_id=?1",
        params![trial_id, heartbeat],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "trial",
        trial_id,
        "heartbeat",
        None,
        Some(&json!({
            "owner_uuid": owner_uuid,
            "supervisor_identity": supervisor_identity,
            "heartbeat_at": heartbeat,
        })),
    )?;
    transaction.commit()?;
    Ok(())
}

pub(crate) fn complete_deterministic_trial(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    trial_id: &str,
    completion: &TrialCompletion,
    credential: &TrialCompletionCredential,
) -> Result<Classification> {
    validate_nonblank("actor", actor)?;
    let receipt_bytes = read_object(object_root, &completion.receipt)?;
    let receipt: TrialReceipt = serde_json::from_slice(&receipt_bytes)
        .context("trial receipt is not canonical typed evaluator output")?;
    if serde_json::to_vec(&receipt)? != receipt_bytes {
        bail!("trial receipt must use canonical compact JSON serialization");
    }
    if receipt
        .reason_code
        .as_deref()
        .is_some_and(|reason| reason.trim().is_empty())
    {
        bail!("trial receipt reason_code must be absent or nonblank");
    }
    let durable_trial =
        trial(connection, trial_id)?.with_context(|| format!("unknown trial '{trial_id}'"))?;
    let manifest = campaign_manifest(connection, &durable_trial.campaign_id)?;
    validate_trial_receipt_schema(&durable_trial, &receipt, &manifest)?;
    validate_judge_build_receipt(object_root, &durable_trial, &receipt, &manifest)?;
    let materialization =
        materialization_by_receipt(connection, &durable_trial.materialization_receipt_sha256)?
            .context("completed trial candidate materialization disappeared")?;
    let baseline = materialization_by_receipt(
        connection,
        &durable_trial.baseline_materialization_receipt_sha256,
    )?
    .context("completed trial baseline materialization disappeared")?;
    verify_trial_worktrees_live(
        connection,
        &manifest,
        &materialization,
        &baseline,
        &durable_trial.working_directory,
    )?;
    validate_completion_binding(&durable_trial, &receipt, credential, &manifest)?;
    validate_completion_usage(
        connection,
        &durable_trial,
        &receipt,
        completion.receipt.bytes,
        &manifest,
    )?;
    let measured_usage = completion_usage(&receipt, completion.receipt.bytes)?;
    let classification =
        classify_deterministic(&manifest.objectives, &receipt.observations, false)?;
    validate_calibration_outcome(&durable_trial, &receipt, &classification, &manifest)?;
    let outcome = json!({
        "schema": "papertiger-mise.trial-outcome.v1",
        "receipt": completion.receipt,
        "classification": classification,
    });

    let transaction = begin_mutation(connection)?;
    let current =
        trial_in(&transaction, trial_id)?.context("trial disappeared before completion")?;
    if current.status == TrialStatus::Succeeded {
        if current.outcome.as_ref() == Some(&outcome) {
            settle_bound_budget_in(
                &transaction,
                actor,
                BoundReservation {
                    campaign_id: &current.campaign_id,
                    reservation_id: &current.reservation_id,
                    use_kind: "trial",
                    entity_key: trial_id,
                },
                SettlementMode::Measured,
                &measured_usage,
                Some("trial-completed"),
            )?;
            transaction.commit()?;
            return Ok(classification);
        }
        bail!("trial '{trial_id}' already completed with different evidence");
    }
    if current.status != TrialStatus::Launched {
        bail!(
            "trial '{trial_id}' cannot complete from status '{}'",
            current.status
        );
    }
    record_artifact_in(&transaction, &completion.receipt, "application/json")?;
    transaction.execute(
        "INSERT INTO candidate_artifacts (candidate_id, role, sha256)
         VALUES (?1, ?2, ?3)",
        params![
            current.candidate_id,
            format!("trial-evidence:{trial_id}"),
            completion.receipt.sha256
        ],
    )?;
    for (ordinal, observation) in receipt.observations.iter().enumerate() {
        transaction.execute(
            "INSERT INTO measurements (trial_id, ordinal, objective, raw_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                trial_id,
                i64::try_from(ordinal)?,
                observation.objective,
                serde_json::to_string(observation)?
            ],
        )?;
    }
    transaction.execute(
        "UPDATE trials SET status='succeeded', outcome_json=?2, finished_at=?3
         WHERE trial_id=?1",
        params![trial_id, serde_json::to_string(&outcome)?, now()],
    )?;
    transaction.execute(
        "UPDATE candidates SET disposition='evaluating', updated_at=?2
         WHERE candidate_id=?1 AND disposition IN ('proposed','materialized','evaluating')",
        params![current.candidate_id, now()],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "trial",
        trial_id,
        "completed",
        receipt.reason_code.as_deref(),
        Some(&outcome),
    )?;
    settle_bound_budget_in(
        &transaction,
        actor,
        BoundReservation {
            campaign_id: &current.campaign_id,
            reservation_id: &current.reservation_id,
            use_kind: "trial",
            entity_key: trial_id,
        },
        SettlementMode::Measured,
        &measured_usage,
        Some("trial-completed"),
    )?;
    transaction.commit()?;
    Ok(classification)
}

fn verify_trial_worktrees_live(
    connection: &Connection,
    manifest: &CampaignManifest,
    materialization: &MaterializationRecord,
    baseline: &MaterializationRecord,
    working_directory: &str,
) -> Result<()> {
    for record in [materialization, baseline] {
        let proposal: crate::candidate::CandidateProposal =
            serde_json::from_str(&proposal_json_for(connection, &record.candidate_id)?)?;
        verify_materialized_worktree(
            manifest,
            &proposal,
            record,
            Path::new(&record.worktree_locator),
        )?;
    }
    let candidate_root = std::fs::canonicalize(&materialization.worktree_locator)?;
    let working = std::fs::canonicalize(working_directory)
        .context("canonicalize evaluator working directory")?;
    let workspace_root = std::fs::canonicalize(&manifest.execution_limits.workspace_root_locator)?;
    if working != candidate_root && !working.starts_with(&candidate_root) {
        bail!("evaluator working directory resolves outside its candidate materialization");
    }
    if working == workspace_root || !working.starts_with(&workspace_root) {
        bail!("evaluator working directory resolves outside the frozen workspace root");
    }
    Ok(())
}

fn validate_completion_binding(
    trial: &TrialRecord,
    receipt: &TrialReceipt,
    credential: &TrialCompletionCredential,
    manifest: &CampaignManifest,
) -> Result<()> {
    if receipt.trial_id != trial.trial_id
        || receipt.campaign_id != trial.campaign_id
        || receipt.candidate_id != trial.candidate_id
        || receipt.materialization_receipt_sha256 != trial.materialization_receipt_sha256
        || receipt.baseline_materialization_receipt_sha256
            != trial.baseline_materialization_receipt_sha256
        || receipt.result_tree != trial.result_tree
        || receipt.working_directory != trial.working_directory
        || receipt.tier != trial.tier
    {
        bail!("trial receipt does not bind the exact durable trial identity");
    }
    if receipt.owner_uuid != credential.owner_uuid
        || receipt.supervisor_identity != credential.supervisor_identity
        || receipt.process_birth_identity != credential.process_birth_identity
        || trial.owner_uuid.as_deref() != Some(credential.owner_uuid.as_str())
        || trial.supervisor_identity.as_deref() != Some(credential.supervisor_identity.as_str())
        || trial.process_birth_identity.as_deref()
            != Some(credential.process_birth_identity.as_str())
    {
        bail!("trial receipt completion credential differs from durable process ownership");
    }
    if receipt.launcher_sha256 != manifest.evaluator.launcher_sha256.0
        || receipt.evaluator_sha256 != manifest.evaluator.evaluator_sha256.0
        || receipt.protocol != manifest.evaluator.protocol
    {
        bail!("trial completion differs from the frozen evaluator identity or protocol");
    }
    let expected_fixture = if trial.tier == "calibration.no_op" {
        &manifest.calibration.no_op.fixture_sha256.0
    } else if trial.tier == "calibration.known_bad" {
        &manifest.calibration.known_bad.fixture_sha256.0
    } else {
        &manifest
            .holdouts
            .tiers
            .iter()
            .find(|tier| tier.key == trial.tier)
            .with_context(|| format!("unknown trial tier '{}'", trial.tier))?
            .fixture_sha256
            .0
    };
    if &receipt.fixture_sha256 != expected_fixture {
        bail!("trial completion differs from the frozen fixture identity");
    }
    let observed_objectives = receipt
        .observations
        .iter()
        .map(|observation| observation.objective.as_str())
        .collect::<Vec<_>>();
    let expected_objectives = manifest
        .objectives
        .iter()
        .map(|objective| objective.key.as_str())
        .collect::<Vec<_>>();
    if observed_objectives != expected_objectives {
        bail!(
            "trial observations are not in the frozen canonical objective order; expected [{}], observed [{}]; emit one observation per admitted objective in exactly the expected order",
            expected_objectives.join(", "),
            observed_objectives.join(", ")
        );
    }
    Ok(())
}

fn validate_trial_receipt_schema(
    trial: &TrialRecord,
    receipt: &TrialReceipt,
    manifest: &CampaignManifest,
) -> Result<()> {
    match (
        &manifest.evaluator.rust_build_environment,
        &manifest.evaluator.judge_build,
    ) {
        (None, None) => {
            if receipt.schema != "papertiger-mise.trial-receipt.v1"
                || receipt.environment_sha256.is_some()
                || receipt.judge_build.is_some()
            {
                bail!("ordinary deterministic trials require an exact v1 receipt");
            }
        }
        (Some(_), None) => {
            if receipt.schema != "papertiger-mise.trial-receipt.v2" {
                bail!("Rust-build trials require an exact v2 environment-bound receipt");
            }
            let expected = sha256(&serde_json::to_vec(&trial.environment)?);
            if receipt.environment_sha256.as_deref() != Some(expected.as_str()) {
                bail!("trial receipt does not bind the exact runtime-owned Rust environment");
            }
            if receipt.judge_build.is_some() {
                bail!("v2 Rust-build trial receipt cannot contain a judge build");
            }
        }
        (_, Some(_)) => {
            if receipt.schema != "papertiger-mise.trial-receipt.v3" {
                bail!("judge-build trials require an exact v3 receipt");
            }
            let expected = sha256(&serde_json::to_vec(&trial.environment)?);
            if receipt.environment_sha256.as_deref() != Some(expected.as_str()) {
                bail!("judge-build receipt does not bind the exact runtime-owned environment");
            }
            if receipt.judge_build.is_none() {
                bail!("v3 trial receipt omitted its judge build");
            }
        }
    }
    Ok(())
}

fn validate_judge_build_receipt(
    object_root: &Path,
    trial: &TrialRecord,
    receipt: &TrialReceipt,
    manifest: &CampaignManifest,
) -> Result<()> {
    let Some(binding) = &manifest.evaluator.judge_build else {
        if receipt.judge_build.is_some() {
            bail!("trial receipt contains an unadmitted judge build");
        }
        return Ok(());
    };
    let build = receipt
        .judge_build
        .as_ref()
        .context("trial receipt omitted its frozen judge build")?;
    if build.schema != "papertiger-mise.judge-build-receipt.v1"
        || build.argv != binding.argv
        || build.toolchain_name != binding.toolchain_name
        || build.toolchain_version != binding.toolchain_version
        || build.toolchain_executable_sha256 != binding.toolchain_executable_sha256.0
        || build.environment_sha256 != sha256(&serde_json::to_vec(&trial.environment)?)
        || build.source_tree != trial.result_tree
        || build.executable_locator != binding.executable_locator
        || build.executable.bytes > binding.maximum_executable_bytes
    {
        bail!("judge build receipt differs from its frozen trial and manifest binding");
    }
    verify_object(object_root, &build.executable)
        .context("judge executable CAS object failed exact identity")
}

fn validate_completion_usage(
    connection: &Connection,
    trial: &TrialRecord,
    receipt: &TrialReceipt,
    receipt_bytes: u64,
    manifest: &CampaignManifest,
) -> Result<()> {
    let measured = completion_usage_map(receipt, receipt_bytes)?;
    let expected_artifact_bytes = receipt_bytes
        .checked_add(
            receipt
                .judge_build
                .as_ref()
                .map(|build| build.executable.bytes)
                .unwrap_or(0),
        )
        .context("retained trial artifact byte count overflow")?;
    if measured.get(&BudgetResource::Trials) != Some(&1)
        || measured.get(&BudgetResource::Failures) != Some(&0)
        || measured.get(&BudgetResource::ArtifactBytes) != Some(&expected_artifact_bytes)
    {
        bail!(
            "successful trial usage must charge one trial, zero failures, and exact retained receipt plus judge-executable bytes"
        );
    }
    let disclosure = measured.get(&BudgetResource::HoldoutDisclosures).copied();
    if trial.tier.starts_with("calibration.") {
        if disclosure.is_some() {
            bail!("calibration completion must not charge a holdout disclosure");
        }
    } else if disclosure != Some(1) {
        bail!("holdout completion must charge exactly one disclosure");
    }
    let wall = measured
        .get(&BudgetResource::WallTimeMilliseconds)
        .copied()
        .unwrap_or(0);
    if wall == 0 || wall > manifest.execution_limits.maximum_trial_wall_time_ms {
        bail!("measured trial wall time is absent or exceeds the instantaneous limit");
    }
    let reserved = reservation_rows_for_use(connection, &trial.campaign_id, &trial.reservation_id)?;
    if measured.keys().copied().collect::<BTreeSet<_>>()
        != reserved.keys().copied().collect::<BTreeSet<_>>()
    {
        bail!("trial completion must measure every reserved resource exactly once");
    }
    for (resource, amount) in measured {
        if amount > reserved[&resource].0 {
            bail!("measured {resource} exceeds its durable reservation");
        }
    }
    Ok(())
}

fn completion_usage(receipt: &TrialReceipt, receipt_bytes: u64) -> Result<Vec<BudgetSettlement>> {
    Ok(completion_usage_map(receipt, receipt_bytes)?
        .into_iter()
        .map(|(resource, actual_amount)| BudgetSettlement {
            resource,
            actual_amount,
        })
        .collect())
}

fn completion_usage_map(
    receipt: &TrialReceipt,
    receipt_bytes: u64,
) -> Result<BTreeMap<BudgetResource, u64>> {
    let mut measured = BTreeMap::new();
    for settlement in &receipt.measured_usage {
        if settlement.resource == BudgetResource::ArtifactBytes {
            bail!("trial receipt artifact-byte use is derived from its exact retained bytes");
        }
        if measured
            .insert(settlement.resource, settlement.actual_amount)
            .is_some()
        {
            bail!("trial receipt repeats a measured budget resource");
        }
    }
    let retained_artifact_bytes = receipt_bytes
        .checked_add(
            receipt
                .judge_build
                .as_ref()
                .map(|build| build.executable.bytes)
                .unwrap_or(0),
        )
        .context("retained trial artifact byte count overflow")?;
    measured.insert(BudgetResource::ArtifactBytes, retained_artifact_bytes);
    Ok(measured)
}

pub(crate) fn record_integrity_failure(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    trial_id: &str,
    failure: &IntegrityFailure,
) -> Result<()> {
    validate_nonblank("integrity_failure.reason_code", &failure.reason_code)?;
    verify_object(object_root, &failure.evidence)?;
    let durable_trial =
        trial(connection, trial_id)?.with_context(|| format!("unknown trial '{trial_id}'"))?;
    let manifest = campaign_manifest(connection, &durable_trial.campaign_id)?;
    let expected_fixture = expected_fixture_sha256(&manifest, &durable_trial.tier)?;
    if failure.expected_outer_judge_sha256 != manifest.generation.outer_judge_executable_sha256.0
        || failure.expected_launcher_sha256 != manifest.evaluator.launcher_sha256.0
        || failure.expected_evaluator_sha256 != manifest.evaluator.evaluator_sha256.0
        || failure.expected_fixture_sha256 != expected_fixture
    {
        bail!("integrity failure does not bind the frozen expected judge identities");
    }
    let expected_inputs = frozen_judge_input_bindings(&manifest, &durable_trial.tier)?;
    let mut mismatch_roles = BTreeSet::new();
    for mismatch in &failure.frozen_input_mismatches {
        validate_nonblank(
            "integrity_failure.frozen_input_mismatches.role",
            &mismatch.role,
        )?;
        let expected = expected_inputs
            .iter()
            .find(|binding| binding.role == mismatch.role)
            .with_context(|| {
                format!(
                    "integrity failure names unknown frozen input role '{}'",
                    mismatch.role
                )
            })?;
        if mismatch.locator != expected.locator
            || mismatch.expected_identity != expected.expected_identity
        {
            bail!(
                "integrity failure frozen input '{}' does not bind its admitted locator and identity",
                mismatch.role
            );
        }
        if mismatch.observed_identity == mismatch.expected_identity {
            bail!(
                "integrity failure frozen input '{}' has no observed mismatch",
                mismatch.role
            );
        }
        if !mismatch_roles.insert(mismatch.role.as_str()) {
            bail!(
                "integrity failure repeats frozen input role '{}'",
                mismatch.role
            );
        }
    }
    let canonical_mismatch = failure.observed_outer_judge_sha256
        != failure.expected_outer_judge_sha256
        || failure.observed_launcher_sha256 != failure.expected_launcher_sha256
        || failure.observed_evaluator_sha256 != failure.expected_evaluator_sha256
        || failure.observed_fixture_sha256 != failure.expected_fixture_sha256;
    if !canonical_mismatch && failure.frozen_input_mismatches.is_empty() {
        bail!("integrity failure must contain an actual frozen judge mismatch");
    }
    let reservation = reservation_rows_for_use(
        connection,
        &durable_trial.campaign_id,
        &durable_trial.reservation_id,
    )?;
    if failure.evidence.bytes
        > reservation
            .get(&BudgetResource::ArtifactBytes)
            .map(|row| row.0)
            .unwrap_or(0)
    {
        bail!("integrity evidence exceeds the trial artifact reservation");
    }
    let outcome = json!({
        "schema": "papertiger-mise.integrity-failure.v2",
        "failure": failure,
    });
    if !durable_trial.status.has_live_ownership()
        && durable_trial.status != TrialStatus::IntegrityFailed
    {
        bail!(
            "trial '{trial_id}' cannot record integrity failure from status '{}'",
            durable_trial.status
        );
    }
    let note = format!("integrity-failure:{}", failure.evidence.sha256);
    let transaction = begin_mutation(connection)?;
    let current = trial_in(&transaction, trial_id)?.context("trial disappeared")?;
    if current.status == TrialStatus::IntegrityFailed {
        if current.outcome.as_ref() == Some(&outcome) {
            settle_bound_budget_in(
                &transaction,
                actor,
                BoundReservation {
                    campaign_id: &current.campaign_id,
                    reservation_id: &current.reservation_id,
                    use_kind: "trial",
                    entity_key: trial_id,
                },
                SettlementMode::ChargeReservation,
                &[],
                Some(&note),
            )?;
            transaction.commit()?;
            return Ok(());
        }
        bail!("trial '{trial_id}' already has different integrity-failure evidence");
    }
    if !current.status.has_live_ownership() {
        bail!(
            "trial '{trial_id}' cannot record integrity failure from status '{}'",
            current.status
        );
    }
    record_artifact_in(&transaction, &failure.evidence, "application/json")?;
    settle_bound_budget_in(
        &transaction,
        actor,
        BoundReservation {
            campaign_id: &current.campaign_id,
            reservation_id: &current.reservation_id,
            use_kind: "trial",
            entity_key: trial_id,
        },
        SettlementMode::ChargeReservation,
        &[],
        Some(&note),
    )?;
    transaction.execute(
        "UPDATE trials SET status='integrity_failed', outcome_json=?2, finished_at=?3
         WHERE trial_id=?1",
        params![trial_id, serde_json::to_string(&outcome)?, now()],
    )?;
    transaction.execute(
        "UPDATE candidates SET disposition='infrastructure_failed', result_json=?2, updated_at=?3
         WHERE candidate_id=?1 AND disposition='evaluating'",
        params![
            current.candidate_id,
            serde_json::to_string(&outcome)?,
            now()
        ],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "trial",
        trial_id,
        "integrity-failed",
        Some(&failure.reason_code),
        Some(&outcome),
    )?;
    transaction.commit()?;
    Ok(())
}

fn expected_fixture_binding(manifest: &CampaignManifest, tier: &str) -> Result<(String, String)> {
    if tier == "calibration.no_op" {
        Ok((
            manifest.calibration.no_op.fixture_locator.clone(),
            manifest.calibration.no_op.fixture_sha256.0.clone(),
        ))
    } else if tier == "calibration.known_bad" {
        Ok((
            manifest.calibration.known_bad.fixture_locator.clone(),
            manifest.calibration.known_bad.fixture_sha256.0.clone(),
        ))
    } else {
        let fixture = manifest
            .holdouts
            .tiers
            .iter()
            .find(|declared| declared.key == tier)
            .with_context(|| format!("unknown trial tier '{tier}'"))?;
        Ok((
            fixture.fixture_locator.clone(),
            fixture.fixture_sha256.0.clone(),
        ))
    }
}

fn expected_fixture_sha256(manifest: &CampaignManifest, tier: &str) -> Result<String> {
    Ok(expected_fixture_binding(manifest, tier)?.1)
}

mod adjudication;
pub(crate) use adjudication::verified_trial_receipt;
pub use adjudication::{
    adjudicate_deterministic_candidate, verify_candidate_integrity, verify_nomination_integrity,
};
use adjudication::{
    require_deterministic_runtime, validate_calibration_outcome, verify_completed_trial_evidence,
};

/// Reconcile a launch whose exact child process is proven absent. Budget,
/// evidence, and the trial transition commit atomically.
mod trial_recovery;
use trial_recovery::reconcile_lost_trial;
pub use trial_recovery::{abandon_owned_trial, recover_workspace_trial};

pub fn trial(connection: &Connection, trial_id: &str) -> Result<Option<TrialRecord>> {
    trial_in(connection, trial_id)
}

pub(crate) fn materialization(
    connection: &Connection,
    candidate_id: &str,
) -> Result<Option<MaterializationRecord>> {
    materialization_in(connection, candidate_id)
}

fn candidate_in(connection: &Connection, candidate_id: &str) -> Result<Option<CandidateRecord>> {
    connection
        .query_row(
            "SELECT candidate_id, campaign_id, patch_sha256, negative_fingerprint,
                    disposition, result_json FROM candidates WHERE candidate_id=?1",
            params![candidate_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?
        .map(
            |(
                candidate_id,
                campaign_id,
                material_sha256,
                negative_fingerprint,
                disposition,
                result,
            )| {
                Ok(CandidateRecord {
                    candidate_id,
                    campaign_id,
                    material_sha256,
                    negative_fingerprint,
                    disposition: parse_disposition(&disposition)?,
                    result: result
                        .map(|value| serde_json::from_str(&value))
                        .transpose()?,
                })
            },
        )
        .transpose()
}

fn proposal_json_for(connection: &Connection, candidate_id: &str) -> Result<String> {
    Ok(connection.query_row(
        "SELECT proposal_json FROM candidates WHERE candidate_id=?1",
        params![candidate_id],
        |row| row.get(0),
    )?)
}

fn materialization_in(
    connection: &Connection,
    candidate_id: &str,
) -> Result<Option<MaterializationRecord>> {
    Ok(connection
        .query_row(
            "SELECT candidate_id, reservation_id, receipt_sha256, result_tree, worktree_locator
             FROM materializations WHERE candidate_id=?1",
            params![candidate_id],
            |row| {
                Ok(MaterializationRecord {
                    candidate_id: row.get(0)?,
                    reservation_id: row.get(1)?,
                    receipt_sha256: row.get(2)?,
                    result_tree: row.get(3)?,
                    worktree_locator: row.get(4)?,
                })
            },
        )
        .optional()?)
}

fn materialization_by_receipt(
    connection: &Connection,
    receipt_sha256: &str,
) -> Result<Option<MaterializationRecord>> {
    Ok(connection
        .query_row(
            "SELECT candidate_id, reservation_id, receipt_sha256, result_tree, worktree_locator
             FROM materializations WHERE receipt_sha256=?1",
            params![receipt_sha256],
            |row| {
                Ok(MaterializationRecord {
                    candidate_id: row.get(0)?,
                    reservation_id: row.get(1)?,
                    receipt_sha256: row.get(2)?,
                    result_tree: row.get(3)?,
                    worktree_locator: row.get(4)?,
                })
            },
        )
        .optional()?)
}

fn nomination_in(connection: &Connection, candidate_id: &str) -> Result<Option<NominationRecord>> {
    Ok(connection
        .query_row(
            "SELECT nomination_id, campaign_id, candidate_id, receipt_sha256, receipt_json, created_at
             FROM nominations WHERE candidate_id=?1",
            params![candidate_id],
            nomination_from_row,
        )
        .optional()?)
}

pub fn nominations(
    connection: &Connection,
    campaign_id: Option<&str>,
) -> Result<Vec<NominationRecord>> {
    let query = match campaign_id {
        Some(_) => {
            "SELECT nomination_id, campaign_id, candidate_id, receipt_sha256, receipt_json, created_at
             FROM nominations WHERE campaign_id=?1 ORDER BY created_at, nomination_id"
        }
        None => {
            "SELECT nomination_id, campaign_id, candidate_id, receipt_sha256, receipt_json, created_at
             FROM nominations ORDER BY created_at, nomination_id"
        }
    };
    let mut statement = connection.prepare(query)?;
    let records = match campaign_id {
        Some(campaign_id) => statement
            .query_map(params![campaign_id], nomination_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        None => statement
            .query_map([], nomination_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    };
    Ok(records)
}

fn nomination_by_id(
    connection: &Connection,
    nomination_id: &str,
) -> Result<Option<NominationRecord>> {
    Ok(connection
        .query_row(
            "SELECT nomination_id, campaign_id, candidate_id, receipt_sha256, receipt_json, created_at
             FROM nominations WHERE nomination_id=?1",
            params![nomination_id],
            nomination_from_row,
        )
        .optional()?)
}

fn nomination_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NominationRecord> {
    Ok(NominationRecord {
        nomination_id: row.get(0)?,
        campaign_id: row.get(1)?,
        candidate_id: row.get(2)?,
        receipt_sha256: row.get(3)?,
        receipt_json: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn trial_in(connection: &Connection, trial_id: &str) -> Result<Option<TrialRecord>> {
    connection
        .query_row(
            "SELECT trial_id, campaign_id, candidate_id, materialization_receipt_sha256,
                    baseline_materialization_receipt_sha256, result_tree, working_directory,
                    reservation_id, tier, command_json, environment_json, status, owner_uuid,
                    pid, process_birth_identity, supervisor_identity, heartbeat_at, outcome_json
             FROM trials WHERE trial_id=?1",
            params![trial_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<i64>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<String>>(17)?,
                ))
            },
        )
        .optional()?
        .map(|row| {
            Ok(TrialRecord {
                trial_id: row.0,
                campaign_id: row.1,
                candidate_id: row.2,
                materialization_receipt_sha256: row.3,
                baseline_materialization_receipt_sha256: row.4,
                result_tree: row.5,
                working_directory: row.6,
                reservation_id: row.7,
                tier: row.8,
                argv: serde_json::from_str(&row.9)?,
                environment: serde_json::from_str(&row.10)?,
                status: TrialStatus::parse_column("trials.status", &row.11)?,
                owner_uuid: row.12,
                pid: row.13.map(u32::try_from).transpose()?,
                process_birth_identity: row.14,
                supervisor_identity: row.15,
                heartbeat_at: row.16,
                outcome: row
                    .17
                    .map(|value| serde_json::from_str(&value))
                    .transpose()?,
            })
        })
        .transpose()
}

fn validate_trial_intent(intent: &TrialIntent) -> Result<()> {
    for (field, value) in [
        ("trial_id", intent.trial_id.as_str()),
        ("campaign_id", intent.campaign_id.as_str()),
        ("candidate_id", intent.candidate_id.as_str()),
        ("reservation_id", intent.reservation_id.as_str()),
        ("tier", intent.tier.as_str()),
        ("owner_uuid", intent.owner_uuid.as_str()),
        ("supervisor_identity", intent.supervisor_identity.as_str()),
    ] {
        validate_nonblank(field, value)?;
    }
    if intent.argv.is_empty() || intent.argv.iter().any(String::is_empty) {
        bail!("trial argv must contain nonempty exact arguments");
    }
    if !intent.environment.is_object() {
        bail!("trial environment must be a JSON object");
    }
    Ok(())
}

fn disposition_str(disposition: CandidateDisposition) -> &'static str {
    match disposition {
        CandidateDisposition::Proposed => "proposed",
        CandidateDisposition::Materialized => "materialized",
        CandidateDisposition::Evaluating => "evaluating",
        CandidateDisposition::Rejected => "rejected",
        CandidateDisposition::Inconclusive => "inconclusive",
        CandidateDisposition::InfrastructureFailed => "infrastructure_failed",
        CandidateDisposition::Nominated => "nominated",
    }
}

fn parse_disposition(value: &str) -> Result<CandidateDisposition> {
    Ok(match value {
        "proposed" => CandidateDisposition::Proposed,
        "materialized" => CandidateDisposition::Materialized,
        "evaluating" => CandidateDisposition::Evaluating,
        "rejected" => CandidateDisposition::Rejected,
        "inconclusive" => CandidateDisposition::Inconclusive,
        "infrastructure_failed" => CandidateDisposition::InfrastructureFailed,
        "nominated" => CandidateDisposition::Nominated,
        _ => bail!("unknown stored candidate disposition '{value}'"),
    })
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
