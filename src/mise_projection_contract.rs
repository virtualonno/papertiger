use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{sha256, validate_sha256};

pub const MISE_PLANNER_PROJECTION_SCHEMA_V1: &str = "papertiger.mise-planner-projection.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiseSourceProjection {
    pub repository_id: String,
    pub base_commit: String,
    pub base_tree: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiseMutationProjection {
    pub allowlist: Vec<String>,
    pub protected_paths: Vec<String>,
    pub changed_paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiseProjectionDisposition {
    Nominated,
    Rejected,
    Inconclusive,
    InfrastructureFailed,
}

impl MiseProjectionDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nominated => "nominated",
            Self::Rejected => "rejected",
            Self::Inconclusive => "inconclusive",
            Self::InfrastructureFailed => "infrastructure_failed",
        }
    }

    const fn required_limitation(self) -> &'static str {
        match self {
            Self::Nominated => "nomination-is-evidence-not-integration-or-promotion",
            Self::Rejected => "rejected-evidence-does-not-authorize-integration",
            Self::Inconclusive => "inconclusive-evidence-does-not-authorize-integration",
            Self::InfrastructureFailed => {
                "infrastructure-failure-is-diagnostic-evidence-not-candidate-judgment"
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiseBudgetProjection {
    pub resource: String,
    pub unit: String,
    pub hard_limit: u64,
    pub reserved_amount: u64,
    pub spent_amount: u64,
    pub available_amount: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MisePlannerProjection {
    pub schema: String,
    pub campaign_id: String,
    pub manifest_sha256: String,
    pub candidate_id: String,
    pub nomination_id: Option<String>,
    pub source: MiseSourceProjection,
    pub mutation: MiseMutationProjection,
    pub disposition: MiseProjectionDisposition,
    pub evidence_grade: Option<String>,
    pub candidate_material_sha256: String,
    /// Exact compact canonical bytes, retained as text because the supported
    /// candidate material contract is canonical JSON.
    pub candidate_material_json: String,
    pub result: Value,
    pub relied_upon_evidence_ids: Vec<String>,
    pub limitations: Vec<String>,
    pub budgets: Vec<MiseBudgetProjection>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MisePlannerProjectionSummary {
    pub campaign_id: String,
    pub manifest_sha256: String,
    pub candidate_id: String,
    pub nomination_id: Option<String>,
    pub source: MiseSourceProjection,
    pub mutation: MiseMutationProjection,
    pub disposition: MiseProjectionDisposition,
    pub evidence_grade: Option<String>,
    pub candidate_material_sha256: String,
    pub limitations: Vec<String>,
    pub budgets: Vec<MiseBudgetProjection>,
}

impl MisePlannerProjection {
    pub fn validate(&self) -> Result<()> {
        if self.schema != MISE_PLANNER_PROJECTION_SCHEMA_V1 {
            bail!("Mise planner projection schema must be {MISE_PLANNER_PROJECTION_SCHEMA_V1}");
        }
        for (name, value) in [
            ("campaign_id", self.campaign_id.as_str()),
            ("source.repository_id", self.source.repository_id.as_str()),
            ("source.base_commit", self.source.base_commit.as_str()),
            ("source.base_tree", self.source.base_tree.as_str()),
        ] {
            require_nonblank(name, value)?;
        }
        validate_sha256(&self.manifest_sha256, "projection manifest_sha256")?;
        validate_sha256(&self.candidate_id, "projection candidate_id")?;
        validate_sha256(
            &self.candidate_material_sha256,
            "projection candidate_material_sha256",
        )?;
        if self.disposition == MiseProjectionDisposition::Nominated {
            validate_sha256(
                self.nomination_id
                    .as_deref()
                    .context("nominated projection requires nomination_id")?,
                "projection nomination_id",
            )?;
            require_nonblank(
                "projection evidence_grade",
                self.evidence_grade
                    .as_deref()
                    .context("nominated projection requires evidence_grade")?,
            )?;
        } else if self.nomination_id.is_some() {
            bail!("only a nominated projection may carry nomination_id");
        }
        if !self.result.is_object() {
            bail!("Mise planner projection result must be a JSON object");
        }

        validate_sorted_paths("mutation.allowlist", &self.mutation.allowlist, false)?;
        validate_sorted_paths(
            "mutation.protected_paths",
            &self.mutation.protected_paths,
            true,
        )?;
        validate_sorted_paths("mutation.changed_paths", &self.mutation.changed_paths, true)?;
        for path in &self.mutation.changed_paths {
            if !self
                .mutation
                .allowlist
                .iter()
                .any(|allowed| path_matches_scope(path, allowed))
            {
                bail!("projected changed path '{path}' is outside the admitted mutation allowlist");
            }
            if self
                .mutation
                .protected_paths
                .iter()
                .any(|protected| path_matches_scope(path, protected))
            {
                bail!("projected changed path '{path}' intersects an admitted protected path");
            }
        }

        let material_bytes = self.candidate_material_json.as_bytes();
        if sha256(material_bytes) != self.candidate_material_sha256 {
            bail!("projected candidate material bytes differ from candidate_material_sha256");
        }
        let material: Value = serde_json::from_slice(material_bytes)
            .context("projected candidate material is not JSON")?;
        if material.get("schema").and_then(Value::as_str)
            != Some("papertiger-mise.candidate-material.v1")
        {
            bail!("projected candidate material has an unsupported schema");
        }
        let material_paths = material
            .pointer("/scope/changed_paths")
            .and_then(Value::as_array)
            .context("projected candidate material lacks scope.changed_paths")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .context("candidate material changed path is not a string")
            })
            .collect::<Result<Vec<_>>>()?;
        if material_paths != self.mutation.changed_paths {
            bail!("projected changed paths differ from the exact candidate material scope");
        }

        validate_sorted_nonblank(
            "relied_upon_evidence_ids",
            &self.relied_upon_evidence_ids,
            false,
        )?;
        validate_sorted_nonblank("limitations", &self.limitations, false)?;
        if !self
            .limitations
            .iter()
            .any(|value| value == self.disposition.required_limitation())
        {
            bail!(
                "projection disposition '{}' requires limitation '{}'",
                self.disposition.as_str(),
                self.disposition.required_limitation()
            );
        }
        if self.budgets.is_empty() {
            bail!("Mise planner projection requires consumed budget balances");
        }
        let mut resources = BTreeSet::new();
        for budget in &self.budgets {
            require_nonblank("projection budget resource", &budget.resource)?;
            require_nonblank("projection budget unit", &budget.unit)?;
            if !resources.insert(budget.resource.as_str()) {
                bail!("projection repeats budget resource '{}'", budget.resource);
            }
            let accounted = budget
                .reserved_amount
                .checked_add(budget.spent_amount)
                .and_then(|amount| amount.checked_add(budget.available_amount))
                .context("projection budget accounting overflow")?;
            if accounted != budget.hard_limit {
                bail!(
                    "projection budget '{}' does not account exactly to its hard limit",
                    budget.resource
                );
            }
        }
        if self
            .budgets
            .windows(2)
            .any(|pair| pair[0].resource >= pair[1].resource)
        {
            bail!("projection budgets must be uniquely sorted by resource");
        }
        Ok(())
    }

    pub fn projection_sha256(&self) -> Result<String> {
        self.validate()?;
        Ok(sha256(&serde_json::to_vec(self)?))
    }

    pub fn summary(&self) -> MisePlannerProjectionSummary {
        MisePlannerProjectionSummary {
            campaign_id: self.campaign_id.clone(),
            manifest_sha256: self.manifest_sha256.clone(),
            candidate_id: self.candidate_id.clone(),
            nomination_id: self.nomination_id.clone(),
            source: self.source.clone(),
            mutation: self.mutation.clone(),
            disposition: self.disposition,
            evidence_grade: self.evidence_grade.clone(),
            candidate_material_sha256: self.candidate_material_sha256.clone(),
            limitations: self.limitations.clone(),
            budgets: self.budgets.clone(),
        }
    }
}

fn require_nonblank(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{name} must be nonblank");
    }
    Ok(())
}

fn validate_sorted_paths(name: &str, paths: &[String], empty_allowed: bool) -> Result<()> {
    if !empty_allowed && paths.is_empty() {
        bail!("{name} must be nonempty");
    }
    for path in paths {
        validate_portable_relative_path(name, path)?;
    }
    if paths.windows(2).any(|pair| pair[0] >= pair[1]) {
        bail!("{name} must be uniquely sorted");
    }
    Ok(())
}

fn validate_portable_relative_path(name: &str, path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|component| {
            component.is_empty() || component == "." || component == ".." || component.contains(':')
        })
    {
        bail!("{name} contains nonportable relative path '{path}'");
    }
    Ok(())
}

