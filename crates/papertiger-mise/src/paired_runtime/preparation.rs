use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

use super::{
    PairedCohortRecord, PairedPreparationOutcome, PreparePairedCohortSpec, canonical_json_bytes,
    cohort_columns, durable_cohort, load_manifest, now, paired_cohort, participant_label,
    record_event_in_mutation, require_same_preparation, validate_candidate_and_participants,
    validate_research_slot, validate_token,
};
use crate::adapter::{prepare_paired_adapter_cohort, verify_paired_adapter_runtime};
use crate::budget::{BudgetRequest, BudgetResource, reserve_budget_in};
use crate::digest::sha256;
use crate::manifest::{ContainmentGrade, Sha256Digest};
use crate::object::{preserve_object, record_indexed_object};
use crate::statistics::{PairedCandidateContext, PairedCohort};
use crate::store::begin_mutation;
pub fn prepare_paired_cohort(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    spec: &PreparePairedCohortSpec,
) -> Result<(PairedPreparationOutcome, PairedCohortRecord)> {
    validate_token("paired cohort actor", actor)?;
    validate_token("paired cohort id", &spec.cohort_id)?;
    validate_token("paired reservation id", &spec.reservation_id)?;
    let manifest = load_manifest(connection, &spec.campaign_id)?;
    let plan = manifest
        .paired_analysis
        .as_ref()
        .context("campaign has no admitted paired analysis contract")?;
    if manifest.containment == ContainmentGrade::Sealed {
        bail!(
            "local paired execution cannot satisfy a sealed campaign; use the future platform-neutral attested-worker path"
        );
    }
    let binding = plan
        .trial_adapter
        .as_ref()
        .context("paired analysis has no frozen trial adapter")?;
    verify_paired_adapter_runtime(binding)?;
    validate_candidate_and_participants(connection, &manifest, spec)?;
    let candidate_identity = Sha256Digest(spec.candidate_id.clone());
    let candidate_context = PairedCandidateContext {
        cohort: spec.cohort,
        candidate_identity_sha256: &candidate_identity,
        revealed_order_seed: &spec.revealed_order_seed,
    };
    let prepared = prepare_paired_adapter_cohort(
        &spec.cohort_id,
        plan,
        &manifest.objectives,
        &candidate_context,
        &spec.participants,
    )?;
    validate_research_slot(connection, spec, &prepared.schedule_sha256)?;
    let binding_bytes = serde_json::to_vec(binding)?;
    let binding_sha256 = Sha256Digest(sha256(&binding_bytes));
    let request_objects = prepared
        .requests
        .iter()
        .map(|request| preserve_object(object_root, &canonical_json_bytes(request)?))
        .collect::<Result<Vec<_>>>()?;
    let run_count = u64::try_from(request_objects.len())?;
    let wall_reservation = binding
        .maximum_wall_time_ms
        .checked_mul(run_count)
        .context("paired cohort wall-time reservation overflow")?;
    let mut budget_requests = vec![
        BudgetRequest::new(BudgetResource::Trials, run_count)?,
        BudgetRequest::new(BudgetResource::WallTimeMilliseconds, wall_reservation)?,
        BudgetRequest::new(BudgetResource::Failures, 1)?,
    ];
    if matches!(spec.cohort, PairedCohort::Research { .. }) {
        budget_requests.push(BudgetRequest::new(
            BudgetResource::HoldoutDisclosures,
            run_count,
        )?);
    }
    let transaction = begin_mutation(connection)?;
    if transaction
        .query_row(
            "SELECT 1 FROM paired_cohorts WHERE cohort_id=?1",
            params![spec.cohort_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        transaction.commit()?;
        let durable = durable_cohort(connection, &spec.cohort_id)?
            .context("existing paired cohort disappeared")?;
        if durable.revealed_order_seed != spec.revealed_order_seed {
            bail!("paired cohort id already exists with a different order-seed reveal");
        }
        let existing = durable.record;
        require_same_preparation(&existing, spec, &prepared.schedule_sha256, &binding_sha256)?;
        return Ok((PairedPreparationOutcome::Existing, existing));
    }
    reserve_budget_in(
        &transaction,
        actor,
        &spec.campaign_id,
        &spec.reservation_id,
        &budget_requests,
    )?;
    transaction.execute(
        "INSERT INTO budget_reservation_uses
         (campaign_id, reservation_id, use_kind, entity_key, bound_at)
         VALUES (?1, ?2, 'paired_cohort', ?3, ?4)",
        params![spec.campaign_id, spec.reservation_id, spec.cohort_id, now()],
    )?;
    for object in &request_objects {
        record_indexed_object(&transaction, object, "application/json")?;
    }
    let (cohort_kind, analysis_slot) = cohort_columns(spec.cohort);
    transaction.execute(
        "INSERT INTO paired_cohorts
         (cohort_id, campaign_id, candidate_id, cohort_kind, analysis_slot,
          schedule_sha256, order_seed_reveal, participants_json, reservation_id,
          adapter_binding_sha256, recorded_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            spec.cohort_id,
            spec.campaign_id,
            spec.candidate_id,
            cohort_kind,
            analysis_slot.map(i64::from),
            prepared.schedule_sha256.0,
            spec.revealed_order_seed,
            serde_json::to_string(&spec.participants)?,
            spec.reservation_id,
            binding_sha256.0,
            actor,
            now(),
        ],
    )?;
    for (ordinal, (request, object)) in prepared.requests.iter().zip(&request_objects).enumerate() {
        transaction.execute(
            "INSERT INTO paired_runs
             (execution_id, cohort_id, ordinal, block_index, participant,
              request_sha256, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                request.execution_id,
                spec.cohort_id,
                i64::try_from(ordinal)?,
                i64::from(request.block_index),
                participant_label(request.participant_role),
                object.sha256,
                now(),
            ],
        )?;
    }
    record_event_in_mutation(
        &transaction,
        actor,
        "paired_cohort",
        &spec.cohort_id,
        "prepared",
        None,
        Some(&json!({
            "campaign_id": spec.campaign_id,
            "candidate_id": spec.candidate_id,
            "cohort": spec.cohort,
            "schedule_sha256": prepared.schedule_sha256,
            "run_count": run_count,
            "reservation_id": spec.reservation_id,
        })),
    )?;
    transaction.commit()?;
    let record = paired_cohort(connection, &spec.cohort_id)?
        .context("prepared paired cohort disappeared")?;
    Ok((PairedPreparationOutcome::Prepared, record))
}
