use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::adapter::{
    DomainTrialResult, PairedExecutionParticipants, PairedParticipantRole, PairedTrialRequest,
    execute_paired_adapter_cohort_with, parse_validate_paired_trial_result,
    prepare_paired_adapter_cohort, verify_paired_adapter_runtime,
};
use crate::budget::{
    BoundReservation, BudgetResource, BudgetSettlement, SettlementMode, settle_bound_budget_in,
};
use crate::executor::{
    ExecutionCapabilities, SupervisedExecutionOutcome, SupervisionHooks, execute_supervised,
};
use crate::lifecycle::{NominationRecord, VerifiedNominationEvidence};
use crate::manifest::{CampaignManifest, ContainmentGrade, Sha256Digest};
use crate::object::{
    PreservedObject, indexed_object, preserve_object, read_object, read_verified_json,
    record_indexed_object,
};
use crate::process_identity::{ProcessObservation, observe_process};
use crate::state::{EvidenceGrade, PairedCohortReasonCode, PairedCohortStatus, PairedRunStatus};
use crate::statistics::{
    NoOpCalibrationResult, PairedCandidateContext, PairedClassification, PairedCohort,
    PairedDisposition, assess_known_bad_cohort, assess_no_op_cohort, classify_paired_fixed,
    paired_analysis_slot,
};
use crate::store::{begin_mutation, campaign, now, record_event_in_mutation};
use crate::validation::validate_bounded_token as validate_token;

