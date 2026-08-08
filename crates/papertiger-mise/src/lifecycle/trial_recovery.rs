use super::*;

pub(super) fn reconcile_lost_trial(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    trial_id: &str,
    proof: &AbsenceProof,
) -> Result<()> {
    validate_nonblank("absence_proof.verifier", &proof.verifier)?;
    validate_nonblank("absence_proof.observed_at", &proof.observed_at)?;
    validate_nonblank(
        "absence_proof.supervisor_identity",
        &proof.supervisor_identity,
    )?;
    verify_object(object_root, &proof.evidence)?;
    let trial =
        trial(connection, trial_id)?.with_context(|| format!("unknown trial '{trial_id}'"))?;
    if !trial.status.has_live_ownership() && trial.status != TrialStatus::InfrastructureFailed {
        bail!(
            "trial '{trial_id}' cannot be reconciled from status '{}'",
            trial.status
        );
    }
    if trial.supervisor_identity.as_deref() != Some(&proof.supervisor_identity)
        || trial.process_birth_identity != proof.process_birth_identity
    {
        bail!("absence proof does not bind the trial's exact durable process ownership");
    }
    let reservation =
        reservation_rows_for_use(connection, &trial.campaign_id, &trial.reservation_id)?;
    if proof.evidence.bytes
        > reservation
            .get(&BudgetResource::ArtifactBytes)
            .map(|row| row.0)
            .unwrap_or(0)
    {
        bail!("absence evidence exceeds the trial artifact reservation");
    }
    let settlement_note = format!("absence-proof:{}", proof.evidence.sha256);
    let transaction = begin_mutation(connection)?;
    let current =
        trial_in(&transaction, trial_id)?.context("trial disappeared during reconciliation")?;
    if current.status == TrialStatus::InfrastructureFailed {
        if current
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.get("absence_proof"))
            == Some(&serde_json::to_value(proof)?)
        {
            settle_bound_budget_in(
                &transaction,
                actor,
                BoundReservation {
                    campaign_id: &current.campaign_id,
                    reservation_id: &current.reservation_id,
                    use_kind: "trial",
                    entity_key: trial_id,
                },
                SettlementMode::ChargeReservation,
                &[],
                Some(&settlement_note),
            )?;
            transaction.commit()?;
            return Ok(());
        }
        bail!("trial '{trial_id}' was reconciled with different absence evidence");
    }
    if !current.status.has_live_ownership() {
        bail!(
            "trial '{trial_id}' cannot be reconciled from status '{}'",
            current.status
        );
    }
    if current.supervisor_identity.as_deref() != Some(&proof.supervisor_identity)
        || current.process_birth_identity != proof.process_birth_identity
    {
        bail!("absence proof does not bind the trial's exact durable process ownership");
    }
    record_artifact_in(&transaction, &proof.evidence, "application/json")?;
    settle_bound_budget_in(
        &transaction,
        actor,
        BoundReservation {
            campaign_id: &current.campaign_id,
            reservation_id: &current.reservation_id,
            use_kind: "trial",
            entity_key: trial_id,
        },
        SettlementMode::ChargeReservation,
        &[],
        Some(&settlement_note),
    )?;
    let outcome = json!({
        "reason": "owned-process-absent",
        "absence_proof": proof,
        "reservation_charged": true,
    });
    transaction.execute(
        "UPDATE trials SET status='infrastructure_failed', outcome_json=?2, finished_at=?3
         WHERE trial_id=?1",
        params![trial_id, serde_json::to_string(&outcome)?, now()],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "trial",
        trial_id,
        "reconciled-infrastructure-failure",
        Some(&settlement_note),
        Some(&outcome),
    )?;
    transaction.commit()?;
    Ok(())
}

/// Conservatively close an ambiguous pre-launch trial owner. This operation
/// intentionally records no process-absence proof: an operator is choosing to
/// charge the reservation and retire the durable intent after an interrupted
/// launch window.
pub fn abandon_owned_trial(
    connection: &Connection,
    actor: &str,
    trial_id: &str,
    reason: &str,
) -> Result<SettlementOutcome> {
    validate_nonblank("actor", actor)?;
    validate_nonblank("trial_id", trial_id)?;
    validate_nonblank("reason", reason)?;
    let outcome_json = json!({
        "schema": "papertiger-mise.trial-abandonment.v1",
        "reason": reason,
        "reservation_charged": true,
        "process_absence_claimed": false,
    });
    let transaction = begin_mutation(connection)?;
    let current =
        trial_in(&transaction, trial_id)?.with_context(|| format!("unknown trial '{trial_id}'"))?;
    if current.status == TrialStatus::InfrastructureFailed
        && current.outcome.as_ref() == Some(&outcome_json)
    {
        let outcome = settle_bound_budget_in(
            &transaction,
            actor,
            BoundReservation {
                campaign_id: &current.campaign_id,
                reservation_id: &current.reservation_id,
                use_kind: "trial",
                entity_key: trial_id,
            },
            SettlementMode::ChargeReservation,
            &[],
            Some("owned-trial-abandoned"),
        )?;
        transaction.commit()?;
        return Ok(outcome);
    }
    if current.status == TrialStatus::Launched {
        bail!(
            "launched trial '{trial_id}' cannot be abandoned; run `papertiger-mise trial recover {trial_id}` to prove its exact process is absent"
        );
    }
    if current.status != TrialStatus::Owned {
        bail!(
            "trial '{trial_id}' cannot be abandoned from status '{}'",
            current.status
        );
    }
    let outcome = settle_bound_budget_in(
        &transaction,
        actor,
        BoundReservation {
            campaign_id: &current.campaign_id,
            reservation_id: &current.reservation_id,
            use_kind: "trial",
            entity_key: trial_id,
        },
        SettlementMode::ChargeReservation,
        &[],
        Some("owned-trial-abandoned"),
    )?;
    transaction.execute(
        "UPDATE trials SET status='infrastructure_failed', outcome_json=?2, finished_at=?3
         WHERE trial_id=?1",
        params![trial_id, serde_json::to_string(&outcome_json)?, now()],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "trial",
        trial_id,
        "abandoned-before-launch",
        Some(reason),
        Some(&outcome_json),
    )?;
    transaction.commit()?;
    Ok(outcome)
}

