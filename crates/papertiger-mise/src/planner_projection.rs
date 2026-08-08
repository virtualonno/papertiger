use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use papertiger::{
    MISE_PLANNER_PROJECTION_SCHEMA_V1, MiseBudgetProjection, MiseMutationProjection,
    MisePlannerProjection, MiseProjectionDisposition, MiseSourceProjection,
};
use rusqlite::Connection;
use serde_json::Value;

use crate::budget::budget_balances;
use crate::candidate::CandidateDisposition;
use crate::lifecycle::{
    VerifiedCandidateEvidence, verify_candidate_integrity, verify_nomination_integrity,
};
use crate::manifest::ContainmentGrade;

pub fn derive_nomination_planner_projection(
    connection: &Connection,
    object_root: &Path,
    nomination_id: &str,
) -> Result<MisePlannerProjection> {
    let nomination = verify_nomination_integrity(connection, object_root, nomination_id)?;
    let candidate =
        verify_candidate_integrity(connection, object_root, &nomination.nomination.candidate_id)?;
    if candidate.manifest_sha256 != nomination.manifest_sha256
        || candidate.manifest.campaign_id != nomination.nomination.campaign_id
    {
        bail!("verified nomination and candidate disagree on their admitted campaign");
    }
    let mut relied_upon = nomination
        .relied_upon_trial_ids
        .iter()
        .chain(nomination.relied_upon_paired_cohort_ids.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    relied_upon.insert(nomination.nomination.receipt_sha256.clone());
    relied_upon.insert(candidate.manifest_sha256.clone());
    relied_upon.insert(candidate.candidate.material_sha256.clone());
    let limitations = [
        "deterministic-development-evidence-is-not-deployment-authority".to_owned(),
        "nomination-is-evidence-not-integration-or-promotion".to_owned(),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect();
    build_projection(
        connection,
        candidate,
        MiseProjectionDisposition::Nominated,
        Some(nomination.nomination.nomination_id),
        Some(nomination.evidence_grade.as_str().to_owned()),
        nomination.candidate_result,
        relied_upon.into_iter().collect(),
        limitations,
    )
}

pub fn derive_candidate_planner_projection(
    connection: &Connection,
    object_root: &Path,
    candidate_id: &str,
) -> Result<MisePlannerProjection> {
    let candidate = verify_candidate_integrity(connection, object_root, candidate_id)?;
    let disposition = match candidate.candidate.disposition {
        CandidateDisposition::Rejected => MiseProjectionDisposition::Rejected,
        CandidateDisposition::Inconclusive => MiseProjectionDisposition::Inconclusive,
        CandidateDisposition::InfrastructureFailed => {
            MiseProjectionDisposition::InfrastructureFailed
        }
        CandidateDisposition::Nominated => bail!(
            "nominated candidate evidence must be projected by nomination; run `papertiger-mise projection inspect --nomination <nomination-id>`"
        ),
        state => bail!(
            "candidate disposition '{state:?}' is not terminal projectable evidence; adjudicate or reconcile it first"
        ),
    };
    let result = candidate
        .candidate
        .result
        .clone()
        .context("terminal candidate has no durable result")?;
    let mut relied_upon = BTreeSet::from([
        candidate.manifest_sha256.clone(),
        candidate.candidate.material_sha256.clone(),
    ]);
    collect_result_evidence(&result, &mut relied_upon)?;
    let limitations = [disposition_limitation(disposition).to_owned()]
        .into_iter()
        .collect();
    build_projection(
        connection,
        candidate,
        disposition,
        None,
        None,
        result,
        relied_upon.into_iter().collect(),
        limitations,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_projection(
    connection: &Connection,
    verified: VerifiedCandidateEvidence,
    disposition: MiseProjectionDisposition,
    nomination_id: Option<String>,
    evidence_grade: Option<String>,
    result: Value,
    relied_upon_evidence_ids: Vec<String>,
    limitations: Vec<String>,
) -> Result<MisePlannerProjection> {
    let manifest = &verified.manifest;
    let budgets = budget_balances(connection, &manifest.campaign_id)?
        .into_iter()
        .map(|budget| MiseBudgetProjection {
            resource: budget.resource.as_str().to_owned(),
            unit: budget.unit,
            hard_limit: budget.hard_limit,
            reserved_amount: budget.reserved_amount,
            spent_amount: budget.spent_amount,
            available_amount: budget.available_amount,
        })
        .collect();
    let projection = MisePlannerProjection {
        schema: MISE_PLANNER_PROJECTION_SCHEMA_V1.to_owned(),
        campaign_id: manifest.campaign_id.clone(),
        manifest_sha256: verified.manifest_sha256,
        candidate_id: verified.candidate.candidate_id,
        nomination_id,
        source: MiseSourceProjection {
            repository_id: manifest.source.repository_id.clone(),
            base_commit: manifest.source.base_commit.clone(),
            base_tree: manifest.source.base_tree.clone(),
        },
        mutation: MiseMutationProjection {
            allowlist: manifest.mutation_scope.allowlist.clone(),
            protected_paths: manifest.mutation_scope.protected_paths.clone(),
            changed_paths: verified
                .candidate_material
                .scope
                .changed_paths
                .into_iter()
                .collect(),
        },
        disposition,
        evidence_grade,
        candidate_material_sha256: verified.candidate.material_sha256,
        candidate_material_json: verified.candidate_material_json,
        result,
        relied_upon_evidence_ids,
        limitations: with_containment_limitation(manifest.containment, limitations),
        budgets,
    };
    projection.validate()?;
    Ok(projection)
}

fn with_containment_limitation(
    containment: ContainmentGrade,
    limitations: Vec<String>,
) -> Vec<String> {
    limitations
        .into_iter()
        .chain(match containment {
            ContainmentGrade::WorkspaceOnly => {
                Some("workspace-only-containment-is-not-adversarial-isolation".to_owned())
            }
            ContainmentGrade::Sealed => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn disposition_limitation(disposition: MiseProjectionDisposition) -> &'static str {
    match disposition {
        MiseProjectionDisposition::Rejected => "rejected-evidence-does-not-authorize-integration",
        MiseProjectionDisposition::Inconclusive => {
            "inconclusive-evidence-does-not-authorize-integration"
        }
        MiseProjectionDisposition::InfrastructureFailed => {
            "infrastructure-failure-is-diagnostic-evidence-not-candidate-judgment"
        }
        MiseProjectionDisposition::Nominated => {
            "nomination-is-evidence-not-integration-or-promotion"
        }
    }
}

fn collect_result_evidence(result: &Value, evidence: &mut BTreeSet<String>) -> Result<()> {
    if let Some(values) = result
        .get("trial_evidence_sha256")
        .and_then(Value::as_array)
    {
        for value in values {
            evidence.insert(
                value
                    .as_str()
                    .context("trial_evidence_sha256 contains a non-string")?
                    .to_owned(),
            );
        }
    }
    if let Some(value) = result
        .pointer("/failure/evidence/sha256")
        .and_then(Value::as_str)
    {
        evidence.insert(value.to_owned());
    }
    Ok(())
}
