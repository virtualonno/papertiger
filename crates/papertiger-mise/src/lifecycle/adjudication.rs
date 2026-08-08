use super::*;

pub(super) fn validate_calibration_outcome(
    trial: &TrialRecord,
    receipt: &TrialReceipt,
    classification: &Classification,
    manifest: &CampaignManifest,
) -> Result<()> {
    if trial.tier == "calibration.no_op"
        && (classification.disposition != CandidateDisposition::Inconclusive
            || receipt
                .observations
                .iter()
                .any(|observation| observation.baseline != observation.candidate))
    {
        bail!("no-op calibration was not exactly flat under the deterministic protocol");
    }
    if trial.tier == "calibration.known_bad"
        && (classification.disposition != CandidateDisposition::Rejected
            || receipt.reason_code.as_deref()
                != Some(
                    manifest
                        .calibration
                        .known_bad
                        .expected_rejection_code
                        .as_str(),
                ))
    {
        bail!("known-bad calibration did not produce its exact expected rejection");
    }
    Ok(())
}

pub fn adjudicate_deterministic_candidate(
    connection: &Connection,
    actor: &str,
    candidate_id: &str,
) -> Result<Option<NominationRecord>> {
    validate_nonblank("actor", actor)?;
    let durable_candidate = candidate(connection, candidate_id)?
        .with_context(|| format!("unknown candidate '{candidate_id}'"))?;
    let manifest = campaign_manifest(connection, &durable_candidate.campaign_id)?;
    require_deterministic_runtime(&manifest)?;
    require_campaign_integrity(connection, &manifest)?;
    if durable_candidate.material_sha256 == manifest.calibration.no_op_material_sha256().0
        || durable_candidate.material_sha256 == manifest.calibration.known_bad_material_sha256().0
    {
        bail!("calibration candidates are evidence for the judge and cannot be adjudicated");
    }
    let derived = derive_deterministic_candidate_result(connection, candidate_id, &manifest)?;
    let result = derived.result;
    let disposition = derived.disposition;
    let reason_code = match disposition {
        CandidateDisposition::Nominated => "qualified",
        CandidateDisposition::Rejected => "objective-rejected",
        CandidateDisposition::Inconclusive => "inconclusive",
        _ => bail!("deterministic adjudication produced a nonterminal disposition"),
    };

    let transaction = begin_mutation(connection)?;
    let current = candidate_in(&transaction, candidate_id)?
        .context("candidate disappeared before adjudication")?;
    if let Some(existing) = &current.result {
        if existing == &result && current.disposition == disposition {
            let nomination = nomination_in(&transaction, candidate_id)?;
            transaction.commit()?;
            return Ok(nomination);
        }
        bail!("candidate '{candidate_id}' already has a different terminal result");
    }
    transaction.execute(
        "UPDATE candidates SET disposition=?2, result_json=?3, updated_at=?4
         WHERE candidate_id=?1",
        params![
            candidate_id,
            disposition_str(disposition),
            serde_json::to_string(&result)?,
            now()
        ],
    )?;
    let nomination = if disposition == CandidateDisposition::Nominated {
        let created_at = now();
        let manifest_sha256: String = transaction.query_row(
            "SELECT manifest_sha256 FROM campaigns WHERE campaign_id=?1",
            params![current.campaign_id],
            |row| row.get(0),
        )?;
        let receipt = json!({
            "schema": "papertiger-mise.nomination.v1",
            "campaign_id": current.campaign_id,
            "manifest_sha256": manifest_sha256,
            "candidate_id": candidate_id,
            "result": result,
            "created_at": created_at,
        });
        let receipt_json = serde_json::to_string(&receipt)?;
        let receipt_sha256 = sha256(receipt_json.as_bytes());
        let nomination_id = receipt_sha256.clone();
        transaction.execute(
            "INSERT INTO nominations
             (nomination_id, campaign_id, candidate_id, receipt_sha256, receipt_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                nomination_id,
                current.campaign_id,
                candidate_id,
                receipt_sha256,
                receipt_json,
                created_at
            ],
        )?;
        Some(NominationRecord {
            nomination_id,
            campaign_id: current.campaign_id.clone(),
            candidate_id: candidate_id.to_owned(),
            receipt_sha256,
            receipt_json,
            created_at,
        })
    } else {
        let evidence_sha256 = derived
            .candidate_trials
            .first()
            .context("terminal candidate has no durable trial evidence")?
            .evidence_sha256
            .clone();
        transaction.execute(
            "INSERT INTO negative_evidence
             (campaign_id, negative_fingerprint, candidate_id, reason_code, evidence_sha256, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                current.campaign_id,
                current.negative_fingerprint,
                candidate_id,
                reason_code,
                evidence_sha256,
                now()
            ],
        )?;
        None
    };
    record_event_in_mutation(
        &transaction,
        actor,
        "candidate",
        candidate_id,
        disposition_str(disposition),
        Some(reason_code),
        Some(&result),
    )?;
    transaction.commit()?;
    Ok(nomination)
}

