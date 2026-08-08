use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::admission::{VerifiedCampaignAdmission, verify_campaign_admission};
use crate::budget::BudgetLimit;
use crate::digest::{sha256, validate_sha256};
use crate::manifest::CampaignManifest;
use crate::object::{
    PreservedObject, object_locator, preserve_object, read_verified_json, verify_object,
};
use crate::promotion::{PromotionGateBinding, VerifiedPapertigerGate, verify_papertiger_gate};
use crate::state::EvidenceGrade;
use crate::store::AdmissionOutcome;

pub const PARENT_PROMOTION_PROOF_SCHEMA_V1: &str = "papertiger-mise.parent-promotion-proof.v1";
pub const PARENT_PROMOTION_PROOF_SCHEMA_V2: &str = "papertiger-mise.parent-promotion-proof.v2";
pub const SUCCESSOR_ADMISSION_SCOPE_V1: &str = "development_successor_admission_only";

/// Operator-reviewable proof that one exact nomination may become the parent
/// of one exact successor campaign. This is lineage authority only: it does
/// not authorize deployment and does not weaken sealed promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParentPromotionProof {
    pub schema: String,
    pub scope: String,
    pub parent_campaign_id: String,
    pub parent_manifest_sha256: String,
    pub parent_runtime_generation: u32,
    pub parent_recursion_depth: u32,
    pub parent_nomination_id: String,
    pub parent_nomination_sha256: String,
    pub promoted_candidate_id: String,
    pub promoted_candidate_result_sha256: String,
    pub promoted_materialization_receipt_sha256: String,
    pub promoted_source_tree: String,
    pub parent_evidence_grade: EvidenceGrade,
    pub relied_upon_trial_ids: Vec<String>,
    pub relied_upon_paired_cohort_ids: Vec<String>,
    pub successor_campaign_id: String,
    pub successor_manifest_sha256: String,
    pub successor_runtime_generation: u32,
    pub successor_recursion_depth: u32,
    pub successor_outer_judge_executable_locator: String,
    pub successor_outer_judge_executable_sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promoted_judge_build_trial_receipts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promoted_judge_executable_sha256: Option<String>,
    pub successor_budget_debit: Vec<BudgetLimit>,
}

