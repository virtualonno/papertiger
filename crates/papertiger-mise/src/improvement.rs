use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use papertiger::{sha256, validate_sha256};

pub const PARADIGM_REGISTRY_SCHEMA_V1: &str = "papertiger.improvement-paradigm-registry.v1";
pub const PARADIGM_TEMPLATE_SCHEMA_V1: &str = "papertiger.improvement-paradigm-template.v1";
pub const PROJECT_IMPROVEMENT_BRIEF_SCHEMA_V1: &str = "papertiger.project-improvement-brief.v1";
pub const BRIEF_APPROVAL_SCHEMA_V1: &str = "papertiger.project-improvement-brief-approval.v1";
pub const COMPILED_IMPROVEMENT_DRAFT_SCHEMA_V1: &str = "papertiger.compiled-improvement-draft.v1";

const CURRENT_REGISTRY: &str = include_str!("../../../docs/mise_templates/v2/registry.json");
const HISTORICAL_REGISTRY_V1: &str = include_str!("../../../docs/mise_templates/v1/registry.json");
const REQUIRED_PARADIGMS: [&str; 8] = [
    "capability_effectiveness",
    "compatibility_portability",
    "correctness_quality",
    "maintainability_changeability",
    "operator_usability",
    "performance_resource_efficiency",
    "reliability_recovery",
    "security_trust_boundary",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParadigmRegistry {
    pub schema: String,
    pub templates: Vec<ParadigmTemplate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParadigmTemplate {
    pub schema: String,
    pub key: String,
    pub version: u32,
    pub discovery_questions: Vec<String>,
    pub admissible_primary_outcomes: Vec<String>,
    pub required_objective_roles: Vec<ObjectiveRole>,
    pub required_countermetrics: Vec<String>,
    pub required_controls: Vec<String>,
    pub fixture_holdout_guidance: Vec<String>,
    pub candidate_scope_guidance: Vec<String>,
    pub invalid_proxy_warnings: Vec<String>,
    pub stop_policy_defaults: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectImprovementBrief {
    pub schema: String,
    pub brief_id: String,
    pub authority: String,
    pub project: BriefProject,
    pub template: BriefTemplateBinding,
    pub evidence: Vec<BriefEvidence>,
    pub opportunity: ImprovementOpportunity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefProject {
    pub name: String,
    pub repository_locator: String,
    pub observed_revision: String,
    pub worktree_dirty: bool,
    pub intent: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefTemplateBinding {
    pub key: String,
    pub version: u32,
    pub registry_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceFreshness {
    Live,
    Sampled,
    Stale,
    Unavailable,
    Inferred,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefEvidence {
    pub key: String,
    pub freshness: EvidenceFreshness,
    pub locator: Option<String>,
    pub sha256: Option<String>,
    pub observation: String,
    pub limitation: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureDisclosure {
    DisclosedWorkspace,
    Sealed,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefFixture {
    pub key: String,
    pub locator: Option<String>,
    pub disclosure: FixtureDisclosure,
    pub oracle: String,
    pub limitation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRequirement {
    pub key: String,
    pub locator: Option<String>,
    pub sha256: Option<String>,
    pub required_behavior: String,
    pub evidence: EvidenceFreshness,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImprovementOpportunity {
    pub intended_outcome: String,
    pub quantitative_primary: String,
    pub inference_scope: String,
    pub anti_goodhart: AntiGoodhartControls,
    pub native_observables: Vec<String>,
    pub invariants: Vec<String>,
    pub protected_countermetrics: Vec<String>,
    pub negative_controls: Vec<String>,
    pub candidate_scope: Vec<String>,
    pub fixtures: Vec<BriefFixture>,
    pub environment_requirements: Vec<EnvironmentRequirement>,
    pub uncertainties: Vec<String>,
    pub resource_costs: Vec<String>,
    pub objectives: Vec<BriefObjective>,
    pub mutation_allowlist: Vec<String>,
    pub budgets: Vec<BriefBudget>,
    pub stop_rules: BriefStopRules,
    pub adapter_requirement: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AntiGoodhartControls {
    pub capability_deletion: String,
    pub workload_narrowing: String,
    pub cost_displacement: String,
    pub test_weakening: String,
    pub self_certification: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefObjective {
    pub key: String,
    pub role: ObjectiveRole,
    pub direction: String,
    pub unit: String,
    pub minimum_practical_change_units: i64,
    pub regression_tolerance_units: i64,
    pub acceptance_threshold_units: Option<i64>,
}

/// Exhaustive objective roles shared by planning briefs and admitted Mise
/// manifests. Serde rejects unknown vocabulary before either authority acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveRole {
    HardConstraint,
    Primary,
    Protected,
    Diagnostic,
}

impl ObjectiveRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HardConstraint => "hard_constraint",
            Self::Primary => "primary",
            Self::Protected => "protected",
            Self::Diagnostic => "diagnostic",
        }
    }
}

/// Boolean measurements can express constraints, but never quantitative
/// primary improvement. Keep this rule shared by brief and manifest admission.
pub fn objective_unit_is_boolean(unit: &str) -> bool {
    unit.trim().eq_ignore_ascii_case("boolean")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefBudget {
    pub resource: String,
    pub hard_limit: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefStopRules {
    pub maximum_candidate_analyses: u32,
    pub maximum_infrastructure_failures: u32,
    pub maximum_trials_without_improvement: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BriefApproval {
    pub schema: String,
    pub brief_sha256: String,
    pub approved_by: String,
    pub approved_at: String,
    pub decision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledImprovementDraft {
    pub schema: String,
    pub authority: String,
    pub brief_sha256: String,
    pub approval_sha256: String,
    pub template: BriefTemplateBinding,
    pub task_graph: Vec<CompiledTask>,
    pub campaign: CompiledCampaignDraft,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledTask {
    pub key: String,
    pub title: String,
    pub depends_on: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledCampaignDraft {
    pub project: BriefProject,
    pub intended_outcome: String,
    pub inference_scope: String,
    pub anti_goodhart: AntiGoodhartControls,
    pub objectives: Vec<BriefObjective>,
    pub fixtures: Vec<BriefFixture>,
    pub environment_requirements: Vec<EnvironmentRequirement>,
    pub mutation_allowlist: Vec<String>,
    pub budgets: Vec<BriefBudget>,
    pub stop_rules: BriefStopRules,
    pub adapter_requirement: String,
    pub admission_requires_explicit_command: bool,
}

pub fn builtin_paradigm_registry() -> Result<(ParadigmRegistry, String)> {
    let bytes = CURRENT_REGISTRY.as_bytes();
    Ok((validate_paradigm_registry(bytes)?, sha256(bytes)))
}

fn paradigm_registry_for_digest(digest: &str) -> Result<ParadigmRegistry> {
    for bytes in [
        CURRENT_REGISTRY.as_bytes(),
        HISTORICAL_REGISTRY_V1.as_bytes(),
    ] {
        if sha256(bytes) == digest {
            return validate_paradigm_registry(bytes);
        }
    }
    let current = sha256(CURRENT_REGISTRY.as_bytes());
    bail!(
        "brief template registry SHA-256 is unknown; create the brief from the current built-in registry {current}"
    )
}

pub fn validate_paradigm_registry(bytes: &[u8]) -> Result<ParadigmRegistry> {
    let registry: ParadigmRegistry =
        serde_json::from_slice(bytes).context("parse improvement-paradigm registry JSON")?;
    if registry.schema != PARADIGM_REGISTRY_SCHEMA_V1 {
        bail!(
            "paradigm registry schema must be {PARADIGM_REGISTRY_SCHEMA_V1}, found '{}'",
            registry.schema
        );
    }
    let mut keys = BTreeSet::new();
    for template in &registry.templates {
        validate_template(template)?;
        if !keys.insert(template.key.as_str()) {
            bail!("duplicate paradigm template key '{}'", template.key);
        }
    }
    let expected = REQUIRED_PARADIGMS.into_iter().collect::<BTreeSet<_>>();
    if keys != expected {
        bail!(
            "registry must contain exactly the eight canonical paradigms; expected {expected:?}, found {keys:?}"
        );
    }
    Ok(registry)
}

pub fn paradigm_registry_sha256(bytes: &[u8]) -> String {
    sha256(bytes)
}

pub fn validate_project_improvement_brief(bytes: &[u8]) -> Result<ProjectImprovementBrief> {
    let brief: ProjectImprovementBrief =
        serde_json::from_slice(bytes).context("parse project improvement brief JSON")?;
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("parse project improvement brief JSON")?;
    reject_unresolved_placeholders(&value, "brief")?;
    if brief.schema != PROJECT_IMPROVEMENT_BRIEF_SCHEMA_V1 {
        bail!("project improvement brief schema must be {PROJECT_IMPROVEMENT_BRIEF_SCHEMA_V1}");
    }
    if brief.authority != "planning_input_only" {
        bail!("project improvement brief authority must be planning_input_only");
    }
    for (name, value) in [
        ("brief_id", brief.brief_id.as_str()),
        ("project.name", brief.project.name.as_str()),
        (
            "project.repository_locator",
            brief.project.repository_locator.as_str(),
        ),
        (
            "project.observed_revision",
            brief.project.observed_revision.as_str(),
        ),
        ("project.intent", brief.project.intent.as_str()),
        (
            "opportunity.intended_outcome",
            brief.opportunity.intended_outcome.as_str(),
        ),
        (
            "opportunity.quantitative_primary",
            brief.opportunity.quantitative_primary.as_str(),
        ),
        (
            "opportunity.inference_scope",
            brief.opportunity.inference_scope.as_str(),
        ),
    ] {
        if value.trim().is_empty() {
            bail!("project improvement brief requires {name}");
        }
    }
    validate_sha256(&brief.template.registry_sha256, "template.registry_sha256")?;
    let registry = paradigm_registry_for_digest(&brief.template.registry_sha256)?;
    let template = registry
        .templates
        .iter()
        .find(|template| template.key == brief.template.key)
        .with_context(|| format!("unknown brief paradigm '{}'", brief.template.key))?;
    if template.version != brief.template.version {
        bail!(
            "brief paradigm '{}' version must be {}, found {}",
            template.key,
            template.version,
            brief.template.version
        );
    }
    if brief.evidence.is_empty() {
        bail!("project improvement brief requires explicit evidence");
    }
    let mut evidence_keys = BTreeSet::new();
    for evidence in &brief.evidence {
        if !evidence_keys.insert(evidence.key.as_str()) {
            bail!("duplicate brief evidence key '{}'", evidence.key);
        }
        if evidence.observation.trim().is_empty() {
            bail!("brief evidence '{}' requires an observation", evidence.key);
        }
        if let Some(digest) = &evidence.sha256 {
            validate_sha256(digest, "evidence.sha256")?;
            if evidence.locator.is_none() {
                bail!(
                    "brief evidence '{}' digest requires a locator",
                    evidence.key
                );
            }
        }
        if !matches!(evidence.freshness, EvidenceFreshness::Live)
            && evidence.limitation.as_deref().is_none_or(str::is_empty)
        {
            bail!(
                "non-live brief evidence '{}' requires an explicit limitation",
                evidence.key
            );
        }
    }
    for (name, values) in [
        ("native_observables", &brief.opportunity.native_observables),
        ("invariants", &brief.opportunity.invariants),
        (
            "protected_countermetrics",
            &brief.opportunity.protected_countermetrics,
        ),
        ("negative_controls", &brief.opportunity.negative_controls),
        ("candidate_scope", &brief.opportunity.candidate_scope),
        ("uncertainties", &brief.opportunity.uncertainties),
        ("resource_costs", &brief.opportunity.resource_costs),
        ("mutation_allowlist", &brief.opportunity.mutation_allowlist),
    ] {
        if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
            bail!("project improvement brief requires nonempty {name}");
        }
    }
    if brief.opportunity.adapter_requirement.trim().is_empty() {
        bail!("project improvement brief requires adapter_requirement");
    }
    for (name, proof) in [
        (
            "capability_deletion",
            &brief.opportunity.anti_goodhart.capability_deletion,
        ),
        (
            "workload_narrowing",
            &brief.opportunity.anti_goodhart.workload_narrowing,
        ),
        (
            "cost_displacement",
            &brief.opportunity.anti_goodhart.cost_displacement,
        ),
        (
            "test_weakening",
            &brief.opportunity.anti_goodhart.test_weakening,
        ),
        (
            "self_certification",
            &brief.opportunity.anti_goodhart.self_certification,
        ),
    ] {
        if proof.trim().is_empty() {
            bail!("project improvement brief requires anti_goodhart.{name} evidence");
        }
    }
    validate_objective_portfolio(&brief.opportunity.objectives)?;
    if brief.opportunity.budgets.is_empty()
        || brief
            .opportunity
            .budgets
            .iter()
            .any(|budget| budget.resource.trim().is_empty() || budget.hard_limit == 0)
    {
        bail!("project improvement brief requires positive typed budgets");
    }
    if brief.opportunity.stop_rules.maximum_candidate_analyses == 0
        || brief.opportunity.stop_rules.maximum_infrastructure_failures == 0
        || brief
            .opportunity
            .stop_rules
            .maximum_trials_without_improvement
            == 0
    {
        bail!("project improvement brief requires finite positive stop rules");
    }
    if brief.opportunity.fixtures.is_empty() {
        bail!("project improvement brief requires fixtures");
    }
    for fixture in &brief.opportunity.fixtures {
        if fixture.oracle.trim().is_empty() {
            bail!("brief fixture '{}' requires an oracle", fixture.key);
        }
        if matches!(fixture.disclosure, FixtureDisclosure::Unavailable)
            && fixture.limitation.as_deref().is_none_or(str::is_empty)
        {
            bail!(
                "unavailable brief fixture '{}' requires a limitation",
                fixture.key
            );
        }
    }
    if brief.opportunity.environment_requirements.is_empty() {
        bail!("project improvement brief requires environment requirements");
    }
    for requirement in &brief.opportunity.environment_requirements {
        if requirement.required_behavior.trim().is_empty() {
            bail!(
                "brief environment requirement '{}' requires behavior",
                requirement.key
            );
        }
        if let Some(digest) = &requirement.sha256 {
            validate_sha256(digest, "environment requirement sha256")?;
            if requirement.locator.is_none() {
                bail!(
                    "brief environment requirement '{}' digest requires a locator",
                    requirement.key
                );
            }
        }
    }
    Ok(brief)
}

fn validate_objective_portfolio(objectives: &[BriefObjective]) -> Result<()> {
    let mut keys = BTreeSet::new();
    let mut roles = BTreeSet::new();
    for objective in objectives {
        if !keys.insert(objective.key.as_str()) {
            bail!("duplicate brief objective '{}'", objective.key);
        }
        roles.insert(objective.role);
        if !matches!(objective.direction.as_str(), "maximize" | "minimize") {
            bail!("brief objective '{}' has invalid direction", objective.key);
        }
        if objective.role == ObjectiveRole::Primary
            && (objective_unit_is_boolean(&objective.unit)
                || objective.minimum_practical_change_units <= 0)
        {
            bail!(
                "primary brief objective '{}' must be quantitative with positive practical change",
                objective.key
            );
        }
        if objective.role == ObjectiveRole::HardConstraint
            && objective.acceptance_threshold_units.is_none()
        {
            bail!(
                "hard-constraint brief objective '{}' requires an acceptance threshold",
                objective.key
            );
        }
    }
    for required in [
        ObjectiveRole::Primary,
        ObjectiveRole::HardConstraint,
        ObjectiveRole::Protected,
    ] {
        if !roles.contains(&required) {
            bail!(
                "brief objective portfolio requires role '{}'",
                required.as_str()
            );
        }
    }
    for required in ["correctness", "compatibility"] {
        if !objectives.iter().any(|objective| {
            objective.key == required && objective.role == ObjectiveRole::HardConstraint
        }) {
            bail!("brief objective portfolio requires a '{required}' hard constraint");
        }
    }
    Ok(())
}

pub fn compile_project_improvement_brief(
    brief_bytes: &[u8],
    approval_bytes: &[u8],
) -> Result<CompiledImprovementDraft> {
    let brief = validate_project_improvement_brief(brief_bytes)?;
    let approval: BriefApproval =
        serde_json::from_slice(approval_bytes).context("parse brief approval JSON")?;
    if approval.schema != BRIEF_APPROVAL_SCHEMA_V1 || approval.decision != "compile_draft" {
        bail!("brief approval must authorize compile_draft under {BRIEF_APPROVAL_SCHEMA_V1}");
    }
    let brief_sha256 = sha256(brief_bytes);
    if approval.brief_sha256 != brief_sha256 {
        bail!("brief approval SHA-256 does not match the exact brief bytes");
    }
    validate_sha256(&approval.brief_sha256, "approval.brief_sha256")?;
    if approval.approved_by.trim().is_empty() || approval.approved_at.trim().is_empty() {
        bail!("brief approval requires actor and timestamp");
    }
    if brief.project.worktree_dirty {
        bail!("brief compiler refuses a dirty observed project revision");
    }
    if !matches!(brief.project.observed_revision.len(), 40 | 64)
        || !brief
            .project
            .observed_revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!(
            "brief compiler requires a full lowercase hexadecimal Git object ID (40 or 64 characters)"
        );
    }
    let unresolved_fixtures = brief
        .opportunity
        .fixtures
        .iter()
        .filter(|fixture| matches!(fixture.disclosure, FixtureDisclosure::Unavailable))
        .map(|fixture| fixture.key.as_str())
        .collect::<Vec<_>>();
    if !unresolved_fixtures.is_empty() {
        bail!("brief compiler refuses unavailable fixtures: {unresolved_fixtures:?}");
    }
    let unresolved_environment = brief
        .opportunity
        .environment_requirements
        .iter()
        .filter(|requirement| {
            !matches!(requirement.evidence, EvidenceFreshness::Live)
                || requirement.locator.is_none()
        })
        .map(|requirement| requirement.key.as_str())
        .collect::<Vec<_>>();
    if !unresolved_environment.is_empty() {
        bail!(
            "brief compiler refuses unresolved environment requirements: {unresolved_environment:?}"
        );
    }
    let task_graph = vec![
        CompiledTask {
            key: "freeze-campaign-artifacts".to_owned(),
            title: "Freeze campaign fixtures, adapter, evaluator, and environment".to_owned(),
            depends_on: Vec::new(),
        },
        CompiledTask {
            key: "preflight-campaign".to_owned(),
            title: "Preflight the non-admitted campaign draft".to_owned(),
            depends_on: vec!["freeze-campaign-artifacts".to_owned()],
        },
        CompiledTask {
            key: "execute-campaign".to_owned(),
            title: "Execute the separately admitted finite campaign".to_owned(),
            depends_on: vec!["preflight-campaign".to_owned()],
        },
        CompiledTask {
            key: "independent-integration-decision".to_owned(),
            title: "Review terminal evidence without automatic integration".to_owned(),
            depends_on: vec!["execute-campaign".to_owned()],
        },
    ];
    Ok(CompiledImprovementDraft {
        schema: COMPILED_IMPROVEMENT_DRAFT_SCHEMA_V1.to_owned(),
        authority: "non_admitted_draft".to_owned(),
        brief_sha256,
        approval_sha256: sha256(approval_bytes),
        template: brief.template,
        task_graph,
        campaign: CompiledCampaignDraft {
            project: brief.project,
            intended_outcome: brief.opportunity.intended_outcome,
            inference_scope: brief.opportunity.inference_scope,
            anti_goodhart: brief.opportunity.anti_goodhart,
            objectives: brief.opportunity.objectives,
            fixtures: brief.opportunity.fixtures,
            environment_requirements: brief.opportunity.environment_requirements,
            mutation_allowlist: brief.opportunity.mutation_allowlist,
            budgets: brief.opportunity.budgets,
            stop_rules: brief.opportunity.stop_rules,
            adapter_requirement: brief.opportunity.adapter_requirement,
            admission_requires_explicit_command: true,
        },
    })
}

fn reject_unresolved_placeholders(value: &serde_json::Value, path: &str) -> Result<()> {
    match value {
        serde_json::Value::String(text) => {
            let normalized = text.trim().to_ascii_lowercase();
            let unresolved = normalized == "todo"
                || normalized == "tbd"
                || normalized == "placeholder"
                || normalized.contains("<fill")
                || normalized.contains("${")
                || normalized.contains("{{");
            if unresolved {
                bail!("project improvement brief contains unresolved placeholder at {path}");
            }
        }
        serde_json::Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                reject_unresolved_placeholders(child, &format!("{path}[{index}]"))?;
            }
        }
        serde_json::Value::Object(values) => {
            for (key, child) in values {
                reject_unresolved_placeholders(child, &format!("{path}.{key}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_template(template: &ParadigmTemplate) -> Result<()> {
    if template.schema != PARADIGM_TEMPLATE_SCHEMA_V1 {
        bail!(
            "template '{}' schema must be {PARADIGM_TEMPLATE_SCHEMA_V1}",
            template.key
        );
    }
    if template.version == 0 {
        bail!("template '{}' version must be positive", template.key);
    }
    for (name, values) in [
        ("discovery_questions", &template.discovery_questions),
        (
            "admissible_primary_outcomes",
            &template.admissible_primary_outcomes,
        ),
        ("required_countermetrics", &template.required_countermetrics),
        ("required_controls", &template.required_controls),
        (
            "fixture_holdout_guidance",
            &template.fixture_holdout_guidance,
        ),
        (
            "candidate_scope_guidance",
            &template.candidate_scope_guidance,
        ),
        ("invalid_proxy_warnings", &template.invalid_proxy_warnings),
        ("stop_policy_defaults", &template.stop_policy_defaults),
    ] {
        if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
            bail!("template '{}' requires nonempty {name}", template.key);
        }
        for value in values {
            reject_project_authority_leak(&template.key, name, value)?;
        }
    }
    if template.required_objective_roles.is_empty() {
        bail!(
            "template '{}' requires nonempty required_objective_roles",
            template.key
        );
    }
    let roles = template
        .required_objective_roles
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let required_roles = [
        ObjectiveRole::HardConstraint,
        ObjectiveRole::Primary,
        ObjectiveRole::Protected,
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    if roles != required_roles {
        bail!(
            "template '{}' must require exactly primary, hard_constraint, and protected objective roles",
            template.key
        );
    }
    Ok(())
}

fn reject_project_authority_leak(key: &str, field: &str, value: &str) -> Result<()> {
    let lower = value.to_ascii_lowercase();
    let forbidden = [
        "\\",
        "/",
        ".exe",
        ".sqlite",
        "cargo ",
        "git ",
        "ghidra",
        "papertiger task",
        "qualified",
        "rejected",
        "promote",
        "deploy",
        "threshold=",
        "target=",
    ];
    if let Some(token) = forbidden.into_iter().find(|token| lower.contains(token)) {
        bail!(
            "template '{key}' field {field} leaks project command, path, threshold, or verdict token '{token}'"
        );
    }
    if value.chars().any(|character| character.is_ascii_digit()) {
        bail!(
            "template '{key}' field {field} contains a numeric project threshold; templates describe shapes, briefs bind values"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_is_complete_and_content_addressed() {
        let (registry, digest) = builtin_paradigm_registry().expect("validate built-in registry");
        assert_eq!(registry.templates.len(), REQUIRED_PARADIGMS.len());
        assert!(
            registry
                .templates
                .iter()
                .all(|template| template.version == 2)
        );
        assert_eq!(digest.len(), 64);
        assert!(!CURRENT_REGISTRY.to_ascii_lowercase().contains("canary"));
    }

    #[test]
    fn historical_registry_remains_readable_without_becoming_current() {
        let digest = sha256(HISTORICAL_REGISTRY_V1.as_bytes());
        let registry = paradigm_registry_for_digest(&digest).expect("historical registry");
        assert!(
            registry
                .templates
                .iter()
                .all(|template| template.version == 1)
        );
        assert_ne!(digest, builtin_paradigm_registry().unwrap().1);
    }

    #[test]
    fn shared_digest_contract_refuses_noncanonical_values() {
        let digest = sha256(b"papertiger");
        assert!(validate_sha256(&digest, "fixture digest").is_ok());
        for invalid in ["", "abc", &digest.to_uppercase(), &"g".repeat(64)] {
            let error = validate_sha256(invalid, "fixture digest")
                .expect_err("noncanonical digest must be refused");
            assert!(error.to_string().starts_with("fixture digest"));
        }
    }

    #[test]
    fn project_specific_command_is_refused() {
        let (mut registry, _) = builtin_paradigm_registry().expect("built-in registry");
        registry.templates[0]
            .required_controls
            .push("run cargo test".to_owned());
        let bytes = serde_json::to_vec(&registry).expect("serialize registry");
        let error = validate_paradigm_registry(&bytes).expect_err("command leakage must fail");
        assert!(error.to_string().contains("cargo "));
    }

    #[test]
    fn synthetic_example_brief_is_valid_planning_input() {
        let bytes = include_bytes!(
            "../../../docs/mise_profiles/example-project.runtime-readiness.brief.json"
        );
        let brief = validate_project_improvement_brief(bytes).expect("validate example brief");
        assert_eq!(brief.authority, "planning_input_only");
        assert!(
            brief
                .evidence
                .iter()
                .any(|item| item.freshness == EvidenceFreshness::Unavailable)
        );
        assert!(brief.opportunity.fixtures.iter().any(|fixture| {
            fixture.disclosure == FixtureDisclosure::DisclosedWorkspace
                && fixture.limitation.is_some()
        }));
    }

    #[test]
    fn brief_objective_roles_are_exhaustive() {
        let mut value: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../../docs/mise_profiles/example-project.runtime-readiness.brief.json"
        ))
        .unwrap();
        let mut invalid = value["opportunity"]["objectives"][0].clone();
        invalid["key"] = serde_json::json!("unknown-role-probe");
        invalid["role"] = serde_json::json!("banana");
        value["opportunity"]["objectives"]
            .as_array_mut()
            .unwrap()
            .push(invalid);
        let error = validate_project_improvement_brief(&serde_json::to_vec(&value).unwrap())
            .expect_err("unknown objective role must fail before validation");
        assert!(
            format!("{error:#}").contains("unknown variant"),
            "{error:#}"
        );
    }

    #[test]
    fn compiler_refuses_dirty_or_unresolved_brief_before_draft() {
        let brief_bytes = include_bytes!(
            "../../../docs/mise_profiles/example-project.runtime-readiness.brief.json"
        );
        let approval = BriefApproval {
            schema: BRIEF_APPROVAL_SCHEMA_V1.to_owned(),
            brief_sha256: sha256(brief_bytes),
            approved_by: "operator".to_owned(),
            approved_at: "2026-08-03T00:00:00Z".to_owned(),
            decision: "compile_draft".to_owned(),
        };
        let error = compile_project_improvement_brief(
            brief_bytes,
            &serde_json::to_vec(&approval).expect("approval JSON"),
        )
        .expect_err("dirty brief must refuse");
        assert!(
            error
                .to_string()
                .contains("dirty observed project revision")
        );
    }

    fn compilable_brief() -> ProjectImprovementBrief {
        let mut brief = validate_project_improvement_brief(include_bytes!(
            "../../../docs/mise_profiles/example-project.runtime-readiness.brief.json"
        ))
        .expect("example brief");
        brief.project.worktree_dirty = false;
        brief.project.observed_revision = "a".repeat(40);
        brief
            .opportunity
            .fixtures
            .retain(|fixture| fixture.disclosure != FixtureDisclosure::Unavailable);
        brief
            .opportunity
            .environment_requirements
            .retain(|requirement| {
                requirement.evidence == EvidenceFreshness::Live && requirement.locator.is_some()
            });
        brief
    }

    fn compile_error(brief: ProjectImprovementBrief) -> String {
        let brief_bytes = serde_json::to_vec(&brief).expect("brief JSON");
        let approval = BriefApproval {
            schema: BRIEF_APPROVAL_SCHEMA_V1.to_owned(),
            brief_sha256: sha256(&brief_bytes),
            approved_by: "operator".to_owned(),
            approved_at: "2026-08-04T00:00:00Z".to_owned(),
            decision: "compile_draft".to_owned(),
        };
        format!(
            "{:#}",
            compile_project_improvement_brief(
                &brief_bytes,
                &serde_json::to_vec(&approval).expect("approval JSON")
            )
            .expect_err("invalid brief must not compile")
        )
    }

    #[test]
    fn compiler_refusal_inventory_is_executable_contract() {
        let mut brief = compilable_brief();
        brief.project.observed_revision = "abc123".to_owned();
        assert!(compile_error(brief).contains("full lowercase hexadecimal Git object ID"));

        let mut brief = compilable_brief();
        brief.opportunity.fixtures[0].disclosure = FixtureDisclosure::Unavailable;
        brief.opportunity.fixtures[0].limitation = Some("not retained".to_owned());
        assert!(compile_error(brief).contains("unavailable fixtures"));

        let mut brief = compilable_brief();
        brief.opportunity.environment_requirements[0].evidence = EvidenceFreshness::Inferred;
        assert!(compile_error(brief).contains("unresolved environment requirements"));

        let mut brief = compilable_brief();
        let primary = brief
            .opportunity
            .objectives
            .iter_mut()
            .find(|objective| objective.role == ObjectiveRole::Primary)
            .expect("primary objective");
        primary.unit = "boolean".to_owned();
        assert!(compile_error(brief).contains("must be quantitative"));

        let mut brief = compilable_brief();
        brief.opportunity.protected_countermetrics.clear();
        assert!(compile_error(brief).contains("protected_countermetrics"));

        let mut brief = compilable_brief();
        brief
            .opportunity
            .objectives
            .retain(|objective| objective.key != "correctness");
        assert!(compile_error(brief).contains("'correctness' hard constraint"));

        let mut brief = compilable_brief();
        brief.opportunity.budgets.clear();
        assert!(compile_error(brief).contains("positive typed budgets"));

        let mut brief = compilable_brief();
        brief.opportunity.stop_rules.maximum_candidate_analyses = 0;
        assert!(compile_error(brief).contains("finite positive stop rules"));
    }

    #[test]
    fn anti_goodhart_contract_refuses_missing_scope_or_named_constraints() {
        let bytes = include_bytes!(
            "../../../docs/mise_profiles/example-project.runtime-readiness.brief.json"
        );
        let mut brief = validate_project_improvement_brief(bytes).expect("example brief");
        brief.opportunity.inference_scope.clear();
        let error = validate_project_improvement_brief(
            &serde_json::to_vec(&brief).expect("serialize brief without scope"),
        )
        .expect_err("missing inference scope must fail");
        assert!(error.to_string().contains("inference_scope"));

        let mut brief = validate_project_improvement_brief(bytes).expect("example brief");
        brief
            .opportunity
            .objectives
            .retain(|objective| objective.key != "compatibility");
        let error = validate_project_improvement_brief(
            &serde_json::to_vec(&brief).expect("serialize brief without compatibility"),
        )
        .expect_err("missing compatibility constraint must fail");
        assert!(
            error
                .to_string()
                .contains("'compatibility' hard constraint")
        );
    }

    #[test]
    fn unresolved_placeholder_is_refused_before_compilation() {
        let mut brief = validate_project_improvement_brief(include_bytes!(
            "../../../docs/mise_profiles/example-project.runtime-readiness.brief.json"
        ))
        .expect("example brief");
        brief.opportunity.anti_goodhart.self_certification = "TBD".to_owned();
        let error = validate_project_improvement_brief(
            &serde_json::to_vec(&brief).expect("serialize placeholder brief"),
        )
        .expect_err("placeholder must fail");
        assert!(error.to_string().contains("unresolved placeholder"));
    }

    #[test]
    fn compiler_emits_non_admitted_draft_after_exact_approval() {
        let brief = compilable_brief();
        let brief_bytes = serde_json::to_vec(&brief).expect("brief JSON");
        let approval = BriefApproval {
            schema: BRIEF_APPROVAL_SCHEMA_V1.to_owned(),
            brief_sha256: sha256(&brief_bytes),
            approved_by: "operator".to_owned(),
            approved_at: "2026-08-03T00:00:00Z".to_owned(),
            decision: "compile_draft".to_owned(),
        };
        let draft = compile_project_improvement_brief(
            &brief_bytes,
            &serde_json::to_vec(&approval).expect("approval JSON"),
        )
        .expect("compile draft");
        assert_eq!(draft.authority, "non_admitted_draft");
        assert!(draft.campaign.admission_requires_explicit_command);
        assert!(!draft.campaign.inference_scope.is_empty());
        assert!(!draft.campaign.anti_goodhart.self_certification.is_empty());
        assert_eq!(draft.task_graph.len(), 4);
    }
}