pub(super) fn require_deterministic_runtime(manifest: &CampaignManifest) -> Result<()> {
    if manifest.paired_analysis.is_some() {
        bail!(
            "the deterministic runtime cannot execute or adjudicate a paired-analysis campaign; use the paired adapter runner"
        );
    }
    Ok(())
}

#[derive(Debug)]
struct DerivedCandidateResult {
    result: Value,
    disposition: CandidateDisposition,
    calibration_trial_ids: Vec<String>,
    candidate_trials: Vec<QualifiedTrial>,
}

fn derive_deterministic_candidate_result(
    connection: &Connection,
    candidate_id: &str,
    manifest: &CampaignManifest,
) -> Result<DerivedCandidateResult> {
    let calibration_trial_ids = require_calibrations(connection, manifest)?;
    let candidate_trials = qualified_candidate_trials(connection, candidate_id, manifest)?;
    let observations = candidate_trials
        .first()
        .context("candidate has no successful deterministic trials")?
        .observations
        .clone();
    if candidate_trials
        .iter()
        .any(|trial| trial.observations != observations)
    {
        bail!("deterministic candidate trials disagree; use a noisy evaluation protocol instead");
    }
    let classification = classify_deterministic(&manifest.objectives, &observations, true)?;
    let disposition = classification.disposition;
    let trial_ids = candidate_trials
        .iter()
        .map(|trial| trial.trial_id.clone())
        .collect::<Vec<_>>();
    let trial_evidence = candidate_trials
        .iter()
        .map(|trial| trial.evidence_sha256.clone())
        .collect::<Vec<_>>();
    let result = json!({
        "schema": "papertiger-mise.candidate-result.v1",
        "classification": classification,
        "candidate_trial_ids": trial_ids,
        "calibration_trial_ids": calibration_trial_ids,
        "trial_evidence_sha256": trial_evidence,
    });
    Ok(DerivedCandidateResult {
        result,
        disposition,
        calibration_trial_ids,
        candidate_trials,
    })
}

