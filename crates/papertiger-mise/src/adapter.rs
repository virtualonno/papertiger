use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::digest::{sha256, validate_sha256};
use crate::executor::execute_bounded;
use crate::manifest::{ObjectiveSpec, Sha256Digest};
use crate::path_identity::portable_absolute;
use crate::statistics::{
    PairedAnalysisPlan, PairedBlockObservation, PairedCandidateContext, PairedCohort,
    PairedObjectiveObservation, PairedRunOrder, paired_run_order, paired_schedule_sha256,
};
use crate::validation::validate_bounded_token as validate_token;

pub const PAIRED_ADAPTER_BINDING_SCHEMA_V1: &str = "papertiger-mise.paired-adapter-binding.v1";
pub const PAIRED_TRIAL_REQUEST_SCHEMA_V2: &str = "papertiger-mise.paired-trial-request.v2";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedAdapterBinding {
    pub schema: String,
    pub executable_locator: String,
    pub executable_sha256: Sha256Digest,
    pub argv: Vec<String>,
    pub working_directory: String,
    pub environment: BTreeMap<String, String>,
    pub result_schema: String,
    pub maximum_wall_time_ms: u64,
    pub maximum_output_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainParticipant {
    pub key: String,
    pub identity_sha256: Sha256Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainSessionEvidence {
    pub receipt_sha256: Sha256Digest,
    pub started_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainObjectiveMeasurement {
    pub objective: String,
    pub baseline_units: i64,
    pub candidate_units: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainBlockMeasurement {
    pub block_index: u32,
    pub baseline: DomainSessionEvidence,
    pub candidate: DomainSessionEvidence,
    pub measurements: Vec<DomainObjectiveMeasurement>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainObservationResult {
    pub schema: String,
    pub observation_id: String,
    pub request_sha256: Sha256Digest,
    pub adapter_executable_sha256: Sha256Digest,
    /// Domain-owned evidence that authorized observation. Mise preserves it
    /// exactly but does not assign domain meaning to its fields.
    pub domain_authority: Value,
    /// Domain-owned execution context. The exact request digest prevents an
    /// adapter from substituting caller-supplied measurements or decisions.
    pub domain_context: Value,
    pub baseline: DomainParticipant,
    pub candidate: DomainParticipant,
    pub blocks: Vec<DomainBlockMeasurement>,
}

#[derive(Clone, Debug)]
pub struct VerifiedAdapterResult {
    pub binding: PairedAdapterBinding,
    pub request_bytes: Vec<u8>,
    pub output_bytes: Vec<u8>,
    pub result: DomainObservationResult,
    pub elapsed_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedParticipantRole {
    Baseline,
    Candidate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedExecutionParticipant {
    pub identity_sha256: Sha256Digest,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedExecutionParticipants {
    pub baseline: PairedExecutionParticipant,
    pub candidate: PairedExecutionParticipant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedTrialObjective {
    pub objective: String,
    pub scale10: u8,
    pub measurement_summary_protocol: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedTrialRequest {
    pub schema: String,
    pub execution_id: String,
    pub experiment_id: String,
    pub schedule_sha256: Sha256Digest,
    pub cohort: PairedCohort,
    pub block_index: u32,
    pub order: PairedRunOrder,
    pub participant_role: PairedParticipantRole,
    pub participant: PairedExecutionParticipant,
    pub fixture_sha256: Sha256Digest,
    pub environment_profile_sha256: Sha256Digest,
    pub workload_seed: String,
    pub objectives: Vec<PairedTrialObjective>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainTrialMeasurement {
    pub objective: String,
    pub units: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainTrialResult {
    pub schema: String,
    pub execution_id: String,
    pub request_sha256: Sha256Digest,
    pub adapter_executable_sha256: Sha256Digest,
    pub participant_identity_sha256: Sha256Digest,
    /// Exact canonical domain-owned receipt. Mise preserves these bytes and
    /// derives the digest itself; a bare digest is not reopenable evidence.
    pub domain_trial_receipt: Value,
    pub domain_authority: Value,
    pub measurements: Vec<DomainTrialMeasurement>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedAdapterCohort {
    pub experiment_id: String,
    pub schedule_sha256: Sha256Digest,
    pub blocks: Vec<PairedBlockObservation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedPairedAdapterCohort {
    pub(crate) schedule_sha256: Sha256Digest,
    pub(crate) requests: Vec<PairedTrialRequest>,
}

impl DomainTrialResult {
    pub fn domain_trial_receipt_sha256(&self) -> Result<Sha256Digest> {
        if !self.domain_trial_receipt.is_object() {
            bail!("paired domain trial receipt must be an object-valued canonical document");
        }
        Ok(Sha256Digest(sha256(&serde_json::to_vec(
            &self.domain_trial_receipt,
        )?)))
    }
}

#[derive(Debug)]
struct ExecutedAdapterDocument {
    binding: PairedAdapterBinding,
    request_bytes: Vec<u8>,
    output_bytes: Vec<u8>,
    value: Value,
    elapsed_ms: u64,
}

impl PairedAdapterBinding {
    pub fn validate(&self) -> Result<()> {
        self.validate_contract()?;
        validate_absolute_existing("adapter executable", &self.executable_locator, true)?;
        validate_absolute_existing("adapter working directory", &self.working_directory, false)?;
        Ok(())
    }

    pub(crate) fn validate_contract(&self) -> Result<()> {
        if self.schema != PAIRED_ADAPTER_BINDING_SCHEMA_V1 {
            bail!("paired adapter binding schema must be '{PAIRED_ADAPTER_BINDING_SCHEMA_V1}'");
        }
        validate_absolute_syntax("adapter executable", &self.executable_locator)?;
        validate_sha256(&self.executable_sha256.0, "adapter executable")?;
        validate_absolute_syntax("adapter working directory", &self.working_directory)?;
        if self.argv.first() != Some(&self.executable_locator)
            || self
                .argv
                .iter()
                .any(|argument| argument.is_empty() || argument.contains('\0'))
        {
            bail!(
                "paired adapter argv must begin with its exact executable and contain no empty or NUL arguments"
            );
        }
        validate_token("paired adapter result schema", &self.result_schema)?;
        if !(10..=86_400_000).contains(&self.maximum_wall_time_ms) {
            bail!("paired adapter wall-time bound must be between 10 ms and 24 hours");
        }
        if !(1..=64 * 1024 * 1024).contains(&self.maximum_output_bytes) {
            bail!("paired adapter output bound must be between 1 byte and 64 MiB");
        }
        for (key, value) in &self.environment {
            if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
                bail!("paired adapter environment must contain exact portable entries");
            }
        }
        Ok(())
    }
}

pub fn execute_paired_adapter(
    binding: &PairedAdapterBinding,
    domain_request: &Value,
) -> Result<VerifiedAdapterResult> {
    let execution = execute_adapter_document(binding, domain_request)?;
    let request_sha256 = sha256(&execution.request_bytes);
    let result: DomainObservationResult =
        serde_json::from_value(execution.value).context("type paired adapter result")?;
    validate_result(binding, &result, &request_sha256)?;
    Ok(VerifiedAdapterResult {
        binding: execution.binding,
        request_bytes: execution.request_bytes,
        output_bytes: execution.output_bytes,
        result,
        elapsed_ms: execution.elapsed_ms,
    })
}

/// Execute a complete fixed paired cohort in Mise's frozen AB/BA order.
/// Measurements can enter only through the exact external adapter result.
pub fn execute_paired_adapter_cohort(
    experiment_id: &str,
    plan: &PairedAnalysisPlan,
    objectives: &[ObjectiveSpec],
    candidate: &PairedCandidateContext<'_>,
    participants: &PairedExecutionParticipants,
) -> Result<PairedAdapterCohort> {
    let trial_adapter = plan
        .trial_adapter
        .as_ref()
        .context("paired plan has no frozen trial adapter")?;
    execute_paired_adapter_cohort_with(
        experiment_id,
        plan,
        objectives,
        candidate,
        participants,
        |request| execute_domain_trial_adapter(trial_adapter, request),
    )
}

pub(crate) fn execute_paired_adapter_cohort_with<F>(
    experiment_id: &str,
    plan: &PairedAnalysisPlan,
    objectives: &[ObjectiveSpec],
    candidate: &PairedCandidateContext<'_>,
    participants: &PairedExecutionParticipants,
    mut execute: F,
) -> Result<PairedAdapterCohort>
where
    F: FnMut(&PairedTrialRequest) -> Result<DomainTrialResult>,
{
    let prepared =
        prepare_paired_adapter_cohort(experiment_id, plan, objectives, candidate, participants)?;
    let trial_adapter = plan
        .trial_adapter
        .as_ref()
        .context("paired plan has no frozen trial adapter")?;
    let mut receipts = BTreeSet::new();
    let mut results = BTreeMap::new();
    for request in &prepared.requests {
        let result = execute(request)?;
        let request_sha256 = sha256(&serde_json::to_vec(&serde_json::to_value(request)?)?);
        validate_domain_trial_result(trial_adapter, request, &result, &request_sha256)?;
        let domain_receipt_sha256 = result.domain_trial_receipt_sha256()?;
        if !receipts.insert(domain_receipt_sha256.0.clone()) {
            bail!("paired adapter reused a domain trial receipt");
        }
        results.insert((request.block_index, request.participant_role), result);
    }
    let mut blocks = Vec::with_capacity(plan.blocks.len());
    for request in prepared
        .requests
        .iter()
        .filter(|request| request.participant_role == PairedParticipantRole::Baseline)
    {
        let baseline = results
            .remove(&(request.block_index, PairedParticipantRole::Baseline))
            .context("paired baseline adapter result disappeared")?;
        let candidate_result = results
            .remove(&(request.block_index, PairedParticipantRole::Candidate))
            .context("paired candidate adapter result disappeared")?;
        let candidate_measurements = candidate_result
            .measurements
            .iter()
            .map(|measurement| (measurement.objective.as_str(), measurement.units))
            .collect::<BTreeMap<_, _>>();
        let observations = baseline
            .measurements
            .iter()
            .map(|measurement| {
                Ok(PairedObjectiveObservation {
                    objective: measurement.objective.clone(),
                    baseline_units: measurement.units,
                    candidate_units: *candidate_measurements
                        .get(measurement.objective.as_str())
                        .context("candidate domain measurement disappeared")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        blocks.push(PairedBlockObservation {
            block_index: request.block_index,
            order: request.order,
            fixture_sha256: request.fixture_sha256.clone(),
            environment_profile_sha256: request.environment_profile_sha256.clone(),
            workload_seed: request.workload_seed.clone(),
            baseline_trial_receipt_sha256: baseline.domain_trial_receipt_sha256()?,
            candidate_trial_receipt_sha256: candidate_result.domain_trial_receipt_sha256()?,
            observations,
        });
    }
    Ok(PairedAdapterCohort {
        experiment_id: experiment_id.to_owned(),
        schedule_sha256: prepared.schedule_sha256,
        blocks,
    })
}

pub(crate) fn prepare_paired_adapter_cohort(
    experiment_id: &str,
    plan: &PairedAnalysisPlan,
    objectives: &[ObjectiveSpec],
    candidate: &PairedCandidateContext<'_>,
    participants: &PairedExecutionParticipants,
) -> Result<PreparedPairedAdapterCohort> {
    validate_token("paired execution experiment", experiment_id)?;
    plan.validate(objectives)?;
    plan.trial_adapter
        .as_ref()
        .context("paired plan has no frozen trial adapter")?
        .validate()?;
    let participants_equal =
        participants.baseline.identity_sha256 == participants.candidate.identity_sha256;
    if participants.candidate.identity_sha256 != *candidate.candidate_identity_sha256
        || (candidate.cohort == PairedCohort::NoOpCalibration && !participants_equal)
        || (candidate.cohort != PairedCohort::NoOpCalibration && participants_equal)
    {
        bail!("paired execution participants differ from the frozen candidate context");
    }
    for (role, participant) in [
        ("baseline", &participants.baseline),
        ("candidate", &participants.candidate),
    ] {
        validate_sha256(
            &participant.identity_sha256.0,
            &format!("paired {role} identity"),
        )?;
        validate_token(&format!("paired {role} revision"), &participant.revision)?;
    }
    let schedule_sha256 = paired_schedule_sha256(plan, objectives, candidate)?;
    let mut designs = plan.blocks.iter().collect::<Vec<_>>();
    designs.sort_by_key(|block| block.block_index);
    let mut requests = Vec::with_capacity(designs.len() * 2);
    for design in designs {
        let order = paired_run_order(plan, objectives, candidate, design.block_index)?;
        let fixture_sha256 = match candidate.cohort {
            PairedCohort::Research { .. } => design.fixture_sha256.clone(),
            PairedCohort::NoOpCalibration => plan.calibration_fixtures.no_op.sha256.clone(),
            PairedCohort::KnownBadCalibration => plan.calibration_fixtures.known_bad.sha256.clone(),
        };
        let roles = match order {
            PairedRunOrder::BaselineFirst => [
                PairedParticipantRole::Baseline,
                PairedParticipantRole::Candidate,
            ],
            PairedRunOrder::CandidateFirst => [
                PairedParticipantRole::Candidate,
                PairedParticipantRole::Baseline,
            ],
        };
        for role in roles {
            let participant = match role {
                PairedParticipantRole::Baseline => &participants.baseline,
                PairedParticipantRole::Candidate => &participants.candidate,
            };
            requests.push(PairedTrialRequest {
                schema: PAIRED_TRIAL_REQUEST_SCHEMA_V2.to_owned(),
                execution_id: trial_execution_id(
                    experiment_id,
                    &schedule_sha256,
                    design.block_index,
                    role,
                    participant,
                ),
                experiment_id: experiment_id.to_owned(),
                schedule_sha256: schedule_sha256.clone(),
                cohort: candidate.cohort,
                block_index: design.block_index,
                order,
                participant_role: role,
                participant: participant.clone(),
                fixture_sha256: fixture_sha256.clone(),
                environment_profile_sha256: design.environment_profile_sha256.clone(),
                workload_seed: design.workload_seed.clone(),
                objectives: objectives
                    .iter()
                    .map(|objective| {
                        let policy = plan
                            .objective_policy(&objective.key)
                            .context("paired objective policy disappeared")?;
                        Ok(PairedTrialObjective {
                            objective: objective.key.clone(),
                            scale10: policy.scale10,
                            measurement_summary_protocol: policy
                                .measurement_summary_protocol
                                .clone(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            });
        }
    }
    Ok(PreparedPairedAdapterCohort {
        schedule_sha256,
        requests,
    })
}

fn execute_adapter_document(
    binding: &PairedAdapterBinding,
    request: &Value,
) -> Result<ExecutedAdapterDocument> {
    binding.validate()?;
    reject_decision_input(request)?;
    let request_bytes = serde_json::to_vec(request)?;
    let executable_path = Path::new(&binding.executable_locator);
    let before = hash_plain_file(executable_path, "adapter executable")?;
    if before != binding.executable_sha256.0 {
        bail!("paired adapter executable differs from its frozen binding before launch");
    }
    let working_directory = std::fs::canonicalize(&binding.working_directory)
        .context("canonicalize paired adapter working directory")?;
    if portable_absolute(&working_directory)? != binding.working_directory {
        bail!("paired adapter working directory differs from its canonical binding");
    }
    let execution = execute_bounded(
        &binding.argv,
        &working_directory,
        &binding.environment,
        &request_bytes,
        binding.maximum_wall_time_ms,
        binding.maximum_output_bytes,
    )?;
    let after = hash_plain_file(executable_path, "adapter executable")?;
    if after != before {
        bail!("paired adapter executable changed during invocation");
    }
    if !execution.stderr.is_empty() {
        bail!("successful paired adapter wrote to stderr");
    }
    let output_bytes = strip_one_line_ending(&execution.stdout)?;
    let canonical_value: Value =
        serde_json::from_slice(output_bytes).context("parse paired adapter result")?;
    if serde_json::to_vec(&canonical_value)? != output_bytes {
        bail!("paired adapter result is not canonical compact JSON");
    }
    Ok(ExecutedAdapterDocument {
        binding: binding.clone(),
        request_bytes,
        output_bytes: output_bytes.to_vec(),
        value: canonical_value,
        elapsed_ms: execution.elapsed_ms,
    })
}

fn execute_domain_trial_adapter(
    binding: &PairedAdapterBinding,
    request: &PairedTrialRequest,
) -> Result<DomainTrialResult> {
    let request_value = serde_json::to_value(request)?;
    let execution = execute_adapter_document(binding, &request_value)?;
    let result: DomainTrialResult =
        serde_json::from_value(execution.value).context("type paired trial adapter result")?;
    Ok(result)
}

pub(crate) fn verify_paired_adapter_runtime(binding: &PairedAdapterBinding) -> Result<()> {
    binding.validate()?;
    let executable = Path::new(&binding.executable_locator);
    if hash_plain_file(executable, "paired adapter executable")? != binding.executable_sha256.0 {
        bail!("paired adapter executable differs from its frozen binding");
    }
    let working_directory = std::fs::canonicalize(&binding.working_directory)
        .context("canonicalize paired adapter working directory")?;
    if portable_absolute(&working_directory)? != binding.working_directory {
        bail!("paired adapter working directory differs from its canonical binding");
    }
    Ok(())
}

pub(crate) fn parse_validate_paired_trial_result(
    binding: &PairedAdapterBinding,
    request: &PairedTrialRequest,
    stdout: &[u8],
) -> Result<(Vec<u8>, DomainTrialResult)> {
    let output_bytes = strip_one_line_ending(stdout)?.to_vec();
    let value: Value =
        serde_json::from_slice(&output_bytes).context("parse paired trial result")?;
    if serde_json::to_vec(&value)? != output_bytes {
        bail!("paired trial result is not canonical compact JSON");
    }
    let result: DomainTrialResult =
        serde_json::from_value(value).context("type paired trial adapter result")?;
    let request_sha256 = sha256(&serde_json::to_vec(&serde_json::to_value(request)?)?);
    validate_domain_trial_result(binding, request, &result, &request_sha256)?;
    Ok((output_bytes, result))
}

pub(crate) fn validate_domain_trial_result(
    binding: &PairedAdapterBinding,
    request: &PairedTrialRequest,
    result: &DomainTrialResult,
    request_sha256: &str,
) -> Result<()> {
    if result.schema != binding.result_schema {
        bail!("paired trial result schema differs from its frozen adapter binding");
    }
    if result.execution_id != request.execution_id {
        bail!("paired trial result execution id differs from its frozen request");
    }
    if result.request_sha256.0 != request_sha256 {
        bail!("paired trial result request digest differs from its exact canonical request");
    }
    if result.adapter_executable_sha256 != binding.executable_sha256 {
        bail!("paired trial result executable digest differs from its frozen adapter binding");
    }
    if result.participant_identity_sha256 != request.participant.identity_sha256 {
        bail!("paired trial result participant differs from its frozen request");
    }
    if !result.domain_authority.is_object() {
        bail!("paired trial result domain authority must be an object");
    }
    result.domain_trial_receipt_sha256()?;
    let expected = request
        .objectives
        .iter()
        .map(|objective| objective.objective.as_str())
        .collect::<BTreeSet<_>>();
    let observed = result
        .measurements
        .iter()
        .map(|measurement| measurement.objective.as_str())
        .collect::<BTreeSet<_>>();
    if observed.len() != result.measurements.len() || observed != expected {
        bail!("paired trial result must contain every admitted objective exactly once");
    }
    Ok(())
}

fn trial_execution_id(
    experiment_id: &str,
    schedule_sha256: &Sha256Digest,
    block_index: u32,
    role: PairedParticipantRole,
    participant: &PairedExecutionParticipant,
) -> String {
    let role = match role {
        PairedParticipantRole::Baseline => "baseline",
        PairedParticipantRole::Candidate => "candidate",
    };
    sha256(
        format!(
            "papertiger-mise.paired-trial.v1\n{experiment_id}\n{}\n{block_index}\n{role}\n{}\n{}",
            schedule_sha256.0, participant.identity_sha256.0, participant.revision
        )
        .as_bytes(),
    )
}

fn validate_result(
    binding: &PairedAdapterBinding,
    result: &DomainObservationResult,
    request_sha256: &str,
) -> Result<()> {
    if result.schema != binding.result_schema
        || result.request_sha256.0 != request_sha256
        || result.adapter_executable_sha256 != binding.executable_sha256
    {
        bail!(
            "paired adapter result differs from its frozen executable, protocol, or exact request"
        );
    }
    validate_token("domain observation id", &result.observation_id)?;
    for (label, digest) in [
        (
            "domain baseline identity",
            &result.baseline.identity_sha256.0,
        ),
        (
            "domain candidate identity",
            &result.candidate.identity_sha256.0,
        ),
    ] {
        validate_sha256(digest, label)?;
    }
    if !result.domain_authority.is_object() || !result.domain_context.is_object() {
        bail!("paired adapter result requires object-valued domain authority and context");
    }
    if result.baseline.key == result.candidate.key || result.blocks.is_empty() {
        bail!("paired adapter result requires distinct participants and at least one block");
    }
    let mut blocks = BTreeSet::new();
    let mut receipts = BTreeSet::new();
    for block in &result.blocks {
        if !blocks.insert(block.block_index)
            || !receipts.insert(block.baseline.receipt_sha256.0.as_str())
            || !receipts.insert(block.candidate.receipt_sha256.0.as_str())
            || block.measurements.is_empty()
        {
            bail!("paired adapter result repeats a block or domain trial receipt");
        }
        validate_sha256(
            &block.baseline.receipt_sha256.0,
            "baseline domain trial receipt",
        )?;
        validate_sha256(
            &block.candidate.receipt_sha256.0,
            "candidate domain trial receipt",
        )?;
        if block.baseline.started_at.trim() != block.baseline.started_at
            || block.candidate.started_at.trim() != block.candidate.started_at
            || block.baseline.started_at.is_empty()
            || block.candidate.started_at.is_empty()
        {
            bail!("paired adapter result contains a noncanonical domain start time");
        }
        let mut objectives = BTreeSet::new();
        for measurement in &block.measurements {
            validate_token("domain objective", &measurement.objective)?;
            if !objectives.insert(measurement.objective.as_str()) {
                bail!("paired adapter result repeats an objective within one block");
            }
        }
    }
    Ok(())
}

fn reject_decision_input(value: &Value) -> Result<()> {
    const PROHIBITED: &[&str] = &[
        "baseline_units",
        "candidate_units",
        "classification",
        "decision_eligible",
        "disposition",
        "measurements",
        "nomination",
    ];
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                if PROHIBITED.contains(&key.as_str()) {
                    bail!("paired adapter request may not contain caller-supplied '{key}'");
                }
                reject_decision_input(nested)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                reject_decision_input(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn strip_one_line_ending(stdout: &[u8]) -> Result<&[u8]> {
    let stripped = stdout
        .strip_suffix(b"\r\n")
        .or_else(|| stdout.strip_suffix(b"\n"))
        .unwrap_or(stdout);
    if stripped.is_empty() || stripped.contains(&b'\n') || stripped.contains(&b'\r') {
        bail!("paired adapter must emit exactly one compact JSON document");
    }
    Ok(stripped)
}

fn hash_plain_file(path: &Path, role: &str) -> Result<String> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect {role} {}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("{role} must be a plain file");
    }
    Ok(sha256(&std::fs::read(path)?))
}

fn validate_absolute_syntax(field: &str, value: &str) -> Result<()> {
    let path = Path::new(value);
    if !path.is_absolute()
        || path.components().any(|component| {
            !matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
    {
        bail!("{field} must be a normalized absolute local path");
    }
    Ok(())
}

fn validate_absolute_existing(field: &str, value: &str, file: bool) -> Result<()> {
    validate_absolute_syntax(field, value)?;
    let path = Path::new(value);
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("inspect {field} {value}"))?;
    if (file && (!metadata.is_file() || metadata.file_type().is_symlink()))
        || (!file && !metadata.is_dir())
    {
        bail!("{field} has the wrong filesystem type");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statistics::{PairedDisposition, classify_paired_fixed};

    #[test]
    fn decision_fields_are_refused_recursively() {
        let request = serde_json::json!({"domain":{"measurements":[]}});
        assert!(reject_decision_input(&request).is_err());
    }

    #[test]
    fn mise_sequences_every_adjacent_pair_before_classifying_adapter_results() {
        let plan = crate::statistics::tests::plan();
        let objectives = crate::statistics::tests::objectives();
        let candidate_identity = Sha256Digest("e".repeat(64));
        let candidate = PairedCandidateContext {
            cohort: PairedCohort::Research {
                candidate_analysis_slot: 1,
            },
            candidate_identity_sha256: &candidate_identity,
            revealed_order_seed: b"mise-test-research-order-seed-0001",
        };
        let participants = PairedExecutionParticipants {
            baseline: PairedExecutionParticipant {
                identity_sha256: Sha256Digest("b".repeat(64)),
                revision: "baseline-tree".to_owned(),
            },
            candidate: PairedExecutionParticipant {
                identity_sha256: candidate_identity.clone(),
                revision: "candidate-tree".to_owned(),
            },
        };
        let mut calls = Vec::new();
        let trial_adapter = plan.trial_adapter.as_ref().expect("trial adapter");
        let verified = execute_paired_adapter_cohort_with(
            "paired-fixture",
            &plan,
            &objectives,
            &candidate,
            &participants,
            |request| {
                calls.push((request.block_index, request.participant_role));
                let measurements = request
                    .objectives
                    .iter()
                    .map(|objective| DomainTrialMeasurement {
                        objective: objective.objective.clone(),
                        units: match (objective.objective.as_str(), request.participant_role) {
                            ("correct", _) => 1,
                            ("frame-ms", PairedParticipantRole::Baseline) => 10_000,
                            ("frame-ms", PairedParticipantRole::Candidate) => 8_000,
                            ("memory-mib", _) => 100_000,
                            _ => unreachable!("frozen objective"),
                        },
                    })
                    .collect();
                Ok(DomainTrialResult {
                    schema: trial_adapter.result_schema.clone(),
                    execution_id: request.execution_id.clone(),
                    request_sha256: Sha256Digest(sha256(&serde_json::to_vec(
                        &serde_json::to_value(request)?,
                    )?)),
                    adapter_executable_sha256: trial_adapter.executable_sha256.clone(),
                    participant_identity_sha256: request.participant.identity_sha256.clone(),
                    domain_trial_receipt: serde_json::json!({
                        "execution_id": request.execution_id,
                        "fixture": true,
                    }),
                    domain_authority: serde_json::json!({"fixture": true}),
                    measurements,
                })
            },
        )
        .expect("execute frozen cohort");
        assert_eq!(
            calls.len(),
            usize::try_from(plan.required_blocks).unwrap() * 2
        );
        for (pair, block) in calls.chunks_exact(2).zip(&verified.blocks) {
            assert_eq!(pair[0].0, block.block_index);
            assert_eq!(pair[1].0, block.block_index);
            assert_eq!(
                [pair[0].1, pair[1].1],
                match block.order {
                    PairedRunOrder::BaselineFirst => [
                        PairedParticipantRole::Baseline,
                        PairedParticipantRole::Candidate,
                    ],
                    PairedRunOrder::CandidateFirst => [
                        PairedParticipantRole::Candidate,
                        PairedParticipantRole::Baseline,
                    ],
                }
            );
        }
        let classification =
            classify_paired_fixed(&plan, &objectives, &candidate, &verified.blocks)
                .expect("classify only the executed receipt-bound blocks");
        assert_eq!(classification.disposition, PairedDisposition::Qualified);
    }
}
