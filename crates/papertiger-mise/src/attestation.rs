use std::path::Path;

use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::digest::sha256;
use crate::manifest::{CampaignManifest, ContainmentGrade, HoldoutDisclosure, HoldoutTierKind};
use crate::object::{PreservedObject, read_object};
use crate::store::{begin_mutation, campaign, now, record_event_in_mutation};

pub const SEALED_ATTESTATION_SCHEMA_V2: &str = "papertiger-mise.sealed-attestation.v2";
pub const SEALED_ATTESTATION_PROTOCOL_V2: &str = "papertiger-mise.ed25519-sealed.v2";
pub const TRUSTED_CONTAINMENT_POLICY_SCHEMA_V2: &str =
    "papertiger-mise.trusted-containment-policy.v2";

/// Operator-owned trust root. It is deliberately external to the campaign: a
/// self-improving campaign cannot nominate its own attestation key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedContainmentPolicy {
    pub schema: String,
    pub protocol: String,
    pub issuer_identity: String,
    pub public_key_ed25519: String,
    pub executor_sha256: String,
    pub profile_sha256: String,
}

impl TrustedContainmentPolicy {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }

    pub fn sha256(&self) -> Result<String> {
        Ok(sha256(&self.canonical_bytes()?))
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != TRUSTED_CONTAINMENT_POLICY_SCHEMA_V2
            || self.protocol != SEALED_ATTESTATION_PROTOCOL_V2
            || self.issuer_identity.trim().is_empty()
        {
            bail!("unsupported or incomplete trusted containment policy");
        }
        decode_fixed_hex::<32>(&self.public_key_ed25519, "policy public key")?;
        decode_fixed_hex::<32>(&self.executor_sha256, "policy executor SHA-256")?;
        decode_fixed_hex::<32>(&self.profile_sha256, "policy profile SHA-256")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedAttestationPayload {
    pub schema: String,
    pub campaign_id: String,
    pub manifest_sha256: String,
    pub candidate_id: String,
    pub trial_id: String,
    pub tier: String,
    pub trial_receipt_sha256: String,
    pub materialization_receipt_sha256: String,
    pub baseline_materialization_receipt_sha256: String,
    pub candidate_tree_before: String,
    pub candidate_tree_after: String,
    pub baseline_tree_before: String,
    pub baseline_tree_after: String,
    pub launcher_sha256: String,
    pub evaluator_sha256: String,
    pub fixture_locator: String,
    pub fixture_sha256: String,
    pub executor_sha256: String,
    pub profile_sha256: String,
    pub trusted_policy_sha256: String,
    pub issuer_identity: String,
    pub invocation_sha256: String,
    pub execution_limits_sha256: String,
    pub actual_grade: String,
    pub network_denied: bool,
    pub read_only_evaluator_inputs: bool,
    pub workspace_isolated: bool,
    pub resource_ceilings_enforced: bool,
    pub controller_loss_cleanup_enforced: bool,
    pub maximum_processes: u32,
    pub maximum_memory_bytes: u64,
    pub verdict_only_disclosure: bool,
    pub supervisor_identity: String,
    pub process_birth_identity: String,
    pub boundary_identity: String,
    pub execution_started_at: String,
    pub execution_finished_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedSealedAttestation {
    pub payload: SealedAttestationPayload,
    /// Lowercase hex Ed25519 signature over canonical compact JSON bytes of
    /// `payload`.
    pub signature_ed25519: String,
}

/// Record an independently authenticated sealed-executor claim for an exact
/// completed trial. The trusted key comes from operator policy, never from the
/// campaign being evaluated.
pub fn record_sealed_attestation(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    trial_id: &str,
    evidence: &PreservedObject,
    trusted_policy: &TrustedContainmentPolicy,
) -> Result<bool> {
    if actor.trim().is_empty() || trial_id.trim().is_empty() {
        bail!("attestation actor and trial_id must be nonblank");
    }
    let bytes = read_object(object_root, evidence)?;
    let signed: SignedSealedAttestation =
        serde_json::from_slice(&bytes).context("sealed attestation is not typed canonical JSON")?;
    if serde_json::to_vec(&signed)? != bytes {
        bail!("sealed attestation must use canonical compact JSON serialization");
    }
    let trial = crate::lifecycle::trial(connection, trial_id)?
        .with_context(|| format!("unknown trial '{trial_id}'"))?;
    let campaign_record = campaign(connection, &trial.campaign_id)?
        .with_context(|| format!("unknown campaign '{}'", trial.campaign_id))?;
    let manifest: CampaignManifest = serde_json::from_str(&campaign_record.manifest_json)?;
    verify_signed_attestation(
        connection,
        &signed,
        &trial,
        &manifest,
        &campaign_record.manifest_sha256,
        trusted_policy,
    )?;

    let role = attestation_role(trial_id);
    let transaction = begin_mutation(connection)?;
    let existing = transaction
        .query_row(
            "SELECT sha256 FROM candidate_artifacts WHERE candidate_id=?1 AND role=?2",
            params![trial.candidate_id, role],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing == evidence.sha256 {
            transaction.commit()?;
            return Ok(false);
        }
        bail!("trial '{trial_id}' already has a different sealed attestation");
    }
    transaction.execute(
        "INSERT OR IGNORE INTO artifacts (sha256, bytes, locator, media_type, recorded_at)
         VALUES (?1, ?2, ?3, 'application/json', ?4)",
        params![
            evidence.sha256,
            i64::try_from(evidence.bytes)?,
            evidence.locator,
            now()
        ],
    )?;
    transaction.execute(
        "INSERT INTO candidate_artifacts (candidate_id, role, sha256) VALUES (?1, ?2, ?3)",
        params![trial.candidate_id, role, evidence.sha256],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "trial",
        trial_id,
        "sealed-attestation-recorded",
        None,
        Some(&serde_json::to_value(evidence)?),
    )?;
    transaction.commit()?;
    Ok(true)
}

pub(crate) fn require_sealed_attestation(
    connection: &Connection,
    object_root: &Path,
    trial_id: &str,
    manifest: &CampaignManifest,
    manifest_sha256: &str,
    trusted_policy: &TrustedContainmentPolicy,
) -> Result<String> {
    let trial = crate::lifecycle::trial(connection, trial_id)?
        .with_context(|| format!("unknown relied-upon trial '{trial_id}'"))?;
    let role = attestation_role(trial_id);
    let object = connection
        .query_row(
            "SELECT a.sha256, a.bytes, a.locator
             FROM candidate_artifacts ca JOIN artifacts a ON a.sha256=ca.sha256
             WHERE ca.candidate_id=?1 AND ca.role=?2",
            params![trial.candidate_id, role],
            |row| {
                let signed_bytes = row.get::<_, i64>(1)?;
                let bytes = u64::try_from(signed_bytes).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?;
                Ok(PreservedObject {
                    sha256: row.get(0)?,
                    bytes,
                    locator: row.get(2)?,
                })
            },
        )
        .optional()?
        .with_context(|| format!("relied-upon trial '{trial_id}' has no sealed attestation"))?;
    let bytes = read_object(object_root, &object)?;
    let signed: SignedSealedAttestation = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&signed)? != bytes {
        bail!("sealed attestation is not canonical compact JSON");
    }
    verify_signed_attestation(
        connection,
        &signed,
        &trial,
        manifest,
        manifest_sha256,
        trusted_policy,
    )?;
    Ok(object.sha256)
}

fn verify_signed_attestation(
    connection: &Connection,
    signed: &SignedSealedAttestation,
    trial: &crate::lifecycle::TrialRecord,
    manifest: &CampaignManifest,
    manifest_sha256: &str,
    trusted_policy: &TrustedContainmentPolicy,
) -> Result<()> {
    if manifest.containment != ContainmentGrade::Sealed {
        bail!("sealed attestation is only valid for a sealed admitted campaign");
    }
    trusted_policy.validate()?;
    let requirement = manifest
        .containment_requirement
        .as_ref()
        .context("sealed campaign has no admitted containment requirement")?;
    if requirement.protocol != trusted_policy.protocol
        || requirement.executor_sha256.0 != trusted_policy.executor_sha256
        || requirement.profile_sha256.0 != trusted_policy.profile_sha256
    {
        bail!("operator trust policy does not authorize the campaign's frozen executor/profile");
    }
    let payload = &signed.payload;
    let trial_receipt_sha256 = trial
        .outcome
        .as_ref()
        .and_then(|value| value.pointer("/receipt/sha256"))
        .and_then(serde_json::Value::as_str)
        .context("attested trial has no successful durable receipt")?;
    let (fixture_locator, fixture_sha256) = expected_fixture_binding(manifest, &trial.tier)?;
    let baseline_result_tree = materialization_tree_by_receipt(
        connection,
        &trial.baseline_materialization_receipt_sha256,
    )?;
    let invocation_sha256 = sha256_json(&json!({
        "argv": trial.argv,
        "environment": trial.environment,
        "working_directory": trial.working_directory,
        "candidate_result_tree": trial.result_tree,
        "baseline_result_tree": baseline_result_tree,
    }))?;
    let execution_limits_sha256 = sha256_json(&manifest.execution_limits)?;
    let confirmation = manifest
        .holdouts
        .tiers
        .iter()
        .find(|tier| tier.key == trial.tier)
        .is_some_and(|tier| {
            tier.kind == HoldoutTierKind::Confirmation
                && matches!(tier.disclosure, HoldoutDisclosure::VerdictOnly)
        });
    if confirmation {
        bail!(
            "sealed confirmation attestation v1 is unavailable until the trial receipt and durable outcome are genuinely verdict-only"
        );
    }
    if trial.status != crate::state::TrialStatus::Succeeded
        || payload.schema != SEALED_ATTESTATION_SCHEMA_V2
        || payload.campaign_id != trial.campaign_id
        || payload.manifest_sha256 != manifest_sha256
        || payload.candidate_id != trial.candidate_id
        || payload.trial_id != trial.trial_id
        || payload.tier != trial.tier
        || payload.trial_receipt_sha256 != trial_receipt_sha256
        || payload.materialization_receipt_sha256 != trial.materialization_receipt_sha256
        || payload.baseline_materialization_receipt_sha256
            != trial.baseline_materialization_receipt_sha256
        || payload.candidate_tree_before != trial.result_tree
        || payload.candidate_tree_after != trial.result_tree
        || payload.baseline_tree_before != baseline_result_tree
        || payload.baseline_tree_after != baseline_result_tree
        || payload.launcher_sha256 != manifest.evaluator.launcher_sha256.0
        || payload.evaluator_sha256 != manifest.evaluator.evaluator_sha256.0
        || payload.fixture_locator != fixture_locator
        || payload.fixture_sha256 != fixture_sha256
        || payload.executor_sha256 != trusted_policy.executor_sha256
        || payload.profile_sha256 != trusted_policy.profile_sha256
        || payload.trusted_policy_sha256 != trusted_policy.sha256()?
        || payload.issuer_identity != trusted_policy.issuer_identity
        || payload.invocation_sha256 != invocation_sha256
        || payload.execution_limits_sha256 != execution_limits_sha256
        || payload.actual_grade != "sealed"
        || !payload.network_denied
        || !payload.read_only_evaluator_inputs
        || !payload.workspace_isolated
        || !payload.resource_ceilings_enforced
        || !payload.controller_loss_cleanup_enforced
        || payload.maximum_processes != requirement.maximum_processes
        || payload.maximum_memory_bytes != requirement.maximum_memory_bytes
        || payload.verdict_only_disclosure != confirmation
        || Some(payload.supervisor_identity.as_str()) != trial.supervisor_identity.as_deref()
        || Some(payload.process_birth_identity.as_str()) != trial.process_birth_identity.as_deref()
        || payload.boundary_identity.trim().is_empty()
        || payload.execution_started_at.trim().is_empty()
        || payload.execution_finished_at.trim().is_empty()
    {
        bail!("sealed attestation does not bind the exact completed trial and trusted executor");
    }
    let key_bytes = decode_fixed_hex::<32>(&trusted_policy.public_key_ed25519, "public key")?;
    let signature_bytes = decode_fixed_hex::<64>(&signed.signature_ed25519, "signature")?;
    let key = VerifyingKey::from_bytes(&key_bytes).context("invalid trusted Ed25519 public key")?;
    let signature = Signature::from_bytes(&signature_bytes);
    key.verify(&serde_json::to_vec(payload)?, &signature)
        .context("sealed executor attestation signature is invalid")
}

fn materialization_tree_by_receipt(
    connection: &Connection,
    receipt_sha256: &str,
) -> Result<String> {
    connection
        .query_row(
            "SELECT result_tree FROM materializations WHERE receipt_sha256=?1",
            params![receipt_sha256],
            |row| row.get(0),
        )
        .optional()?
        .with_context(|| format!("unknown materialization receipt '{receipt_sha256}'"))
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
        let tier = manifest
            .holdouts
            .tiers
            .iter()
            .find(|declared| declared.key == tier)
            .with_context(|| format!("unknown trial tier '{tier}'"))?;
        Ok((tier.fixture_locator.clone(), tier.fixture_sha256.0.clone()))
    }
}

fn attestation_role(trial_id: &str) -> String {
    format!("sealed-attestation:{trial_id}")
}

fn sha256_json(value: &impl Serialize) -> Result<String> {
    Ok(sha256(&serde_json::to_vec(value)?))
}

fn decode_fixed_hex<const N: usize>(value: &str, label: &str) -> Result<[u8; N]> {
    if value.len() != N * 2 {
        bail!(
            "sealed attestation {label} must be exactly {} lowercase hex characters",
            N * 2
        );
    }
    let mut bytes = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0]).with_context(|| format!("invalid {label} hex"))?;
        let low = hex_nibble(pair[1]).with_context(|| format!("invalid {label} hex"))?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}
