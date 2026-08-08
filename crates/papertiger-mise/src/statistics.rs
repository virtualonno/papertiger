use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::candidate::CandidateDisposition;
use crate::digest::{sha256, sha256_bytes, validate_sha256};
use crate::manifest::{ObjectiveDirection, ObjectiveRole, ObjectiveSpec, Sha256Digest};
use crate::store::{begin_mutation, campaign, now, record_event_in_mutation};
use crate::validation::validate_ascii_token as validate_token;

pub const PAIRED_ANALYSIS_SCHEMA_V1: &str = "papertiger-mise.paired-analysis.v1";
pub const PAIRED_ANALYSIS_SCHEMA_V2: &str = "papertiger-mise.paired-analysis.v2";
pub const PAIRED_MEASUREMENT_PROTOCOL_V1: &str = "papertiger-mise.paired-measurement.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedAnalysisMethod {
    FixedSampleExactPairedBinomial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedInferenceScope {
    IndependentBlockPopulation {
        population: String,
        sampling_protocol_locator: String,
        sampling_protocol_sha256: Sha256Digest,
    },
    FixedScheduleOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RationalThreshold {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedObjectivePolicy {
    pub objective: String,
    /// Decimal scale shared by every integer measurement and threshold for
    /// this objective. `scale10 = 3` means one stored unit is 0.001 of `unit`.
    pub scale10: u8,
    pub measurement_summary_protocol: String,
    pub minimum_practical_change_units: i64,
    pub regression_tolerance_units: i64,
    pub acceptance_threshold_units: Option<i64>,
    pub target_value_units: Option<i64>,
    pub no_op_maximum_absolute_effect_units: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedBlockDesign {
    pub block_index: u32,
    pub stratum: String,
    pub fixture_locator: String,
    pub fixture_sha256: Sha256Digest,
    pub environment_profile_sha256: Sha256Digest,
    pub workload_seed: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedFixtureBinding {
    pub locator: String,
    pub sha256: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedCalibrationFixtureBindings {
    pub no_op: PairedFixtureBinding,
    pub known_bad: PairedFixtureBinding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedSlotSeedCommitment {
    pub candidate_analysis_slot: u32,
    pub sha256: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedCalibrationSeedCommitments {
    pub no_op_sha256: Sha256Digest,
    pub known_bad_sha256: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedAnalysisPlan {
    pub schema: String,
    pub method: PairedAnalysisMethod,
    /// Exact external process that derives one raw domain trial result. Mise
    /// owns schedule and classification; the adapter owns domain execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_adapter: Option<crate::adapter::PairedAdapterBinding>,
    pub inference_scope: PairedInferenceScope,
    pub required_blocks: u32,
    /// Content-addressed procedure implemented by the trusted scheduler for
    /// generating, retaining, and revealing independent seed material.
    pub order_seed_protocol_locator: String,
    pub order_seed_protocol_sha256: Sha256Digest,
    /// One independent secret-seed commitment per candidate analysis. A seed
    /// is revealed only after the candidate assigned to that slot is frozen.
    pub order_seed_commitments: Vec<PairedSlotSeedCommitment>,
    pub calibration_seed_commitments: PairedCalibrationSeedCommitments,
    pub campaign_familywise_alpha: RationalThreshold,
    /// Finite Bonferroni boundary over candidate confirmation attempts. Holm
    /// correction is then applied across objectives inside each attempt.
    pub maximum_candidate_analyses: u32,
    pub objective_policies: Vec<PairedObjectivePolicy>,
    pub calibration_fixtures: PairedCalibrationFixtureBindings,
    pub blocks: Vec<PairedBlockDesign>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedCohort {
    Research { candidate_analysis_slot: u32 },
    NoOpCalibration,
    KnownBadCalibration,
}

impl PairedCohort {
    fn label(self) -> String {
        match self {
            Self::Research {
                candidate_analysis_slot,
            } => format!("research:{candidate_analysis_slot}"),
            Self::NoOpCalibration => "calibration:no-op".to_owned(),
            Self::KnownBadCalibration => "calibration:known-bad".to_owned(),
        }
    }

    fn research_slot(self) -> Option<u32> {
        match self {
            Self::Research {
                candidate_analysis_slot,
            } => Some(candidate_analysis_slot),
            Self::NoOpCalibration | Self::KnownBadCalibration => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PairedCandidateContext<'a> {
    pub cohort: PairedCohort,
    pub candidate_identity_sha256: &'a Sha256Digest,
    pub revealed_order_seed: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedRunOrder {
    BaselineFirst,
    CandidateFirst,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedBlockObservation {
    pub block_index: u32,
    pub order: PairedRunOrder,
    pub fixture_sha256: Sha256Digest,
    pub environment_profile_sha256: Sha256Digest,
    pub workload_seed: String,
    pub baseline_trial_receipt_sha256: Sha256Digest,
    pub candidate_trial_receipt_sha256: Sha256Digest,
    pub observations: Vec<PairedObjectiveObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedObjectiveObservation {
    pub objective: String,
    pub baseline_units: i64,
    pub candidate_units: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedHypothesisKind {
    PracticalImprovement,
    Noninferiority,
    Regression,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactPValue {
    pub numerator: u64,
    pub denominator: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedHypothesisResult {
    pub kind: PairedHypothesisKind,
    pub margin_units: i64,
    pub threshold_successes: u32,
    pub threshold_failures: u32,
    /// Exact equality is reported separately but remains part of the fixed
    /// sample: equality is success for inclusive practical/noninferiority
    /// contracts and failure for a strict regression contract.
    pub exact_equalities: u32,
    pub one_sided_p: Option<ExactPValue>,
    pub holm_rank: u32,
    pub significant: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedObjectiveResult {
    pub key: String,
    pub role: ObjectiveRole,
    pub direction: ObjectiveDirection,
    pub unit: String,
    pub scale10: u8,
    pub measurement_summary_protocol: String,
    pub median_baseline: MedianOrderStatistics,
    pub median_candidate: MedianOrderStatistics,
    pub median_improvement: MedianOrderStatistics,
    pub acceptance_passed_in_every_block: Option<bool>,
    pub hypotheses: Vec<PairedHypothesisResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedDisposition {
    Rejected,
    Inconclusive,
    Qualified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedClassification {
    pub disposition: PairedDisposition,
    pub reasons: Vec<String>,
    pub required_blocks: u32,
    pub candidate_identity_sha256: Sha256Digest,
    pub cohort: PairedCohort,
    pub schedule_sha256: Sha256Digest,
    pub observations_sha256: Sha256Digest,
    pub inference_scope: PairedInferenceScope,
    pub objectives: Vec<PairedObjectiveResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MedianOrderStatistics {
    /// For odd cohorts `lower_units == upper_units`. For even cohorts the
    /// exact median is the closed interval midpoint of these adjacent values,
    /// avoiding floating-point or rounding in evidence.
    pub lower_units: i64,
    pub upper_units: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoOpCalibrationResult {
    pub passed: bool,
    pub reasons: Vec<String>,
    pub maximum_absolute_effect_units: BTreeMap<String, i64>,
    pub practical_false_positive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedAnalysisSlotRecord {
    pub campaign_id: String,
    pub slot: u32,
    pub candidate_id: String,
    pub schedule_sha256: Sha256Digest,
    pub revealed_order_seed: Vec<u8>,
    pub reserved_at: String,
}

#[derive(Serialize)]
struct PairedPlanIdentity<'a> {
    schema: &'static str,
    plan: PairedAnalysisPlan,
    objectives: Vec<PairedObjectiveIdentity<'a>>,
}

#[derive(Serialize)]
struct PairedObjectiveIdentity<'a> {
    key: &'a str,
    role: ObjectiveRole,
    direction: ObjectiveDirection,
    unit: &'a str,
}

impl PairedAnalysisPlan {
    pub fn validate(&self, objectives: &[ObjectiveSpec]) -> Result<()> {
        if !matches!(
            self.schema.as_str(),
            PAIRED_ANALYSIS_SCHEMA_V1 | PAIRED_ANALYSIS_SCHEMA_V2
        ) {
            bail!("unsupported paired analysis schema '{}'", self.schema);
        }
        match (self.schema.as_str(), &self.trial_adapter) {
            (PAIRED_ANALYSIS_SCHEMA_V1, Some(_)) => {
                bail!("paired analysis v1 cannot claim a frozen trial adapter")
            }
            (PAIRED_ANALYSIS_SCHEMA_V2, None) => {
                bail!("paired analysis v2 requires an exact frozen trial adapter")
            }
            _ => {}
        }
        if self.method != PairedAnalysisMethod::FixedSampleExactPairedBinomial {
            bail!("unsupported paired analysis method");
        }
        if let Some(adapter) = &self.trial_adapter {
            adapter.validate_contract()?;
        }
        if let PairedInferenceScope::IndependentBlockPopulation {
            population,
            sampling_protocol_locator,
            sampling_protocol_sha256,
        } = &self.inference_scope
        {
            validate_token("paired inference population", population)?;
            validate_token(
                "paired inference sampling protocol locator",
                sampling_protocol_locator,
            )?;
            validate_sha256(
                &sampling_protocol_sha256.0,
                "paired inference sampling protocol",
            )?;
        }
        if !(2..=32).contains(&self.required_blocks) {
            bail!("fixed exact-threshold analysis requires between 2 and 32 paired blocks");
        }
        validate_token(
            "paired order seed protocol locator",
            &self.order_seed_protocol_locator,
        )?;
        validate_sha256(
            &self.order_seed_protocol_sha256.0,
            "paired order seed protocol",
        )?;
        if self.blocks.len() != usize::try_from(self.required_blocks)? {
            bail!("paired block designs must match required_blocks exactly");
        }
        self.campaign_familywise_alpha.validate()?;
        if u128::from(self.campaign_familywise_alpha.numerator) * 10
            > u128::from(self.campaign_familywise_alpha.denominator)
        {
            bail!("campaign familywise alpha must be no greater than 1/10");
        }
        if self.maximum_candidate_analyses == 0 {
            bail!("maximum_candidate_analyses must be greater than zero");
        }
        if self.order_seed_commitments.len() != usize::try_from(self.maximum_candidate_analyses)? {
            bail!("paired analysis requires exactly one seed commitment per candidate slot");
        }
        let mut seed_slots = BTreeSet::new();
        let mut seed_commitment_digests = BTreeSet::new();
        for commitment in &self.order_seed_commitments {
            if commitment.candidate_analysis_slot == 0
                || commitment.candidate_analysis_slot > self.maximum_candidate_analyses
                || !seed_slots.insert(commitment.candidate_analysis_slot)
            {
                bail!("paired seed commitments must identify every candidate slot exactly once");
            }
            validate_sha256(&commitment.sha256.0, "paired order seed commitment")?;
            if !seed_commitment_digests.insert(commitment.sha256.0.as_str()) {
                bail!("paired cohort seed commitments must be domain-distinct");
            }
        }
        for (role, digest) in [
            (
                "paired no-op calibration seed commitment",
                &self.calibration_seed_commitments.no_op_sha256,
            ),
            (
                "paired known-bad calibration seed commitment",
                &self.calibration_seed_commitments.known_bad_sha256,
            ),
        ] {
            validate_sha256(&digest.0, role)?;
            if !seed_commitment_digests.insert(digest.0.as_str()) {
                bail!("paired cohort seed commitments must be domain-distinct");
            }
        }
        for (role, fixture) in [
            (
                "paired no-op calibration fixture",
                &self.calibration_fixtures.no_op,
            ),
            (
                "paired known-bad calibration fixture",
                &self.calibration_fixtures.known_bad,
            ),
        ] {
            validate_token(&format!("{role} locator"), &fixture.locator)?;
            validate_sha256(&fixture.sha256.0, role)?;
        }
        let expected = objectives
            .iter()
            .map(|objective| objective.key.as_str())
            .collect::<BTreeSet<_>>();
        if expected.len() != objectives.len() {
            bail!("paired analysis objectives must be unique");
        }
        let definitions = objectives
            .iter()
            .map(|objective| (objective.key.as_str(), objective))
            .collect::<BTreeMap<_, _>>();
        let mut policies = BTreeMap::new();
        for policy in &self.objective_policies {
            validate_token("paired objective key", &policy.objective)?;
            validate_token(
                "paired objective measurement summary protocol",
                &policy.measurement_summary_protocol,
            )?;
            if policy.scale10 > 12 {
                bail!("paired objective '{}' scale10 exceeds 12", policy.objective);
            }
            for (field, value) in [
                (
                    "minimum_practical_change_units",
                    policy.minimum_practical_change_units,
                ),
                (
                    "regression_tolerance_units",
                    policy.regression_tolerance_units,
                ),
                (
                    "no_op_maximum_absolute_effect_units",
                    policy.no_op_maximum_absolute_effect_units,
                ),
            ] {
                if value < 0 {
                    bail!(
                        "paired objective '{}' {field} must be nonnegative",
                        policy.objective
                    );
                }
            }
            let definition = definitions
                .get(policy.objective.as_str())
                .with_context(|| format!("unknown paired objective '{}'", policy.objective))?;
            require_exact_policy_match(
                &policy.objective,
                "minimum_practical_change",
                definition.minimum_practical_change,
                policy.minimum_practical_change_units,
                policy.scale10,
            )?;
            require_exact_policy_match(
                &policy.objective,
                "regression_tolerance",
                definition.regression_tolerance,
                policy.regression_tolerance_units,
                policy.scale10,
            )?;
            require_exact_optional_policy_match(
                &policy.objective,
                "acceptance_threshold",
                definition.acceptance_threshold,
                policy.acceptance_threshold_units,
                policy.scale10,
            )?;
            require_exact_optional_policy_match(
                &policy.objective,
                "target_value",
                definition.target_value,
                policy.target_value_units,
                policy.scale10,
            )?;
            if definition.role == ObjectiveRole::Primary
                && policy.minimum_practical_change_units
                    <= policy.no_op_maximum_absolute_effect_units
            {
                bail!(
                    "primary objective '{}' practical threshold must exceed its no-op noise ceiling",
                    policy.objective
                );
            }
            if matches!(
                definition.role,
                ObjectiveRole::Primary | ObjectiveRole::Protected
            ) && policy.regression_tolerance_units < policy.no_op_maximum_absolute_effect_units
            {
                bail!(
                    "objective '{}' regression tolerance is below its no-op noise ceiling",
                    policy.objective
                );
            }
            if definition.role == ObjectiveRole::HardConstraint
                && policy.acceptance_threshold_units.is_none()
            {
                bail!(
                    "hard constraint '{}' requires an exact acceptance threshold",
                    policy.objective
                );
            }
            match definition.direction {
                ObjectiveDirection::Target if policy.target_value_units.is_none() => bail!(
                    "target objective '{}' requires an exact target value",
                    policy.objective
                ),
                ObjectiveDirection::Minimize | ObjectiveDirection::Maximize
                    if policy.target_value_units.is_some() =>
                {
                    bail!(
                        "non-target objective '{}' must not declare an exact target value",
                        policy.objective
                    );
                }
                _ => {}
            }
            if policies.insert(policy.objective.as_str(), policy).is_some() {
                bail!(
                    "duplicate paired objective policy for '{}'",
                    policy.objective
                );
            }
        }
        if policies.keys().copied().collect::<BTreeSet<_>>() != expected {
            bail!("paired objective policies must identify every objective exactly once");
        }
        let mut block_indices = BTreeSet::new();
        let mut stratum_sizes = BTreeMap::<&str, usize>::new();
        for block in &self.blocks {
            if block.block_index >= self.required_blocks || !block_indices.insert(block.block_index)
            {
                bail!("paired block designs must contain every frozen index exactly once");
            }
            validate_token("paired block stratum", &block.stratum)?;
            *stratum_sizes.entry(block.stratum.as_str()).or_default() += 1;
            validate_token("paired block workload seed", &block.workload_seed)?;
            validate_token("paired block fixture locator", &block.fixture_locator)?;
            validate_sha256(&block.fixture_sha256.0, "paired block fixture")?;
            validate_sha256(
                &block.environment_profile_sha256.0,
                "paired block environment profile",
            )?;
        }
        for (stratum, size) in stratum_sizes {
            if size < 2 || !size.is_multiple_of(2) {
                bail!(
                    "paired stratum '{stratum}' must contain an even number of at least two blocks"
                );
            }
        }
        if matches!(
            self.inference_scope,
            PairedInferenceScope::IndependentBlockPopulation { .. }
        ) {
            let family = hypothesis_family_size(objectives)?;
            let minimum_p_denominator = 1_u64 << self.required_blocks;
            if u128::from(self.campaign_familywise_alpha.denominator)
                * u128::from(family)
                * u128::from(self.maximum_candidate_analyses)
                > u128::from(self.campaign_familywise_alpha.numerator)
                    * u128::from(minimum_p_denominator)
            {
                bail!(
                    "paired design is underpowered: even unanimous threshold successes cannot clear the campaign-wide first Holm boundary"
                );
            }
        }
        Ok(())
    }

    pub fn canonical_bytes(&self, objectives: &[ObjectiveSpec]) -> Result<Vec<u8>> {
        self.validate(objectives)?;
        let canonical = self.canonicalized();
        let mut objective_identities = objectives
            .iter()
            .map(|objective| PairedObjectiveIdentity {
                key: &objective.key,
                role: objective.role,
                direction: objective.direction,
                unit: &objective.unit,
            })
            .collect::<Vec<_>>();
        objective_identities.sort_by(|left, right| left.key.cmp(right.key));
        Ok(serde_json::to_vec(&PairedPlanIdentity {
            schema: "papertiger-mise.paired-plan-identity.v1",
            plan: canonical,
            objectives: objective_identities,
        })?)
    }

    pub fn sha256(&self, objectives: &[ObjectiveSpec]) -> Result<String> {
        Ok(sha256(&self.canonical_bytes(objectives)?))
    }

    pub(crate) fn canonicalized(&self) -> Self {
        let mut canonical = self.clone();
        canonical
            .order_seed_commitments
            .sort_by_key(|commitment| commitment.candidate_analysis_slot);
        canonical
            .objective_policies
            .sort_by(|left, right| left.objective.cmp(&right.objective));
        canonical.blocks.sort_by_key(|block| block.block_index);
        canonical
    }

    pub fn objective_policy(&self, key: &str) -> Option<&PairedObjectivePolicy> {
        self.objective_policies
            .iter()
            .find(|policy| policy.objective == key)
    }
}

impl RationalThreshold {
    fn validate(&self) -> Result<()> {
        if self.numerator == 0 || self.denominator == 0 || self.numerator >= self.denominator {
            bail!("familywise alpha must be a proper positive rational threshold");
        }
        Ok(())
    }
}

/// Derive a deterministic order balanced within each stratum. The seed must
/// match the pre-candidate commitment, and candidate identity is part of the
/// derivation, preventing an admitted proposer from specializing to a known
/// order without changing its immutable candidate digest.
pub fn paired_run_order(
    plan: &PairedAnalysisPlan,
    objectives: &[ObjectiveSpec],
    candidate: &PairedCandidateContext<'_>,
    block_index: u32,
) -> Result<PairedRunOrder> {
    plan.validate(objectives)?;
    validate_candidate_context(plan, candidate)?;
    let design = plan
        .blocks
        .iter()
        .find(|block| block.block_index == block_index)
        .context("paired block index exceeds the frozen fixed-sample design")?;
    let cohort_label = candidate.cohort.label();
    let research_slot = candidate.cohort.research_slot().unwrap_or(0);
    let stratum_blocks = plan
        .blocks
        .iter()
        .filter(|block| block.stratum == design.stratum)
        .collect::<Vec<_>>();
    let mut ranked = stratum_blocks
        .iter()
        .map(|block| {
            let mut identity = b"papertiger-mise.paired-order.v1\0".to_vec();
            identity.extend_from_slice(plan.sha256(objectives)?.as_bytes());
            identity.push(0);
            identity.extend_from_slice(candidate.candidate_identity_sha256.0.as_bytes());
            identity.push(0);
            identity.extend_from_slice(&research_slot.to_le_bytes());
            identity.push(0);
            identity.extend_from_slice(cohort_label.as_bytes());
            identity.push(0);
            identity.extend_from_slice(design.stratum.as_bytes());
            identity.push(0);
            identity.extend_from_slice(&block.block_index.to_le_bytes());
            identity.push(0);
            identity.extend_from_slice(candidate.revealed_order_seed);
            Ok::<_, anyhow::Error>((sha256_bytes(&identity).to_vec(), block.block_index))
        })
        .collect::<Result<Vec<_>>>()?;
    ranked.sort();
    let baseline_first = stratum_blocks.len() / 2;
    let rank = ranked
        .iter()
        .position(|(_, index)| *index == block_index)
        .context("paired block disappeared from its frozen order schedule")?;
    Ok(if rank < baseline_first {
        PairedRunOrder::BaselineFirst
    } else {
        PairedRunOrder::CandidateFirst
    })
}

/// Identity of the complete realized schedule, including the candidate, its
/// finite confirmation slot, committed seed reveal, frozen workload designs,
/// and every derived AB/BA order.
pub fn paired_schedule_sha256(
    plan: &PairedAnalysisPlan,
    objectives: &[ObjectiveSpec],
    candidate: &PairedCandidateContext<'_>,
) -> Result<Sha256Digest> {
    plan.validate(objectives)?;
    validate_candidate_context(plan, candidate)?;
    let mut blocks = plan.blocks.iter().collect::<Vec<_>>();
    blocks.sort_by_key(|block| block.block_index);
    let mut identity = Vec::new();
    let cohort_label = candidate.cohort.label();
    let research_slot = candidate.cohort.research_slot().unwrap_or(0);
    identity.extend_from_slice(b"papertiger-mise.paired-schedule.v1\0");
    identity.extend_from_slice(plan.sha256(objectives)?.as_bytes());
    identity.push(0);
    identity.extend_from_slice(cohort_label.as_bytes());
    identity.push(0);
    identity.extend_from_slice(candidate.candidate_identity_sha256.0.as_bytes());
    identity.push(0);
    identity.extend_from_slice(&research_slot.to_le_bytes());
    identity.push(0);
    identity.extend_from_slice(&sha256_bytes(candidate.revealed_order_seed));
    for block in blocks {
        let fixture = cohort_fixture(plan, block, candidate.cohort);
        identity.push(0);
        identity.extend_from_slice(&block.block_index.to_le_bytes());
        identity.push(0);
        identity.extend_from_slice(block.stratum.as_bytes());
        identity.push(0);
        identity.extend_from_slice(fixture.0.as_bytes());
        identity.push(0);
        identity.extend_from_slice(fixture.1.0.as_bytes());
        identity.push(0);
        identity.extend_from_slice(block.environment_profile_sha256.0.as_bytes());
        identity.push(0);
        identity.extend_from_slice(block.workload_seed.as_bytes());
        identity.push(0);
        identity.push(
            match paired_run_order(plan, objectives, candidate, block.block_index)? {
                PairedRunOrder::BaselineFirst => 0,
                PairedRunOrder::CandidateFirst => 1,
            },
        );
    }
    Ok(Sha256Digest(sha256(&identity)))
}

/// Canonical identity of the complete realized paired evidence. The digest
/// includes each unique baseline/candidate trial receipt and every exact
/// scaled-integer observation, while remaining independent of caller order.
pub fn paired_observations_sha256(
    plan: &PairedAnalysisPlan,
    objectives: &[ObjectiveSpec],
    candidate: &PairedCandidateContext<'_>,
    blocks: &[PairedBlockObservation],
) -> Result<Sha256Digest> {
    validate_blocks(plan, objectives, candidate, blocks)?;
    let mut canonical = blocks.to_vec();
    canonical.sort_by_key(|block| block.block_index);
    for block in &mut canonical {
        block
            .observations
            .sort_by(|left, right| left.objective.cmp(&right.objective));
    }
    let mut identity = b"papertiger-mise.paired-observations.v1\0".to_vec();
    identity.extend_from_slice(&serde_json::to_vec(&canonical)?);
    Ok(Sha256Digest(sha256(&identity)))
}

fn validate_candidate_context(
    plan: &PairedAnalysisPlan,
    candidate: &PairedCandidateContext<'_>,
) -> Result<()> {
    validate_sha256(
        &candidate.candidate_identity_sha256.0,
        "paired candidate identity",
    )?;
    if candidate.revealed_order_seed.len() < 32 {
        bail!("paired order seed reveal must contain at least 32 bytes");
    }
    let expected_commitment = match candidate.cohort {
        PairedCohort::Research {
            candidate_analysis_slot,
        } => {
            if candidate_analysis_slot == 0
                || candidate_analysis_slot > plan.maximum_candidate_analyses
            {
                bail!("candidate analysis slot is outside the frozen campaign allocation");
            }
            &plan
                .order_seed_commitments
                .iter()
                .find(|commitment| commitment.candidate_analysis_slot == candidate_analysis_slot)
                .context("paired candidate slot has no seed commitment")?
                .sha256
        }
        PairedCohort::NoOpCalibration => &plan.calibration_seed_commitments.no_op_sha256,
        PairedCohort::KnownBadCalibration => &plan.calibration_seed_commitments.known_bad_sha256,
    };
    let observed = sha256(candidate.revealed_order_seed);
    if observed != expected_commitment.0 {
        bail!("paired order seed reveal does not match its frozen commitment");
    }
    Ok(())
}

/// Classify one complete fixed-sample paired cohort. Supplying fewer or more
/// blocks is an error, so repeated calls cannot turn interim peeking into a
/// candidate decision.
pub fn classify_paired_fixed(
    plan: &PairedAnalysisPlan,
    objectives: &[ObjectiveSpec],
    candidate: &PairedCandidateContext<'_>,
    blocks: &[PairedBlockObservation],
) -> Result<PairedClassification> {
    plan.validate(objectives)?;
    validate_candidate_context(plan, candidate)?;
    let parsed = validate_blocks(plan, objectives, candidate, blocks)?;
    let schedule_sha256 = paired_schedule_sha256(plan, objectives, candidate)?;
    let observations_sha256 = paired_observations_sha256(plan, objectives, candidate, blocks)?;
    let inferential = matches!(
        plan.inference_scope,
        PairedInferenceScope::IndependentBlockPopulation { .. }
    );
    let mut results = objectives
        .iter()
        .map(|definition| {
            let policy = plan
                .objective_policy(&definition.key)
                .context("validated paired objective policy disappeared")?;
            let rows = &parsed[definition.key.as_str()];
            let baselines = rows
                .iter()
                .map(|row| row.baseline_units)
                .collect::<Vec<_>>();
            let candidates = rows
                .iter()
                .map(|row| row.candidate_units)
                .collect::<Vec<_>>();
            let improvements = rows
                .iter()
                .map(|row| {
                    improvement_units(definition, policy, row.baseline_units, row.candidate_units)
                })
                .collect::<Result<Vec<_>>>()?;
            let acceptance_passed_in_every_block = policy
                .acceptance_threshold_units
                .map(|threshold| {
                    candidates.iter().try_fold(true, |passed, candidate| {
                        Ok::<bool, anyhow::Error>(
                            passed
                                && acceptance_passed_units(
                                    definition, policy, *candidate, threshold,
                                )?,
                        )
                    })
                })
                .transpose()?;
            let mut hypotheses = Vec::new();
            if definition.role == ObjectiveRole::Primary {
                hypotheses.push(threshold_test(
                    PairedHypothesisKind::PracticalImprovement,
                    policy.minimum_practical_change_units,
                    &improvements,
                    |value, margin| value >= margin,
                    inferential,
                )?);
            }
            if matches!(
                definition.role,
                ObjectiveRole::Primary | ObjectiveRole::Protected
            ) {
                let margin = policy
                    .regression_tolerance_units
                    .checked_neg()
                    .context("paired regression tolerance overflow")?;
                hypotheses.push(threshold_test(
                    PairedHypothesisKind::Noninferiority,
                    margin,
                    &improvements,
                    |value, margin| value >= margin,
                    inferential,
                )?);
                hypotheses.push(threshold_test(
                    PairedHypothesisKind::Regression,
                    margin,
                    &improvements,
                    |value, margin| value < margin,
                    inferential,
                )?);
            }
            Ok(PairedObjectiveResult {
                key: definition.key.clone(),
                role: definition.role,
                direction: definition.direction,
                unit: definition.unit.clone(),
                scale10: policy.scale10,
                measurement_summary_protocol: policy.measurement_summary_protocol.clone(),
                median_baseline: median(&baselines)?,
                median_candidate: median(&candidates)?,
                median_improvement: median(&improvements)?,
                acceptance_passed_in_every_block,
                hypotheses,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    apply_holm(
        &mut results,
        &plan.campaign_familywise_alpha,
        plan.maximum_candidate_analyses,
    )?;

    let mut reasons = Vec::new();
    let hard_failed = results.iter().any(|result| {
        result.role == ObjectiveRole::HardConstraint
            && result.acceptance_passed_in_every_block != Some(true)
    });
    for result in &results {
        if result.role == ObjectiveRole::HardConstraint
            && result.acceptance_passed_in_every_block != Some(true)
        {
            reasons.push(format!(
                "hard constraint '{}' failed in one or more paired blocks",
                result.key
            ));
        }
    }
    let significant_regressions = results
        .iter()
        .filter(|result| {
            hypothesis(result, PairedHypothesisKind::Regression).is_some_and(|test| {
                test.significant || (!inferential && test.threshold_successes > 0)
            })
        })
        .map(|result| result.key.clone())
        .collect::<Vec<_>>();
    for key in &significant_regressions {
        reasons.push(format!(
            "objective '{key}' shows a regression beyond tolerance under its admitted inference scope"
        ));
    }
    if hard_failed || !significant_regressions.is_empty() {
        return Ok(PairedClassification {
            disposition: PairedDisposition::Rejected,
            reasons,
            required_blocks: plan.required_blocks,
            candidate_identity_sha256: candidate.candidate_identity_sha256.clone(),
            cohort: candidate.cohort,
            schedule_sha256,
            observations_sha256,
            inference_scope: plan.inference_scope.clone(),
            objectives: results,
        });
    }
    let primary_improvement = results.iter().any(|result| {
        result.role == ObjectiveRole::Primary
            && hypothesis(result, PairedHypothesisKind::PracticalImprovement)
                .is_some_and(|test| test.significant)
    });
    let noninferiority_proven = results
        .iter()
        .filter(|result| {
            matches!(
                result.role,
                ObjectiveRole::Primary | ObjectiveRole::Protected
            )
        })
        .all(|result| {
            hypothesis(result, PairedHypothesisKind::Noninferiority)
                .is_some_and(|test| test.significant)
        });
    if !primary_improvement {
        reasons.push(
            "no primary objective cleared its practical threshold under campaign-wide candidate allocation and Holm correction".to_owned(),
        );
    }
    if !noninferiority_proven {
        reasons.push(
            "noninferiority was not proven for every primary and protected objective".to_owned(),
        );
    }
    if !inferential {
        reasons.push(
            "fixed-schedule evidence cannot authorize a population-level qualification".to_owned(),
        );
    }
    Ok(PairedClassification {
        disposition: if inferential && primary_improvement && noninferiority_proven {
            PairedDisposition::Qualified
        } else {
            PairedDisposition::Inconclusive
        },
        reasons,
        required_blocks: plan.required_blocks,
        candidate_identity_sha256: candidate.candidate_identity_sha256.clone(),
        cohort: candidate.cohort,
        schedule_sha256,
        observations_sha256,
        inference_scope: plan.inference_scope.clone(),
        objectives: results,
    })
}

pub fn assess_no_op_cohort(
    plan: &PairedAnalysisPlan,
    objectives: &[ObjectiveSpec],
    candidate: &PairedCandidateContext<'_>,
    blocks: &[PairedBlockObservation],
) -> Result<NoOpCalibrationResult> {
    if candidate.cohort != PairedCohort::NoOpCalibration {
        bail!("no-op assessment requires the independently committed no-op cohort");
    }
    plan.validate(objectives)?;
    let parsed = validate_blocks(plan, objectives, candidate, blocks)?;
    let mut passed = true;
    let mut reasons = Vec::new();
    let mut maximum_absolute_effect_units = BTreeMap::new();
    for definition in objectives {
        let policy = plan
            .objective_policy(&definition.key)
            .context("validated paired objective policy disappeared")?;
        let maximum = parsed[definition.key.as_str()]
            .iter()
            .map(|row| {
                improvement_units(definition, policy, row.baseline_units, row.candidate_units)
                    .and_then(checked_abs)
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .max()
            .unwrap_or(0);
        maximum_absolute_effect_units.insert(definition.key.clone(), maximum);
        if maximum > policy.no_op_maximum_absolute_effect_units {
            passed = false;
            reasons.push(format!(
                "no-op objective '{}' exceeded its frozen noise ceiling",
                definition.key
            ));
        }
    }
    let classification = classify_paired_fixed(plan, objectives, candidate, blocks)?;
    let practical_false_positive = classification.objectives.iter().any(|objective| {
        objective.role == ObjectiveRole::Primary
            && hypothesis(objective, PairedHypothesisKind::PracticalImprovement)
                .is_some_and(|result| result.significant)
    });
    if practical_false_positive {
        passed = false;
        reasons.push("no-op calibration produced a significant practical improvement".to_owned());
    }
    Ok(NoOpCalibrationResult {
        passed,
        reasons,
        maximum_absolute_effect_units,
        practical_false_positive,
    })
}

/// A known-bad candidate is evaluator-integrity evidence, not an ordinary
/// research result. Acceptance or an ambiguous outcome therefore refuses the
/// campaign instead of being recorded as merely inconclusive.
pub fn assess_known_bad_cohort(
    plan: &PairedAnalysisPlan,
    objectives: &[ObjectiveSpec],
    candidate: &PairedCandidateContext<'_>,
    blocks: &[PairedBlockObservation],
) -> Result<PairedClassification> {
    if candidate.cohort != PairedCohort::KnownBadCalibration {
        bail!("known-bad assessment requires the independently committed known-bad cohort");
    }
    let classification = classify_paired_fixed(plan, objectives, candidate, blocks)?;
    if classification.disposition != PairedDisposition::Rejected {
        bail!(
            "known-bad calibration was not rejected; paired evaluator integrity is not established"
        );
    }
    Ok(classification)
}

/// Atomically consume one finite campaign-wide confirmation slot after the
/// candidate already exists durably. Reusing a slot or assigning the same
/// candidate to another slot is refused by both this API and SQLite.
#[allow(clippy::too_many_arguments)]
pub fn reserve_paired_analysis_slot(
    connection: &Connection,
    actor: &str,
    campaign_id: &str,
    candidate_id: &str,
    slot: u32,
    revealed_order_seed: &[u8],
) -> Result<PairedAnalysisSlotRecord> {
    validate_token("paired analysis actor", actor)?;
    validate_token("paired analysis campaign", campaign_id)?;
    validate_sha256(candidate_id, "paired candidate identity")?;
    let campaign = campaign(connection, campaign_id)?
        .with_context(|| format!("unknown campaign '{campaign_id}'"))?;
    let manifest: crate::manifest::CampaignManifest = serde_json::from_str(&campaign.manifest_json)
        .context("durable campaign manifest is not valid typed JSON")?;
    manifest.validate()?;
    let plan = manifest
        .paired_analysis
        .as_ref()
        .context("campaign has no admitted paired analysis contract")?;
    let candidate = crate::lifecycle::candidate(connection, candidate_id)?
        .with_context(|| format!("unknown candidate '{candidate_id}'"))?;
    if candidate.campaign_id != campaign_id {
        bail!("paired analysis candidate belongs to another campaign");
    }
    if !matches!(
        candidate.disposition,
        CandidateDisposition::Materialized | CandidateDisposition::Evaluating
    ) {
        bail!("paired analysis requires a materialized nonterminal candidate");
    }
    if candidate.material_sha256 == manifest.calibration.no_op_material_sha256().0
        || candidate.material_sha256 == manifest.calibration.known_bad_material_sha256().0
    {
        bail!("calibration candidates do not consume research confirmation slots");
    }
    let candidate_identity = Sha256Digest(candidate_id.to_owned());
    let analysis = PairedCandidateContext {
        cohort: PairedCohort::Research {
            candidate_analysis_slot: slot,
        },
        candidate_identity_sha256: &candidate_identity,
        revealed_order_seed,
    };
    let schedule_sha256 = paired_schedule_sha256(plan, &manifest.objectives, &analysis)?;
    let transaction = begin_mutation(connection)?;
    if let Some(existing) = paired_analysis_slot_in(&transaction, campaign_id, slot, candidate_id)?
    {
        if existing.campaign_id == campaign_id
            && existing.slot == slot
            && existing.candidate_id == candidate_id
            && existing.schedule_sha256 == schedule_sha256
            && existing.revealed_order_seed == revealed_order_seed
        {
            transaction.commit()?;
            return Ok(existing);
        }
        bail!("paired analysis slot or candidate is already bound to different immutable state");
    }
    let reserved_at = now();
    transaction.execute(
        "INSERT INTO paired_analysis_slots
         (campaign_id, slot, candidate_id, schedule_sha256, order_seed_reveal, reserved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            campaign_id,
            slot,
            candidate_id,
            schedule_sha256.0,
            revealed_order_seed,
            reserved_at
        ],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "paired_analysis_slot",
        &format!("{campaign_id}:{slot}"),
        "paired-analysis-slot-reserved",
        None,
        Some(&json!({
            "campaign_id": campaign_id,
            "slot": slot,
            "candidate_id": candidate_id,
            "schedule_sha256": schedule_sha256.0,
            "order_seed_reveal_sha256": sha256(revealed_order_seed),
        })),
    )?;
    transaction.commit()?;
    paired_analysis_slot(connection, campaign_id, slot)?
        .context("reserved paired analysis slot disappeared")
}

pub fn paired_analysis_slot(
    connection: &Connection,
    campaign_id: &str,
    slot: u32,
) -> Result<Option<PairedAnalysisSlotRecord>> {
    paired_analysis_slot_query(connection, campaign_id, slot, None)
}

fn paired_analysis_slot_in(
    connection: &Connection,
    campaign_id: &str,
    slot: u32,
    candidate_id: &str,
) -> Result<Option<PairedAnalysisSlotRecord>> {
    paired_analysis_slot_query(connection, campaign_id, slot, Some(candidate_id))
}

fn paired_analysis_slot_query(
    connection: &Connection,
    campaign_id: &str,
    slot: u32,
    candidate_id: Option<&str>,
) -> Result<Option<PairedAnalysisSlotRecord>> {
    let sql = if candidate_id.is_some() {
        "SELECT campaign_id, slot, candidate_id, schedule_sha256, order_seed_reveal, reserved_at
         FROM paired_analysis_slots
         WHERE (campaign_id=?1 AND slot=?2) OR candidate_id=?3
         ORDER BY CASE WHEN campaign_id=?1 AND slot=?2 THEN 0 ELSE 1 END
         LIMIT 1"
    } else {
        "SELECT campaign_id, slot, candidate_id, schedule_sha256, order_seed_reveal, reserved_at
         FROM paired_analysis_slots
         WHERE campaign_id=?1 AND slot=?2"
    };
    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<_> {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Vec<u8>>(4)?,
            row.get::<_, String>(5)?,
        ))
    };
    let raw = if let Some(candidate_id) = candidate_id {
        connection
            .query_row(sql, params![campaign_id, slot, candidate_id], map_row)
            .optional()?
    } else {
        connection
            .query_row(sql, params![campaign_id, slot], map_row)
            .optional()?
    };
    Ok(raw.map(
        |(campaign_id, slot, candidate_id, schedule_sha256, revealed_order_seed, reserved_at)| {
            PairedAnalysisSlotRecord {
                campaign_id,
                slot,
                candidate_id,
                schedule_sha256: Sha256Digest(schedule_sha256),
                revealed_order_seed,
                reserved_at,
            }
        },
    ))
}

fn validate_blocks<'a>(
    plan: &PairedAnalysisPlan,
    objectives: &'a [ObjectiveSpec],
    candidate: &PairedCandidateContext<'_>,
    blocks: &'a [PairedBlockObservation],
) -> Result<BTreeMap<&'a str, Vec<&'a PairedObjectiveObservation>>> {
    if blocks.len() != usize::try_from(plan.required_blocks)? {
        bail!(
            "fixed-sample paired analysis requires exactly {} blocks; received {}",
            plan.required_blocks,
            blocks.len()
        );
    }
    let expected_objectives = objectives
        .iter()
        .map(|objective| objective.key.as_str())
        .collect::<BTreeSet<_>>();
    let mut seen_blocks = BTreeSet::new();
    let mut seen_trial_receipts = BTreeSet::new();
    let mut parsed = objectives
        .iter()
        .map(|objective| (objective.key.as_str(), Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for block in blocks {
        if !seen_blocks.insert(block.block_index) || block.block_index >= plan.required_blocks {
            bail!("paired blocks must contain every frozen index exactly once");
        }
        if block.order != paired_run_order(plan, objectives, candidate, block.block_index)? {
            bail!("paired block order differs from the frozen randomized schedule");
        }
        let design = plan
            .blocks
            .iter()
            .find(|design| design.block_index == block.block_index)
            .context("paired block design disappeared")?;
        let fixture = cohort_fixture(plan, design, candidate.cohort);
        if block.fixture_sha256 != *fixture.1
            || block.environment_profile_sha256 != design.environment_profile_sha256
            || block.workload_seed != design.workload_seed
        {
            bail!("paired block evidence differs from its frozen workload design");
        }
        validate_sha256(
            &block.baseline_trial_receipt_sha256.0,
            "paired baseline trial receipt",
        )?;
        validate_sha256(
            &block.candidate_trial_receipt_sha256.0,
            "paired candidate trial receipt",
        )?;
        if block.baseline_trial_receipt_sha256 == block.candidate_trial_receipt_sha256
            || !seen_trial_receipts.insert(block.baseline_trial_receipt_sha256.0.as_str())
            || !seen_trial_receipts.insert(block.candidate_trial_receipt_sha256.0.as_str())
        {
            bail!("each paired run requires a unique immutable trial receipt");
        }
        let observations = block
            .observations
            .iter()
            .map(|observation| (observation.objective.as_str(), observation))
            .collect::<BTreeMap<_, _>>();
        if observations.len() != block.observations.len()
            || observations.keys().copied().collect::<BTreeSet<_>>() != expected_objectives
        {
            bail!("each paired block requires exactly one observation per objective");
        }
        for (key, observation) in observations {
            parsed
                .get_mut(key)
                .context("paired objective disappeared from parsed cohort")?
                .push(observation);
        }
    }
    Ok(parsed)
}

fn cohort_fixture<'a>(
    plan: &'a PairedAnalysisPlan,
    block: &'a PairedBlockDesign,
    cohort: PairedCohort,
) -> (&'a str, &'a Sha256Digest) {
    match cohort {
        PairedCohort::Research { .. } => (&block.fixture_locator, &block.fixture_sha256),
        PairedCohort::NoOpCalibration => (
            &plan.calibration_fixtures.no_op.locator,
            &plan.calibration_fixtures.no_op.sha256,
        ),
        PairedCohort::KnownBadCalibration => (
            &plan.calibration_fixtures.known_bad.locator,
            &plan.calibration_fixtures.known_bad.sha256,
        ),
    }
}

fn threshold_test<F>(
    kind: PairedHypothesisKind,
    margin_units: i64,
    improvements: &[i64],
    success: F,
    inferential: bool,
) -> Result<PairedHypothesisResult>
where
    F: Fn(i64, i64) -> bool,
{
    let threshold_successes = u32::try_from(
        improvements
            .iter()
            .filter(|value| success(**value, margin_units))
            .count(),
    )?;
    let threshold_failures = u32::try_from(improvements.len())?
        .checked_sub(threshold_successes)
        .context("paired threshold counts overflow")?;
    let exact_equalities = u32::try_from(
        improvements
            .iter()
            .filter(|value| **value == margin_units)
            .count(),
    )?;
    let one_sided_p = inferential
        .then(|| {
            exact_binomial_upper_tail(
                threshold_successes,
                threshold_successes + threshold_failures,
            )
        })
        .transpose()?;
    Ok(PairedHypothesisResult {
        kind,
        margin_units,
        threshold_successes,
        threshold_failures,
        exact_equalities,
        one_sided_p,
        holm_rank: 0,
        significant: false,
    })
}

fn exact_binomial_upper_tail(successes: u32, trials: u32) -> Result<ExactPValue> {
    if successes > trials || trials > 32 {
        bail!("exact threshold test supports at most 32 paired blocks");
    }
    let denominator = 1_u64 << trials;
    let mut numerator = 0_u64;
    for count in successes..=trials {
        numerator = numerator
            .checked_add(binomial(trials, count)?)
            .context("exact binomial tail overflow")?;
    }
    let divisor = gcd(numerator, denominator);
    Ok(ExactPValue {
        numerator: numerator / divisor,
        denominator: denominator / divisor,
    })
}

fn binomial(n: u32, k: u32) -> Result<u64> {
    let k = k.min(n - k);
    let mut value = 1_u64;
    for index in 0..k {
        value = value
            .checked_mul(u64::from(n - index))
            .context("binomial coefficient overflow")?
            / u64::from(index + 1);
    }
    Ok(value)
}

fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

fn apply_holm(
    objectives: &mut [PairedObjectiveResult],
    campaign_alpha: &RationalThreshold,
    maximum_candidate_analyses: u32,
) -> Result<()> {
    let mut positions = objectives
        .iter()
        .enumerate()
        .flat_map(|(objective_index, objective)| {
            objective.hypotheses.iter().enumerate().filter_map(
                move |(hypothesis_index, hypothesis)| {
                    hypothesis
                        .one_sided_p
                        .clone()
                        .map(|probability| (objective_index, hypothesis_index, probability))
                },
            )
        })
        .collect::<Vec<_>>();
    positions.sort_by(|left, right| {
        probability_cmp(&left.2, &right.2)
            .then_with(|| objectives[left.0].key.cmp(&objectives[right.0].key))
            .then_with(|| {
                objectives[left.0].hypotheses[left.1]
                    .kind
                    .cmp(&objectives[right.0].hypotheses[right.1].kind)
            })
    });
    let family_size = u32::try_from(positions.len())?;
    let mut continue_rejecting = true;
    for (rank, (objective_index, hypothesis_index, probability)) in
        positions.into_iter().enumerate()
    {
        let rank = u32::try_from(rank)? + 1;
        let remaining = family_size - rank + 1;
        let below_threshold = u128::from(probability.numerator)
            * u128::from(campaign_alpha.denominator)
            * u128::from(remaining)
            * u128::from(maximum_candidate_analyses)
            <= u128::from(campaign_alpha.numerator) * u128::from(probability.denominator);
        let significant = continue_rejecting && below_threshold;
        if !significant {
            continue_rejecting = false;
        }
        let hypothesis = &mut objectives[objective_index].hypotheses[hypothesis_index];
        hypothesis.holm_rank = rank;
        hypothesis.significant = significant;
    }
    Ok(())
}

fn probability_cmp(left: &ExactPValue, right: &ExactPValue) -> Ordering {
    (u128::from(left.numerator) * u128::from(right.denominator))
        .cmp(&(u128::from(right.numerator) * u128::from(left.denominator)))
}

fn hypothesis(
    result: &PairedObjectiveResult,
    kind: PairedHypothesisKind,
) -> Option<&PairedHypothesisResult> {
    result.hypotheses.iter().find(|test| test.kind == kind)
}

fn hypothesis_family_size(objectives: &[ObjectiveSpec]) -> Result<u32> {
    objectives.iter().try_fold(0_u32, |count, objective| {
        let added = match objective.role {
            ObjectiveRole::Primary => 3,
            ObjectiveRole::Protected => 2,
            ObjectiveRole::HardConstraint | ObjectiveRole::Diagnostic => 0,
        };
        count
            .checked_add(added)
            .context("hypothesis family overflow")
    })
}

fn improvement_units(
    definition: &ObjectiveSpec,
    policy: &PairedObjectivePolicy,
    baseline: i64,
    candidate: i64,
) -> Result<i64> {
    match definition.direction {
        ObjectiveDirection::Minimize => baseline
            .checked_sub(candidate)
            .context("paired minimize improvement overflow"),
        ObjectiveDirection::Maximize => candidate
            .checked_sub(baseline)
            .context("paired maximize improvement overflow"),
        ObjectiveDirection::Target => {
            let target = policy
                .target_value_units
                .context("paired target objective has no exact target value")?;
            let baseline_distance = absolute_difference_units(baseline, target)?;
            let candidate_distance = absolute_difference_units(candidate, target)?;
            baseline_distance
                .checked_sub(candidate_distance)
                .context("paired target improvement overflow")
        }
    }
}

fn acceptance_passed_units(
    definition: &ObjectiveSpec,
    policy: &PairedObjectivePolicy,
    candidate: i64,
    threshold: i64,
) -> Result<bool> {
    Ok(match definition.direction {
        ObjectiveDirection::Minimize => candidate <= threshold,
        ObjectiveDirection::Maximize => candidate >= threshold,
        ObjectiveDirection::Target => {
            if threshold < 0 {
                bail!("paired target acceptance distance must be nonnegative");
            }
            let target = policy
                .target_value_units
                .context("paired target objective has no exact target value")?;
            absolute_difference_units(candidate, target)? <= threshold
        }
    })
}

fn absolute_difference_units(left: i64, right: i64) -> Result<i64> {
    let difference = i128::from(left) - i128::from(right);
    i64::try_from(difference.abs()).context("paired absolute difference exceeds i64")
}

fn checked_abs(value: i64) -> Result<i64> {
    value
        .checked_abs()
        .context("paired absolute effect overflow")
}

fn median(values: &[i64]) -> Result<MedianOrderStatistics> {
    if values.is_empty() {
        bail!("cannot compute a paired median without observations");
    }
    let mut values = values.to_vec();
    values.sort();
    let middle = values.len() / 2;
    Ok(if values.len().is_multiple_of(2) {
        MedianOrderStatistics {
            lower_units: values[middle - 1],
            upper_units: values[middle],
        }
    } else {
        MedianOrderStatistics {
            lower_units: values[middle],
            upper_units: values[middle],
        }
    })
}

fn require_exact_policy_match(
    objective: &str,
    field: &str,
    declared: f64,
    exact_units: i64,
    scale10: u8,
) -> Result<()> {
    const MAX_UNIQUELY_REPRESENTABLE_UNITS: u64 = 1_u64 << 50;
    if !declared.is_finite() || exact_units.unsigned_abs() > MAX_UNIQUELY_REPRESENTABLE_UNITS {
        bail!(
            "paired objective '{objective}' exact {field} exceeds the uniquely representable scaled-integer domain"
        );
    }
    let factor = 10_u64
        .checked_pow(u32::from(scale10))
        .context("paired decimal scale overflow")?;
    let represented = exact_units as f64 / factor as f64;
    if declared != represented {
        bail!(
            "paired objective '{objective}' exact {field} does not represent the admitted objective value at scale10={scale10}"
        );
    }
    Ok(())
}

fn require_exact_optional_policy_match(
    objective: &str,
    field: &str,
    declared: Option<f64>,
    exact_units: Option<i64>,
    scale10: u8,
) -> Result<()> {
    match (declared, exact_units) {
        (Some(declared), Some(exact_units)) => {
            require_exact_policy_match(objective, field, declared, exact_units, scale10)
        }
        (None, None) => Ok(()),
        _ => bail!(
            "paired objective '{objective}' exact {field} presence differs from the admitted objective"
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn objectives() -> Vec<ObjectiveSpec> {
        vec![
            ObjectiveSpec {
                key: "correct".to_owned(),
                role: ObjectiveRole::HardConstraint,
                direction: ObjectiveDirection::Maximize,
                unit: "boolean".to_owned(),
                minimum_practical_change: 0.0,
                regression_tolerance: 0.0,
                acceptance_threshold: Some(1.0),
                target_value: None,
            },
            ObjectiveSpec {
                key: "frame-ms".to_owned(),
                role: ObjectiveRole::Primary,
                direction: ObjectiveDirection::Minimize,
                unit: "ms".to_owned(),
                minimum_practical_change: 1.0,
                regression_tolerance: 0.5,
                acceptance_threshold: None,
                target_value: None,
            },
            ObjectiveSpec {
                key: "memory-mib".to_owned(),
                role: ObjectiveRole::Protected,
                direction: ObjectiveDirection::Minimize,
                unit: "MiB".to_owned(),
                minimum_practical_change: 0.0,
                regression_tolerance: 2.0,
                acceptance_threshold: None,
                target_value: None,
            },
        ]
    }

    pub(crate) fn plan() -> PairedAnalysisPlan {
        let order_seed_commitment_sha256 = Sha256Digest(sha256(order_seed()));
        PairedAnalysisPlan {
            schema: PAIRED_ANALYSIS_SCHEMA_V2.to_owned(),
            method: PairedAnalysisMethod::FixedSampleExactPairedBinomial,
            trial_adapter: Some(fixture_trial_adapter()),
            inference_scope: PairedInferenceScope::IndependentBlockPopulation {
                population: "declared-replay-population".to_owned(),
                sampling_protocol_locator: "fixtures/mise/sampling-protocol.json".to_owned(),
                sampling_protocol_sha256: Sha256Digest("6".repeat(64)),
            },
            required_blocks: 8,
            order_seed_protocol_locator: "fixtures/mise/order-seed-protocol.json".to_owned(),
            order_seed_protocol_sha256: Sha256Digest("7".repeat(64)),
            order_seed_commitments: vec![PairedSlotSeedCommitment {
                candidate_analysis_slot: 1,
                sha256: order_seed_commitment_sha256,
            }],
            calibration_seed_commitments: PairedCalibrationSeedCommitments {
                no_op_sha256: Sha256Digest(sha256(no_op_seed())),
                known_bad_sha256: Sha256Digest(sha256(known_bad_seed())),
            },
            campaign_familywise_alpha: RationalThreshold {
                numerator: 1,
                denominator: 20,
            },
            maximum_candidate_analyses: 1,
            objective_policies: vec![
                PairedObjectivePolicy {
                    objective: "correct".to_owned(),
                    scale10: 0,
                    measurement_summary_protocol: "all-pass.v1".to_owned(),
                    minimum_practical_change_units: 0,
                    regression_tolerance_units: 0,
                    acceptance_threshold_units: Some(1),
                    target_value_units: None,
                    no_op_maximum_absolute_effect_units: 0,
                },
                PairedObjectivePolicy {
                    objective: "frame-ms".to_owned(),
                    scale10: 3,
                    measurement_summary_protocol: "nearest-rank-p95.v1".to_owned(),
                    minimum_practical_change_units: 1_000,
                    regression_tolerance_units: 500,
                    acceptance_threshold_units: None,
                    target_value_units: None,
                    no_op_maximum_absolute_effect_units: 250,
                },
                PairedObjectivePolicy {
                    objective: "memory-mib".to_owned(),
                    scale10: 3,
                    measurement_summary_protocol: "maximum.v1".to_owned(),
                    minimum_practical_change_units: 0,
                    regression_tolerance_units: 2_000,
                    acceptance_threshold_units: None,
                    target_value_units: None,
                    no_op_maximum_absolute_effect_units: 500,
                },
            ],
            calibration_fixtures: PairedCalibrationFixtureBindings {
                no_op: PairedFixtureBinding {
                    locator: "sealed://fixture/no-op".to_owned(),
                    sha256: Sha256Digest("a".repeat(64)),
                },
                known_bad: PairedFixtureBinding {
                    locator: "sealed://fixture/known-bad".to_owned(),
                    sha256: Sha256Digest("b".repeat(64)),
                },
            },
            blocks: (0..8)
                .map(|block_index| PairedBlockDesign {
                    block_index,
                    stratum: if block_index % 2 == 0 {
                        "forest"
                    } else {
                        "nether"
                    }
                    .to_owned(),
                    fixture_locator: "sealed://fixture/confirmation".to_owned(),
                    fixture_sha256: Sha256Digest("c".repeat(64)),
                    environment_profile_sha256: Sha256Digest("d".repeat(64)),
                    workload_seed: format!("workload-{block_index}"),
                })
                .collect(),
        }
    }

    fn fixture_trial_adapter() -> crate::adapter::PairedAdapterBinding {
        let executable = std::fs::canonicalize(std::env::current_exe().expect("test executable"))
            .expect("canonical test executable");
        let working_directory =
            std::fs::canonicalize(std::env::current_dir().expect("test working directory"))
                .expect("canonical test working directory");
        let portable = |path: &std::path::Path| {
            path.to_string_lossy()
                .strip_prefix(r"\\?\")
                .unwrap_or(&path.to_string_lossy())
                .replace('\\', "/")
        };
        let executable_locator = portable(&executable);
        crate::adapter::PairedAdapterBinding {
            schema: crate::adapter::PAIRED_ADAPTER_BINDING_SCHEMA_V1.to_owned(),
            executable_sha256: Sha256Digest(sha256(
                &std::fs::read(&executable).expect("read test executable"),
            )),
            argv: vec![executable_locator.clone()],
            executable_locator,
            working_directory: portable(&working_directory),
            environment: BTreeMap::new(),
            result_schema: "fixture.paired-trial-result.v1".to_owned(),
            maximum_wall_time_ms: 1_000,
            maximum_output_bytes: 1_048_576,
        }
    }

    fn order_seed() -> &'static [u8] {
        b"mise-test-research-order-seed-0001"
    }

    fn no_op_seed() -> &'static [u8] {
        b"mise-test-no-op-order-seed-000001"
    }

    fn known_bad_seed() -> &'static [u8] {
        b"mise-test-known-bad-seed-0000001"
    }

    fn candidate() -> Sha256Digest {
        Sha256Digest("e".repeat(64))
    }

    fn research() -> PairedCohort {
        PairedCohort::Research {
            candidate_analysis_slot: 1,
        }
    }

    fn seed_for(cohort: PairedCohort) -> &'static [u8] {
        match cohort {
            PairedCohort::Research { .. } => order_seed(),
            PairedCohort::NoOpCalibration => no_op_seed(),
            PairedCohort::KnownBadCalibration => known_bad_seed(),
        }
    }

    fn order(plan: &PairedAnalysisPlan, cohort: PairedCohort, block_index: u32) -> PairedRunOrder {
        let identity = candidate();
        let candidate = PairedCandidateContext {
            cohort,
            candidate_identity_sha256: &identity,
            revealed_order_seed: seed_for(cohort),
        };
        paired_run_order(plan, &objectives(), &candidate, block_index).expect("order")
    }

    fn blocks(
        cohort: PairedCohort,
        frame_candidate_units: i64,
        memory_candidate_units: i64,
        correct_units: i64,
    ) -> Vec<PairedBlockObservation> {
        let plan = plan();
        (0..plan.required_blocks)
            .map(|block_index| {
                let design = &plan.blocks[usize::try_from(block_index).unwrap()];
                let fixture = cohort_fixture(&plan, design, cohort);
                PairedBlockObservation {
                    block_index,
                    order: order(&plan, cohort, block_index),
                    fixture_sha256: fixture.1.clone(),
                    environment_profile_sha256: design.environment_profile_sha256.clone(),
                    workload_seed: design.workload_seed.clone(),
                    baseline_trial_receipt_sha256: Sha256Digest(format!(
                        "{:064x}",
                        block_index + 1
                    )),
                    candidate_trial_receipt_sha256: Sha256Digest(format!(
                        "{:064x}",
                        block_index + 33
                    )),
                    observations: vec![
                        PairedObjectiveObservation {
                            objective: "correct".to_owned(),
                            baseline_units: 1,
                            candidate_units: correct_units,
                        },
                        PairedObjectiveObservation {
                            objective: "frame-ms".to_owned(),
                            baseline_units: 10_000 + i64::from(block_index) * 100,
                            candidate_units: frame_candidate_units + i64::from(block_index) * 100,
                        },
                        PairedObjectiveObservation {
                            objective: "memory-mib".to_owned(),
                            baseline_units: 100_000,
                            candidate_units: memory_candidate_units,
                        },
                    ],
                }
            })
            .collect()
    }

    fn classify(
        cohort: PairedCohort,
        observations: &[PairedBlockObservation],
    ) -> Result<PairedClassification> {
        let identity = candidate();
        let candidate = PairedCandidateContext {
            cohort,
            candidate_identity_sha256: &identity,
            revealed_order_seed: seed_for(cohort),
        };
        classify_paired_fixed(&plan(), &objectives(), &candidate, observations)
    }

    fn admit_paired_fixture(connection: &Connection) {
        let mut manifest = crate::manifest::tests::valid_manifest();
        manifest.objectives = objectives();
        manifest.evaluator.protocol = PAIRED_MEASUREMENT_PROTOCOL_V1.to_owned();
        let paired = plan();
        manifest.calibration.no_op.fixture_locator =
            paired.calibration_fixtures.no_op.locator.clone();
        manifest.calibration.no_op.fixture_sha256 =
            paired.calibration_fixtures.no_op.sha256.clone();
        manifest.calibration.known_bad.fixture_locator =
            paired.calibration_fixtures.known_bad.locator.clone();
        manifest.calibration.known_bad.fixture_sha256 =
            paired.calibration_fixtures.known_bad.sha256.clone();
        let confirmation = manifest
            .holdouts
            .tiers
            .iter_mut()
            .find(|tier| tier.kind == crate::manifest::HoldoutTierKind::Confirmation)
            .unwrap();
        confirmation.fixture_locator = paired.blocks[0].fixture_locator.clone();
        confirmation.fixture_sha256 = paired.blocks[0].fixture_sha256.clone();
        confirmation.minimum_repetitions = 16;
        confirmation.maximum_disclosures = 16;
        manifest.calibration.no_op.minimum_repetitions = 16;
        manifest.calibration.known_bad.minimum_repetitions = 16;
        manifest.holdouts.disclosure_cap = 21;
        manifest
            .budgets
            .caps
            .iter_mut()
            .find(|cap| cap.resource == crate::budget::BudgetResource::HoldoutDisclosures)
            .unwrap()
            .hard_limit = 21;
        manifest
            .budgets
            .caps
            .iter_mut()
            .find(|cap| cap.resource == crate::budget::BudgetResource::Trials)
            .unwrap()
            .hard_limit = 64;
        manifest.stop_rules.max_trials_without_qualified_improvement = 24;
        manifest.paired_analysis = Some(paired);
        let admission = crate::store::CampaignAdmission::from_manifest(&manifest).unwrap();
        crate::store::admit_campaign(connection, "statistics-test", &admission).unwrap();
        connection
            .execute(
                "INSERT INTO artifacts (sha256, bytes, locator, media_type, recorded_at)
                 VALUES (?1, 1, ?2, 'text/x-diff', ?3)",
                params!["4".repeat(64), "cas://candidate-patch", now()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO candidates
                 (candidate_id, campaign_id, proposal_json, patch_sha256, negative_fingerprint,
                  disposition, result_json, created_at, updated_at)
                 VALUES (?1, ?2, '{}', ?3, ?4, 'materialized', NULL, ?5, ?5)",
                params![
                    candidate().0,
                    manifest.campaign_id,
                    "4".repeat(64),
                    "5".repeat(64),
                    now(),
                ],
            )
            .unwrap();
    }

    #[test]
    fn schedule_is_balanced_deterministic_and_exactly_enforced() {
        let design = plan();
        let orders = (0..design.required_blocks)
            .map(|index| order(&design, research(), index))
            .collect::<Vec<_>>();
        assert_eq!(
            orders
                .iter()
                .filter(|order| **order == PairedRunOrder::BaselineFirst)
                .count(),
            4
        );
        assert_eq!(
            orders,
            (0..8)
                .map(|index| order(&design, research(), index))
                .collect::<Vec<_>>()
        );

        let mut early = blocks(research(), 8_000, 101_000, 1);
        early.pop();
        let error = classify(research(), &early).expect_err("interim peeking must not classify");
        assert!(error.to_string().contains("exactly 8 blocks"));

        let mut wrong_order = blocks(research(), 8_000, 101_000, 1);
        wrong_order[0].order = match wrong_order[0].order {
            PairedRunOrder::BaselineFirst => PairedRunOrder::CandidateFirst,
            PairedRunOrder::CandidateFirst => PairedRunOrder::BaselineFirst,
        };
        assert!(
            classify(research(), &wrong_order)
                .expect_err("order drift")
                .to_string()
                .contains("frozen randomized schedule")
        );

        let identity = candidate();
        let wrong_seed = PairedCandidateContext {
            cohort: research(),
            candidate_identity_sha256: &identity,
            revealed_order_seed: b"mise-test-wrong-order-seed-00000001",
        };
        let error = paired_run_order(&design, &objectives(), &wrong_seed, 0)
            .expect_err("the revealed seed must satisfy its pre-candidate commitment");
        assert!(error.to_string().contains("commitment"));

        let mut wrong_workload = blocks(research(), 8_000, 101_000, 1);
        wrong_workload[0].workload_seed = "candidate-selected".to_owned();
        assert!(
            classify(research(), &wrong_workload)
                .expect_err("workload identity drift")
                .to_string()
                .contains("frozen workload design")
        );

        let first = PairedCandidateContext {
            cohort: research(),
            candidate_identity_sha256: &identity,
            revealed_order_seed: order_seed(),
        };
        let first_schedule = paired_schedule_sha256(&design, &objectives(), &first).unwrap();
        let observations = blocks(research(), 8_000, 101_000, 1);
        let first_observations =
            paired_observations_sha256(&design, &objectives(), &first, &observations).unwrap();
        let mut reordered = observations.clone();
        reordered.reverse();
        for block in &mut reordered {
            block.observations.reverse();
        }
        assert_eq!(
            first_observations,
            paired_observations_sha256(&design, &objectives(), &first, &reordered).unwrap()
        );
        let mut reused_receipt = observations;
        reused_receipt[1].baseline_trial_receipt_sha256 =
            reused_receipt[0].baseline_trial_receipt_sha256.clone();
        assert!(
            paired_observations_sha256(&design, &objectives(), &first, &reused_receipt,)
                .expect_err("one trial receipt cannot count twice")
                .to_string()
                .contains("unique immutable trial receipt")
        );
        let other_candidate = Sha256Digest("f".repeat(64));
        let second = PairedCandidateContext {
            cohort: research(),
            candidate_identity_sha256: &other_candidate,
            revealed_order_seed: order_seed(),
        };
        let second_schedule = paired_schedule_sha256(&design, &objectives(), &second).unwrap();
        assert_ne!(first_schedule, second_schedule);
        let invented_slot = PairedCandidateContext {
            cohort: PairedCohort::Research {
                candidate_analysis_slot: 2,
            },
            candidate_identity_sha256: &identity,
            revealed_order_seed: order_seed(),
        };
        assert!(
            paired_schedule_sha256(&design, &objectives(), &invented_slot)
                .expect_err("a caller cannot invent another confirmation slot")
                .to_string()
                .contains("allocation")
        );
    }

    #[test]
    fn exact_holm_classification_separates_improvement_flat_wrong_and_regressed() {
        let improved = classify(research(), &blocks(research(), 9_000, 101_000, 1))
            .expect("improved at the inclusive practical threshold");
        assert_eq!(improved.disposition, PairedDisposition::Qualified);
        let frame = improved
            .objectives
            .iter()
            .find(|objective| objective.key == "frame-ms")
            .unwrap();
        assert_eq!(
            hypothesis(frame, PairedHypothesisKind::PracticalImprovement)
                .unwrap()
                .one_sided_p,
            Some(ExactPValue {
                numerator: 1,
                denominator: 256
            })
        );
        assert_eq!(
            hypothesis(frame, PairedHypothesisKind::PracticalImprovement)
                .unwrap()
                .exact_equalities,
            8
        );

        let flat = classify(research(), &blocks(research(), 10_000, 100_000, 1)).expect("flat");
        assert_eq!(flat.disposition, PairedDisposition::Inconclusive);

        let wrong = classify(research(), &blocks(research(), 8_000, 100_000, 0)).expect("wrong");
        assert_eq!(wrong.disposition, PairedDisposition::Rejected);

        let regressed =
            classify(research(), &blocks(research(), 8_000, 103_000, 1)).expect("regressed");
        assert_eq!(regressed.disposition, PairedDisposition::Rejected);

        let mut overflow = blocks(research(), 8_000, 100_000, 1);
        let frame = overflow[0]
            .observations
            .iter_mut()
            .find(|observation| observation.objective == "frame-ms")
            .unwrap();
        frame.baseline_units = i64::MAX;
        frame.candidate_units = i64::MIN;
        assert!(
            classify(research(), &overflow)
                .expect_err("decision arithmetic overflow must refuse")
                .to_string()
                .contains("overflow")
        );

        assert_eq!(
            exact_binomial_upper_tail(10, 10).unwrap(),
            ExactPValue {
                numerator: 1,
                denominator: 1024,
            }
        );
    }

    #[test]
    fn exact_binomial_and_holm_match_nontrivial_reference_values() {
        assert_eq!(
            exact_binomial_upper_tail(6, 8).expect("nontrivial exact tail"),
            ExactPValue {
                numerator: 37,
                denominator: 256,
            }
        );

        let mut objectives = classify(research(), &blocks(research(), 9_000, 101_000, 1))
            .expect("reference classification")
            .objectives;
        objectives.retain(|objective| objective.hypotheses.len() >= 2);
        objectives.truncate(1);
        objectives[0].hypotheses.truncate(2);
        objectives[0].hypotheses[0].one_sided_p = Some(ExactPValue {
            numerator: 1,
            denominator: 100,
        });
        objectives[0].hypotheses[1].one_sided_p = Some(ExactPValue {
            numerator: 3,
            denominator: 100,
        });
        apply_holm(
            &mut objectives,
            &RationalThreshold {
                numerator: 1,
                denominator: 20,
            },
            1,
        )
        .expect("exact Holm step-down");
        assert_eq!(objectives[0].hypotheses[0].holm_rank, 1);
        assert_eq!(objectives[0].hypotheses[1].holm_rank, 2);
        assert!(objectives[0].hypotheses[0].significant);
        assert!(objectives[0].hypotheses[1].significant);

        objectives[0].hypotheses[1].one_sided_p = Some(ExactPValue {
            numerator: 3,
            denominator: 50,
        });
        apply_holm(
            &mut objectives,
            &RationalThreshold {
                numerator: 1,
                denominator: 20,
            },
            1,
        )
        .expect("Holm first nonsignificant stops later rejection");
        assert!(objectives[0].hypotheses[0].significant);
        assert!(!objectives[0].hypotheses[1].significant);
    }

    #[test]
    fn no_op_calibration_and_plan_power_are_fail_closed() {
        let identity = candidate();
        let candidate = PairedCandidateContext {
            cohort: PairedCohort::NoOpCalibration,
            candidate_identity_sha256: &identity,
            revealed_order_seed: no_op_seed(),
        };
        let no_op = blocks(PairedCohort::NoOpCalibration, 10_100, 100_250, 1);
        let calibration =
            assess_no_op_cohort(&plan(), &objectives(), &candidate, &no_op).expect("no-op");
        assert!(calibration.passed);
        assert!(!calibration.practical_false_positive);

        let noisy = blocks(PairedCohort::NoOpCalibration, 10_500, 100_250, 1);
        assert!(
            !assess_no_op_cohort(&plan(), &objectives(), &candidate, &noisy)
                .expect("noisy no-op")
                .passed
        );

        let mut underpowered = plan();
        underpowered.maximum_candidate_analyses = 3;
        underpowered.order_seed_commitments = (1..=3)
            .map(|slot| PairedSlotSeedCommitment {
                candidate_analysis_slot: slot,
                sha256: Sha256Digest(format!("{slot:064x}")),
            })
            .collect();
        assert!(
            underpowered
                .validate(&objectives())
                .expect_err("underpowered plan")
                .to_string()
                .contains("underpowered")
        );

        let mut ambiguous = plan();
        ambiguous
            .objective_policies
            .iter_mut()
            .find(|policy| policy.objective == "frame-ms")
            .unwrap()
            .minimum_practical_change_units = 999;
        assert!(
            ambiguous
                .validate(&objectives())
                .expect_err("the float declaration and exact noisy policy cannot diverge")
                .to_string()
                .contains("does not represent")
        );
    }

    #[test]
    fn known_bad_calibration_uses_the_same_frozen_classifier() {
        let identity = candidate();
        let candidate = PairedCandidateContext {
            cohort: PairedCohort::KnownBadCalibration,
            candidate_identity_sha256: &identity,
            revealed_order_seed: known_bad_seed(),
        };
        let known_bad_blocks = blocks(PairedCohort::KnownBadCalibration, 20_000, 100_000, 0);
        let known_bad =
            assess_known_bad_cohort(&plan(), &objectives(), &candidate, &known_bad_blocks)
                .expect("known bad");
        assert_eq!(known_bad.disposition, PairedDisposition::Rejected);

        let false_calibration = blocks(PairedCohort::KnownBadCalibration, 9_000, 100_000, 1);
        assert!(
            assess_known_bad_cohort(&plan(), &objectives(), &candidate, &false_calibration)
                .expect_err("a known-bad calibration unexpectedly accepted must stop the campaign")
                .to_string()
                .contains("integrity")
        );
    }

    #[test]
    fn fixed_schedule_reports_no_p_values_and_cannot_qualify() {
        let mut fixed = plan();
        fixed.inference_scope = PairedInferenceScope::FixedScheduleOnly;
        let identity = candidate();
        let candidate = PairedCandidateContext {
            cohort: research(),
            candidate_identity_sha256: &identity,
            revealed_order_seed: order_seed(),
        };
        let mut observations = blocks(research(), 9_000, 101_000, 1);
        for block in &mut observations {
            block.order = paired_run_order(&fixed, &objectives(), &candidate, block.block_index)
                .expect("fixed schedule order");
        }
        let classification =
            classify_paired_fixed(&fixed, &objectives(), &candidate, &observations)
                .expect("fixed schedule classification");
        assert_eq!(classification.disposition, PairedDisposition::Inconclusive);
        assert!(
            classification
                .objectives
                .iter()
                .flat_map(|objective| &objective.hypotheses)
                .all(|hypothesis| hypothesis.one_sided_p.is_none() && !hypothesis.significant)
        );
        assert!(
            classification
                .reasons
                .iter()
                .any(|reason| reason.contains("fixed-schedule evidence"))
        );
    }

    #[test]
    fn inference_admission_refuses_weak_seed_and_stratum_contracts() {
        let mut high_alpha = plan();
        high_alpha.campaign_familywise_alpha = RationalThreshold {
            numerator: u32::MAX - 1,
            denominator: u32::MAX,
        };
        assert!(
            high_alpha
                .validate(&objectives())
                .expect_err("alpha above one tenth")
                .to_string()
                .contains("no greater than 1/10")
        );

        let mut singleton = plan();
        for (index, block) in singleton.blocks.iter_mut().enumerate() {
            block.stratum = format!("stratum-{index}");
        }
        assert!(
            singleton
                .validate(&objectives())
                .expect_err("singleton strata cannot be order-balanced")
                .to_string()
                .contains("even number of at least two")
        );

        let identity = candidate();
        let short_seed = PairedCandidateContext {
            cohort: research(),
            candidate_identity_sha256: &identity,
            revealed_order_seed: b"too-short",
        };
        assert!(
            paired_schedule_sha256(&plan(), &objectives(), &short_seed)
                .expect_err("short seed")
                .to_string()
                .contains("at least 32 bytes")
        );

        assert!(
            require_exact_policy_match(
                "hostile-boundary",
                "minimum_practical_change",
                ((1_i64 << 53) + 1) as f64,
                (1_i64 << 53) + 1,
                0,
            )
            .expect_err("f64-colliding scaled integers must not be admitted")
            .to_string()
            .contains("uniquely representable")
        );
    }

    #[test]
    fn paired_v1_remains_readable_while_v2_requires_the_frozen_trial_adapter() {
        let objectives = objectives();
        let mut legacy = plan();
        legacy.schema = PAIRED_ANALYSIS_SCHEMA_V1.to_owned();
        legacy.trial_adapter = None;
        legacy.validate(&objectives).expect("legacy v1 plan");
        let bytes = serde_json::to_vec(&legacy).expect("legacy JSON");
        assert!(!String::from_utf8_lossy(&bytes).contains("trial_adapter"));
        let reopened: PairedAnalysisPlan = serde_json::from_slice(&bytes).expect("reopen v1");
        reopened
            .validate(&objectives)
            .expect("validate reopened v1");

        let mut missing = plan();
        missing.trial_adapter = None;
        assert!(missing.validate(&objectives).is_err());

        let mut mislabelled = plan();
        mislabelled.schema = PAIRED_ANALYSIS_SCHEMA_V1.to_owned();
        assert!(mislabelled.validate(&objectives).is_err());
    }

    #[test]
    fn calibration_assessors_require_their_independent_cohorts() {
        let identity = candidate();
        let research_candidate = PairedCandidateContext {
            cohort: research(),
            candidate_identity_sha256: &identity,
            revealed_order_seed: order_seed(),
        };
        assert!(
            assess_no_op_cohort(
                &plan(),
                &objectives(),
                &research_candidate,
                &blocks(research(), 10_000, 100_000, 1),
            )
            .expect_err("research seed cannot stand in for no-op calibration")
            .to_string()
            .contains("no-op cohort")
        );
        assert!(
            assess_known_bad_cohort(
                &plan(),
                &objectives(),
                &research_candidate,
                &blocks(research(), 20_000, 100_000, 0),
            )
            .expect_err("research seed cannot stand in for known-bad calibration")
            .to_string()
            .contains("known-bad cohort")
        );
    }

    #[test]
    fn durable_slots_prevent_candidate_or_schedule_reuse() {
        let temporary = tempfile::tempdir().unwrap();
        let connection = crate::store::open_for_init(temporary.path().join("mise.sqlite")).unwrap();
        crate::store::init(&connection).unwrap();
        admit_paired_fixture(&connection);

        let reserved = reserve_paired_analysis_slot(
            &connection,
            "statistics-test",
            "fixture-rsi-1",
            &candidate().0,
            1,
            order_seed(),
        )
        .expect("reserve slot");
        assert_eq!(reserved.revealed_order_seed, order_seed());
        assert_eq!(
            reserved,
            reserve_paired_analysis_slot(
                &connection,
                "statistics-test",
                "fixture-rsi-1",
                &candidate().0,
                1,
                order_seed(),
            )
            .expect("same reservation is idempotent")
        );
    }
}