/// Recover a durably launched WorkspaceOnly trial after supervisor restart.
///
/// The observer derives the process state and OS birth identity itself. It
/// refuses an exact live process and the ambiguous `owned` pre-launch window;
/// callers cannot supply or mint an absence claim.
pub fn recover_workspace_trial(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    trial_id: &str,
) -> Result<ColdRecoveryOutcome> {
    validate_nonblank("actor", actor)?;
    validate_nonblank("trial_id", trial_id)?;
    let durable =
        trial(connection, trial_id)?.with_context(|| format!("unknown trial '{trial_id}'"))?;
    let manifest = campaign_manifest(connection, &durable.campaign_id)?;
    require_deterministic_runtime(&manifest)?;
    if durable.status == TrialStatus::Succeeded {
        return match recover_succeeded_trial_settlement(
            connection,
            actor,
            object_root,
            &durable,
            &manifest,
        )? {
            SettlementOutcome::Settled => Ok(ColdRecoveryOutcome::Reconciled),
            SettlementOutcome::Existing => Ok(ColdRecoveryOutcome::AlreadyReconciled),
        };
    }
    if durable.status == TrialStatus::InfrastructureFailed {
        let proof: AbsenceProof = serde_json::from_value(
            durable
                .outcome
                .as_ref()
                .and_then(|outcome| outcome.get("absence_proof"))
                .cloned()
                .context("terminal trial is not a replayable absence reconciliation")?,
        )?;
        verify_object(object_root, &proof.evidence)?;
        return Ok(ColdRecoveryOutcome::AlreadyReconciled);
    }
    if durable.status != TrialStatus::Launched {
        bail!(
            "trial '{trial_id}' cannot be cold-recovered from status '{}'; only an OS-bound launched process is decidable",
            durable.status
        );
    }
    let pid = durable.pid.context("launched trial has no durable PID")?;
    let expected_birth = durable
        .process_birth_identity
        .as_deref()
        .context("launched trial has no durable process birth identity")?;
    let supervisor_identity = durable
        .supervisor_identity
        .as_deref()
        .context("launched trial has no durable supervisor identity")?;
    let observation = observe_process(pid).context("inspect durable evaluator process")?;
    if matches!(
        &observation,
        ProcessObservation::Active {
            process_birth_identity
        } if process_birth_identity == expected_birth
    ) {
        bail!(
            "trial '{trial_id}' still owns its exact live evaluator process {pid}; cold recovery refused"
        );
    }
    let evidence = ProcessAbsenceEvidence {
        schema: "papertiger-mise.process-absence-evidence.v1",
        pid,
        expected_process_birth_identity: expected_birth,
        observation: &observation,
        platform: std::env::consts::OS,
    };
    let evidence = crate::object::preserve_object(object_root, &serde_json::to_vec(&evidence)?)?;
    let proof = AbsenceProof {
        verifier: "papertiger-mise.os-process-observer.v1".to_owned(),
        observed_at: Utc::now().to_rfc3339(),
        supervisor_identity: supervisor_identity.to_owned(),
        process_birth_identity: Some(expected_birth.to_owned()),
        evidence,
    };
    reconcile_lost_trial(connection, actor, object_root, trial_id, &proof)?;
    Ok(ColdRecoveryOutcome::Reconciled)
}

fn recover_succeeded_trial_settlement(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    durable: &TrialRecord,
    manifest: &CampaignManifest,
) -> Result<SettlementOutcome> {
    verify_completed_trial_evidence(connection, object_root, &durable.trial_id, manifest)?;
    let receipt_object: PreservedObject = serde_json::from_value(
        durable
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.get("receipt"))
            .cloned()
            .context("successful trial outcome has no receipt pointer")?,
    )?;
    let receipt_bytes = read_object(object_root, &receipt_object)?;
    let receipt: TrialReceipt = serde_json::from_slice(&receipt_bytes)?;
    validate_completion_usage(
        connection,
        durable,
        &receipt,
        receipt_object.bytes,
        manifest,
    )?;
    let measured_usage = completion_usage(&receipt, receipt_object.bytes)?;
    let transaction = begin_mutation(connection)?;
    let current = trial_in(&transaction, &durable.trial_id)?
        .context("successful trial disappeared during settlement recovery")?;
    if current.status != TrialStatus::Succeeded || current.outcome != durable.outcome {
        bail!("successful trial changed during settlement recovery");
    }
    let outcome = settle_bound_budget_in(
        &transaction,
        actor,
        BoundReservation {
            campaign_id: &current.campaign_id,
            reservation_id: &current.reservation_id,
            use_kind: "trial",
            entity_key: &current.trial_id,
        },
        SettlementMode::Measured,
        &measured_usage,
        Some("trial-completed"),
    )?;
    if outcome == SettlementOutcome::Settled {
        record_event_in_mutation(
            &transaction,
            actor,
            "trial",
            &current.trial_id,
            "settlement-recovered",
            Some(
                "terminal trial evidence reverified; reserved budget settled without evaluator replay",
            ),
            Some(&json!({"receipt": receipt_object})),
        )?;
    }
    transaction.commit()?;
    Ok(outcome)
}