impl ParentPromotionProof {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut canonical = self.clone();
        canonical
            .successor_budget_debit
            .sort_by_key(|limit| limit.resource);
        canonical.relied_upon_trial_ids.sort();
        canonical.relied_upon_paired_cohort_ids.sort();
        canonical.promoted_judge_build_trial_receipts.sort();
        Ok(serde_json::to_vec(&canonical)?)
    }

    pub fn sha256(&self) -> Result<String> {
        Ok(sha256(&self.canonical_bytes()?))
    }

    pub fn evidence_locator(&self) -> Result<String> {
        Ok(format!(
            "papertiger-mise:parent-promotion-proof/{}",
            self.sha256()?
        ))
    }

    fn validate(&self) -> Result<()> {
        if !matches!(
            self.schema.as_str(),
            PARENT_PROMOTION_PROOF_SCHEMA_V1 | PARENT_PROMOTION_PROOF_SCHEMA_V2
        ) {
            bail!(
                "unsupported parent promotion proof schema '{}' (expected '{PARENT_PROMOTION_PROOF_SCHEMA_V1}' or '{PARENT_PROMOTION_PROOF_SCHEMA_V2}')",
                self.schema
            );
        }
        if self.scope != SUCCESSOR_ADMISSION_SCOPE_V1 {
            bail!("parent promotion proof is not scoped to successor admission only");
        }
        for (field, digest) in [
            ("parent_manifest_sha256", &self.parent_manifest_sha256),
            ("parent_nomination_id", &self.parent_nomination_id),
            ("parent_nomination_sha256", &self.parent_nomination_sha256),
            (
                "promoted_candidate_result_sha256",
                &self.promoted_candidate_result_sha256,
            ),
            (
                "promoted_materialization_receipt_sha256",
                &self.promoted_materialization_receipt_sha256,
            ),
            ("successor_manifest_sha256", &self.successor_manifest_sha256),
            (
                "successor_outer_judge_executable_sha256",
                &self.successor_outer_judge_executable_sha256,
            ),
        ] {
            validate_sha256(digest, field)?;
        }
        match self.schema.as_str() {
            PARENT_PROMOTION_PROOF_SCHEMA_V1 => {
                if !self.promoted_judge_build_trial_receipts.is_empty()
                    || self.promoted_judge_executable_sha256.is_some()
                {
                    bail!("historical v1 parent proof cannot claim a judge build receipt");
                }
            }
            PARENT_PROMOTION_PROOF_SCHEMA_V2 => {
                if self.promoted_judge_build_trial_receipts.is_empty() {
                    bail!("v2 parent proof requires at least one judge-build trial receipt");
                }
                let mut unique = BTreeSet::new();
                for receipt in &self.promoted_judge_build_trial_receipts {
                    validate_sha256(receipt, "promoted_judge_build_trial_receipt")?;
                    if !unique.insert(receipt) {
                        bail!("v2 parent proof repeats a judge-build trial receipt");
                    }
                }
                let executable = self
                    .promoted_judge_executable_sha256
                    .as_deref()
                    .context("v2 parent proof omitted the promoted judge executable SHA-256")?;
                validate_sha256(executable, "promoted_judge_executable_sha256")?;
                if executable != self.successor_outer_judge_executable_sha256 {
                    bail!(
                        "successor outer judge differs from the parent-produced judge executable"
                    );
                }
            }
            _ => unreachable!("schema checked above"),
        }
        if self.parent_nomination_id != self.parent_nomination_sha256 {
            bail!("parent nomination ID must equal its content-addressed receipt SHA-256");
        }
        if self.parent_recursion_depth.checked_add(1) != Some(self.successor_recursion_depth) {
            bail!("successor recursion depth must be exactly one after its parent");
        }
        if self.parent_runtime_generation.checked_add(1) != Some(self.successor_runtime_generation)
        {
            bail!("successor runtime generation must be exactly one after its parent");
        }
        if self.successor_budget_debit.is_empty() {
            bail!("successor promotion proof must debit a nonempty budget envelope");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreservedParentPromotionProof {
    pub proof: ParentPromotionProof,
    pub object: PreservedObject,
    pub evidence_locator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedParentPromotionGate {
    pub parent_nomination_id: String,
    pub parent_campaign_id: String,
    pub promoted_candidate_id: String,
    pub successor_campaign_id: String,
    pub parent_promotion_proof_sha256: String,
    pub plan_slug: String,
    pub task_seq: i64,
    pub task_title: String,
    pub task_status: String,
    pub gate_name: String,
    pub evidence_locator: String,
    pub evidence_sha256: String,
    pub closed_at: String,
}

#[derive(Debug)]
pub struct VerifiedSuccessorAdmission {
    campaign: VerifiedCampaignAdmission,
    proof: ParentPromotionProof,
    proof_object: PreservedObject,
    gate: VerifiedParentPromotionGate,
    object_root: PathBuf,
    papertiger_database: PathBuf,
    gate_binding: PromotionGateBinding,
}

impl VerifiedSuccessorAdmission {
    pub fn campaign_id(&self) -> &str {
        self.campaign.campaign_id()
    }

    pub fn manifest_sha256(&self) -> &str {
        self.campaign.manifest_sha256()
    }

    pub fn proof(&self) -> &ParentPromotionProof {
        &self.proof
    }

    pub fn proof_object(&self) -> &PreservedObject {
        &self.proof_object
    }

    pub fn gate(&self) -> &VerifiedParentPromotionGate {
        &self.gate
    }
}

pub fn derive_parent_promotion_proof(
    connection: &Connection,
    object_root: &Path,
    parent_nomination_id: &str,
    successor_manifest: &CampaignManifest,
) -> Result<ParentPromotionProof> {
    if parent_nomination_id.trim().is_empty() {
        bail!("parent nomination ID must be nonblank");
    }
    if let Some(existing) = reopen_existing_parent_proof(
        connection,
        object_root,
        parent_nomination_id,
        successor_manifest,
    )? {
        return Ok(existing);
    }
    successor_manifest.validate()?;
    let parent_binding = successor_manifest
        .generation
        .parent_campaign
        .as_ref()
        .context("successor campaign must bind an exact parent campaign")?;
    let verified = crate::lifecycle::verify_nomination_integrity(
        connection,
        object_root,
        parent_nomination_id,
    )
    .context("rederive parent nomination")?;
    if verified.nomination.campaign_id != parent_binding.campaign_id
        || verified.manifest_sha256 != parent_binding.manifest_sha256.0
        || verified.manifest.generation.runtime_generation != parent_binding.runtime_generation
        || verified.manifest.generation.recursion_depth != parent_binding.recursion_depth
    {
        bail!("successor parent binding differs from the exact re-derived nomination campaign");
    }
    if verified.manifest.generation.maximum_recursion_depth
        != successor_manifest.generation.maximum_recursion_depth
    {
        bail!("successor must preserve its root campaign's maximum recursion depth");
    }
    if verified.manifest.source.repository_id != successor_manifest.source.repository_id {
        bail!("successor source repository_id must match its parent campaign");
    }
    let materialization =
        crate::lifecycle::materialization(connection, &verified.nomination.candidate_id)?
            .context("nominated parent candidate has no durable materialization")?;
    if successor_manifest.source.base_tree != materialization.result_tree {
        bail!(
            "successor source tree differs from the nominated parent materialization; integrate the exact nominated tree before authoring the successor manifest"
        );
    }
    if verified.candidate_trial_ids.is_empty() {
        bail!(
            "parent nomination has no deterministic candidate trial that can carry a judge build receipt; run a development campaign with evaluator.judge_build"
        );
    }
    let mut promoted_judge_build_trial_receipts = Vec::new();
    let mut promoted_judge_executable_sha256 = None;
    for trial_id in &verified.candidate_trial_ids {
        let (trial_receipt_object, trial_receipt) = crate::lifecycle::verified_trial_receipt(
            connection,
            object_root,
            trial_id,
            &verified.manifest,
        )?;
        let build = trial_receipt.judge_build.as_ref().with_context(|| {
            format!(
                "parent candidate trial '{trial_id}' has no supervisor-verified judge build; run a new parent campaign with evaluator.judge_build"
            )
        })?;
        if build.executable.sha256
            != successor_manifest
                .generation
                .outer_judge_executable_sha256
                .0
        {
            bail!(
                "successor outer judge differs from parent candidate trial '{trial_id}' build {}; use the exact CAS-preserved parent-built executable",
                build.executable.sha256
            );
        }
        if promoted_judge_executable_sha256
            .as_ref()
            .is_some_and(|sha256| sha256 != &build.executable.sha256)
        {
            bail!("parent candidate trials produced divergent judge executable identities");
        }
        promoted_judge_executable_sha256 = Some(build.executable.sha256.clone());
        promoted_judge_build_trial_receipts.push(trial_receipt_object.sha256);
    }
    let manifest_sha256 = successor_manifest.sha256()?;
    let mut successor_budget_debit = successor_manifest.budgets.caps.clone();
    successor_budget_debit.sort_by_key(|limit| limit.resource);
    let mut relied_upon_trial_ids = verified.relied_upon_trial_ids;
    relied_upon_trial_ids.sort();
    let mut relied_upon_paired_cohort_ids = verified.relied_upon_paired_cohort_ids;
    relied_upon_paired_cohort_ids.sort();
    let proof = ParentPromotionProof {
        schema: PARENT_PROMOTION_PROOF_SCHEMA_V2.to_owned(),
        scope: SUCCESSOR_ADMISSION_SCOPE_V1.to_owned(),
        parent_campaign_id: verified.nomination.campaign_id,
        parent_manifest_sha256: verified.manifest_sha256,
        parent_runtime_generation: verified.manifest.generation.runtime_generation,
        parent_recursion_depth: verified.manifest.generation.recursion_depth,
        parent_nomination_id: parent_nomination_id.to_owned(),
        parent_nomination_sha256: verified.nomination.receipt_sha256,
        promoted_candidate_id: verified.nomination.candidate_id,
        promoted_candidate_result_sha256: sha256(&serde_json::to_vec(&verified.candidate_result)?),
        promoted_materialization_receipt_sha256: materialization.receipt_sha256,
        promoted_source_tree: materialization.result_tree,
        parent_evidence_grade: verified.evidence_grade,
        relied_upon_trial_ids,
        relied_upon_paired_cohort_ids,
        successor_campaign_id: successor_manifest.campaign_id.clone(),
        successor_manifest_sha256: manifest_sha256,
        successor_runtime_generation: successor_manifest.generation.runtime_generation,
        successor_recursion_depth: successor_manifest.generation.recursion_depth,
        successor_outer_judge_executable_locator: successor_manifest
            .generation
            .outer_judge_executable_locator
            .clone(),
        successor_outer_judge_executable_sha256: successor_manifest
            .generation
            .outer_judge_executable_sha256
            .0
            .clone(),
        promoted_judge_build_trial_receipts,
        promoted_judge_executable_sha256,
        successor_budget_debit,
    };
    proof.validate()?;
    Ok(proof)
}

fn reopen_existing_parent_proof(
    connection: &Connection,
    object_root: &Path,
    parent_nomination_id: &str,
    successor_manifest: &CampaignManifest,
) -> Result<Option<ParentPromotionProof>> {
    let Some(record) =
        crate::store::successor_admission(connection, &successor_manifest.campaign_id)?
    else {
        return Ok(None);
    };
    let successor_manifest_sha256 = sha256(&successor_manifest.historical_canonical_bytes()?);
    if record.parent_nomination_id != parent_nomination_id
        || record.successor_manifest_sha256 != successor_manifest_sha256
    {
        bail!("already-admitted successor differs from the requested immutable parent or manifest");
    }
    let object = crate::object::indexed_object(connection, &record.parent_promotion_proof_sha256)?;
    let (proof, _): (ParentPromotionProof, Vec<u8>) =
        read_verified_json(object_root, &object, "parent promotion proof")?;
    if proof.sha256()? != record.parent_promotion_proof_sha256
        || proof.parent_nomination_id != parent_nomination_id
        || proof.successor_campaign_id != successor_manifest.campaign_id
        || proof.successor_manifest_sha256 != successor_manifest_sha256
    {
        bail!("admitted successor parent proof does not reopen to its exact immutable identity");
    }
    let verified = crate::lifecycle::verify_nomination_integrity(
        connection,
        object_root,
        parent_nomination_id,
    )
    .context("reopen admitted historical parent nomination")?;
    if verified.nomination.campaign_id != proof.parent_campaign_id
        || verified.nomination.candidate_id != proof.promoted_candidate_id
        || verified.nomination.receipt_sha256 != proof.parent_nomination_sha256
    {
        bail!("historical parent proof differs from the re-derived parent nomination");
    }
    Ok(Some(proof))
}

pub fn preserve_parent_promotion_proof(
    connection: &Connection,
    object_root: &Path,
    parent_nomination_id: &str,
    successor_manifest: &CampaignManifest,
) -> Result<PreservedParentPromotionProof> {
    let proof = derive_parent_promotion_proof(
        connection,
        object_root,
        parent_nomination_id,
        successor_manifest,
    )?;
    let bytes = proof.canonical_bytes()?;
    let object = preserve_object(object_root, &bytes)?;
    Ok(PreservedParentPromotionProof {
        evidence_locator: proof.evidence_locator()?,
        proof,
        object,
    })
}

pub fn verify_parent_promotion_gate(
    connection: &Connection,
    object_root: &Path,
    papertiger_database: &Path,
    parent_nomination_id: &str,
    successor_manifest: &CampaignManifest,
    binding: &PromotionGateBinding,
) -> Result<(
    ParentPromotionProof,
    PreservedObject,
    VerifiedParentPromotionGate,
)> {
    let proof = derive_parent_promotion_proof(
        connection,
        object_root,
        parent_nomination_id,
        successor_manifest,
    )?;
    let bytes = proof.canonical_bytes()?;
    let proof_sha256 = proof.sha256()?;
    let proof_object = PreservedObject {
        sha256: proof_sha256.clone(),
        bytes: u64::try_from(bytes.len()).context("parent promotion proof is too large")?,
        locator: object_locator(&proof_sha256)?,
    };
    verify_object(object_root, &proof_object)
        .with_context(|| {
            format!(
                "parent promotion proof is not retained in the Mise object store; run `papertiger-mise promotion derive-parent --nomination {parent_nomination_id} --successor-manifest <manifest.json> --objects {}`",
                object_root.display()
            )
        })?;
    let gate = verify_papertiger_gate(
        papertiger_database,
        binding,
        &proof.evidence_locator()?,
        &proof_object.sha256,
        "parent promotion proof",
    )?;
    let verified_gate = verified_gate(parent_nomination_id, &proof, gate)?;
    Ok((proof, proof_object, verified_gate))
}

pub fn verify_successor_admission(
    connection: &Connection,
    manifest_path: impl AsRef<Path>,
    object_root: impl AsRef<Path>,
    papertiger_database: impl AsRef<Path>,
    parent_nomination_id: &str,
    binding: &PromotionGateBinding,
) -> Result<VerifiedSuccessorAdmission> {
    let campaign = verify_campaign_admission(manifest_path)?;
    if campaign.manifest().generation.parent_campaign.is_none() {
        bail!("campaign is a root; use `papertiger-mise campaign admit <manifest>`");
    }
    let (proof, proof_object, gate) = verify_parent_promotion_gate(
        connection,
        object_root.as_ref(),
        papertiger_database.as_ref(),
        parent_nomination_id,
        campaign.manifest(),
        binding,
    )?;
    Ok(VerifiedSuccessorAdmission {
        campaign,
        proof,
        proof_object,
        gate,
        object_root: object_root.as_ref().to_path_buf(),
        papertiger_database: papertiger_database.as_ref().to_path_buf(),
        gate_binding: binding.clone(),
    })
}

pub fn admit_verified_successor(
    connection: &Connection,
    actor: &str,
    verified: &VerifiedSuccessorAdmission,
) -> Result<AdmissionOutcome> {
    let current = verify_campaign_admission(verified.campaign.manifest_path())?;
    if current.campaign_id() != verified.campaign_id()
        || current.manifest_sha256() != verified.manifest_sha256()
    {
        bail!("verified successor admission became stale before the database mutation");
    }
    let (proof, proof_object, gate) = verify_parent_promotion_gate(
        connection,
        &verified.object_root,
        &verified.papertiger_database,
        &verified.proof.parent_nomination_id,
        current.manifest(),
        &verified.gate_binding,
    )?;
    if proof != verified.proof || proof_object != verified.proof_object || gate != verified.gate {
        bail!("verified successor authority became stale before the database mutation");
    }
    crate::store::admit_successor_campaign(
        connection,
        actor,
        current.admission(),
        &proof,
        &proof_object,
        &gate,
    )
}

fn verified_gate(
    parent_nomination_id: &str,
    proof: &ParentPromotionProof,
    gate: VerifiedPapertigerGate,
) -> Result<VerifiedParentPromotionGate> {
    Ok(VerifiedParentPromotionGate {
        parent_nomination_id: parent_nomination_id.to_owned(),
        parent_campaign_id: proof.parent_campaign_id.clone(),
        promoted_candidate_id: proof.promoted_candidate_id.clone(),
        successor_campaign_id: proof.successor_campaign_id.clone(),
        parent_promotion_proof_sha256: proof.sha256()?,
        plan_slug: gate.plan_slug,
        task_seq: gate.task_seq,
        task_title: gate.task_title,
        task_status: gate.task_status,
        gate_name: gate.gate_name,
        evidence_locator: gate.evidence_locator,
        evidence_sha256: gate.evidence_sha256,
        closed_at: gate.closed_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::BudgetResource;

    fn proof_with_grade(grade: EvidenceGrade) -> ParentPromotionProof {
        ParentPromotionProof {
            schema: PARENT_PROMOTION_PROOF_SCHEMA_V1.to_owned(),
            scope: SUCCESSOR_ADMISSION_SCOPE_V1.to_owned(),
            parent_campaign_id: "parent-campaign".to_owned(),
            parent_manifest_sha256: "1".repeat(64),
            parent_runtime_generation: 1,
            parent_recursion_depth: 0,
            parent_nomination_id: "2".repeat(64),
            parent_nomination_sha256: "2".repeat(64),
            promoted_candidate_id: "3".repeat(64),
            promoted_candidate_result_sha256: "4".repeat(64),
            promoted_materialization_receipt_sha256: "5".repeat(64),
            promoted_source_tree: "6".repeat(40),
            parent_evidence_grade: grade,
            relied_upon_trial_ids: vec!["parent-trial".to_owned()],
            relied_upon_paired_cohort_ids: Vec::new(),
            successor_campaign_id: "successor-campaign".to_owned(),
            successor_manifest_sha256: "7".repeat(64),
            successor_runtime_generation: 2,
            successor_recursion_depth: 1,
            successor_outer_judge_executable_locator: "C:/mise.exe".to_owned(),
            successor_outer_judge_executable_sha256: "8".repeat(64),
            promoted_judge_build_trial_receipts: Vec::new(),
            promoted_judge_executable_sha256: None,
            successor_budget_debit: vec![BudgetLimit {
                resource: BudgetResource::Trials,
                hard_limit: 1,
            }],
        }
    }

    #[test]
    fn parent_evidence_grade_is_an_explicit_allowlist() {
        proof_with_grade(EvidenceGrade::DeterministicDevelopment)
            .validate()
            .expect("deterministic development grade");
        proof_with_grade(EvidenceGrade::WorkspaceOnlyDevelopment)
            .validate()
            .expect("paired development grade");
        let mut value =
            serde_json::to_value(proof_with_grade(EvidenceGrade::DeterministicDevelopment))
                .unwrap();
        value["parent_evidence_grade"] = serde_json::json!("future_implicit_grade");
        let error = serde_json::from_value::<ParentPromotionProof>(value)
            .expect_err("unknown grade must fail closed");
        assert!(error.to_string().contains("unknown variant"), "{error:#}");
    }
}