pub const PAIRED_EXECUTION_RECEIPT_SCHEMA_V1: &str = "papertiger-mise.paired-execution-receipt.v1";
pub const PAIRED_COHORT_RECEIPT_SCHEMA_V1: &str = "papertiger-mise.paired-cohort-receipt.v1";
pub const PAIRED_NOMINATION_RECEIPT_SCHEMA_V1: &str = "papertiger-mise.paired-nomination.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivePairedNominationSpec {
    pub research_cohort_id: String,
    pub no_op_cohort_id: String,
    pub known_bad_cohort_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparePairedCohortSpec {
    pub cohort_id: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub cohort: PairedCohort,
    pub revealed_order_seed: Vec<u8>,
    pub participants: PairedExecutionParticipants,
    pub reservation_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedPreparationOutcome {
    Prepared,
    Existing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedCohortRecord {
    pub cohort_id: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub cohort: PairedCohort,
    pub schedule_sha256: Sha256Digest,
    pub participants: PairedExecutionParticipants,
    pub reservation_id: String,
    pub adapter_binding_sha256: Sha256Digest,
    pub status: PairedCohortStatus,
    pub result_sha256: Option<String>,
    pub receipt_sha256: Option<String>,
    pub reason_code: Option<String>,
    pub recorded_by: String,
    pub created_at: String,
    pub finished_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedRunRecord {
    pub execution_id: String,
    pub cohort_id: String,
    pub ordinal: u32,
    pub block_index: u32,
    pub participant: PairedParticipantRole,
    pub request_sha256: String,
    pub status: PairedRunStatus,
    pub pid: Option<u32>,
    pub process_birth_identity: Option<String>,
    pub adapter_result_sha256: Option<String>,
    pub domain_receipt_sha256: Option<String>,
    pub execution_receipt_sha256: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub failure_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedRunOutcome {
    Completed(Box<PairedRunRecord>),
    ReadyForAdjudication,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedExecutionReceipt {
    pub schema: String,
    pub cohort_id: String,
    pub execution_id: String,
    pub request: PreservedObject,
    pub adapter_result: PreservedObject,
    pub domain_receipt: PreservedObject,
    pub adapter_binding_sha256: Sha256Digest,
    pub process_birth_identity: String,
    pub elapsed_ms: u64,
    pub execution_capabilities: ExecutionCapabilities,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairedFailureReceipt {
    schema: String,
    cohort_id: String,
    execution_id: String,
    failure_code: String,
    detail: String,
    process_birth_identity: Option<String>,
    elapsed_ms: u64,
    stdout: PreservedObject,
    stderr: PreservedObject,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "result", rename_all = "snake_case")]
pub enum PairedCohortAdjudication {
    Research(PairedClassification),
    NoOpCalibration(NoOpCalibrationResult),
    KnownBadCalibration(PairedClassification),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedCohortReceipt {
    pub schema: String,
    pub cohort_id: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub schedule_sha256: Sha256Digest,
    pub execution_receipts: Vec<PreservedObject>,
    pub result: PreservedObject,
    pub reason_code: PairedCohortReasonCode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedPairedCohortEvidence {
    pub cohort: PairedCohortRecord,
    pub adjudication: PairedCohortAdjudication,
    pub receipt: PreservedObject,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairedNominationReceipt {
    schema: String,
    campaign_id: String,
    manifest_sha256: String,
    candidate_id: String,
    evidence_grade: EvidenceGrade,
    no_op_cohort_id: String,
    known_bad_cohort_id: String,
    research_cohort_id: String,
    result: Value,
    created_at: String,
}

#[derive(Clone, Debug)]
struct DurableCohort {
    record: PairedCohortRecord,
    revealed_order_seed: Vec<u8>,
}

struct ReplayedCohort {
    adjudication: PairedCohortAdjudication,
    status: PairedCohortStatus,
    reason_code: PairedCohortReasonCode,
    execution_receipts: Vec<PreservedObject>,
    elapsed_total: u64,
    run_count: usize,
}

struct CompletedRunEvidence<'a> {
    request: &'a PreservedObject,
    result_bytes: &'a [u8],
    result: &'a DomainTrialResult,
    elapsed_ms: u64,
    process_birth_identity: &'a str,
    capabilities: &'a ExecutionCapabilities,
}

struct ReopenedPairedRun {
    result: DomainTrialResult,
    execution_receipt: PreservedObject,
    elapsed_ms: u64,
}

fn reopen_paired_run(
    connection: &Connection,
    object_root: &Path,
    plan: &crate::statistics::PairedAnalysisPlan,
    durable: &DurableCohort,
    run: &PairedRunRecord,
    expected_request: &PairedTrialRequest,
) -> Result<ReopenedPairedRun> {
    let request_object = indexed_object(connection, &run.request_sha256)?;
    let (request, _): (PairedTrialRequest, Vec<u8>) =
        read_verified_json(object_root, &request_object, "paired run request")?;
    if &request != expected_request {
        bail!("paired run request differs from its exact regenerated schedule");
    }
    let result_object = indexed_object(
        connection,
        run.adapter_result_sha256
            .as_deref()
            .context("successful paired run lost its adapter result")?,
    )?;
    let result_bytes = read_object(object_root, &result_object)?;
    let (_, result) = parse_validate_paired_trial_result(
        plan.trial_adapter
            .as_ref()
            .context("paired adapter disappeared")?,
        &request,
        &result_bytes,
    )?;
    let domain_object = indexed_object(
        connection,
        run.domain_receipt_sha256
            .as_deref()
            .context("successful paired run lost its domain receipt")?,
    )?;
    let (domain_receipt, _): (Value, Vec<u8>) =
        read_verified_json(object_root, &domain_object, "paired domain receipt")?;
    if domain_receipt != result.domain_trial_receipt {
        bail!("paired domain receipt failed exact CAS rederivation");
    }
    let execution_object = indexed_object(
        connection,
        run.execution_receipt_sha256
            .as_deref()
            .context("successful paired run lost its Mise execution receipt")?,
    )?;
    let (receipt, _): (PairedExecutionReceipt, Vec<u8>) =
        read_verified_json(object_root, &execution_object, "paired execution receipt")?;
    if receipt.cohort_id != durable.record.cohort_id
        || receipt.execution_id != run.execution_id
        || receipt.request != request_object
        || receipt.adapter_result != result_object
        || receipt.domain_receipt != domain_object
        || receipt.adapter_binding_sha256 != durable.record.adapter_binding_sha256
    {
        bail!("Mise paired execution receipt failed exact replay verification");
    }
    Ok(ReopenedPairedRun {
        result,
        execution_receipt: execution_object,
        elapsed_ms: receipt.elapsed_ms,
    })
}

#[allow(clippy::too_many_arguments)]
fn classify_replayed_cohort(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    durable: &DurableCohort,
    manifest: &CampaignManifest,
    plan: &crate::statistics::PairedAnalysisPlan,
    candidate_context: &PairedCandidateContext<'_>,
    blocks: &[crate::statistics::PairedBlockObservation],
    retain_calibration_failure: bool,
) -> Result<(
    PairedCohortAdjudication,
    PairedCohortStatus,
    PairedCohortReasonCode,
)> {
    match durable.record.cohort {
        PairedCohort::Research { .. } => {
            let result =
                classify_paired_fixed(plan, &manifest.objectives, candidate_context, blocks)?;
            let (status, reason) = match result.disposition {
                PairedDisposition::Qualified => (
                    PairedCohortStatus::Qualified,
                    PairedCohortReasonCode::ResearchQualified,
                ),
                PairedDisposition::Rejected => (
                    PairedCohortStatus::Rejected,
                    PairedCohortReasonCode::ResearchRejected,
                ),
                PairedDisposition::Inconclusive => (
                    PairedCohortStatus::Inconclusive,
                    PairedCohortReasonCode::ResearchInconclusive,
                ),
            };
            Ok((PairedCohortAdjudication::Research(result), status, reason))
        }
        PairedCohort::NoOpCalibration => {
            let result =
                assess_no_op_cohort(plan, &manifest.objectives, candidate_context, blocks)?;
            if !result.passed {
                if retain_calibration_failure {
                    terminal_cohort_failure(
                        connection,
                        actor,
                        object_root,
                        &durable.record,
                        PairedCohortReasonCode::NoOpCalibrationFailed,
                        &serde_json::to_value(&result)?,
                    )?;
                }
                bail!("no-op calibration failed; retained cohort is integrity_failed");
            }
            Ok((
                PairedCohortAdjudication::NoOpCalibration(result),
                PairedCohortStatus::Calibrated,
                PairedCohortReasonCode::NoOpCalibrationPassed,
            ))
        }
        PairedCohort::KnownBadCalibration => {
            let result = match assess_known_bad_cohort(
                plan,
                &manifest.objectives,
                candidate_context,
                blocks,
            ) {
                Ok(result) => result,
                Err(error) => {
                    if retain_calibration_failure {
                        terminal_cohort_failure(
                            connection,
                            actor,
                            object_root,
                            &durable.record,
                            PairedCohortReasonCode::KnownBadCalibrationNotRejected,
                            &json!({"error": format!("{error:#}")}),
                        )?;
                    }
                    bail!("known-bad calibration failed; retained cohort is integrity_failed");
                }
            };
            Ok((
                PairedCohortAdjudication::KnownBadCalibration(result),
                PairedCohortStatus::Rejected,
                PairedCohortReasonCode::KnownBadCalibrationRejected,
            ))
        }
    }
}

mod preparation;
pub use preparation::prepare_paired_cohort;

pub fn execute_next_paired_run(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    cohort_id: &str,
) -> Result<PairedRunOutcome> {
    validate_token("paired cohort actor", actor)?;
    let durable = durable_cohort(connection, cohort_id)?
        .with_context(|| format!("unknown paired cohort '{cohort_id}'"))?;
    if !durable.record.status.is_active() {
        bail!(
            "paired cohort '{cohort_id}' is terminal in status '{}'",
            durable.record.status
        );
    }
    if let Some(launched) = next_run_with_status(connection, cohort_id, PairedRunStatus::Launched)?
    {
        bail!(
            "paired run '{}' has durable live ownership; run `papertiger-mise paired recover {}` before continuing",
            launched.execution_id,
            launched.execution_id
        );
    }
    let Some(run) = next_run_with_status(connection, cohort_id, PairedRunStatus::Prepared)? else {
        return Ok(PairedRunOutcome::ReadyForAdjudication);
    };
    let manifest = load_manifest(connection, &durable.record.campaign_id)?;
    let binding = manifest
        .paired_analysis
        .as_ref()
        .and_then(|plan| plan.trial_adapter.as_ref())
        .context("durable paired campaign lost its trial adapter")?;
    let request_object = indexed_object(connection, &run.request_sha256)?;
    let request_bytes = read_object(object_root, &request_object)?;
    let request: PairedTrialRequest = serde_json::from_slice(&request_bytes)?;
    if canonical_json_bytes(&request)? != request_bytes || request.execution_id != run.execution_id
    {
        return terminal_run_failure(
            connection,
            actor,
            object_root,
            &durable.record,
            &run,
            PairedRunStatus::IntegrityFailed,
            "prepared-request-integrity-failed",
            "prepared paired request failed exact CAS replay",
            None,
            1,
            &[],
            &[],
        );
    }
    if let Err(error) = verify_paired_adapter_runtime(binding) {
        return terminal_run_failure(
            connection,
            actor,
            object_root,
            &durable.record,
            &run,
            PairedRunStatus::IntegrityFailed,
            "adapter-binding-drift",
            &format!("{error:#}"),
            None,
            1,
            &[],
            &[],
        );
    }
    let mut launched = |pid, process_birth_identity: &str| {
        mark_paired_run_launched(
            connection,
            actor,
            cohort_id,
            &run.execution_id,
            pid,
            process_birth_identity,
        )
    };
    let mut heartbeat = || heartbeat_paired_run(connection, actor, &run.execution_id);
    let execution = execute_supervised(
        &binding.argv,
        Path::new(&binding.working_directory),
        &binding.environment,
        &request_bytes,
        binding.maximum_wall_time_ms,
        binding.maximum_output_bytes,
        Some(SupervisionHooks {
            launched: &mut launched,
            heartbeat: &mut heartbeat,
        }),
    )?;
    let execution = match execution {
        SupervisedExecutionOutcome::Completed(execution) => execution,
        SupervisedExecutionOutcome::Failed(failure) => {
            return terminal_run_failure(
                connection,
                actor,
                object_root,
                &durable.record,
                &run,
                PairedRunStatus::InfrastructureFailed,
                failure.reason,
                &failure.detail,
                failure.process_birth_identity.as_deref(),
                failure.elapsed_ms,
                &failure.stdout,
                &failure.stderr,
            );
        }
    };
    if let Err(error) = verify_paired_adapter_runtime(binding) {
        return terminal_run_failure(
            connection,
            actor,
            object_root,
            &durable.record,
            &run,
            PairedRunStatus::IntegrityFailed,
            "adapter-binding-drift",
            &format!("{error:#}"),
            Some(&execution.process_birth_identity),
            execution.elapsed_ms,
            &execution.stdout,
            &execution.stderr,
        );
    }
    if !execution.stderr.is_empty() {
        return terminal_run_failure(
            connection,
            actor,
            object_root,
            &durable.record,
            &run,
            PairedRunStatus::InfrastructureFailed,
            "unexpected-adapter-stderr",
            "successful paired adapter wrote to stderr",
            Some(&execution.process_birth_identity),
            execution.elapsed_ms,
            &execution.stdout,
            &execution.stderr,
        );
    }
    let (result_bytes, result) =
        match parse_validate_paired_trial_result(binding, &request, &execution.stdout) {
            Ok(result) => result,
            Err(error) => {
                return terminal_run_failure(
                    connection,
                    actor,
                    object_root,
                    &durable.record,
                    &run,
                    PairedRunStatus::IntegrityFailed,
                    "adapter-result-integrity-failed",
                    &format!("{error:#}"),
                    Some(&execution.process_birth_identity),
                    execution.elapsed_ms,
                    &execution.stdout,
                    &execution.stderr,
                );
            }
        };
    if let Err(error) = complete_paired_run(
        connection,
        actor,
        object_root,
        &durable.record,
        &run,
        &CompletedRunEvidence {
            request: &request_object,
            result_bytes: &result_bytes,
            result: &result,
            elapsed_ms: execution.elapsed_ms,
            process_birth_identity: &execution.process_birth_identity,
            capabilities: &execution.capabilities,
        },
    ) {
        return terminal_run_failure(
            connection,
            actor,
            object_root,
            &durable.record,
            &run,
            PairedRunStatus::IntegrityFailed,
            "execution-evidence-commit-failed",
            &format!("{error:#}"),
            Some(&execution.process_birth_identity),
            execution.elapsed_ms,
            &execution.stdout,
            &execution.stderr,
        );
    }
    Ok(PairedRunOutcome::Completed(Box::new(
        paired_run(connection, &run.execution_id)?.context("completed paired run disappeared")?,
    )))
}

fn replay_paired_cohort(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    durable: &DurableCohort,
    manifest: &CampaignManifest,
    retain_calibration_failure: bool,
) -> Result<ReplayedCohort> {
    let cohort_id = &durable.record.cohort_id;
    let plan = manifest
        .paired_analysis
        .as_ref()
        .context("durable campaign lost its paired analysis contract")?;
    let candidate_identity = Sha256Digest(durable.record.candidate_id.clone());
    let candidate_context = PairedCandidateContext {
        cohort: durable.record.cohort,
        candidate_identity_sha256: &candidate_identity,
        revealed_order_seed: &durable.revealed_order_seed,
    };
    require_calibration_authority(connection, &durable.record)?;
    let prepared = prepare_paired_adapter_cohort(
        cohort_id,
        plan,
        &manifest.objectives,
        &candidate_context,
        &durable.record.participants,
    )?;
    if prepared.schedule_sha256 != durable.record.schedule_sha256 {
        bail!("durable paired cohort schedule failed exact rederivation");
    }
    let runs = paired_runs(connection, cohort_id)?;
    if runs.len() != prepared.requests.len()
        || runs
            .iter()
            .any(|run| run.status != PairedRunStatus::Succeeded)
    {
        bail!("paired adjudication requires every predeclared run to have succeeded exactly once");
    }
    let mut results = BTreeMap::new();
    let mut execution_receipts = Vec::with_capacity(runs.len());
    let mut elapsed_total = 0_u64;
    for (ordinal, (run, expected_request)) in runs.iter().zip(&prepared.requests).enumerate() {
        if usize::try_from(run.ordinal)? != ordinal
            || run.execution_id != expected_request.execution_id
        {
            bail!("paired run order differs from the predeclared schedule");
        }
        let reopened = reopen_paired_run(
            connection,
            object_root,
            plan,
            durable,
            run,
            expected_request,
        )?;
        elapsed_total = elapsed_total
            .checked_add(reopened.elapsed_ms)
            .context("paired elapsed-time sum overflow")?;
        execution_receipts.push(reopened.execution_receipt);
        results.insert(run.execution_id.clone(), reopened.result);
    }
    let cohort = execute_paired_adapter_cohort_with(
        cohort_id,
        plan,
        &manifest.objectives,
        &candidate_context,
        &durable.record.participants,
        |request| {
            results
                .remove(&request.execution_id)
                .context("stored paired result disappeared during reconstruction")
        },
    )?;
    let (adjudication, status, reason_code) = classify_replayed_cohort(
        connection,
        actor,
        object_root,
        durable,
        manifest,
        plan,
        &candidate_context,
        &cohort.blocks,
        retain_calibration_failure,
    )?;
    Ok(ReplayedCohort {
        adjudication,
        status,
        reason_code,
        execution_receipts,
        elapsed_total,
        run_count: runs.len(),
    })
}

pub fn adjudicate_paired_cohort(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    cohort_id: &str,
) -> Result<(PairedCohortRecord, PairedCohortAdjudication)> {
    validate_token("paired cohort actor", actor)?;
    let durable = durable_cohort(connection, cohort_id)?
        .with_context(|| format!("unknown paired cohort '{cohort_id}'"))?;
    if durable.record.status != PairedCohortStatus::Running {
        bail!("paired adjudication requires a running complete cohort");
    }
    let manifest = load_manifest(connection, &durable.record.campaign_id)?;
    let replay = replay_paired_cohort(connection, actor, object_root, &durable, &manifest, true);
    let replayed = match replay {
        Ok(replayed) => replayed,
        Err(error) => {
            let current = paired_cohort(connection, cohort_id)?
                .context("failed paired cohort disappeared during adjudication")?;
            if current.status == PairedCohortStatus::Running {
                terminal_cohort_failure(
                    connection,
                    actor,
                    object_root,
                    &current,
                    PairedCohortReasonCode::AdjudicationEvidenceInvalid,
                    &json!({"error": format!("{error:#}")}),
                )?;
                bail!(
                    "paired evidence replay failed; retained cohort is integrity_failed: {error:#}"
                );
            }
            return Err(error);
        }
    };
    let result_object = preserve_object(object_root, &serde_json::to_vec(&replayed.adjudication)?)?;
    let receipt = PairedCohortReceipt {
        schema: PAIRED_COHORT_RECEIPT_SCHEMA_V1.to_owned(),
        cohort_id: cohort_id.to_owned(),
        campaign_id: durable.record.campaign_id.clone(),
        candidate_id: durable.record.candidate_id.clone(),
        schedule_sha256: durable.record.schedule_sha256.clone(),
        execution_receipts: replayed.execution_receipts,
        result: result_object.clone(),
        reason_code: replayed.reason_code,
    };
    let receipt_object = preserve_object(object_root, &serde_json::to_vec(&receipt)?)?;
    let transaction = begin_mutation(connection)?;
    record_indexed_object(&transaction, &result_object, "application/json")?;
    record_indexed_object(&transaction, &receipt_object, "application/json")?;
    transaction.execute(
        "UPDATE paired_cohorts
         SET status=?2, result_sha256=?3, receipt_sha256=?4,
             reason_code=?5, finished_at=?6
         WHERE cohort_id=?1 AND status='running'",
        params![
            cohort_id,
            replayed.status.as_str(),
            result_object.sha256,
            receipt_object.sha256,
            replayed.reason_code.as_str(),
            now(),
        ],
    )?;
    let mut settlements = vec![
        BudgetSettlement {
            resource: BudgetResource::Trials,
            actual_amount: u64::try_from(replayed.run_count)?,
        },
        BudgetSettlement {
            resource: BudgetResource::WallTimeMilliseconds,
            actual_amount: replayed.elapsed_total,
        },
        BudgetSettlement {
            resource: BudgetResource::Failures,
            actual_amount: 0,
        },
    ];
    if matches!(durable.record.cohort, PairedCohort::Research { .. }) {
        settlements.push(BudgetSettlement {
            resource: BudgetResource::HoldoutDisclosures,
            actual_amount: u64::try_from(replayed.run_count)?,
        });
    }
    settle_bound_budget_in(
        &transaction,
        actor,
        BoundReservation {
            campaign_id: &durable.record.campaign_id,
            reservation_id: &durable.record.reservation_id,
            use_kind: "paired_cohort",
            entity_key: cohort_id,
        },
        SettlementMode::Measured,
        &settlements,
        Some(replayed.reason_code.as_str()),
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "paired_cohort",
        cohort_id,
        "adjudicated",
        Some(replayed.reason_code.as_str()),
        Some(&json!({
            "status": replayed.status,
            "result_sha256": result_object.sha256,
            "receipt_sha256": receipt_object.sha256,
        })),
    )?;
    transaction.commit()?;
    Ok((
        paired_cohort(connection, cohort_id)?.context("adjudicated paired cohort disappeared")?,
        replayed.adjudication,
    ))
}

pub fn verify_paired_cohort_integrity(
    connection: &Connection,
    object_root: &Path,
    cohort_id: &str,
) -> Result<VerifiedPairedCohortEvidence> {
    validate_token("paired cohort id", cohort_id)?;
    let durable = durable_cohort(connection, cohort_id)?
        .with_context(|| format!("unknown paired cohort '{cohort_id}'"))?;
    if !durable.record.status.is_successful_terminal() {
        bail!(
            "paired cohort integrity requires a successful terminal cohort, found '{}'",
            durable.record.status
        );
    }
    let manifest = load_manifest(connection, &durable.record.campaign_id)?;
    let replayed = replay_paired_cohort(
        connection,
        "paired-evidence-verifier",
        object_root,
        &durable,
        &manifest,
        false,
    )?;
    if replayed.status != durable.record.status
        || durable.record.reason_code.as_deref() != Some(replayed.reason_code.as_str())
    {
        bail!("terminal paired cohort differs from CAS-only reclassification");
    }

    let result_object = indexed_object(
        connection,
        durable
            .record
            .result_sha256
            .as_deref()
            .context("terminal paired cohort lost its result")?,
    )?;
    let (stored_adjudication, _): (PairedCohortAdjudication, Vec<u8>) =
        read_verified_json(object_root, &result_object, "paired cohort adjudication")?;
    if replayed.adjudication != stored_adjudication {
        bail!("terminal paired cohort result differs from CAS-only reclassification");
    }
    let receipt_object = indexed_object(
        connection,
        durable
            .record
            .receipt_sha256
            .as_deref()
            .context("terminal paired cohort lost its receipt")?,
    )?;
    let (receipt, _): (PairedCohortReceipt, Vec<u8>) =
        read_verified_json(object_root, &receipt_object, "paired cohort receipt")?;
    if receipt.schema != PAIRED_COHORT_RECEIPT_SCHEMA_V1
        || receipt.cohort_id != durable.record.cohort_id
        || receipt.campaign_id != durable.record.campaign_id
        || receipt.candidate_id != durable.record.candidate_id
        || receipt.schedule_sha256 != durable.record.schedule_sha256
        || receipt.execution_receipts != replayed.execution_receipts
        || receipt.result != result_object
        || receipt.reason_code != replayed.reason_code
    {
        bail!("terminal paired cohort receipt failed exact CAS rederivation");
    }
    Ok(VerifiedPairedCohortEvidence {
        cohort: durable.record,
        adjudication: replayed.adjudication,
        receipt: receipt_object,
    })
}

pub fn derive_paired_nomination(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    spec: &DerivePairedNominationSpec,
) -> Result<NominationRecord> {
    validate_token("paired nomination actor", actor)?;
    validate_token("research cohort id", &spec.research_cohort_id)?;
    validate_token("no-op cohort id", &spec.no_op_cohort_id)?;
    validate_token("known-bad cohort id", &spec.known_bad_cohort_id)?;
    let no_op = verify_paired_cohort_integrity(connection, object_root, &spec.no_op_cohort_id)?;
    let known_bad =
        verify_paired_cohort_integrity(connection, object_root, &spec.known_bad_cohort_id)?;
    let research =
        verify_paired_cohort_integrity(connection, object_root, &spec.research_cohort_id)?;
    require_nomination_cohorts(&no_op, &known_bad, &research)?;

    let manifest = load_manifest(connection, &research.cohort.campaign_id)?;
    let evidence_grade = match manifest.containment {
        ContainmentGrade::WorkspaceOnly => EvidenceGrade::WorkspaceOnlyDevelopment,
        ContainmentGrade::Sealed => bail!(
            "sealed paired nomination remains unavailable until the attested worker produces genuinely verdict-only cohort receipts"
        ),
    };
    let result = paired_candidate_result(evidence_grade, &no_op, &known_bad, &research)?;
    let (candidate_campaign, disposition, existing_result) = connection
        .query_row(
            "SELECT campaign_id, disposition, result_json FROM candidates WHERE candidate_id=?1",
            params![research.cohort.candidate_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .context("qualified paired cohort lost its candidate")?;
    if candidate_campaign != research.cohort.campaign_id {
        bail!("qualified paired cohort candidate belongs to another campaign");
    }
    if disposition == "nominated" {
        let existing =
            crate::lifecycle::nominations(connection, Some(&research.cohort.campaign_id))?
                .into_iter()
                .find(|nomination| nomination.candidate_id == research.cohort.candidate_id)
                .context("nominated paired candidate lost its immutable nomination")?;
        let verified = crate::lifecycle::verify_nomination_integrity(
            connection,
            object_root,
            &existing.nomination_id,
        )?;
        if verified.candidate_result != result
            || verified.relied_upon_paired_cohort_ids
                != vec![
                    spec.no_op_cohort_id.clone(),
                    spec.known_bad_cohort_id.clone(),
                    spec.research_cohort_id.clone(),
                ]
        {
            bail!("paired candidate was already nominated from different evidence");
        }
        return Ok(existing);
    }
    if !matches!(disposition.as_str(), "materialized" | "evaluating") || existing_result.is_some() {
        bail!(
            "paired nomination requires a materialized or evaluating candidate without a terminal result"
        );
    }

    let manifest_sha256 = campaign(connection, &research.cohort.campaign_id)?
        .context("paired nomination campaign disappeared")?
        .manifest_sha256;
    let created_at = now();
    let receipt = PairedNominationReceipt {
        schema: PAIRED_NOMINATION_RECEIPT_SCHEMA_V1.to_owned(),
        campaign_id: research.cohort.campaign_id.clone(),
        manifest_sha256,
        candidate_id: research.cohort.candidate_id.clone(),
        evidence_grade,
        no_op_cohort_id: spec.no_op_cohort_id.clone(),
        known_bad_cohort_id: spec.known_bad_cohort_id.clone(),
        research_cohort_id: spec.research_cohort_id.clone(),
        result: result.clone(),
        created_at: created_at.clone(),
    };
    let receipt_json = serde_json::to_string(&receipt)?;
    let receipt_object = preserve_object(object_root, receipt_json.as_bytes())?;
    let nomination = NominationRecord {
        nomination_id: receipt_object.sha256.clone(),
        campaign_id: receipt.campaign_id.clone(),
        candidate_id: receipt.candidate_id.clone(),
        receipt_sha256: receipt_object.sha256.clone(),
        receipt_json,
        created_at,
    };

    let transaction = begin_mutation(connection)?;
    record_indexed_object(&transaction, &receipt_object, "application/json")?;
    if disposition == "materialized" {
        let changed = transaction.execute(
            "UPDATE candidates SET disposition='evaluating', updated_at=?2
             WHERE candidate_id=?1 AND disposition='materialized' AND result_json IS NULL",
            params![nomination.candidate_id, now()],
        )?;
        if changed != 1 {
            bail!("paired candidate lost its materialized state before nomination");
        }
    }
    let changed = transaction.execute(
        "UPDATE candidates SET disposition='nominated', result_json=?2, updated_at=?3
         WHERE candidate_id=?1 AND disposition='evaluating' AND result_json IS NULL",
        params![
            nomination.candidate_id,
            serde_json::to_string(&result)?,
            now()
        ],
    )?;
    if changed != 1 {
        bail!("paired candidate lost its evaluating state before nomination");
    }
    transaction.execute(
        "INSERT INTO nominations
         (nomination_id, campaign_id, candidate_id, receipt_sha256, receipt_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            nomination.nomination_id,
            nomination.campaign_id,
            nomination.candidate_id,
            nomination.receipt_sha256,
            nomination.receipt_json,
            nomination.created_at,
        ],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "candidate",
        &nomination.candidate_id,
        "nominated",
        Some("paired-qualified"),
        Some(&json!({
            "nomination_id": nomination.nomination_id,
            "evidence_grade": evidence_grade,
            "paired_cohort_ids": [
                spec.no_op_cohort_id,
                spec.known_bad_cohort_id,
                spec.research_cohort_id,
            ],
        })),
    )?;
    transaction.commit()?;
    crate::lifecycle::verify_nomination_integrity(
        connection,
        object_root,
        &nomination.nomination_id,
    )?;
    Ok(nomination)
}

fn require_nomination_cohorts(
    no_op: &VerifiedPairedCohortEvidence,
    known_bad: &VerifiedPairedCohortEvidence,
    research: &VerifiedPairedCohortEvidence,
) -> Result<()> {
    if no_op.cohort.campaign_id != research.cohort.campaign_id
        || known_bad.cohort.campaign_id != research.cohort.campaign_id
    {
        bail!("paired nomination cohorts belong to different campaigns");
    }
    if !matches!(
        no_op.adjudication,
        PairedCohortAdjudication::NoOpCalibration(NoOpCalibrationResult { passed: true, .. })
    ) || no_op.cohort.status != PairedCohortStatus::Calibrated
        || no_op.cohort.reason_code.as_deref()
            != Some(PairedCohortReasonCode::NoOpCalibrationPassed.as_str())
    {
        bail!("paired nomination requires the exact passing no-op calibration cohort");
    }
    if !matches!(
        known_bad.adjudication,
        PairedCohortAdjudication::KnownBadCalibration(PairedClassification {
            disposition: PairedDisposition::Rejected,
            ..
        })
    ) || known_bad.cohort.status != PairedCohortStatus::Rejected
        || known_bad.cohort.reason_code.as_deref()
            != Some(PairedCohortReasonCode::KnownBadCalibrationRejected.as_str())
    {
        bail!("paired nomination requires the exact rejected known-bad calibration cohort");
    }
    if !matches!(
        research.adjudication,
        PairedCohortAdjudication::Research(PairedClassification {
            disposition: PairedDisposition::Qualified,
            ..
        })
    ) || research.cohort.status != PairedCohortStatus::Qualified
        || research.cohort.reason_code.as_deref()
            != Some(PairedCohortReasonCode::ResearchQualified.as_str())
    {
        bail!("paired nomination requires an exactly qualified research cohort");
    }
    Ok(())
}

fn paired_candidate_result(
    evidence_grade: EvidenceGrade,
    no_op: &VerifiedPairedCohortEvidence,
    known_bad: &VerifiedPairedCohortEvidence,
    research: &VerifiedPairedCohortEvidence,
) -> Result<Value> {
    Ok(json!({
        "schema": "papertiger-mise.paired-candidate-result.v1",
        "classification": research.adjudication,
        "evidence_grade": evidence_grade,
        "cohorts": [
            {
                "cohort_id": no_op.cohort.cohort_id,
                "receipt": no_op.receipt,
            },
            {
                "cohort_id": known_bad.cohort.cohort_id,
                "receipt": known_bad.receipt,
            },
            {
                "cohort_id": research.cohort.cohort_id,
                "receipt": research.receipt,
            },
        ],
    }))
}

pub(crate) fn verify_paired_nomination_evidence(
    connection: &Connection,
    object_root: &Path,
    nomination: NominationRecord,
    manifest: CampaignManifest,
    manifest_sha256: String,
    candidate_result: &Value,
) -> Result<VerifiedNominationEvidence> {
    let receipt: PairedNominationReceipt = serde_json::from_str(&nomination.receipt_json)?;
    if receipt.schema != PAIRED_NOMINATION_RECEIPT_SCHEMA_V1
        || receipt.campaign_id != nomination.campaign_id
        || receipt.manifest_sha256 != manifest_sha256
        || receipt.candidate_id != nomination.candidate_id
        || receipt.created_at != nomination.created_at
    {
        bail!("paired nomination receipt identity differs from its durable record");
    }
    let receipt_object = indexed_object(connection, &nomination.receipt_sha256)?;
    let (cas_receipt, cas_bytes): (PairedNominationReceipt, Vec<u8>) =
        read_verified_json(object_root, &receipt_object, "paired nomination receipt")?;
    if cas_receipt != receipt || cas_bytes != nomination.receipt_json.as_bytes() {
        bail!("paired nomination receipt differs from its exact CAS object");
    }
    let no_op = verify_paired_cohort_integrity(connection, object_root, &receipt.no_op_cohort_id)?;
    let known_bad =
        verify_paired_cohort_integrity(connection, object_root, &receipt.known_bad_cohort_id)?;
    let research =
        verify_paired_cohort_integrity(connection, object_root, &receipt.research_cohort_id)?;
    require_nomination_cohorts(&no_op, &known_bad, &research)?;
    if research.cohort.candidate_id != nomination.candidate_id {
        bail!("paired nomination research cohort identifies another candidate");
    }
    let rederived = paired_candidate_result(receipt.evidence_grade, &no_op, &known_bad, &research)?;
    if candidate_result != &rederived || receipt.result != rederived {
        bail!("paired nomination candidate result differs from CAS-only rederivation");
    }
    Ok(VerifiedNominationEvidence {
        nomination,
        manifest,
        manifest_sha256,
        candidate_result: rederived,
        relied_upon_trial_ids: Vec::new(),
        candidate_trial_ids: Vec::new(),
        relied_upon_paired_cohort_ids: vec![
            receipt.no_op_cohort_id,
            receipt.known_bad_cohort_id,
            receipt.research_cohort_id,
        ],
        evidence_grade: receipt.evidence_grade,
    })
}

pub fn recover_paired_run(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    execution_id: &str,
) -> Result<PairedRunRecord> {
    let run = paired_run(connection, execution_id)?
        .with_context(|| format!("unknown paired run '{execution_id}'"))?;
    if run.status != PairedRunStatus::Launched {
        bail!(
            "paired recovery requires a launched run, found '{}'",
            run.status
        );
    }
    let durable =
        paired_cohort(connection, &run.cohort_id)?.context("paired run lost its cohort")?;
    let pid = run.pid.context("launched paired run has no PID")?;
    let birth = run
        .process_birth_identity
        .as_deref()
        .context("launched paired run has no process birth identity")?;
    match observe_process(pid)? {
        ProcessObservation::Active {
            process_birth_identity,
        } if process_birth_identity == birth => bail!(
            "paired run '{execution_id}' still owns a live process; wait and rerun `papertiger-mise paired recover {execution_id}`"
        ),
        ProcessObservation::Exited {
            process_birth_identity,
        } if process_birth_identity != birth => {
            bail!("paired recovery observed an impossible exited birth-identity mismatch")
        }
        _ => {}
    }
    let detail = format!(
        "cold recovery proved exact process ownership absent for pid {pid} and birth identity {birth}"
    );
    let outcome = terminal_run_failure(
        connection,
        actor,
        object_root,
        &durable,
        &run,
        PairedRunStatus::InfrastructureFailed,
        "controller-interrupted",
        &detail,
        Some(birth),
        1,
        &[],
        &[],
    )?;
    match outcome {
        PairedRunOutcome::Completed(record) => Ok(*record),
        PairedRunOutcome::ReadyForAdjudication => unreachable!(),
    }
}

pub fn paired_cohort(
    connection: &Connection,
    cohort_id: &str,
) -> Result<Option<PairedCohortRecord>> {
    Ok(durable_cohort(connection, cohort_id)?.map(|durable| durable.record))
}

pub fn paired_cohorts(
    connection: &Connection,
    campaign_id: &str,
) -> Result<Vec<PairedCohortRecord>> {
    validate_token("paired campaign id", campaign_id)?;
    let mut statement = connection.prepare(
        "SELECT cohort_id FROM paired_cohorts
         WHERE campaign_id=?1 ORDER BY created_at, cohort_id",
    )?;
    let cohort_ids = statement
        .query_map(params![campaign_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    cohort_ids
        .into_iter()
        .map(|cohort_id| {
            paired_cohort(connection, &cohort_id)?
                .with_context(|| format!("listed paired cohort '{cohort_id}' disappeared"))
        })
        .collect()
}

pub fn paired_run(connection: &Connection, execution_id: &str) -> Result<Option<PairedRunRecord>> {
    connection
        .query_row(
            "SELECT execution_id, cohort_id, ordinal, block_index, participant,
                    request_sha256, status, pid, process_birth_identity,
                    adapter_result_sha256, domain_receipt_sha256,
                    execution_receipt_sha256, elapsed_ms, failure_code
             FROM paired_runs WHERE execution_id=?1",
            params![execution_id],
            map_run,
        )
        .optional()
        .map_err(Into::into)
}

pub fn paired_runs(connection: &Connection, cohort_id: &str) -> Result<Vec<PairedRunRecord>> {
    validate_token("paired cohort id", cohort_id)?;
    let mut statement = connection.prepare(
        "SELECT execution_id, cohort_id, ordinal, block_index, participant,
                request_sha256, status, pid, process_birth_identity,
                adapter_result_sha256, domain_receipt_sha256,
                execution_receipt_sha256, elapsed_ms, failure_code
         FROM paired_runs WHERE cohort_id=?1 ORDER BY ordinal",
    )?;
    statement
        .query_map(params![cohort_id], map_run)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn next_run_with_status(
    connection: &Connection,
    cohort_id: &str,
    status: PairedRunStatus,
) -> Result<Option<PairedRunRecord>> {
    connection
        .query_row(
            "SELECT execution_id, cohort_id, ordinal, block_index, participant,
                    request_sha256, status, pid, process_birth_identity,
                    adapter_result_sha256, domain_receipt_sha256,
                    execution_receipt_sha256, elapsed_ms, failure_code
             FROM paired_runs WHERE cohort_id=?1 AND status=?2 ORDER BY ordinal LIMIT 1",
            params![cohort_id, status.as_str()],
            map_run,
        )
        .optional()
        .map_err(Into::into)
}

fn map_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<PairedRunRecord> {
    let participant: String = row.get(4)?;
    let status: String = row.get(6)?;
    Ok(PairedRunRecord {
        execution_id: row.get(0)?,
        cohort_id: row.get(1)?,
        ordinal: u32::try_from(row.get::<_, i64>(2)?).map_err(sql_conversion)?,
        block_index: u32::try_from(row.get::<_, i64>(3)?).map_err(sql_conversion)?,
        participant: parse_participant(&participant)
            .map_err(|error| sql_conversion(std::io::Error::other(error.to_string())))?,
        request_sha256: row.get(5)?,
        status: PairedRunStatus::parse_column("paired_runs.status", &status)
            .map_err(|error| sql_conversion(std::io::Error::other(error.to_string())))?,
        pid: row
            .get::<_, Option<i64>>(7)?
            .map(u32::try_from)
            .transpose()
            .map_err(sql_conversion)?,
        process_birth_identity: row.get(8)?,
        adapter_result_sha256: row.get(9)?,
        domain_receipt_sha256: row.get(10)?,
        execution_receipt_sha256: row.get(11)?,
        elapsed_ms: row
            .get::<_, Option<i64>>(12)?
            .map(u64::try_from)
            .transpose()
            .map_err(sql_conversion)?,
        failure_code: row.get(13)?,
    })
}

fn durable_cohort(connection: &Connection, cohort_id: &str) -> Result<Option<DurableCohort>> {
    let row = connection
        .query_row(
            "SELECT campaign_id, candidate_id, cohort_kind, analysis_slot,
                    schedule_sha256, order_seed_reveal, participants_json,
                    reservation_id, adapter_binding_sha256, status,
                    result_sha256, receipt_sha256, reason_code, recorded_by,
                    created_at, finished_at
             FROM paired_cohorts WHERE cohort_id=?1",
            params![cohort_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, Option<String>>(15)?,
                ))
            },
        )
        .optional()?;
    let Some((
        campaign_id,
        candidate_id,
        cohort_kind,
        analysis_slot,
        schedule_sha256,
        revealed_order_seed,
        participants_json,
        reservation_id,
        adapter_binding_sha256,
        status,
        result_sha256,
        receipt_sha256,
        reason_code,
        recorded_by,
        created_at,
        finished_at,
    )) = row
    else {
        return Ok(None);
    };
    let status = PairedCohortStatus::parse_column("paired_cohorts.status", &status)?;
    if status.is_successful_terminal() {
        let reason = reason_code
            .as_deref()
            .context("successful terminal paired_cohorts.reason_code is missing")?;
        PairedCohortReasonCode::parse_column("paired_cohorts.reason_code", reason)?;
    }
    Ok(Some(DurableCohort {
        record: PairedCohortRecord {
            cohort_id: cohort_id.to_owned(),
            campaign_id,
            candidate_id,
            cohort: parse_cohort(&cohort_kind, analysis_slot)?,
            schedule_sha256: Sha256Digest(schedule_sha256),
            participants: serde_json::from_str(&participants_json)
                .context("durable paired participants are not typed canonical JSON")?,
            reservation_id,
            adapter_binding_sha256: Sha256Digest(adapter_binding_sha256),
            status,
            result_sha256,
            receipt_sha256,
            reason_code,
            recorded_by,
            created_at,
            finished_at,
        },
        revealed_order_seed,
    }))
}

fn terminal_cohort_failure(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    cohort: &PairedCohortRecord,
    reason_code: PairedCohortReasonCode,
    result: &Value,
) -> Result<()> {
    let result_object = preserve_object(object_root, &serde_json::to_vec(result)?)?;
    let runs = paired_runs(connection, &cohort.cohort_id)?;
    let run_evidence = runs
        .iter()
        .map(|run| {
            json!({
                "execution_id": run.execution_id,
                "status": run.status,
                "execution_receipt_sha256": run.execution_receipt_sha256,
                "failure_code": run.failure_code,
            })
        })
        .collect::<Vec<_>>();
    let receipt_value = json!({
        "schema": "papertiger-mise.paired-cohort-failure-receipt.v1",
        "cohort_id": cohort.cohort_id,
        "campaign_id": cohort.campaign_id,
        "candidate_id": cohort.candidate_id,
        "schedule_sha256": cohort.schedule_sha256,
        "run_evidence": run_evidence,
        "result": result_object.clone(),
        "reason_code": reason_code,
    });
    let receipt_object = preserve_object(object_root, &serde_json::to_vec(&receipt_value)?)?;
    let transaction = begin_mutation(connection)?;
    record_indexed_object(&transaction, &result_object, "application/json")?;
    record_indexed_object(&transaction, &receipt_object, "application/json")?;
    let changed = transaction.execute(
        "UPDATE paired_cohorts
         SET status='integrity_failed', result_sha256=?2, receipt_sha256=?3,
             reason_code=?4, finished_at=?5
         WHERE cohort_id=?1 AND status='running'",
        params![
            cohort.cohort_id,
            result_object.sha256,
            receipt_object.sha256,
            reason_code.as_str(),
            now(),
        ],
    )?;
    if changed != 1 {
        bail!("paired cohort integrity failure lost its running authority");
    }
    settle_bound_budget_in(
        &transaction,
        actor,
        BoundReservation {
            campaign_id: &cohort.campaign_id,
            reservation_id: &cohort.reservation_id,
            use_kind: "paired_cohort",
            entity_key: &cohort.cohort_id,
        },
        SettlementMode::ChargeReservation,
        &[],
        Some(reason_code.as_str()),
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "paired_cohort",
        &cohort.cohort_id,
        "integrity_failed",
        Some(reason_code.as_str()),
        Some(&json!({"receipt_sha256": receipt_object.sha256})),
    )?;
    transaction.commit()?;
    Ok(())
}

fn complete_paired_run(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    cohort: &PairedCohortRecord,
    run: &PairedRunRecord,
    evidence: &CompletedRunEvidence<'_>,
) -> Result<()> {
    let result_object = preserve_object(object_root, evidence.result_bytes)?;
    let domain_bytes = serde_json::to_vec(&evidence.result.domain_trial_receipt)?;
    let domain_object = preserve_object(object_root, &domain_bytes)?;
    if domain_object.sha256 != evidence.result.domain_trial_receipt_sha256()?.0 {
        bail!("preserved paired domain receipt differs from its derived digest");
    }
    let receipt = PairedExecutionReceipt {
        schema: PAIRED_EXECUTION_RECEIPT_SCHEMA_V1.to_owned(),
        cohort_id: cohort.cohort_id.clone(),
        execution_id: run.execution_id.clone(),
        request: evidence.request.clone(),
        adapter_result: result_object.clone(),
        domain_receipt: domain_object.clone(),
        adapter_binding_sha256: cohort.adapter_binding_sha256.clone(),
        process_birth_identity: evidence.process_birth_identity.to_owned(),
        elapsed_ms: evidence.elapsed_ms,
        execution_capabilities: evidence.capabilities.clone(),
    };
    let receipt_object = preserve_object(object_root, &serde_json::to_vec(&receipt)?)?;
    let transaction = begin_mutation(connection)?;
    for object in [&result_object, &domain_object, &receipt_object] {
        record_indexed_object(&transaction, object, "application/json")?;
    }
    transaction.execute(
        "INSERT INTO paired_live_domain_receipts (receipt_sha256, execution_id)
         VALUES (?1, ?2)",
        params![domain_object.sha256, run.execution_id],
    )?;
    let changed = transaction.execute(
        "UPDATE paired_runs
         SET status='succeeded', adapter_result_sha256=?2,
             domain_receipt_sha256=?3, execution_receipt_sha256=?4,
             elapsed_ms=?5, finished_at=?6
         WHERE execution_id=?1 AND status='launched'",
        params![
            run.execution_id,
            result_object.sha256,
            domain_object.sha256,
            receipt_object.sha256,
            i64::try_from(evidence.elapsed_ms)?,
            now(),
        ],
    )?;
    if changed != 1 {
        bail!("paired run completion lost its exact launched ownership");
    }
    record_event_in_mutation(
        &transaction,
        actor,
        "paired_run",
        &run.execution_id,
        "succeeded",
        None,
        Some(&json!({
            "receipt_sha256": receipt_object.sha256,
            "domain_receipt_sha256": domain_object.sha256,
            "elapsed_ms": evidence.elapsed_ms,
        })),
    )?;
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn terminal_run_failure(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    cohort: &PairedCohortRecord,
    run: &PairedRunRecord,
    terminal_status: PairedRunStatus,
    failure_code: &str,
    detail: &str,
    process_birth_identity: Option<&str>,
    elapsed_ms: u64,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<PairedRunOutcome> {
    let cohort_terminal_status = match terminal_status {
        PairedRunStatus::InfrastructureFailed => PairedCohortStatus::InfrastructureFailed,
        PairedRunStatus::IntegrityFailed => PairedCohortStatus::IntegrityFailed,
        other => bail!("paired run failure cannot record nonterminal status '{other}'"),
    };
    let stdout_object = preserve_object(object_root, stdout)?;
    let stderr_object = preserve_object(object_root, stderr)?;
    let receipt = PairedFailureReceipt {
        schema: "papertiger-mise.paired-execution-failure-receipt.v1".to_owned(),
        cohort_id: cohort.cohort_id.clone(),
        execution_id: run.execution_id.clone(),
        failure_code: failure_code.to_owned(),
        detail: detail.to_owned(),
        process_birth_identity: process_birth_identity.map(str::to_owned),
        elapsed_ms: elapsed_ms.max(1),
        stdout: stdout_object.clone(),
        stderr: stderr_object.clone(),
    };
    let receipt_object = preserve_object(object_root, &serde_json::to_vec(&receipt)?)?;
    let transaction = begin_mutation(connection)?;
    record_indexed_object(&transaction, &stdout_object, "application/octet-stream")?;
    record_indexed_object(&transaction, &stderr_object, "application/octet-stream")?;
    record_indexed_object(&transaction, &receipt_object, "application/json")?;
    let changed = transaction.execute(
        "UPDATE paired_runs
         SET status=?2, execution_receipt_sha256=?3, elapsed_ms=?4,
             failure_code=?5, finished_at=?6
         WHERE execution_id=?1 AND status IN ('prepared','launched')",
        params![
            run.execution_id,
            terminal_status.as_str(),
            receipt_object.sha256,
            i64::try_from(elapsed_ms.max(1))?,
            failure_code,
            now(),
        ],
    )?;
    if changed != 1 {
        bail!("paired run failure reconciliation lost its nonterminal ownership");
    }
    transaction.execute(
        "UPDATE paired_cohorts
         SET status=?2, receipt_sha256=?3, reason_code=?4, finished_at=?5
         WHERE cohort_id=?1 AND status IN ('prepared','running')",
        params![
            cohort.cohort_id,
            cohort_terminal_status.as_str(),
            receipt_object.sha256,
            failure_code,
            now(),
        ],
    )?;
    settle_bound_budget_in(
        &transaction,
        actor,
        BoundReservation {
            campaign_id: &cohort.campaign_id,
            reservation_id: &cohort.reservation_id,
            use_kind: "paired_cohort",
            entity_key: &cohort.cohort_id,
        },
        SettlementMode::ChargeReservation,
        &[],
        Some(failure_code),
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "paired_run",
        &run.execution_id,
        terminal_status.as_str(),
        Some(detail),
        Some(&json!({
            "failure_code": failure_code,
            "receipt_sha256": receipt_object.sha256,
        })),
    )?;
    transaction.commit()?;
    Ok(PairedRunOutcome::Completed(Box::new(
        paired_run(connection, &run.execution_id)?.context("failed paired run disappeared")?,
    )))
}

fn mark_paired_run_launched(
    connection: &Connection,
    actor: &str,
    cohort_id: &str,
    execution_id: &str,
    pid: u32,
    process_birth_identity: &str,
) -> Result<()> {
    let transaction = begin_mutation(connection)?;
    let at = now();
    let changed = transaction.execute(
        "UPDATE paired_runs
         SET status='launched', pid=?2, process_birth_identity=?3, heartbeat_at=?4
         WHERE execution_id=?1 AND status='prepared'",
        params![execution_id, i64::from(pid), process_birth_identity, at],
    )?;
    if changed != 1 {
        bail!("paired run launch could not bind its exact prepared intent");
    }
    transaction.execute(
        "UPDATE paired_cohorts SET status='running'
         WHERE cohort_id=?1 AND status='prepared'",
        params![cohort_id],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "paired_run",
        execution_id,
        "launched",
        None,
        Some(&json!({
            "pid": pid,
            "process_birth_identity": process_birth_identity,
        })),
    )?;
    transaction.commit()?;
    Ok(())
}

fn heartbeat_paired_run(connection: &Connection, actor: &str, execution_id: &str) -> Result<()> {
    let transaction = begin_mutation(connection)?;
    let changed = transaction.execute(
        "UPDATE paired_runs SET heartbeat_at=?2
         WHERE execution_id=?1 AND status='launched'",
        params![execution_id, now()],
    )?;
    if changed != 1 {
        bail!("paired run heartbeat lost its exact launched ownership");
    }
    record_event_in_mutation(
        &transaction,
        actor,
        "paired_run",
        execution_id,
        "heartbeat",
        None,
        None,
    )?;
    transaction.commit()?;
    Ok(())
}

fn require_calibration_authority(
    connection: &Connection,
    cohort: &PairedCohortRecord,
) -> Result<()> {
    if !matches!(cohort.cohort, PairedCohort::Research { .. }) {
        return Ok(());
    }
    for (kind, status, reason) in [
        (
            "no_op_calibration",
            PairedCohortStatus::Calibrated,
            PairedCohortReasonCode::NoOpCalibrationPassed,
        ),
        (
            "known_bad_calibration",
            PairedCohortStatus::Rejected,
            PairedCohortReasonCode::KnownBadCalibrationRejected,
        ),
    ] {
        let present = connection
            .query_row(
                "SELECT 1 FROM paired_cohorts
                 WHERE campaign_id=?1 AND cohort_kind=?2 AND status=?3 AND reason_code=?4",
                params![cohort.campaign_id, kind, status.as_str(), reason.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !present {
            bail!(
                "research adjudication requires a durable completed {kind} status; run and adjudicate calibration first"
            );
        }
    }
    Ok(())
}

fn validate_candidate_and_participants(
    connection: &Connection,
    manifest: &CampaignManifest,
    spec: &PreparePairedCohortSpec,
) -> Result<()> {
    let (candidate_campaign, candidate_patch, candidate_disposition) = connection
        .query_row(
            "SELECT campaign_id, patch_sha256, disposition FROM candidates WHERE candidate_id=?1",
            params![spec.candidate_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .with_context(|| format!("unknown paired candidate '{}'", spec.candidate_id))?;
    if candidate_campaign != spec.campaign_id
        || !matches!(
            candidate_disposition.as_str(),
            "materialized" | "evaluating"
        )
    {
        bail!("paired cohort requires a materialized nonterminal candidate in the same campaign");
    }
    let (baseline_id, baseline_tree) = candidate_materialization_by_patch(
        connection,
        &spec.campaign_id,
        &manifest.calibration.no_op_material_sha256().0,
    )?;
    let candidate_tree = candidate_materialization(connection, &spec.candidate_id)?;
    let expected_patch = match spec.cohort {
        PairedCohort::Research { .. } => None,
        PairedCohort::NoOpCalibration => Some(&manifest.calibration.no_op_material_sha256().0),
        PairedCohort::KnownBadCalibration => {
            Some(&manifest.calibration.known_bad_material_sha256().0)
        }
    };
    if expected_patch.is_some_and(|expected| candidate_patch != *expected) {
        bail!("paired calibration candidate differs from its frozen campaign identity");
    }
    if spec.participants.baseline.identity_sha256.0 != baseline_id
        || spec.participants.baseline.revision != baseline_tree
        || spec.participants.candidate.identity_sha256.0 != spec.candidate_id
        || spec.participants.candidate.revision != candidate_tree
    {
        bail!("paired participants do not bind the exact durable materializations");
    }
    Ok(())
}

fn validate_research_slot(
    connection: &Connection,
    spec: &PreparePairedCohortSpec,
    schedule_sha256: &Sha256Digest,
) -> Result<()> {
    if let PairedCohort::Research {
        candidate_analysis_slot,
    } = spec.cohort
    {
        let slot = paired_analysis_slot(connection, &spec.campaign_id, candidate_analysis_slot)?
            .context("research cohort has no reserved paired analysis slot")?;
        if slot.candidate_id != spec.candidate_id
            || slot.schedule_sha256 != *schedule_sha256
            || slot.revealed_order_seed != spec.revealed_order_seed
        {
            bail!("research cohort differs from its atomically reserved analysis slot");
        }
    }
    Ok(())
}

fn candidate_materialization(connection: &Connection, candidate_id: &str) -> Result<String> {
    connection
        .query_row(
            "SELECT result_tree FROM materializations WHERE candidate_id=?1",
            params![candidate_id],
            |row| row.get(0),
        )
        .with_context(|| format!("candidate '{candidate_id}' has no materialization"))
}

fn candidate_materialization_by_patch(
    connection: &Connection,
    campaign_id: &str,
    patch_sha256: &str,
) -> Result<(String, String)> {
    connection
        .query_row(
            "SELECT c.candidate_id, m.result_tree
             FROM candidates c JOIN materializations m ON m.candidate_id=c.candidate_id
             WHERE c.campaign_id=?1 AND c.patch_sha256=?2",
            params![campaign_id, patch_sha256],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("campaign no-op baseline has no exact materialization")
}

fn load_manifest(connection: &Connection, campaign_id: &str) -> Result<CampaignManifest> {
    let record = campaign(connection, campaign_id)?
        .with_context(|| format!("unknown campaign '{campaign_id}'"))?;
    let manifest: CampaignManifest = serde_json::from_str(&record.manifest_json)
        .context("durable campaign manifest is not typed JSON")?;
    manifest.validate()?;
    Ok(manifest)
}

fn require_same_preparation(
    existing: &PairedCohortRecord,
    spec: &PreparePairedCohortSpec,
    schedule_sha256: &Sha256Digest,
    binding_sha256: &Sha256Digest,
) -> Result<()> {
    if existing.campaign_id != spec.campaign_id
        || existing.candidate_id != spec.candidate_id
        || existing.cohort != spec.cohort
        || existing.schedule_sha256 != *schedule_sha256
        || existing.participants != spec.participants
        || existing.reservation_id != spec.reservation_id
        || existing.adapter_binding_sha256 != *binding_sha256
    {
        bail!("paired cohort id already exists with different immutable preparation");
    }
    Ok(())
}

fn cohort_columns(cohort: PairedCohort) -> (&'static str, Option<u32>) {
    match cohort {
        PairedCohort::Research {
            candidate_analysis_slot,
        } => ("research", Some(candidate_analysis_slot)),
        PairedCohort::NoOpCalibration => ("no_op_calibration", None),
        PairedCohort::KnownBadCalibration => ("known_bad_calibration", None),
    }
}

fn parse_cohort(kind: &str, slot: Option<i64>) -> Result<PairedCohort> {
    match (kind, slot) {
        ("research", Some(slot)) => Ok(PairedCohort::Research {
            candidate_analysis_slot: u32::try_from(slot)?,
        }),
        ("no_op_calibration", None) => Ok(PairedCohort::NoOpCalibration),
        ("known_bad_calibration", None) => Ok(PairedCohort::KnownBadCalibration),
        _ => bail!("durable paired cohort kind and slot are inconsistent"),
    }
}

fn participant_label(participant: PairedParticipantRole) -> &'static str {
    match participant {
        PairedParticipantRole::Baseline => "baseline",
        PairedParticipantRole::Candidate => "candidate",
    }
}

fn parse_participant(value: &str) -> Result<PairedParticipantRole> {
    match value {
        "baseline" => Ok(PairedParticipantRole::Baseline),
        "candidate" => Ok(PairedParticipantRole::Candidate),
        _ => bail!("unknown durable paired participant '{value}'"),
    }
}

fn canonical_json_bytes(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&serde_json::to_value(value)?)?)
}

fn sql_conversion(error: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, Box::new(error))
}

#[cfg(test)]
#[path = "paired_runtime/tests.rs"]
mod tests;