/// Reopen every relied-upon evaluator receipt from CAS and re-derive the exact
/// deterministic nomination. This is the read-only evidence path used by
/// promotion; it does not trust duplicated measurements, outcomes, or result
/// JSON merely because they are internally self-consistent in SQLite.
pub fn verify_nomination_integrity(
    connection: &Connection,
    object_root: &Path,
    nomination_id: &str,
) -> Result<VerifiedNominationEvidence> {
    let nomination = nomination_by_id(connection, nomination_id)?
        .with_context(|| format!("unknown durable nomination '{nomination_id}'"))?;
    if sha256(nomination.receipt_json.as_bytes()) != nomination.receipt_sha256
        || nomination.nomination_id != nomination.receipt_sha256
    {
        bail!("nomination receipt identity does not recompute exactly");
    }
    let campaign_record = crate::store::campaign(connection, &nomination.campaign_id)?
        .context("nomination campaign disappeared")?;
    let manifest: CampaignManifest = serde_json::from_str(&campaign_record.manifest_json)?;
    if sha256(&manifest.historical_canonical_bytes()?) != campaign_record.manifest_sha256 {
        bail!("admitted manifest identity does not recompute from canonical content");
    }
    require_campaign_integrity(connection, &manifest)?;
    let durable_candidate = candidate(connection, &nomination.candidate_id)?
        .context("nominated candidate disappeared")?;
    if durable_candidate.disposition != CandidateDisposition::Nominated {
        bail!("nomination does not identify a nominated candidate");
    }

    verify_candidate_evidence(connection, object_root, &durable_candidate, &manifest)?;
    let receipt: Value = serde_json::from_str(&nomination.receipt_json)?;
    if receipt.get("schema").and_then(Value::as_str)
        == Some(crate::paired_runtime::PAIRED_NOMINATION_RECEIPT_SCHEMA_V1)
    {
        return crate::paired_runtime::verify_paired_nomination_evidence(
            connection,
            object_root,
            nomination,
            manifest,
            campaign_record.manifest_sha256,
            durable_candidate
                .result
                .as_ref()
                .context("nominated paired candidate lost its result")?,
        );
    }
    let derived =
        derive_deterministic_candidate_result(connection, &nomination.candidate_id, &manifest)?;
    if derived.disposition != CandidateDisposition::Nominated
        || durable_candidate.result.as_ref() != Some(&derived.result)
    {
        bail!("durable candidate result differs from evidence re-derivation");
    }
    let mut relied_upon_trial_ids = derived.calibration_trial_ids.clone();
    relied_upon_trial_ids.extend(
        derived
            .candidate_trials
            .iter()
            .map(|trial| trial.trial_id.clone()),
    );
    let mut unique = BTreeSet::new();
    for trial_id in &relied_upon_trial_ids {
        if !unique.insert(trial_id.clone()) {
            bail!("nomination evidence cohort repeats trial '{trial_id}'");
        }
        verify_completed_trial_evidence(connection, object_root, trial_id, &manifest)?;
    }
    if receipt.get("schema").and_then(Value::as_str) != Some("papertiger-mise.nomination.v1")
        || receipt.get("campaign_id").and_then(Value::as_str)
            != Some(nomination.campaign_id.as_str())
        || receipt.get("manifest_sha256").and_then(Value::as_str)
            != Some(campaign_record.manifest_sha256.as_str())
        || receipt.get("candidate_id").and_then(Value::as_str)
            != Some(nomination.candidate_id.as_str())
        || receipt.get("created_at").and_then(Value::as_str) != Some(nomination.created_at.as_str())
        || receipt.get("result") != Some(&derived.result)
    {
        bail!("nomination receipt does not bind its re-derived evidence result");
    }
    let candidate_trial_ids = derived
        .candidate_trials
        .iter()
        .map(|trial| trial.trial_id.clone())
        .collect();
    Ok(VerifiedNominationEvidence {
        nomination,
        manifest,
        manifest_sha256: campaign_record.manifest_sha256,
        candidate_result: derived.result,
        relied_upon_trial_ids,
        candidate_trial_ids,
        relied_upon_paired_cohort_ids: Vec::new(),
        evidence_grade: EvidenceGrade::DeterministicDevelopment,
    })
}

/// Reopen the exact admitted manifest and typed candidate material from CAS,
/// then recompute the candidate and materialization bindings without trusting
/// a duplicated planner-side representation.
pub fn verify_candidate_integrity(
    connection: &Connection,
    object_root: &Path,
    candidate_id: &str,
) -> Result<VerifiedCandidateEvidence> {
    let candidate = candidate(connection, candidate_id)?
        .with_context(|| format!("unknown durable candidate '{candidate_id}'"))?;
    let campaign_record = crate::store::campaign(connection, &candidate.campaign_id)?
        .context("candidate campaign disappeared")?;
    let manifest: CampaignManifest = serde_json::from_str(&campaign_record.manifest_json)?;
    if sha256(&manifest.historical_canonical_bytes()?) != campaign_record.manifest_sha256 {
        bail!("admitted manifest identity does not recompute from canonical content");
    }
    require_campaign_integrity(connection, &manifest)?;
    verify_candidate_evidence(connection, object_root, &candidate, &manifest)?;
    let material_object = artifact_object(connection, &candidate.material_sha256)?;
    let material_bytes = read_object(object_root, &material_object)?;
    let candidate_material = CandidateMaterial::parse_canonical(&material_bytes).context(
        "planner projection supports the typed Git change-set material contract only; run a new typed-material campaign",
    )?;
    let candidate_material_json = String::from_utf8(material_bytes)
        .context("canonical candidate material is not UTF-8 JSON")?;
    Ok(VerifiedCandidateEvidence {
        candidate,
        manifest,
        manifest_sha256: campaign_record.manifest_sha256,
        candidate_material,
        candidate_material_json,
    })
}

