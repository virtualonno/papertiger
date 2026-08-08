use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

pub use crate::improvement::ObjectiveRole;
use crate::improvement::objective_unit_is_boolean;

use crate::budget::{BudgetLimit, BudgetResource};
use crate::digest::sha256;

pub const CAMPAIGN_SCHEMA_V1: &str = "papertiger-mise.campaign.v1";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Immutable, content-bound admission contract for one Mise campaign.
///
/// Collection fields that are semantically sets are sorted before canonical
/// serialization. Ordered fields, notably evaluator `argv`, retain their
/// declared order.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignManifest {
    pub schema: String,
    pub campaign_id: String,
    pub source: SourceBinding,
    pub mutation_scope: MutationScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_material: Option<CandidateMaterialContract>,
    pub adapter: AdapterBinding,
    pub evaluator: EvaluatorBinding,
    pub execution_limits: ExecutionLimits,
    pub objectives: Vec<ObjectiveSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_analysis: Option<crate::statistics::PairedAnalysisPlan>,
    pub budgets: CumulativeBudgetCaps,
    pub stop_rules: StopRules,
    pub containment: ContainmentGrade,
    pub containment_requirement: Option<ContainmentRequirement>,
    pub holdouts: HoldoutPolicy,
    pub generation: GenerationBinding,
    pub calibration: CalibrationRequirements,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateMaterialContract {
    pub kind: String,
    pub protocol: String,
    pub media_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBinding {
    /// Stable identity of the owning repository, independent of a mutable
    /// checkout name or remote URL.
    pub repository_id: String,
    /// Exact operator-visible repository location at admission time.
    pub repository_locator: String,
    pub repository_identity_sha256: Sha256Digest,
    pub git_object_format: GitObjectFormat,
    pub base_commit: String,
    pub base_tree: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitObjectFormat {
    Sha1,
    Sha256,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MutationScope {
    /// Repository-relative path prefixes the candidate may change.
    pub allowlist: Vec<String>,
    /// Repository-relative path prefixes that take precedence over the
    /// allowlist and must remain unchanged.
    pub protected_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorBinding {
    /// Exact process argv. Element zero is the executable; no shell command
    /// string is accepted by this schema.
    pub argv: Vec<String>,
    pub working_directory: String,
    /// Exact ambient environment visible to the evaluator. Candidate-specific
    /// inputs travel through the measurement protocol, not inherited process state.
    pub environment: BTreeMap<String, String>,
    /// Canonical absolute executable used to launch the frozen evaluator.
    pub launcher_locator: String,
    pub launcher_sha256: Sha256Digest,
    pub evaluator_locator: String,
    pub evaluator_sha256: Sha256Digest,
    pub fixture_bundle_locator: String,
    pub fixture_bundle_sha256: Sha256Digest,
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_build_environment: Option<RustBuildEnvironment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_build: Option<JudgeBuildBinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeBuildBinding {
    /// Exact command the frozen evaluator attests that it executed. The
    /// supervisor does not infer a shell command from this argv.
    pub argv: Vec<String>,
    pub toolchain_name: String,
    pub toolchain_version: String,
    pub toolchain_executable_locator: String,
    pub toolchain_executable_sha256: Sha256Digest,
    /// Path relative to the fresh runtime-owned judge-build root.
    pub executable_locator: String,
    pub maximum_executable_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RustBuildEnvironment {
    pub cargo_executable_locator: String,
    pub cargo_executable_sha256: Sha256Digest,
    pub rustc_executable_locator: String,
    pub rustc_executable_sha256: Sha256Digest,
    pub toolchain: String,
    pub lockfile_locator: String,
    pub lockfile_sha256: Sha256Digest,
    pub cargo_config_locator: String,
    pub cargo_config_sha256: Sha256Digest,
    pub vendored_sources_locator: String,
    pub vendored_sources_tree: String,
    /// Optional only so immutable manifests admitted before linker binding
    /// remain readable. Every new Rust campaign must provide it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linker: Option<RustLinkerBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RustLinkerBinding {
    pub target_triple: String,
    pub executable_locator: String,
    pub executable_sha256: Sha256Digest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterBinding {
    pub name: String,
    pub protocol: String,
    pub implementation_locator: String,
    pub implementation_sha256: Sha256Digest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLimits {
    pub workspace_root_locator: String,
    /// Optional only so immutable manifests admitted before the dedicated
    /// runtime root remain readable. New toolchain-backed campaigns require it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_root_locator: Option<String>,
    pub maximum_trial_wall_time_ms: u64,
    pub maximum_trial_output_bytes: u64,
    pub network: NetworkPolicy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    Denied,
    /// Explicitly records that WorkspaceOnly execution has no network sandbox.
    Unrestricted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveSpec {
    pub key: String,
    pub role: ObjectiveRole,
    pub direction: ObjectiveDirection,
    pub unit: String,
    /// Smallest direction-normalized absolute change that counts as a practical gain.
    /// A paired analysis supplies the exact scaled-integer decision value.
    pub minimum_practical_change: f64,
    /// Largest direction-normalized absolute degradation permitted.
    /// A paired analysis supplies the exact scaled-integer decision value.
    pub regression_tolerance: f64,
    /// Absolute acceptance boundary, required for hard constraints.
    pub acceptance_threshold: Option<f64>,
    /// Absolute target, required only for target-directed objectives.
    pub target_value: Option<f64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveDirection {
    Minimize,
    Maximize,
    Target,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CumulativeBudgetCaps {
    /// Every limit is monotonically charged over the campaign and all of its
    /// descendants. Instantaneous containment limits belong to the executor,
    /// not this cumulative ledger.
    pub caps: Vec<BudgetLimit>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StopRules {
    pub not_before_unix_ms: u64,
    pub deadline_unix_ms: u64,
    pub max_consecutive_infrastructure_failures: u32,
    pub max_trials_without_qualified_improvement: u32,
    pub stop_on_evaluator_integrity_failure: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainmentGrade {
    /// Portable local supervision with a detached checkout, exact invocation,
    /// wall/output bounds, and fail-closed stream quiescence. This is lifecycle
    /// hygiene, never an adversarial security or resource-isolation boundary.
    WorkspaceOnly,
    /// Platform-neutral attested worker boundary with read-only evaluator
    /// inputs, denied ambient access, and hidden confirmation fixtures.
    Sealed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContainmentRequirement {
    /// Platform-neutral attestation protocol. It describes required outcomes,
    /// never a host mechanism such as a Job Object or cgroup.
    pub protocol: String,
    pub executor_locator: String,
    pub executor_sha256: Sha256Digest,
    pub profile_sha256: Sha256Digest,
    pub maximum_processes: u32,
    pub maximum_memory_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HoldoutPolicy {
    pub disclosure_cap: u64,
    pub tiers: Vec<HoldoutTier>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HoldoutTier {
    pub key: String,
    pub kind: HoldoutTierKind,
    pub fixture_locator: String,
    pub fixture_sha256: Sha256Digest,
    pub maximum_disclosures: u64,
    pub minimum_repetitions: u32,
    pub disclosure: HoldoutDisclosure,
}

#[derive(Clone, Copy, Debug, Deserialize, Ord, PartialOrd, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldoutTierKind {
    Public,
    Exploration,
    Confirmation,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldoutDisclosure {
    Raw,
    Aggregate,
    VerdictOnly,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationBinding {
    /// Generation of the frozen runtime judging this campaign.
    pub runtime_generation: u32,
    pub recursion_depth: u32,
    pub maximum_recursion_depth: u32,
    pub outer_judge_executable_sha256: Sha256Digest,
    pub outer_judge_executable_locator: String,
    pub proposal_policy_sha256: Sha256Digest,
    pub proposal_policy_locator: String,
    pub parent_campaign: Option<ParentCampaignBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParentCampaignBinding {
    pub campaign_id: String,
    pub manifest_sha256: Sha256Digest,
    pub runtime_generation: u32,
    pub recursion_depth: u32,
}

pub const MAXIMUM_RECURSION_DEPTH: u32 = 32;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationRequirements {
    pub no_op: NoOpCalibration,
    pub known_bad: KnownBadCalibration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoOpCalibration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_patch_sha256: Option<Sha256Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_material_sha256: Option<Sha256Digest>,
    pub fixture_locator: String,
    pub fixture_sha256: Sha256Digest,
    pub minimum_repetitions: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnownBadCalibration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_patch_sha256: Option<Sha256Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_material_sha256: Option<Sha256Digest>,
    pub fixture_locator: String,
    pub fixture_sha256: Sha256Digest,
    pub minimum_repetitions: u32,
    pub expected_rejection_code: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Sha256Digest(pub String);

impl CampaignManifest {
    pub fn validate(&self) -> Result<()> {
        if self.schema != CAMPAIGN_SCHEMA_V1 {
            bail!(
                "unsupported campaign schema '{}' (expected '{CAMPAIGN_SCHEMA_V1}')",
                self.schema
            );
        }
        validate_identifier("campaign_id", &self.campaign_id)?;
        self.source.validate()?;
        self.mutation_scope.validate()?;
        if let Some(contract) = &self.candidate_material {
            contract.validate()?;
        }
        self.adapter.validate()?;
        self.evaluator.validate()?;
        self.mutation_scope
            .require_protected("adapter", &self.adapter.implementation_locator)?;
        self.mutation_scope
            .require_protected("evaluator", &self.evaluator.evaluator_locator)?;
        self.mutation_scope
            .require_protected("fixture bundle", &self.evaluator.fixture_bundle_locator)?;
        if let Some(environment) = &self.evaluator.rust_build_environment {
            self.mutation_scope
                .require_protected("Rust lockfile", &environment.lockfile_locator)?;
            self.mutation_scope
                .require_protected("Cargo config", &environment.cargo_config_locator)?;
            self.mutation_scope.require_protected(
                "vendored Rust sources",
                &environment.vendored_sources_locator,
            )?;
            validate_git_object(
                "evaluator.rust_build_environment.vendored_sources_tree",
                &environment.vendored_sources_tree,
                self.source.git_object_format,
            )?;
        }
        self.execution_limits.validate(&self.budgets)?;
        validate_objectives(&self.objectives)?;
        if let Some(plan) = &self.paired_analysis {
            plan.validate(&self.objectives)?;
            if self.evaluator.protocol != crate::statistics::PAIRED_MEASUREMENT_PROTOCOL_V1 {
                bail!(
                    "paired analysis requires evaluator protocol '{}'",
                    crate::statistics::PAIRED_MEASUREMENT_PROTOCOL_V1
                );
            }
            self.mutation_scope.require_protected(
                "paired order seed protocol",
                &plan.order_seed_protocol_locator,
            )?;
            validate_repo_relative_path(
                "paired order seed protocol locator",
                &plan.order_seed_protocol_locator,
                false,
            )?;
            for (role, paired, manifest_fixture) in [
                (
                    "paired no-op calibration fixture",
                    &plan.calibration_fixtures.no_op,
                    (
                        self.calibration.no_op.fixture_locator.as_str(),
                        &self.calibration.no_op.fixture_sha256,
                    ),
                ),
                (
                    "paired known-bad calibration fixture",
                    &plan.calibration_fixtures.known_bad,
                    (
                        self.calibration.known_bad.fixture_locator.as_str(),
                        &self.calibration.known_bad.fixture_sha256,
                    ),
                ),
            ] {
                if paired.locator != manifest_fixture.0 || paired.sha256 != *manifest_fixture.1 {
                    bail!("{role} must exactly match the manifest calibration binding");
                }
            }
            let evidence_tier = self.paired_evidence_tier()?;
            if let Some(adapter) = &plan.trial_adapter
                && (adapter.maximum_wall_time_ms > self.execution_limits.maximum_trial_wall_time_ms
                    || adapter.maximum_output_bytes
                        > self.execution_limits.maximum_trial_output_bytes)
            {
                bail!(
                    "paired trial-adapter bounds may not exceed the campaign's portable execution bounds"
                );
            }
            for block in &plan.blocks {
                let role = format!("paired block {} fixture_locator", block.block_index);
                if block.fixture_locator.contains("://") {
                    if self.containment != ContainmentGrade::Sealed {
                        bail!("{role} may be opaque only under sealed containment");
                    }
                    validate_opaque_fixture_locator(&role, &block.fixture_locator)?;
                } else {
                    validate_repo_relative_path(&role, &block.fixture_locator, true)?;
                    self.mutation_scope
                        .require_protected("paired research fixture", &block.fixture_locator)?;
                }
                if block.fixture_locator != evidence_tier.fixture_locator
                    || block.fixture_sha256 != evidence_tier.fixture_sha256
                {
                    bail!(
                        "paired research block {} must exactly match the campaign's {:?} evidence-tier binding",
                        block.block_index,
                        evidence_tier.kind,
                    );
                }
            }
            if let crate::statistics::PairedInferenceScope::IndependentBlockPopulation {
                sampling_protocol_locator,
                ..
            } = &plan.inference_scope
            {
                self.mutation_scope
                    .require_protected("paired sampling protocol", sampling_protocol_locator)?;
                validate_repo_relative_path(
                    "paired sampling protocol locator",
                    sampling_protocol_locator,
                    false,
                )?;
            }
        }
        self.budgets.validate()?;
        self.stop_rules.validate()?;
        self.validate_containment_requirement()?;
        self.holdouts.validate(self.containment, &self.budgets)?;
        self.generation.validate(&self.campaign_id)?;
        self.calibration
            .validate(self.candidate_material.as_ref())?;
        self.validate_fixture_access_boundary()?;
        self.validate_cross_contract()?;
        Ok(())
    }

    fn validate_fixture_access_boundary(&self) -> Result<()> {
        if self.containment == ContainmentGrade::Sealed {
            return Ok(());
        }
        for (role, locator) in [
            (
                "calibration.no_op.fixture_locator",
                self.calibration.no_op.fixture_locator.as_str(),
            ),
            (
                "calibration.known_bad.fixture_locator",
                self.calibration.known_bad.fixture_locator.as_str(),
            ),
        ] {
            validate_repo_relative_path(role, locator, true)?;
        }
        for tier in &self.holdouts.tiers {
            validate_repo_relative_path(
                &format!("holdout tier '{}' fixture_locator", tier.key),
                &tier.fixture_locator,
                true,
            )?;
        }
        Ok(())
    }

    fn paired_evidence_tier(&self) -> Result<&HoldoutTier> {
        let kind = match self.containment {
            ContainmentGrade::WorkspaceOnly => HoldoutTierKind::Exploration,
            ContainmentGrade::Sealed => HoldoutTierKind::Confirmation,
        };
        self.holdouts
            .tiers
            .iter()
            .find(|tier| tier.kind == kind)
            .with_context(|| format!("paired analysis requires a {kind:?} evidence tier"))
    }

    /// Deterministic compact JSON for the manifest's semantic content.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let canonical = self.canonicalized();
        serde_json::to_vec(&canonical).context("serialize canonical Mise campaign manifest")
    }

    pub fn sha256(&self) -> Result<String> {
        Ok(sha256(&self.canonical_bytes()?))
    }

    /// Reproduce an already-admitted manifest identity without applying rules
    /// introduced after that immutable admission. New admissions must never
    /// use this path.
    pub(crate) fn historical_canonical_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(&self.canonicalized())
            .context("serialize historical canonical Mise campaign manifest")
    }

    fn canonicalized(&self) -> Self {
        let mut canonical = self.clone();
        canonical.mutation_scope.allowlist.sort();
        canonical.mutation_scope.protected_paths.sort();
        canonical
            .objectives
            .sort_by(|left, right| left.key.cmp(&right.key));
        if let Some(plan) = &mut canonical.paired_analysis {
            *plan = plan.canonicalized();
        }
        for objective in &mut canonical.objectives {
            normalize_zero(&mut objective.minimum_practical_change);
            normalize_zero(&mut objective.regression_tolerance);
            if let Some(value) = &mut objective.acceptance_threshold {
                normalize_zero(value);
            }
            if let Some(value) = &mut objective.target_value {
                normalize_zero(value);
            }
        }
        canonical.budgets.caps.sort_by_key(|cap| cap.resource);
        canonical.holdouts.tiers.sort_by(|left, right| {
            (left.kind, left.key.as_str()).cmp(&(right.kind, right.key.as_str()))
        });
        canonical
    }

    fn validate_cross_contract(&self) -> Result<()> {
        if (self.evaluator.rust_build_environment.is_some() || self.evaluator.judge_build.is_some())
            && self.execution_limits.runtime_root_locator.is_none()
        {
            bail!(
                "toolchain-backed campaigns require execution_limits.runtime_root_locator; choose an absolute operator-owned scratch root with enough host path budget"
            );
        }
        let holdout_repetitions = self.holdouts.tiers.iter().try_fold(0_u64, |sum, tier| {
            sum.checked_add(u64::from(tier.minimum_repetitions))
                .ok_or_else(|| anyhow!("minimum holdout repetition total overflow"))
        })?;
        let minimum_trials = holdout_repetitions
            .checked_add(u64::from(self.calibration.no_op.minimum_repetitions))
            .and_then(|sum| {
                sum.checked_add(u64::from(self.calibration.known_bad.minimum_repetitions))
            })
            .ok_or_else(|| anyhow!("minimum campaign trial total overflow"))?;
        if self.budgets.limit(BudgetResource::Trials).unwrap_or(0) < minimum_trials {
            bail!("trial budget cannot satisfy calibration plus one qualified candidate");
        }
        if self.budgets.limit(BudgetResource::Candidates).unwrap_or(0) < 3 {
            bail!("candidate budget must admit no-op, known-bad, and one research candidate");
        }
        if let Some(plan) = &self.paired_analysis {
            let required_candidates = u64::from(plan.maximum_candidate_analyses)
                .checked_add(2)
                .ok_or_else(|| anyhow!("paired candidate budget requirement overflow"))?;
            if self.budgets.limit(BudgetResource::Candidates).unwrap_or(0) < required_candidates {
                bail!("candidate budget cannot satisfy paired candidate analyses plus calibration");
            }
            let paired_trials_per_cohort = u64::from(plan.required_blocks)
                .checked_mul(2)
                .ok_or_else(|| anyhow!("paired trial budget requirement overflow"))?;
            if u64::from(self.calibration.no_op.minimum_repetitions) < paired_trials_per_cohort
                || u64::from(self.calibration.known_bad.minimum_repetitions)
                    < paired_trials_per_cohort
            {
                bail!(
                    "paired calibration requires two trial executions per frozen block for both calibration cohorts"
                );
            }
            let evidence_tier = self.paired_evidence_tier()?;
            let all_candidate_trials = paired_trials_per_cohort
                .checked_mul(u64::from(plan.maximum_candidate_analyses))
                .ok_or_else(|| anyhow!("paired evidence-tier trial requirement overflow"))?;
            if u64::from(evidence_tier.minimum_repetitions) < all_candidate_trials {
                bail!(
                    "paired evidence-tier repetitions cannot satisfy every frozen candidate analysis"
                );
            }
            if evidence_tier.maximum_disclosures < all_candidate_trials {
                bail!(
                    "paired evidence-tier disclosure capacity cannot satisfy every frozen candidate analysis"
                );
            }
        }
        if u64::from(self.stop_rules.max_trials_without_qualified_improvement) < holdout_repetitions
        {
            bail!("no-improvement stop rule fires before one candidate can qualify");
        }
        Ok(())
    }

    fn validate_containment_requirement(&self) -> Result<()> {
        match (self.containment, self.execution_limits.network) {
            (ContainmentGrade::WorkspaceOnly, NetworkPolicy::Unrestricted) => {}
            (ContainmentGrade::WorkspaceOnly, NetworkPolicy::Denied) => {
                bail!("WorkspaceOnly cannot claim an enforced denied-network boundary");
            }
            (ContainmentGrade::Sealed, NetworkPolicy::Denied) => {}
            (ContainmentGrade::Sealed, NetworkPolicy::Unrestricted) => {
                bail!("sealed containment requires denied network access");
            }
        }
        match (self.containment, self.containment_requirement.as_ref()) {
            (ContainmentGrade::Sealed, Some(binding)) => {
                validate_exact_string("containment_requirement.protocol", &binding.protocol)?;
                if binding.protocol != crate::attestation::SEALED_ATTESTATION_PROTOCOL_V2 {
                    bail!("sealed campaigns require the platform-neutral v2 attestation protocol");
                }
                validate_repo_relative_path(
                    "containment_requirement.executor_locator",
                    &binding.executor_locator,
                    true,
                )?;
                binding
                    .executor_sha256
                    .validate("containment_requirement.executor_sha256")?;
                binding
                    .profile_sha256
                    .validate("containment_requirement.profile_sha256")?;
                if binding.maximum_processes == 0 || binding.maximum_memory_bytes == 0 {
                    bail!(
                        "sealed containment requires nonzero portable process and memory ceilings"
                    );
                }
                self.mutation_scope
                    .require_protected("containment executor", &binding.executor_locator)?;
            }
            (ContainmentGrade::Sealed, None) => {
                bail!("sealed campaigns require a cryptographic containment attestor");
            }
            (_, Some(_)) => {
                bail!("containment attestors are only valid for sealed campaigns");
            }
            (_, None) => {}
        }
        Ok(())
    }
}

impl SourceBinding {
    fn validate(&self) -> Result<()> {
        validate_identifier("source.repository_id", &self.repository_id)?;
        validate_exact_string("source.repository_locator", &self.repository_locator)?;
        self.repository_identity_sha256
            .validate("source.repository_identity_sha256")?;
        validate_git_object(
            "source.base_commit",
            &self.base_commit,
            self.git_object_format,
        )?;
        validate_git_object("source.base_tree", &self.base_tree, self.git_object_format)?;
        if self.base_commit == self.base_tree {
            bail!("source.base_commit and source.base_tree must be distinct Git objects");
        }
        Ok(())
    }
}

impl MutationScope {
    fn validate(&self) -> Result<()> {
        if self.allowlist.is_empty() {
            bail!("mutation_scope.allowlist must not be empty");
        }
        validate_unique_paths("mutation_scope.allowlist", &self.allowlist, true)?;
        validate_unique_paths(
            "mutation_scope.protected_paths",
            &self.protected_paths,
            false,
        )?;
        Ok(())
    }

    fn require_protected(&self, role: &str, locator: &str) -> Result<()> {
        let protected = self.protected_paths.iter().any(|prefix| {
            locator == prefix
                || locator
                    .strip_prefix(prefix)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        });
        if !protected {
            bail!("{role} locator '{locator}' is not inside mutation_scope.protected_paths");
        }
        Ok(())
    }
}

impl EvaluatorBinding {
    fn validate(&self) -> Result<()> {
        if self.argv.is_empty() {
            bail!("evaluator.argv must contain the executable at element zero");
        }
        for (index, argument) in self.argv.iter().enumerate() {
            if argument.is_empty() {
                bail!("evaluator.argv[{index}] must not be empty");
            }
            if argument.contains('\0') {
                bail!("evaluator.argv[{index}] must not contain NUL");
            }
        }
        for (key, value) in &self.environment {
            validate_identifier("evaluator.environment key", key)?;
            validate_exact_string("evaluator.environment value", value)?;
        }
        validate_repo_relative_path(
            "evaluator.working_directory",
            &self.working_directory,
            false,
        )?;
        validate_repo_relative_path("evaluator.evaluator_locator", &self.evaluator_locator, true)?;
        validate_local_absolute_path("evaluator.launcher_locator", &self.launcher_locator)?;
        self.launcher_sha256.validate("evaluator.launcher_sha256")?;
        validate_repo_relative_path(
            "evaluator.fixture_bundle_locator",
            &self.fixture_bundle_locator,
            true,
        )?;
        if self.argv.first() != Some(&self.launcher_locator) {
            bail!("evaluator.argv[0] must equal the exact launcher_locator");
        }
        if !self
            .argv
            .iter()
            .any(|argument| argument == &self.evaluator_locator)
        {
            bail!("evaluator.argv must contain evaluator_locator as one exact argument");
        }
        self.evaluator_sha256
            .validate("evaluator.evaluator_sha256")?;
        self.fixture_bundle_sha256
            .validate("evaluator.fixture_bundle_sha256")?;
        validate_identifier("evaluator.protocol", &self.protocol)?;
        if let Some(environment) = &self.rust_build_environment {
            environment.validate()?;
            for reserved in [
                "CARGO",
                "CARGO_HOME",
                "CARGO_INCREMENTAL",
                "CARGO_NET_OFFLINE",
                "CARGO_TARGET_DIR",
                "CARGO_TERM_COLOR",
                "PAPERTIGER_MISE_CARGO_EXECUTABLE",
                "RUSTC",
                "RUSTUP_TOOLCHAIN",
                "TEMP",
                "TMP",
                "TMPDIR",
            ] {
                if self.environment.contains_key(reserved) {
                    bail!(
                        "evaluator.environment must not override runtime-owned Rust variable '{reserved}'"
                    );
                }
            }
            let linker_key = environment.linker_environment_key()?.with_context(
                || "new Rust build environments require an exact target linker binding",
            )?;
            if self.environment.contains_key(&linker_key) {
                bail!(
                    "evaluator.environment must not override runtime-owned Rust variable '{linker_key}'"
                );
            }
        }
        if let Some(build) = &self.judge_build {
            build.validate()?;
            if self
                .environment
                .contains_key("PAPERTIGER_MISE_JUDGE_BUILD_ROOT")
            {
                bail!(
                    "evaluator.environment must not override runtime-owned judge-build variable 'PAPERTIGER_MISE_JUDGE_BUILD_ROOT'"
                );
            }
        }
        Ok(())
    }
}

impl JudgeBuildBinding {
    fn validate(&self) -> Result<()> {
        if self.argv.is_empty() {
            bail!("evaluator.judge_build.argv must contain an exact executable and arguments");
        }
        for argument in &self.argv {
            validate_exact_string("evaluator.judge_build.argv argument", argument)?;
        }
        validate_identifier("evaluator.judge_build.toolchain_name", &self.toolchain_name)?;
        validate_exact_string(
            "evaluator.judge_build.toolchain_version",
            &self.toolchain_version,
        )?;
        validate_local_absolute_path(
            "evaluator.judge_build.toolchain_executable_locator",
            &self.toolchain_executable_locator,
        )?;
        self.toolchain_executable_sha256
            .validate("evaluator.judge_build.toolchain_executable_sha256")?;
        if self.argv.first() != Some(&self.toolchain_executable_locator) {
            bail!(
                "evaluator.judge_build.argv[0] must equal the exact toolchain_executable_locator"
            );
        }
        validate_repo_relative_path(
            "evaluator.judge_build.executable_locator",
            &self.executable_locator,
            true,
        )?;
        if self.maximum_executable_bytes == 0 {
            bail!("evaluator.judge_build.maximum_executable_bytes must be nonzero");
        }
        Ok(())
    }
}

impl RustBuildEnvironment {
    fn validate(&self) -> Result<()> {
        validate_local_absolute_path(
            "evaluator.rust_build_environment.cargo_executable_locator",
            &self.cargo_executable_locator,
        )?;
        self.cargo_executable_sha256
            .validate("evaluator.rust_build_environment.cargo_executable_sha256")?;
        validate_local_absolute_path(
            "evaluator.rust_build_environment.rustc_executable_locator",
            &self.rustc_executable_locator,
        )?;
        self.rustc_executable_sha256
            .validate("evaluator.rust_build_environment.rustc_executable_sha256")?;
        validate_identifier(
            "evaluator.rust_build_environment.toolchain",
            &self.toolchain,
        )?;
        validate_repo_relative_path(
            "evaluator.rust_build_environment.lockfile_locator",
            &self.lockfile_locator,
            true,
        )?;
        self.lockfile_sha256
            .validate("evaluator.rust_build_environment.lockfile_sha256")?;
        validate_repo_relative_path(
            "evaluator.rust_build_environment.cargo_config_locator",
            &self.cargo_config_locator,
            true,
        )?;
        self.cargo_config_sha256
            .validate("evaluator.rust_build_environment.cargo_config_sha256")?;
        validate_repo_relative_path(
            "evaluator.rust_build_environment.vendored_sources_locator",
            &self.vendored_sources_locator,
            false,
        )?;
        if self.lockfile_locator == self.cargo_config_locator {
            bail!("Rust lockfile and Cargo config must be distinct artifacts");
        }
        let linker = self.linker.as_ref().with_context(|| {
            "new Rust build environments require linker.target_triple plus an exact linker executable path and SHA-256"
        })?;
        linker.validate()?;
        Ok(())
    }

    pub(crate) fn linker_environment_key(&self) -> Result<Option<String>> {
        self.linker
            .as_ref()
            .map(RustLinkerBinding::environment_key)
            .transpose()
    }
}

impl RustLinkerBinding {
    fn validate(&self) -> Result<()> {
        let _ = self.environment_key()?;
        validate_local_absolute_path(
            "evaluator.rust_build_environment.linker.executable_locator",
            &self.executable_locator,
        )?;
        self.executable_sha256
            .validate("evaluator.rust_build_environment.linker.executable_sha256")?;
        Ok(())
    }

    fn environment_key(&self) -> Result<String> {
        let field = "evaluator.rust_build_environment.linker.target_triple";
        validate_identifier(field, &self.target_triple)?;
        if self
            .target_triple
            .bytes()
            .any(|byte| byte.is_ascii_uppercase() || byte == b'.')
            || self.target_triple.starts_with('-')
            || self.target_triple.ends_with('-')
            || self.target_triple.contains("--")
        {
            bail!(
                "{field} must be a canonical lowercase Rust target triple using alphanumerics, '_' and single '-' separators"
            );
        }
        Ok(format!(
            "CARGO_TARGET_{}_LINKER",
            self.target_triple.replace('-', "_").to_ascii_uppercase()
        ))
    }
}

impl AdapterBinding {
    fn validate(&self) -> Result<()> {
        validate_identifier("adapter.name", &self.name)?;
        validate_identifier("adapter.protocol", &self.protocol)?;
        validate_repo_relative_path(
            "adapter.implementation_locator",
            &self.implementation_locator,
            true,
        )?;
        self.implementation_sha256
            .validate("adapter.implementation_sha256")?;
        Ok(())
    }
}

impl CandidateMaterialContract {
    fn validate(&self) -> Result<()> {
        if self.kind != "git_change_set"
            || self.protocol != crate::candidate::GIT_CHANGE_SET_PROTOCOL_V1
            || self.media_type != crate::candidate::GIT_CHANGE_SET_MEDIA_TYPE
        {
            bail!("candidate_material must declare the exact supported Git change-set contract");
        }
        Ok(())
    }
}

impl ExecutionLimits {
    fn validate(&self, budgets: &CumulativeBudgetCaps) -> Result<()> {
        validate_exact_string(
            "execution_limits.workspace_root_locator",
            &self.workspace_root_locator,
        )?;
        if !std::path::Path::new(&self.workspace_root_locator).is_absolute() {
            bail!("execution_limits.workspace_root_locator must be absolute");
        }
        if let Some(runtime_root) = &self.runtime_root_locator {
            validate_exact_string("execution_limits.runtime_root_locator", runtime_root)?;
            if !std::path::Path::new(runtime_root).is_absolute() {
                bail!("execution_limits.runtime_root_locator must be absolute");
            }
        }
        if self.maximum_trial_wall_time_ms == 0 || self.maximum_trial_output_bytes == 0 {
            bail!("portable wall-time and output limits must be greater than zero");
        }
        let wall_cap = budgets
            .limit(BudgetResource::WallTimeMilliseconds)
            .ok_or_else(|| anyhow!("missing cumulative wall-time budget"))?;
        if self.maximum_trial_wall_time_ms > wall_cap {
            bail!("per-trial wall-time limit exceeds the cumulative campaign wall-time budget");
        }
        if let Some(artifact_cap) = budgets.limit(BudgetResource::ArtifactBytes)
            && self.maximum_trial_output_bytes > artifact_cap
        {
            bail!("per-trial output limit exceeds the cumulative artifact-byte budget");
        }
        Ok(())
    }
}

impl CumulativeBudgetCaps {
    fn validate(&self) -> Result<()> {
        let mut resources = BTreeSet::new();
        for cap in &self.caps {
            if cap.hard_limit == 0 {
                bail!("budget cap {:?} must be greater than zero", cap.resource);
            }
            if !resources.insert(cap.resource) {
                bail!("duplicate cumulative budget cap for {:?}", cap.resource);
            }
        }
        for required in [
            BudgetResource::WallTimeMilliseconds,
            BudgetResource::Candidates,
            BudgetResource::Trials,
            BudgetResource::Failures,
            BudgetResource::HoldoutDisclosures,
            BudgetResource::ArtifactBytes,
            BudgetResource::DiskBytesWritten,
        ] {
            if !resources.contains(&required) {
                bail!("missing required cumulative budget cap for {required:?}");
            }
        }
        Ok(())
    }

    fn limit(&self, resource: BudgetResource) -> Option<u64> {
        self.caps
            .iter()
            .find(|cap| cap.resource == resource)
            .map(|cap| cap.hard_limit)
    }
}

impl StopRules {
    fn validate(&self) -> Result<()> {
        if self.not_before_unix_ms == 0 {
            bail!("stop_rules.not_before_unix_ms must be nonzero");
        }
        if self.deadline_unix_ms <= self.not_before_unix_ms {
            bail!("stop_rules.deadline_unix_ms must be after not_before_unix_ms");
        }
        if self.max_consecutive_infrastructure_failures == 0 {
            bail!("max_consecutive_infrastructure_failures must be greater than zero");
        }
        if self.max_trials_without_qualified_improvement == 0 {
            bail!("max_trials_without_qualified_improvement must be greater than zero");
        }
        if !self.stop_on_evaluator_integrity_failure {
            bail!("stop_on_evaluator_integrity_failure must be true");
        }
        Ok(())
    }
}

impl HoldoutPolicy {
    fn validate(
        &self,
        containment: ContainmentGrade,
        budgets: &CumulativeBudgetCaps,
    ) -> Result<()> {
        if self.disclosure_cap == 0 {
            bail!("holdouts.disclosure_cap must be greater than zero");
        }
        if self.tiers.is_empty() {
            bail!("holdouts.tiers must not be empty");
        }
        let mut keys = BTreeSet::new();
        let mut kinds = BTreeSet::new();
        let mut tier_total = 0_u64;
        for tier in &self.tiers {
            validate_identifier("holdout tier key", &tier.key)?;
            if !keys.insert(tier.key.as_str()) {
                bail!("duplicate holdout tier key '{}'", tier.key);
            }
            if !kinds.insert(tier.kind) {
                bail!("duplicate holdout tier kind {:?}", tier.kind);
            }
            tier.fixture_sha256
                .validate("holdout tier fixture_sha256")?;
            validate_exact_string("holdout tier fixture_locator", &tier.fixture_locator)?;
            if tier.maximum_disclosures == 0 {
                bail!(
                    "holdout tier '{}' maximum_disclosures must be greater than zero",
                    tier.key
                );
            }
            if tier.minimum_repetitions == 0 {
                bail!(
                    "holdout tier '{}' requires at least one repetition",
                    tier.key
                );
            }
            if u64::from(tier.minimum_repetitions) > tier.maximum_disclosures {
                bail!(
                    "holdout tier '{}' repetition requirement exceeds its disclosure cap",
                    tier.key
                );
            }
            tier_total = tier_total
                .checked_add(tier.maximum_disclosures)
                .ok_or_else(|| anyhow!("holdout tier disclosure total overflow"))?;
            if tier.kind == HoldoutTierKind::Confirmation {
                if containment != ContainmentGrade::Sealed {
                    bail!("confirmation holdout requires sealed containment");
                }
                if !tier
                    .fixture_locator
                    .strip_prefix("sealed://")
                    .is_some_and(|capability| {
                        !capability.is_empty()
                            && !capability.contains(char::is_whitespace)
                            && !capability.contains('#')
                    })
                {
                    bail!(
                        "confirmation holdout requires an undisclosed sealed:// capability locator"
                    );
                }
                if !matches!(tier.disclosure, HoldoutDisclosure::VerdictOnly) {
                    bail!("confirmation holdout disclosure must be verdict_only");
                }
            }
        }
        if tier_total > self.disclosure_cap {
            bail!(
                "holdout tier disclosure total {tier_total} exceeds disclosure_cap {}",
                self.disclosure_cap
            );
        }
        let ledger_cap = budgets
            .limit(BudgetResource::HoldoutDisclosures)
            .ok_or_else(|| anyhow!("missing holdout disclosure budget"))?;
        if self.disclosure_cap > ledger_cap {
            bail!(
                "holdouts.disclosure_cap {} exceeds cumulative budget {ledger_cap}",
                self.disclosure_cap
            );
        }
        Ok(())
    }
}

impl GenerationBinding {
    fn validate(&self, campaign_id: &str) -> Result<()> {
        self.outer_judge_executable_sha256
            .validate("generation.outer_judge_executable_sha256")?;
        validate_local_absolute_path(
            "generation.outer_judge_executable_locator",
            &self.outer_judge_executable_locator,
        )?;
        self.proposal_policy_sha256
            .validate("generation.proposal_policy_sha256")?;
        validate_repo_relative_path(
            "generation.proposal_policy_locator",
            &self.proposal_policy_locator,
            true,
        )?;
        if self.maximum_recursion_depth == 0 {
            bail!("generation.maximum_recursion_depth must be greater than zero");
        }
        if self.maximum_recursion_depth > MAXIMUM_RECURSION_DEPTH {
            bail!(
                "generation.maximum_recursion_depth exceeds the supported bound {MAXIMUM_RECURSION_DEPTH}"
            );
        }
        if self.recursion_depth > self.maximum_recursion_depth {
            bail!("generation.recursion_depth exceeds maximum_recursion_depth");
        }
        match (self.recursion_depth, &self.parent_campaign) {
            (0, None) => {
                if self.runtime_generation != 1 {
                    bail!("root campaign runtime generation must be exactly one");
                }
            }
            (0, Some(_)) => bail!("root campaign at recursion depth zero must not name a parent"),
            (_, None) => bail!("non-root campaign must bind an exact parent campaign"),
            (depth, Some(parent)) => {
                validate_identifier(
                    "generation.parent_campaign.campaign_id",
                    &parent.campaign_id,
                )?;
                parent
                    .manifest_sha256
                    .validate("generation.parent_campaign.manifest_sha256")?;
                if parent.campaign_id == campaign_id {
                    bail!("campaign must not name itself as its parent");
                }
                if parent.recursion_depth.checked_add(1) != Some(depth) {
                    bail!("parent recursion depth must be exactly one less than child depth");
                }
                if parent.runtime_generation.checked_add(1) != Some(self.runtime_generation) {
                    bail!("child runtime generation must be exactly one after its parent");
                }
            }
        }
        Ok(())
    }
}

fn validate_local_absolute_path(field: &str, value: &str) -> Result<()> {
    validate_exact_string(field, value)?;
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

impl CalibrationRequirements {
    pub fn no_op_material_sha256(&self) -> &Sha256Digest {
        self.no_op
            .candidate_material_sha256
            .as_ref()
            .or(self.no_op.candidate_patch_sha256.as_ref())
            .expect("validated calibration identity")
    }

    pub fn known_bad_material_sha256(&self) -> &Sha256Digest {
        self.known_bad
            .candidate_material_sha256
            .as_ref()
            .or(self.known_bad.candidate_patch_sha256.as_ref())
            .expect("validated calibration identity")
    }

    fn validate(&self, material: Option<&CandidateMaterialContract>) -> Result<()> {
        let (no_op, known_bad) = match material {
            None => {
                if self.no_op.candidate_material_sha256.is_some()
                    || self.known_bad.candidate_material_sha256.is_some()
                {
                    bail!(
                        "legacy Git-patch campaigns require candidate_patch_sha256 calibration fields"
                    );
                }
                (
                    self.no_op
                        .candidate_patch_sha256
                        .as_ref()
                        .context("legacy no-op calibration requires candidate_patch_sha256")?,
                    self.known_bad
                        .candidate_patch_sha256
                        .as_ref()
                        .context("legacy known-bad calibration requires candidate_patch_sha256")?,
                )
            }
            Some(_) => {
                if self.no_op.candidate_patch_sha256.is_some()
                    || self.known_bad.candidate_patch_sha256.is_some()
                {
                    bail!(
                        "typed candidate-material campaigns do not accept legacy candidate_patch_sha256 fields"
                    );
                }
                (
                    self.no_op
                        .candidate_material_sha256
                        .as_ref()
                        .context("typed no-op calibration requires candidate_material_sha256")?,
                    self.known_bad.candidate_material_sha256.as_ref().context(
                        "typed known-bad calibration requires candidate_material_sha256",
                    )?,
                )
            }
        };
        no_op.validate("calibration.no_op candidate identity")?;
        self.no_op
            .fixture_sha256
            .validate("calibration.no_op.fixture_sha256")?;
        validate_exact_string(
            "calibration.no_op.fixture_locator",
            &self.no_op.fixture_locator,
        )?;
        if material.is_none() && no_op.0 != EMPTY_SHA256 {
            bail!("no-op calibration candidate patch must be the SHA-256 of empty bytes");
        }
        if self.no_op.minimum_repetitions < 2 {
            bail!("no-op calibration requires at least two repetitions to expose run noise");
        }
        known_bad.validate("calibration.known_bad candidate identity")?;
        self.known_bad
            .fixture_sha256
            .validate("calibration.known_bad.fixture_sha256")?;
        validate_exact_string(
            "calibration.known_bad.fixture_locator",
            &self.known_bad.fixture_locator,
        )?;
        if known_bad.0 == no_op.0 || (material.is_none() && known_bad.0 == EMPTY_SHA256) {
            bail!("known-bad calibration must bind candidate material distinct from the no-op");
        }
        if self.known_bad.minimum_repetitions == 0 {
            bail!("known-bad calibration requires at least one repetition");
        }
        validate_identifier(
            "calibration.known_bad.expected_rejection_code",
            &self.known_bad.expected_rejection_code,
        )?;
        Ok(())
    }
}

impl Sha256Digest {
    fn validate(&self, field: &str) -> Result<()> {
        validate_lower_hex(field, &self.0, 64)
    }
}

fn validate_objectives(objectives: &[ObjectiveSpec]) -> Result<()> {
    if objectives.is_empty() {
        bail!("objectives must not be empty");
    }
    let mut keys = BTreeSet::new();
    let mut has_primary = false;
    let mut has_measurable_primary = false;
    let mut has_hard_constraint = false;
    for objective in objectives {
        validate_identifier("objective key", &objective.key)?;
        validate_exact_string("objective unit", &objective.unit)?;
        if !keys.insert(objective.key.as_str()) {
            bail!("duplicate objective key '{}'", objective.key);
        }
        has_primary |= matches!(objective.role, ObjectiveRole::Primary);
        if objective.role == ObjectiveRole::Primary && objective_unit_is_boolean(&objective.unit) {
            bail!(
                "primary objective '{}' must be quantitative; use boolean objectives only as hard constraints",
                objective.key
            );
        }
        has_measurable_primary |=
            objective.role == ObjectiveRole::Primary && objective.minimum_practical_change > 0.0;
        has_hard_constraint |= matches!(objective.role, ObjectiveRole::HardConstraint);
        validate_nonnegative_finite(
            "objective.minimum_practical_change",
            objective.minimum_practical_change,
        )?;
        validate_nonnegative_finite(
            "objective.regression_tolerance",
            objective.regression_tolerance,
        )?;
        if let Some(value) = objective.acceptance_threshold {
            validate_finite("objective.acceptance_threshold", value)?;
        }
        if matches!(objective.role, ObjectiveRole::HardConstraint)
            && objective.acceptance_threshold.is_none()
        {
            bail!(
                "hard constraint '{}' requires acceptance_threshold",
                objective.key
            );
        }
        match objective.direction {
            ObjectiveDirection::Target if objective.target_value.is_none() => {
                bail!("target objective '{}' requires target_value", objective.key);
            }
            ObjectiveDirection::Minimize | ObjectiveDirection::Maximize
                if objective.target_value.is_some() =>
            {
                bail!(
                    "non-target objective '{}' must not declare target_value",
                    objective.key
                );
            }
            _ => {}
        }
        if let Some(value) = objective.target_value {
            validate_finite("objective.target_value", value)?;
        }
    }
    if !has_primary {
        bail!("campaign requires at least one primary objective");
    }
    if !has_measurable_primary {
        bail!(
            "campaign requires at least one quantitative primary objective with a nonzero minimum_practical_change"
        );
    }
    if !has_hard_constraint {
        bail!("campaign requires at least one hard constraint");
    }
    Ok(())
}

fn validate_identifier(field: &str, value: &str) -> Result<()> {
    validate_exact_string(field, value)?;
    if value.len() > 128 {
        bail!("{field} exceeds 128 bytes");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("{field} must contain only ASCII letters, digits, '.', '_' or '-'");
    }
    Ok(())
}

fn validate_exact_string(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{field} must not be empty");
    }
    if value.trim() != value {
        bail!("{field} must not contain leading or trailing whitespace");
    }
    if value.contains('\0') {
        bail!("{field} must not contain NUL");
    }
    Ok(())
}

fn validate_git_object(field: &str, value: &str, format: GitObjectFormat) -> Result<()> {
    let length = match format {
        GitObjectFormat::Sha1 => 40,
        GitObjectFormat::Sha256 => 64,
    };
    validate_lower_hex(field, value, length)
}

fn validate_lower_hex(field: &str, value: &str, length: usize) -> Result<()> {
    if value.len() != length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{field} must be exactly {length} hexadecimal characters");
    }
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        bail!("{field} must use canonical lowercase hexadecimal");
    }
    Ok(())
}

fn validate_unique_paths(field: &str, values: &[String], reject_git: bool) -> Result<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        validate_repo_relative_path(field, value, reject_git)?;
        if !seen.insert(value.as_str()) {
            bail!("{field} contains duplicate path '{value}'");
        }
    }
    Ok(())
}

fn validate_opaque_fixture_locator(field: &str, value: &str) -> Result<()> {
    validate_exact_string(field, value)?;
    let (scheme, capability) = value
        .split_once("://")
        .with_context(|| format!("{field} must use an opaque capability locator"))?;
    if scheme != "sealed"
        || capability.is_empty()
        || capability.contains(char::is_whitespace)
        || capability.contains('#')
    {
        bail!("{field} must use a canonical sealed:// capability locator");
    }
    Ok(())
}

fn validate_repo_relative_path(field: &str, value: &str, reject_git: bool) -> Result<()> {
    validate_exact_string(field, value)?;
    if value == "." {
        return Ok(());
    }
    if value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\\')
        || value.contains(':')
        || value.contains('*')
        || value.contains('?')
    {
        bail!("{field} '{value}' must be a canonical repository-relative path prefix");
    }
    if value
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        bail!("{field} '{value}' contains an unsafe path component");
    }
    if reject_git && (value == ".git" || value.starts_with(".git/")) {
        bail!("{field} must not admit Git administrative paths");
    }
    Ok(())
}

fn validate_nonnegative_finite(field: &str, value: f64) -> Result<()> {
    validate_finite(field, value)?;
    if value < 0.0 {
        bail!("{field} must be nonnegative");
    }
    Ok(())
}

fn validate_finite(field: &str, value: f64) -> Result<()> {
    if !value.is_finite() {
        bail!("{field} must be finite");
    }
    Ok(())
}

fn normalize_zero(value: &mut f64) {
    if *value == 0.0 {
        *value = 0.0;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn digest(byte: char) -> Sha256Digest {
        Sha256Digest(byte.to_string().repeat(64))
    }

    fn test_absolute(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("papertiger-mise-{name}"))
            .to_string_lossy()
            .replace('\\', "/")
    }

    pub(crate) fn valid_manifest() -> CampaignManifest {
        let judge = std::fs::canonicalize(std::env::current_exe().expect("test executable"))
            .expect("canonical test executable");
        let judge_text = judge.to_string_lossy();
        let judge_locator = judge_text
            .strip_prefix(r"\\?\")
            .unwrap_or(&judge_text)
            .replace('\\', "/");
        let judge_sha256 = Sha256Digest(sha256(
            &std::fs::read(&judge).expect("test executable bytes"),
        ));
        CampaignManifest {
            schema: CAMPAIGN_SCHEMA_V1.to_owned(),
            campaign_id: "fixture-rsi-1".to_owned(),
            source: SourceBinding {
                repository_id: "virtualonno-fixture".to_owned(),
                repository_locator: test_absolute("fixture-source"),
                repository_identity_sha256: digest('a'),
                git_object_format: GitObjectFormat::Sha1,
                base_commit: "1".repeat(40),
                base_tree: "2".repeat(40),
            },
            mutation_scope: MutationScope {
                allowlist: vec!["src".to_owned(), "tests/fixtures".to_owned()],
                protected_paths: vec!["fixtures/mise".to_owned()],
            },
            adapter: AdapterBinding {
                name: "deterministic-fixture".to_owned(),
                protocol: "papertiger-mise.adapter.v1".to_owned(),
                implementation_locator: "fixtures/mise/adapter.json".to_owned(),
                implementation_sha256: digest('5'),
            },
            evaluator: EvaluatorBinding {
                argv: vec![
                    judge_locator.clone(),
                    "fixtures/mise/evaluator.sh".to_owned(),
                    "--manifest".to_owned(),
                    "run.json".to_owned(),
                ],
                working_directory: ".".to_owned(),
                environment: BTreeMap::from([("MISE_PROTOCOL".to_owned(), "v1".to_owned())]),
                launcher_locator: judge_locator.clone(),
                launcher_sha256: judge_sha256.clone(),
                evaluator_locator: "fixtures/mise/evaluator.sh".to_owned(),
                evaluator_sha256: digest('b'),
                fixture_bundle_locator: "fixtures/mise/confirmation.json".to_owned(),
                fixture_bundle_sha256: digest('c'),
                protocol: "papertiger-mise.measurement.v1".to_owned(),
                rust_build_environment: None,
                judge_build: None,
            },
            execution_limits: ExecutionLimits {
                workspace_root_locator: test_absolute("fixture-run"),
                runtime_root_locator: Some(test_absolute("fixture-runtime")),
                maximum_trial_wall_time_ms: 10_000,
                maximum_trial_output_bytes: 1024 * 1024,
                network: NetworkPolicy::Denied,
            },
            objectives: vec![
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
            ],
            paired_analysis: None,
            budgets: CumulativeBudgetCaps {
                caps: vec![
                    BudgetLimit {
                        resource: BudgetResource::Trials,
                        hard_limit: 20,
                    },
                    BudgetLimit {
                        resource: BudgetResource::WallTimeMilliseconds,
                        hard_limit: 60_000,
                    },
                    BudgetLimit {
                        resource: BudgetResource::Candidates,
                        hard_limit: 10,
                    },
                    BudgetLimit {
                        resource: BudgetResource::Failures,
                        hard_limit: 5,
                    },
                    BudgetLimit {
                        resource: BudgetResource::HoldoutDisclosures,
                        hard_limit: 8,
                    },
                    BudgetLimit {
                        resource: BudgetResource::ArtifactBytes,
                        hard_limit: 16 * 1024 * 1024,
                    },
                    BudgetLimit {
                        resource: BudgetResource::DiskBytesWritten,
                        hard_limit: 256 * 1024 * 1024,
                    },
                ],
            },
            candidate_material: None,
            stop_rules: StopRules {
                not_before_unix_ms: 1,
                deadline_unix_ms: 4_000_000_000_000,
                max_consecutive_infrastructure_failures: 3,
                max_trials_without_qualified_improvement: 8,
                stop_on_evaluator_integrity_failure: true,
            },
            containment: ContainmentGrade::Sealed,
            containment_requirement: Some(ContainmentRequirement {
                protocol: crate::attestation::SEALED_ATTESTATION_PROTOCOL_V2.to_owned(),
                executor_locator: "fixtures/mise/containment-executor.json".to_owned(),
                executor_sha256: digest('4'),
                profile_sha256: digest('3'),
                maximum_processes: 8,
                maximum_memory_bytes: 512 * 1024 * 1024,
            }),
            holdouts: HoldoutPolicy {
                disclosure_cap: 8,
                tiers: vec![
                    HoldoutTier {
                        key: "confirmation".to_owned(),
                        kind: HoldoutTierKind::Confirmation,
                        fixture_locator: "sealed://fixture/confirmation".to_owned(),
                        fixture_sha256: digest('d'),
                        maximum_disclosures: 3,
                        minimum_repetitions: 1,
                        disclosure: HoldoutDisclosure::VerdictOnly,
                    },
                    HoldoutTier {
                        key: "exploration".to_owned(),
                        kind: HoldoutTierKind::Exploration,
                        fixture_locator: "fixtures/mise/exploration.json".to_owned(),
                        fixture_sha256: digest('e'),
                        maximum_disclosures: 5,
                        minimum_repetitions: 1,
                        disclosure: HoldoutDisclosure::Aggregate,
                    },
                ],
            },
            generation: GenerationBinding {
                runtime_generation: 1,
                recursion_depth: 0,
                maximum_recursion_depth: 3,
                outer_judge_executable_sha256: judge_sha256,
                outer_judge_executable_locator: judge_locator,
                proposal_policy_sha256: digest('9'),
                proposal_policy_locator: "fixtures/mise/proposal-policy.json".to_owned(),
                parent_campaign: None,
            },
            calibration: CalibrationRequirements {
                no_op: NoOpCalibration {
                    candidate_patch_sha256: Some(Sha256Digest(EMPTY_SHA256.to_owned())),
                    candidate_material_sha256: None,
                    fixture_locator: "fixtures/mise/calibration-no-op.json".to_owned(),
                    fixture_sha256: digest('7'),
                    minimum_repetitions: 3,
                },
                known_bad: KnownBadCalibration {
                    candidate_patch_sha256: Some(digest('8')),
                    candidate_material_sha256: None,
                    fixture_locator: "fixtures/mise/calibration-known-bad.json".to_owned(),
                    fixture_sha256: digest('6'),
                    minimum_repetitions: 2,
                    expected_rejection_code: "known-regression".to_owned(),
                },
            },
        }
    }

    #[test]
    fn valid_manifest_has_order_independent_canonical_identity() {
        let first = valid_manifest();
        first.validate().unwrap();

        let mut second = first.clone();
        second.mutation_scope.allowlist.reverse();
        second.mutation_scope.protected_paths.reverse();
        second.objectives.reverse();
        second.budgets.caps.reverse();
        second.holdouts.tiers.reverse();

        assert_eq!(
            first.canonical_bytes().unwrap(),
            second.canonical_bytes().unwrap()
        );
        assert_eq!(first.sha256().unwrap(), second.sha256().unwrap());
        assert_eq!(first.sha256().unwrap().len(), 64);
    }

    #[test]
    fn paired_analysis_is_an_immutable_admission_contract_not_an_adapter_option() {
        use crate::statistics::{
            PAIRED_ANALYSIS_SCHEMA_V2, PAIRED_MEASUREMENT_PROTOCOL_V1, PairedAnalysisMethod,
            PairedAnalysisPlan, PairedBlockDesign, PairedCalibrationFixtureBindings,
            PairedFixtureBinding, PairedObjectivePolicy, RationalThreshold,
        };

        let revealed_seed = b"admission-test-secret-seed-00000001";
        let mut manifest = valid_manifest();
        manifest.evaluator.protocol = PAIRED_MEASUREMENT_PROTOCOL_V1.to_owned();
        let confirmation = manifest
            .holdouts
            .tiers
            .iter_mut()
            .find(|tier| tier.kind == HoldoutTierKind::Confirmation)
            .unwrap();
        confirmation.minimum_repetitions = 16;
        confirmation.maximum_disclosures = 16;
        manifest.calibration.no_op.minimum_repetitions = 16;
        manifest.calibration.known_bad.minimum_repetitions = 16;
        manifest.holdouts.disclosure_cap = 21;
        manifest
            .budgets
            .caps
            .iter_mut()
            .find(|cap| cap.resource == BudgetResource::HoldoutDisclosures)
            .unwrap()
            .hard_limit = 21;
        manifest
            .budgets
            .caps
            .iter_mut()
            .find(|cap| cap.resource == BudgetResource::Trials)
            .unwrap()
            .hard_limit = 64;
        manifest.stop_rules.max_trials_without_qualified_improvement = 24;
        manifest.paired_analysis = Some(PairedAnalysisPlan {
            schema: PAIRED_ANALYSIS_SCHEMA_V2.to_owned(),
            method: PairedAnalysisMethod::FixedSampleExactPairedBinomial,
            trial_adapter: Some(crate::adapter::PairedAdapterBinding {
                schema: crate::adapter::PAIRED_ADAPTER_BINDING_SCHEMA_V1.to_owned(),
                executable_locator: manifest.evaluator.launcher_locator.clone(),
                executable_sha256: manifest.evaluator.launcher_sha256.clone(),
                argv: vec![manifest.evaluator.launcher_locator.clone()],
                working_directory: manifest.source.repository_locator.clone(),
                environment: BTreeMap::new(),
                result_schema: "fixture.paired-trial-result.v1".to_owned(),
                maximum_wall_time_ms: 1_000,
                maximum_output_bytes: 1_048_576,
            }),
            inference_scope: crate::statistics::PairedInferenceScope::IndependentBlockPopulation {
                population: "admission-fixture-replays".to_owned(),
                sampling_protocol_locator: "fixtures/mise/sampling-protocol.json".to_owned(),
                sampling_protocol_sha256: digest('2'),
            },
            required_blocks: 8,
            order_seed_protocol_locator: "fixtures/mise/order-seed-protocol.json".to_owned(),
            order_seed_protocol_sha256: digest('7'),
            order_seed_commitments: vec![crate::statistics::PairedSlotSeedCommitment {
                candidate_analysis_slot: 1,
                sha256: Sha256Digest(sha256(revealed_seed)),
            }],
            calibration_seed_commitments: crate::statistics::PairedCalibrationSeedCommitments {
                no_op_sha256: digest('0'),
                known_bad_sha256: digest('1'),
            },
            campaign_familywise_alpha: RationalThreshold {
                numerator: 1,
                denominator: 20,
            },
            maximum_candidate_analyses: 1,
            objective_policies: vec![
                PairedObjectivePolicy {
                    objective: "tests-pass".to_owned(),
                    scale10: 0,
                    measurement_summary_protocol: "all-pass.v1".to_owned(),
                    minimum_practical_change_units: 0,
                    regression_tolerance_units: 0,
                    acceptance_threshold_units: Some(1),
                    target_value_units: None,
                    no_op_maximum_absolute_effect_units: 0,
                },
                PairedObjectivePolicy {
                    objective: "latency-ms".to_owned(),
                    scale10: 3,
                    measurement_summary_protocol: "nearest-rank-p95.v1".to_owned(),
                    minimum_practical_change_units: 1_000,
                    regression_tolerance_units: 0,
                    acceptance_threshold_units: None,
                    target_value_units: None,
                    no_op_maximum_absolute_effect_units: 0,
                },
            ],
            calibration_fixtures: PairedCalibrationFixtureBindings {
                no_op: PairedFixtureBinding {
                    locator: manifest.calibration.no_op.fixture_locator.clone(),
                    sha256: manifest.calibration.no_op.fixture_sha256.clone(),
                },
                known_bad: PairedFixtureBinding {
                    locator: manifest.calibration.known_bad.fixture_locator.clone(),
                    sha256: manifest.calibration.known_bad.fixture_sha256.clone(),
                },
            },
            blocks: (0..8)
                .map(|block_index| PairedBlockDesign {
                    block_index,
                    stratum: "fixture".to_owned(),
                    fixture_locator: "sealed://fixture/confirmation".to_owned(),
                    fixture_sha256: digest('d'),
                    environment_profile_sha256: digest('3'),
                    workload_seed: format!("seed-{block_index}"),
                })
                .collect(),
        });

        manifest.validate().expect("paired admission");
        let mut workspace = manifest.clone();
        workspace.containment = ContainmentGrade::WorkspaceOnly;
        workspace.containment_requirement = None;
        workspace.execution_limits.network = NetworkPolicy::Unrestricted;
        workspace
            .holdouts
            .tiers
            .retain(|tier| tier.kind == HoldoutTierKind::Exploration);
        let exploration = workspace
            .holdouts
            .tiers
            .iter_mut()
            .find(|tier| tier.kind == HoldoutTierKind::Exploration)
            .unwrap();
        exploration.minimum_repetitions = 16;
        exploration.maximum_disclosures = 16;
        let exploration_locator = exploration.fixture_locator.clone();
        let exploration_sha256 = exploration.fixture_sha256.clone();
        workspace.holdouts.disclosure_cap = 16;
        workspace
            .budgets
            .caps
            .iter_mut()
            .find(|cap| cap.resource == BudgetResource::HoldoutDisclosures)
            .unwrap()
            .hard_limit = 16;
        for block in &mut workspace.paired_analysis.as_mut().unwrap().blocks {
            block.fixture_locator = exploration_locator.clone();
            block.fixture_sha256 = exploration_sha256.clone();
        }
        workspace
            .validate()
            .expect("workspace paired analysis uses a disclosed exploration tier");
        let mut wrong_research_fixture = manifest.clone();
        wrong_research_fixture
            .paired_analysis
            .as_mut()
            .unwrap()
            .blocks[0]
            .fixture_sha256 = digest('f');
        assert!(
            wrong_research_fixture
                .validate()
                .expect_err("research blocks cannot impersonate another tier")
                .to_string()
                .contains("evidence-tier binding")
        );
        let mut wrong_calibration_fixture = manifest.clone();
        wrong_calibration_fixture
            .paired_analysis
            .as_mut()
            .unwrap()
            .calibration_fixtures
            .no_op
            .sha256 = digest('f');
        assert!(
            wrong_calibration_fixture
                .validate()
                .expect_err("paired calibration must use the manifest-bound fixture")
                .to_string()
                .contains("manifest calibration binding")
        );
        let mut short_calibration = manifest.clone();
        short_calibration.calibration.no_op.minimum_repetitions = 15;
        assert!(
            short_calibration
                .validate()
                .expect_err("paired calibration needs both runs in every block")
                .to_string()
                .contains("two trial executions")
        );
        let mut short_confirmation = manifest.clone();
        short_confirmation
            .holdouts
            .tiers
            .iter_mut()
            .find(|tier| tier.kind == HoldoutTierKind::Confirmation)
            .unwrap()
            .maximum_disclosures = 15;
        assert!(
            short_confirmation
                .validate()
                .expect_err("paired confirmation needs disclosure capacity for both runs")
                .to_string()
                .contains("repetition requirement exceeds its disclosure cap")
        );
        let first = manifest.sha256().unwrap();
        manifest
            .paired_analysis
            .as_mut()
            .unwrap()
            .objective_policies
            .reverse();
        assert_eq!(first, manifest.sha256().unwrap());

        manifest.evaluator.protocol = "papertiger-mise.measurement.v1".to_owned();
        assert!(
            manifest
                .validate()
                .expect_err("deterministic protocol cannot impersonate paired measurement")
                .to_string()
                .contains("paired analysis requires evaluator protocol")
        );
    }

    #[test]
    fn refuses_unsafe_mutation_scope_and_bad_hashes() {
        let mut manifest = valid_manifest();
        manifest.mutation_scope.allowlist = vec!["../evaluator".to_owned()];
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("unsafe path")
        );

        let mut manifest = valid_manifest();
        manifest.source.base_commit = "ABC".repeat(14);
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn refuses_unbounded_or_integrity_permissive_campaigns() {
        let mut manifest = valid_manifest();
        manifest
            .budgets
            .caps
            .retain(|cap| cap.resource != BudgetResource::Trials);
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("Trials")
        );

        let mut manifest = valid_manifest();
        manifest.stop_rules.deadline_unix_ms = manifest.stop_rules.not_before_unix_ms;
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("deadline")
        );

        let mut manifest = valid_manifest();
        manifest.stop_rules.stop_on_evaluator_integrity_failure = false;
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("must be true")
        );
    }

    #[test]
    fn confirmation_holdout_requires_sealed_verdict_only_disclosure() {
        let mut manifest = valid_manifest();
        manifest.containment = ContainmentGrade::WorkspaceOnly;
        manifest.containment_requirement = None;
        manifest.execution_limits.network = NetworkPolicy::Unrestricted;
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("sealed")
        );

        let mut manifest = valid_manifest();
        manifest.holdouts.tiers[0].disclosure = HoldoutDisclosure::Raw;
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("verdict_only")
        );

        let mut manifest = valid_manifest();
        manifest.holdouts.tiers[0].fixture_locator = "fixtures/mise/confirmation.json".to_owned();
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("sealed://")
        );

        let mut manifest = valid_manifest();
        manifest.holdouts.tiers[0].fixture_locator = "vault://confirmation".to_owned();
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("sealed://")
        );

        let mut manifest = valid_manifest();
        manifest.holdouts.disclosure_cap = 9;
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("budget")
        );
    }

    #[test]
    fn nonsealed_campaigns_require_tracked_local_fixtures() {
        let mut manifest = valid_manifest();
        manifest.containment = ContainmentGrade::WorkspaceOnly;
        manifest.containment_requirement = None;
        manifest.execution_limits.network = NetworkPolicy::Unrestricted;
        manifest
            .holdouts
            .tiers
            .retain(|tier| tier.kind != HoldoutTierKind::Confirmation);
        manifest.holdouts.disclosure_cap = manifest
            .holdouts
            .tiers
            .iter()
            .map(|tier| tier.maximum_disclosures)
            .sum();
        manifest.holdouts.tiers[0].fixture_locator = "remote://mutable-exploration".to_owned();
        let error = manifest
            .validate()
            .expect_err("WorkspaceOnly opaque fixture must be refused");
        assert!(error.to_string().contains("relative"), "{error:#}");
    }

    #[test]
    fn recursion_is_bounded_and_parent_is_exact() {
        let mut child = valid_manifest();
        child.generation.recursion_depth = 1;
        assert!(child.validate().unwrap_err().to_string().contains("parent"));

        child.generation.parent_campaign = Some(ParentCampaignBinding {
            campaign_id: "fixture-parent".to_owned(),
            manifest_sha256: digest('4'),
            runtime_generation: 1,
            recursion_depth: 0,
        });
        child.generation.runtime_generation = 2;
        child.validate().unwrap();

        child.generation.recursion_depth = 4;
        assert!(
            child
                .validate()
                .unwrap_err()
                .to_string()
                .contains("maximum_recursion_depth")
        );
    }

    #[test]
    fn root_generation_and_global_recursion_bound_are_semantic() {
        let mut manifest = valid_manifest();
        manifest.generation.runtime_generation = 7;
        let error = manifest
            .validate()
            .expect_err("root cannot claim a decorative later generation");
        assert!(error.to_string().contains("exactly one"), "{error:#}");

        manifest.generation.runtime_generation = 1;
        manifest.generation.maximum_recursion_depth = MAXIMUM_RECURSION_DEPTH + 1;
        let error = manifest
            .validate()
            .expect_err("global recursion ceiling must be finite");
        assert!(error.to_string().contains("supported bound"), "{error:#}");
    }

    #[test]
    fn primary_objective_must_be_quantitative_and_practically_nonzero() {
        let mut manifest = valid_manifest();
        let primary = manifest
            .objectives
            .iter()
            .find(|objective| objective.role == ObjectiveRole::Primary)
            .map(|objective| objective.key.clone())
            .expect("primary objective");
        let primary_index = manifest
            .objectives
            .iter()
            .position(|objective| objective.key == primary)
            .expect("primary index");
        let mut quantitative_primary = manifest.objectives[primary_index].clone();
        quantitative_primary.key = "secondary-throughput".to_owned();
        manifest.objectives.push(quantitative_primary);
        manifest.objectives[primary_index].unit = "boolean".to_owned();
        let error = manifest
            .validate()
            .expect_err("another quantitative primary must not rescue a boolean primary");
        assert!(
            error.to_string().contains("must be quantitative"),
            "{error:#}"
        );

        manifest.objectives.pop();
        manifest.objectives[primary_index].unit = "milliseconds".to_owned();
        manifest.objectives[primary_index].minimum_practical_change = 0.0;
        let error = manifest
            .validate()
            .expect_err("zero practical threshold must not qualify");
        assert!(error.to_string().contains("nonzero"), "{error:#}");
    }

    #[test]
    fn rust_build_environment_reserves_runtime_owned_variables() {
        let mut manifest = valid_manifest();
        manifest.mutation_scope.protected_paths.extend([
            ".cargo".to_owned(),
            "Cargo.lock".to_owned(),
            "vendor".to_owned(),
        ]);
        let executable = manifest.evaluator.launcher_locator.clone();
        let executable_sha256 = manifest.evaluator.launcher_sha256.clone();
        manifest.evaluator.rust_build_environment = Some(RustBuildEnvironment {
            cargo_executable_locator: executable.clone(),
            cargo_executable_sha256: executable_sha256.clone(),
            rustc_executable_locator: executable.clone(),
            rustc_executable_sha256: executable_sha256.clone(),
            toolchain: "1.95.0".to_owned(),
            lockfile_locator: "Cargo.lock".to_owned(),
            lockfile_sha256: digest('1'),
            cargo_config_locator: ".cargo/config.toml".to_owned(),
            cargo_config_sha256: digest('2'),
            vendored_sources_locator: "vendor".to_owned(),
            vendored_sources_tree: "3".repeat(40),
            linker: Some(RustLinkerBinding {
                target_triple: "x86_64-unknown-linux-gnu".to_owned(),
                executable_locator: executable,
                executable_sha256,
            }),
        });
        manifest.validate().expect("typed Rust environment");

        let mut missing_linker = manifest.clone();
        missing_linker
            .evaluator
            .rust_build_environment
            .as_mut()
            .expect("Rust environment")
            .linker = None;
        let error = missing_linker
            .validate()
            .expect_err("new Rust environment must bind its target linker");
        assert!(error.to_string().contains("exact linker"), "{error:#}");

        let mut invalid_target = manifest.clone();
        invalid_target
            .evaluator
            .rust_build_environment
            .as_mut()
            .and_then(|rust| rust.linker.as_mut())
            .expect("linker")
            .target_triple = "X86_64-unknown-linux-gnu".to_owned();
        let error = invalid_target
            .validate()
            .expect_err("target triples are canonical environment identities");
        assert!(
            error.to_string().contains("canonical lowercase"),
            "{error:#}"
        );

        manifest
            .evaluator
            .environment
            .insert("CARGO_HOME".to_owned(), "shared-cache".to_owned());
        let error = manifest
            .validate()
            .expect_err("manifest cannot inject shared mutable Cargo home");
        assert!(error.to_string().contains("runtime-owned"), "{error:#}");
        manifest.evaluator.environment.remove("CARGO_HOME");
        manifest.evaluator.environment.insert(
            "PAPERTIGER_MISE_CARGO_EXECUTABLE".to_owned(),
            "ambient-cargo".to_owned(),
        );
        let error = manifest
            .validate()
            .expect_err("manifest cannot forge the stable admitted Cargo locator");
        assert!(error.to_string().contains("runtime-owned"), "{error:#}");
        manifest
            .evaluator
            .environment
            .remove("PAPERTIGER_MISE_CARGO_EXECUTABLE");
        manifest.evaluator.environment.insert(
            "TMPDIR".to_owned(),
            "ambient-temporary-directory".to_owned(),
        );
        let error = manifest
            .validate()
            .expect_err("manifest cannot inject a shared process temporary directory");
        assert!(error.to_string().contains("runtime-owned"), "{error:#}");
        manifest.evaluator.environment.remove("TMPDIR");
        manifest.evaluator.environment.insert(
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER".to_owned(),
            "forged-linker".to_owned(),
        );
        let error = manifest
            .validate()
            .expect_err("manifest cannot override its derived target linker variable");
        assert!(
            error
                .to_string()
                .contains("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER"),
            "{error:#}"
        );
    }

    #[test]
    fn judge_build_is_exact_bounded_and_owns_its_runtime_root() {
        let mut manifest = valid_manifest();
        let locator = manifest.evaluator.launcher_locator.clone();
        let sha256 = manifest.evaluator.launcher_sha256.clone();
        manifest.evaluator.judge_build = Some(JudgeBuildBinding {
            argv: vec![locator.clone(), "build-judge".to_owned()],
            toolchain_name: "fixture-builder".to_owned(),
            toolchain_version: "1".to_owned(),
            toolchain_executable_locator: locator,
            toolchain_executable_sha256: sha256,
            executable_locator: "bin/judge".to_owned(),
            maximum_executable_bytes: 1024,
        });
        manifest.validate().expect("valid judge-build binding");

        manifest.evaluator.environment.insert(
            "PAPERTIGER_MISE_JUDGE_BUILD_ROOT".to_owned(),
            "forged".to_owned(),
        );
        let error = manifest
            .validate()
            .expect_err("operator cannot choose the runtime output root");
        assert!(error.to_string().contains("runtime-owned judge-build"));
        manifest
            .evaluator
            .environment
            .remove("PAPERTIGER_MISE_JUDGE_BUILD_ROOT");

        manifest.evaluator.judge_build.as_mut().unwrap().argv[0] = test_absolute("other-builder");
        let error = manifest
            .validate()
            .expect_err("build argv must identify the frozen toolchain");
        assert!(error.to_string().contains("argv[0]"));
    }

    #[test]
    fn historical_identity_does_not_reapply_later_admission_doctrine() {
        let mut historical = valid_manifest();
        historical
            .objectives
            .iter_mut()
            .find(|objective| objective.role == ObjectiveRole::Primary)
            .expect("primary objective")
            .unit = "boolean".to_owned();
        assert!(historical.validate().is_err());
        let first = historical
            .historical_canonical_bytes()
            .expect("historical canonical identity");
        historical.mutation_scope.allowlist.reverse();
        let reordered = historical
            .historical_canonical_bytes()
            .expect("reordered historical identity");
        assert_eq!(first, reordered);
    }

    #[test]
    fn calibration_requires_true_no_op_and_nonempty_known_bad_patch() {
        let mut manifest = valid_manifest();
        manifest.calibration.no_op.candidate_patch_sha256 = Some(digest('5'));
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("no-op")
        );

        let mut manifest = valid_manifest();
        manifest.calibration.known_bad.candidate_patch_sha256 =
            Some(Sha256Digest(EMPTY_SHA256.to_owned()));
        assert!(
            manifest
                .validate()
                .unwrap_err()
                .to_string()
                .contains("known-bad")
        );
    }

    #[test]
    fn local_manifest_schema_has_no_native_resource_or_process_tree_vocabulary() {
        let mut manifest = valid_manifest();
        manifest.containment = ContainmentGrade::WorkspaceOnly;
        manifest.containment_requirement = None;
        manifest.execution_limits.network = NetworkPolicy::Unrestricted;
        manifest
            .holdouts
            .tiers
            .retain(|tier| tier.kind != HoldoutTierKind::Confirmation);
        manifest.holdouts.disclosure_cap = manifest
            .holdouts
            .tiers
            .iter()
            .map(|tier| tier.maximum_disclosures)
            .sum();

        let canonical: serde_json::Value =
            serde_json::from_slice(&manifest.canonical_bytes().expect("canonical manifest"))
                .expect("canonical manifest JSON");
        let limits = canonical["execution_limits"]
            .as_object()
            .expect("execution limits object");
        assert_eq!(
            limits.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "maximum_trial_output_bytes",
                "maximum_trial_wall_time_ms",
                "network",
                "runtime_root_locator",
                "workspace_root_locator",
            ])
        );
        assert_eq!(canonical["containment"], "workspace_only");
        assert!(canonical["containment_requirement"].is_null());

        fn assert_portable_keys(value: &serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    for (key, child) in object {
                        assert!(
                            ![
                                "maximum_processes",
                                "maximum_memory_bytes",
                                "process_tree",
                                "windows",
                                "linux",
                                "macos",
                            ]
                            .contains(&key.as_str()),
                            "found native authority field {key}"
                        );
                        assert_portable_keys(child);
                    }
                }
                serde_json::Value::Array(values) => {
                    for child in values {
                        assert_portable_keys(child);
                    }
                }
                _ => {}
            }
        }
        assert_portable_keys(&canonical);
    }

    #[test]
    fn legacy_native_limit_and_process_tree_claims_are_rejected() {
        let manifest = valid_manifest();
        let mut value = serde_json::to_value(&manifest).expect("manifest JSON");
        value["execution_limits"]["maximum_processes"] = serde_json::json!(2);
        let error = serde_json::from_value::<CampaignManifest>(value)
            .expect_err("native process limit must be an unknown field");
        assert!(error.to_string().contains("maximum_processes"), "{error:#}");

        let mut value = serde_json::to_value(&manifest).expect("manifest JSON");
        value["containment"] = serde_json::json!("process_tree");
        let error = serde_json::from_value::<CampaignManifest>(value)
            .expect_err("process-tree grade must no longer exist");
        assert!(error.to_string().contains("process_tree"), "{error:#}");
    }

    #[test]
    fn sealed_worker_requires_portable_nonzero_resource_ceilings() {
        let mut manifest = valid_manifest();
        manifest
            .containment_requirement
            .as_mut()
            .expect("containment requirement")
            .maximum_processes = 0;
        let error = manifest
            .validate()
            .expect_err("zero portable ceiling must be refused");
        assert!(error.to_string().contains("nonzero portable"), "{error:#}");
    }
}