fn path_matches_scope(path: &str, scope: &str) -> bool {
    path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn validate_sorted_nonblank(name: &str, values: &[String], empty_allowed: bool) -> Result<()> {
    if !empty_allowed && values.is_empty() {
        bail!("projection {name} must be nonempty");
    }
    if values.iter().any(|value| value.trim().is_empty()) {
        bail!("projection {name} contains a blank value");
    }
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        bail!("projection {name} must be uniquely sorted");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> MisePlannerProjection {
        let material = r#"{"schema":"papertiger-mise.candidate-material.v1","kind":"git_change_set","protocol":"papertiger-mise.git-change-set.v1","media_type":"application/vnd.papertiger-mise.git-change-set+json","payload_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","scope":{"changed_paths":["src/lib.rs"],"operations":["modify"]},"change_set":{"schema":"papertiger-mise.git-change-set.v1","changes":[]}}"#;
        MisePlannerProjection {
            schema: MISE_PLANNER_PROJECTION_SCHEMA_V1.to_owned(),
            campaign_id: "subject-objective-a01".to_owned(),
            manifest_sha256: "1".repeat(64),
            candidate_id: "2".repeat(64),
            nomination_id: Some("3".repeat(64)),
            source: MiseSourceProjection {
                repository_id: "fixture".to_owned(),
                base_commit: "abc".to_owned(),
                base_tree: "def".to_owned(),
            },
            mutation: MiseMutationProjection {
                allowlist: vec!["src".to_owned()],
                protected_paths: vec!["tests".to_owned()],
                changed_paths: vec!["src/lib.rs".to_owned()],
            },
            disposition: MiseProjectionDisposition::Nominated,
            evidence_grade: Some("deterministic_development".to_owned()),
            candidate_material_sha256: sha256(material.as_bytes()),
            candidate_material_json: material.to_owned(),
            result: serde_json::json!({"schema": "fixture.result.v1"}),
            relied_upon_evidence_ids: vec!["3".repeat(64)],
            limitations: vec![
                "deterministic-development-evidence-is-not-deployment-authority".to_owned(),
                "nomination-is-evidence-not-integration-or-promotion".to_owned(),
            ],
            budgets: vec![MiseBudgetProjection {
                resource: "trials".to_owned(),
                unit: "count".to_owned(),
                hard_limit: 5,
                reserved_amount: 0,
                spent_amount: 4,
                available_amount: 1,
            }],
        }
    }

    #[test]
    fn exact_projection_identity_excludes_mutable_planner_identity() {
        let projection = fixture();
        projection.validate().expect("valid projection");
        assert_eq!(projection.projection_sha256().unwrap().len(), 64);
        let encoded = serde_json::to_vec(&projection).unwrap();
        assert!(!String::from_utf8(encoded).unwrap().contains("task"));
    }

    #[test]
    fn projection_refuses_scope_budget_and_authority_drift() {
        let mut projection = fixture();
        projection.mutation.changed_paths = vec!["tests/hidden.rs".to_owned()];
        assert!(
            projection
                .validate()
                .unwrap_err()
                .to_string()
                .contains("outside the admitted mutation allowlist")
        );

        projection = fixture();
        projection.budgets[0].spent_amount = 5;
        assert!(
            projection
                .validate()
                .unwrap_err()
                .to_string()
                .contains("does not account exactly")
        );

        projection = fixture();
        projection.limitations.clear();
        assert!(projection.validate().is_err());
    }
}