fn verify_candidate_evidence(
    connection: &Connection,
    object_root: &Path,
    candidate: &CandidateRecord,
    manifest: &CampaignManifest,
) -> Result<()> {
    let proposal: crate::candidate::CandidateProposal =
        serde_json::from_str(&proposal_json_for(connection, &candidate.candidate_id)?)?;
    let material_object = artifact_object(connection, &candidate.material_sha256)?;
    let material_bytes = read_object(object_root, &material_object)?;
    let rebound = if manifest.candidate_material.is_some() {
        crate::candidate::bind_candidate(proposal.clone(), material_bytes)?
    } else {
        bind_legacy_patch_candidate(proposal.clone(), material_bytes)?
    };
    if rebound.candidate_id != candidate.candidate_id
        || rebound.material_sha256 != candidate.material_sha256
        || rebound.negative_fingerprint != candidate.negative_fingerprint
    {
        bail!("nominated candidate identity does not recompute from proposal and material CAS");
    }
    validate_candidate_against_manifest(&rebound, manifest)?;
    let materialization = materialization_in(connection, &candidate.candidate_id)?
        .context("nominated candidate has no materialization")?;
    verify_materialization_receipt(
        connection,
        object_root,
        manifest,
        &proposal,
        &materialization,
    )
}

pub(super) fn verify_completed_trial_evidence(
    connection: &Connection,
    object_root: &Path,
    trial_id: &str,
    manifest: &CampaignManifest,
) -> Result<()> {
    let durable = trial(connection, trial_id)?
        .with_context(|| format!("relied-upon trial '{trial_id}' disappeared"))?;
    if durable.status != TrialStatus::Succeeded {
        bail!("relied-upon trial '{trial_id}' is not successful");
    }
    let outcome = durable
        .outcome
        .as_ref()
        .context("successful trial has no outcome")?;
    if outcome.get("schema").and_then(Value::as_str) != Some("papertiger-mise.trial-outcome.v1") {
        bail!("successful trial outcome has an unsupported schema");
    }
    let object: PreservedObject = serde_json::from_value(
        outcome
            .get("receipt")
            .cloned()
            .context("successful trial outcome has no receipt pointer")?,
    )?;
    let bytes = read_object(object_root, &object)?;
    let receipt: TrialReceipt = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&receipt)? != bytes {
        bail!("trial receipt CAS bytes are not canonical typed evidence");
    }
    let credential = TrialCompletionCredential {
        owner_uuid: durable
            .owner_uuid
            .clone()
            .context("successful trial has no owner")?,
        supervisor_identity: durable
            .supervisor_identity
            .clone()
            .context("successful trial has no supervisor")?,
        process_birth_identity: durable
            .process_birth_identity
            .clone()
            .context("successful trial has no process-birth identity")?,
    };
    validate_completion_binding(&durable, &receipt, &credential, manifest)?;
    validate_trial_receipt_schema(&durable, &receipt, manifest)?;
    validate_judge_build_receipt(object_root, &durable, &receipt, manifest)?;
    let classification =
        classify_deterministic(&manifest.objectives, &receipt.observations, false)?;
    validate_calibration_outcome(&durable, &receipt, &classification, manifest)?;
    if outcome.get("classification") != Some(&serde_json::to_value(&classification)?) {
        bail!("durable trial classification differs from its CAS receipt");
    }
    let mut statement = connection.prepare(
        "SELECT raw_json FROM measurements WHERE trial_id=?1 ORDER BY ordinal, objective",
    )?;
    let durable_observations = statement
        .query_map(params![trial_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|raw| serde_json::from_str(&raw))
        .collect::<serde_json::Result<Vec<DeterministicObservation>>>()?;
    if durable_observations != receipt.observations {
        bail!("durable measurements are not an exact projection of the CAS receipt");
    }
    for receipt_sha256 in [
        &durable.materialization_receipt_sha256,
        &durable.baseline_materialization_receipt_sha256,
    ] {
        let materialization = materialization_by_receipt(connection, receipt_sha256)?
            .with_context(|| format!("unknown trial materialization '{receipt_sha256}'"))?;
        let proposal: crate::candidate::CandidateProposal = serde_json::from_str(
            &proposal_json_for(connection, &materialization.candidate_id)?,
        )?;
        verify_materialization_receipt(
            connection,
            object_root,
            manifest,
            &proposal,
            &materialization,
        )?;
    }
    Ok(())
}

