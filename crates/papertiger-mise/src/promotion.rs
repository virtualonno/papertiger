use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::attestation::TrustedContainmentPolicy;
use crate::digest::{sha256, validate_sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionGateBinding {
    pub task_seq: i64,
    pub gate_name: String,
    pub evidence_locator: String,
    pub evidence_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionEvidenceDigest {
    pub trial_id: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionProof {
    pub schema: String,
    pub nomination_sha256: String,
    pub manifest_sha256: String,
    pub campaign_id: String,
    pub candidate_id: String,
    pub candidate_result_sha256: String,
    pub trial_receipts: Vec<PromotionEvidenceDigest>,
    pub sealed_attestations: Vec<PromotionEvidenceDigest>,
    pub trusted_containment_policy_sha256: String,
}

impl PromotionProof {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    pub fn sha256(&self) -> Result<String> {
        Ok(sha256(&self.canonical_bytes()?))
    }

    pub fn evidence_locator(&self) -> Result<String> {
        Ok(format!(
            "papertiger-mise:promotion-proof/{}",
            self.sha256()?
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedPromotionGate {
    nomination_id: String,
    campaign_id: String,
    candidate_id: String,
    plan_slug: String,
    task_seq: i64,
    task_title: String,
    task_status: String,
    gate_name: String,
    evidence_locator: String,
    evidence_sha256: String,
    promotion_proof_sha256: String,
    trusted_containment_policy_sha256: String,
    closed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedPapertigerGate {
    pub plan_slug: String,
    pub task_seq: i64,
    pub task_title: String,
    pub task_status: String,
    pub gate_name: String,
    pub evidence_locator: String,
    pub evidence_sha256: String,
    pub closed_at: String,
}

/// Re-derive the complete promotion evidence without consulting Papertiger.
/// Once a redacted confirmation receipt protocol exists, operators can use
/// this deterministic proof to close the independent planning gate and
/// `verify_promotion_gate` can recompute it read-only. V1 confirmation
/// attestation currently fails closed before this function can return a proof.
pub fn derive_promotion_proof(
    mise_database: &Path,
    object_root: &Path,
    trusted_policy: &TrustedContainmentPolicy,
    nomination_id: &str,
) -> Result<PromotionProof> {
    if nomination_id.trim().is_empty() {
        bail!("promotion nomination_id must be nonblank");
    }
    trusted_policy.validate()?;
    let mise = crate::store::open_existing_read_only(mise_database)?;
    let verified =
        crate::lifecycle::verify_nomination_integrity(&mise, object_root, nomination_id)?;
    if verified.manifest.containment != crate::manifest::ContainmentGrade::Sealed
        || !verified
            .manifest
            .holdouts
            .tiers
            .iter()
            .any(|tier| tier.kind == crate::manifest::HoldoutTierKind::Confirmation)
    {
        bail!("promotion requires a sealed campaign with an explicit confirmation holdout");
    }
    if !verified.relied_upon_paired_cohort_ids.is_empty() {
        bail!(
            "sealed paired promotion remains unavailable until genuinely verdict-only cohort attestations bind every relied-upon paired execution"
        );
    }
    let mut trial_receipts = Vec::new();
    let mut sealed_attestations = Vec::new();
    for trial_id in &verified.relied_upon_trial_ids {
        let trial = crate::lifecycle::trial(&mise, trial_id)?
            .with_context(|| format!("relied-upon trial '{trial_id}' disappeared"))?;
        let receipt_sha256 = trial
            .outcome
            .as_ref()
            .and_then(|value| value.pointer("/receipt/sha256"))
            .and_then(serde_json::Value::as_str)
            .context("relied-upon trial has no receipt digest")?
            .to_owned();
        trial_receipts.push(PromotionEvidenceDigest {
            trial_id: trial_id.clone(),
            sha256: receipt_sha256,
        });
        sealed_attestations.push(PromotionEvidenceDigest {
            trial_id: trial_id.clone(),
            sha256: crate::attestation::require_sealed_attestation(
                &mise,
                object_root,
                trial_id,
                &verified.manifest,
                &verified.manifest_sha256,
                trusted_policy,
            )?,
        });
    }
    Ok(PromotionProof {
        schema: "papertiger-mise.promotion-proof.v1".to_owned(),
        nomination_sha256: verified.nomination.receipt_sha256,
        manifest_sha256: verified.manifest_sha256,
        campaign_id: verified.nomination.campaign_id,
        candidate_id: verified.nomination.candidate_id,
        candidate_result_sha256: sha256(&serde_json::to_vec(&verified.candidate_result)?),
        trial_receipts,
        sealed_attestations,
        trusted_containment_policy_sha256: trusted_policy.sha256()?,
    })
}

/// Verify the second key in Mise's promotion protocol without acquiring write
/// authority over Papertiger. This function only proves authorization; it does
/// not materialize a patch or mutate either repository.
pub fn verify_promotion_gate(
    mise_database: &Path,
    object_root: &Path,
    trusted_policy: &TrustedContainmentPolicy,
    papertiger_database: &Path,
    nomination_id: &str,
    binding: &PromotionGateBinding,
) -> Result<VerifiedPromotionGate> {
    let proof = derive_promotion_proof(mise_database, object_root, trusted_policy, nomination_id)?;
    let promotion_proof_sha256 = proof.sha256()?;
    let policy_sha256 = proof.trusted_containment_policy_sha256.clone();
    let exact_locator = proof.evidence_locator()?;
    let gate = verify_papertiger_gate(
        papertiger_database,
        binding,
        &exact_locator,
        &promotion_proof_sha256,
        "promotion proof",
    )?;

    Ok(VerifiedPromotionGate {
        nomination_id: nomination_id.to_owned(),
        campaign_id: proof.campaign_id,
        candidate_id: proof.candidate_id,
        plan_slug: gate.plan_slug,
        task_seq: gate.task_seq,
        task_title: gate.task_title,
        task_status: gate.task_status,
        gate_name: gate.gate_name,
        evidence_locator: gate.evidence_locator,
        evidence_sha256: gate.evidence_sha256,
        promotion_proof_sha256,
        trusted_containment_policy_sha256: policy_sha256,
        closed_at: gate.closed_at,
    })
}

pub(crate) fn verify_papertiger_gate(
    papertiger_database: &Path,
    binding: &PromotionGateBinding,
    expected_locator: &str,
    expected_sha256: &str,
    evidence_role: &str,
) -> Result<VerifiedPapertigerGate> {
    validate_binding(binding)?;
    validate_sha256(expected_sha256, evidence_role)?;
    if binding.evidence_locator != expected_locator || binding.evidence_sha256 != expected_sha256 {
        bail!("Papertiger binding does not identify the exact re-derived {evidence_role}");
    }
    let database = papertiger_database
        .to_str()
        .context("Papertiger database path is not valid UTF-8")?;
    let conn = papertiger::open_existing_read_only(database)?;
    let context = papertiger::task_context(&conn, binding.task_seq)?;
    if context.plan.status != "active" {
        bail!("Papertiger plan '{}' is not active", context.plan.slug);
    }
    if !matches!(context.task.status.as_str(), "in_progress" | "done") {
        bail!(
            "Papertiger task #{} is {}; promotion authorization requires in_progress or done",
            binding.task_seq,
            context.task.status
        );
    }
    let gate = context
        .gates
        .iter()
        .find(|gate| gate.name == binding.gate_name)
        .with_context(|| {
            format!(
                "Papertiger task #{} has no gate '{}'",
                binding.task_seq, binding.gate_name
            )
        })?;
    if gate.status != "closed"
        || gate.evidence_locator.as_deref() != Some(binding.evidence_locator.as_str())
        || gate.evidence_sha256.as_deref() != Some(binding.evidence_sha256.as_str())
    {
        bail!(
            "Papertiger gate '{}' on #{} does not exactly authorize this nomination evidence",
            binding.gate_name,
            binding.task_seq
        );
    }

    Ok(VerifiedPapertigerGate {
        plan_slug: context.plan.slug,
        task_seq: context.task.seq,
        task_title: context.task.title,
        task_status: context.task.status,
        gate_name: gate.name.clone(),
        evidence_locator: binding.evidence_locator.clone(),
        evidence_sha256: binding.evidence_sha256.clone(),
        closed_at: gate
            .closed_at
            .clone()
            .context("closed Papertiger gate has no closure timestamp")?,
    })
}

pub(crate) fn validate_binding(binding: &PromotionGateBinding) -> Result<()> {
    if binding.task_seq <= 0 {
        bail!("promotion task_seq must be positive");
    }
    if binding.gate_name.trim().is_empty() || binding.evidence_locator.trim().is_empty() {
        bail!("promotion gate name and evidence locator must be nonblank");
    }
    validate_sha256(&binding.evidence_sha256, "promotion evidence_sha256")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn closed_gate_cannot_rescue_a_fabricated_nomination() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "papertiger-mise-promotion-{}-{unique}.sqlite",
            std::process::id()
        ));
        let mise_path = std::env::temp_dir().join(format!(
            "papertiger-mise-promotion-authority-{}-{unique}.sqlite",
            std::process::id()
        ));
        let database = path.to_str().expect("UTF-8 temp path");
        let conn = papertiger::open_for_init(database).expect("open planner database");
        papertiger::init(&conn).expect("planner schema");
        let plan =
            papertiger::add_plan(&conn, "test", "fixture", "Fixture", "").expect("fixture plan");
        let task = papertiger::add_task(
            &conn,
            "test",
            plan,
            "Authorize fixture candidate",
            "",
            None,
            &[],
            &[],
            0,
            None,
            None,
        )
        .expect("fixture task");
        papertiger::add_gate(
            &conn,
            "test",
            task,
            "promotion",
            "other",
            "Authorize the exact Mise nomination",
        )
        .expect("promotion gate");
        papertiger::start_task(&conn, "test", task, None).expect("start task");
        let digest = "a".repeat(64);
        let nomination_id = "nomination-fixture";
        let evidence_locator = format!("papertiger-mise:nomination/{nomination_id}");
        papertiger::close_gate(
            &conn,
            "test",
            task,
            "promotion",
            &evidence_locator,
            Some(&digest),
            None,
        )
        .expect("close promotion gate");
        drop(conn);

        let mise = crate::store::open_for_init(&mise_path).expect("open Mise authority");
        crate::store::init(&mise).expect("Mise schema");
        let manifest = crate::manifest::tests::valid_manifest();
        let admission =
            crate::store::CampaignAdmission::from_manifest(&manifest).expect("admission");
        crate::store::admit_campaign(&mise, "test", &admission).expect("admit campaign");
        let patch_sha = "b".repeat(64);
        let candidate_id = "c".repeat(64);
        mise.execute(
            "INSERT INTO artifacts (sha256, bytes, locator, media_type, recorded_at)
             VALUES (?1, 1, ?2, 'text/x-diff', ?3)",
            rusqlite::params![
                patch_sha,
                format!("sha256/bb/{patch_sha}"),
                crate::store::now()
            ],
        )
        .expect("patch artifact");
        mise.execute(
            "INSERT INTO candidates
             (candidate_id, campaign_id, proposal_json, patch_sha256, negative_fingerprint,
              disposition, result_json, created_at, updated_at)
             VALUES (?1, ?2, '{}', ?3, ?4, 'nominated', '{}', ?5, ?5)",
            rusqlite::params![
                candidate_id,
                manifest.campaign_id,
                patch_sha,
                "d".repeat(64),
                crate::store::now()
            ],
        )
        .expect("candidate");
        mise.execute(
            "INSERT INTO nominations
             (nomination_id, campaign_id, candidate_id, receipt_sha256, receipt_json, created_at)
             VALUES (?1, ?2, ?3, ?4, '{}', ?5)",
            rusqlite::params![
                nomination_id,
                manifest.campaign_id,
                candidate_id,
                digest,
                crate::store::now()
            ],
        )
        .expect("nomination");
        drop(mise);

        let binding = PromotionGateBinding {
            task_seq: task,
            gate_name: "promotion".to_owned(),
            evidence_locator,
            evidence_sha256: digest,
        };
        let policy = TrustedContainmentPolicy {
            schema: crate::attestation::TRUSTED_CONTAINMENT_POLICY_SCHEMA_V2.to_owned(),
            protocol: crate::attestation::SEALED_ATTESTATION_PROTOCOL_V2.to_owned(),
            issuer_identity: "fixture-independent-issuer".to_owned(),
            public_key_ed25519: "1".repeat(64),
            executor_sha256: "4".repeat(64),
            profile_sha256: "3".repeat(64),
        };
        let object_root = std::env::temp_dir().join(format!(
            "papertiger-mise-promotion-objects-{}-{unique}",
            std::process::id()
        ));
        assert!(
            verify_promotion_gate(
                &mise_path,
                &object_root,
                &policy,
                &path,
                nomination_id,
                &binding,
            )
            .is_err()
        );

        let mut drifted = binding;
        drifted.evidence_sha256 = "b".repeat(64);
        assert!(
            verify_promotion_gate(
                &mise_path,
                &object_root,
                &policy,
                &path,
                nomination_id,
                &drifted,
            )
            .is_err()
        );
        fs::remove_file(path).expect("remove fixture database");
        fs::remove_file(mise_path).expect("remove Mise fixture database");
    }
}