pub(crate) fn verified_trial_receipt(
    connection: &Connection,
    object_root: &Path,
    trial_id: &str,
    manifest: &CampaignManifest,
) -> Result<(PreservedObject, TrialReceipt)> {
    verify_completed_trial_evidence(connection, object_root, trial_id, manifest)?;
    let durable = trial(connection, trial_id)?
        .with_context(|| format!("relied-upon trial '{trial_id}' disappeared"))?;
    let object: PreservedObject = serde_json::from_value(
        durable
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.get("receipt"))
            .cloned()
            .context("successful trial outcome has no receipt pointer")?,
    )?;
    let bytes = read_object(object_root, &object)?;
    let receipt = serde_json::from_slice(&bytes)?;
    Ok((object, receipt))
}

#[derive(Debug)]
struct QualifiedTrial {
    trial_id: String,
    tier: String,
    evidence_sha256: String,
    observations: Vec<DeterministicObservation>,
}

fn require_calibrations(
    connection: &Connection,
    manifest: &CampaignManifest,
) -> Result<Vec<String>> {
    let requirements = [
        (
            "calibration.no_op",
            manifest.calibration.no_op_material_sha256().0.as_str(),
            manifest.calibration.no_op.minimum_repetitions,
        ),
        (
            "calibration.known_bad",
            manifest.calibration.known_bad_material_sha256().0.as_str(),
            manifest.calibration.known_bad.minimum_repetitions,
        ),
    ];
    let mut all = Vec::new();
    for (tier, material_sha256, minimum) in requirements {
        let mut statement = connection.prepare(
            "SELECT t.trial_id FROM trials t
             JOIN candidates c ON c.candidate_id=t.candidate_id
             WHERE t.campaign_id=?1 AND c.patch_sha256=?2 AND t.tier=?3
               AND t.status='succeeded'
             ORDER BY t.created_at, t.trial_id",
        )?;
        let trials = statement
            .query_map(
                params![manifest.campaign_id, material_sha256, tier],
                |row| row.get::<_, String>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if trials.len() < usize::try_from(minimum)? {
            bail!(
                "campaign has {} successful {tier} calibration trial(s), below required {minimum}",
                trials.len()
            );
        }
        all.extend(trials);
    }
    Ok(all)
}

fn qualified_candidate_trials(
    connection: &Connection,
    candidate_id: &str,
    manifest: &CampaignManifest,
) -> Result<Vec<QualifiedTrial>> {
    let mut statement = connection.prepare(
        "SELECT trial_id, tier, outcome_json FROM trials
         WHERE candidate_id=?1 AND status='succeeded'
         ORDER BY created_at, trial_id",
    )?;
    let rows = statement
        .query_map(params![candidate_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut trials = Vec::new();
    for (trial_id, tier, outcome_json) in rows {
        let outcome: Value = serde_json::from_str(&outcome_json)?;
        let evidence_sha256 = outcome
            .pointer("/receipt/sha256")
            .and_then(Value::as_str)
            .context("successful trial outcome has no evidence digest")?
            .to_owned();
        let mut measurements = connection.prepare(
            "SELECT raw_json FROM measurements WHERE trial_id=?1 ORDER BY ordinal, objective",
        )?;
        let observations = measurements
            .query_map(params![trial_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|raw| serde_json::from_str(&raw).context("parse durable measurement"))
            .collect::<Result<Vec<_>>>()?;
        trials.push(QualifiedTrial {
            trial_id,
            tier,
            evidence_sha256,
            observations,
        });
    }
    for tier in &manifest.holdouts.tiers {
        let count = trials.iter().filter(|trial| trial.tier == tier.key).count();
        if count < usize::try_from(tier.minimum_repetitions)? {
            bail!(
                "candidate has {count} successful '{}' trial(s), below required {}",
                tier.key,
                tier.minimum_repetitions
            );
        }
    }
    Ok(trials)
}
