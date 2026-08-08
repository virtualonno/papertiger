use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::{DomainObservationResult, PairedAdapterBinding, execute_paired_adapter};
use crate::digest::sha256;
use crate::object::{
    PreservedObject, indexed_object as artifact, preserve_object, read_object, read_verified_json,
    record_indexed_object as record_artifact,
};
use crate::store::{begin_mutation, now};
use crate::validation::validate_actor;

pub const HISTORICAL_SHADOW_RECEIPT_SCHEMA_V1: &str =
    "papertiger-mise.historical-shadow-receipt.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedEvidenceOutcome {
    Recorded,
    Existing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedRunOrder {
    BaselineFirst,
    CandidateFirst,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalBlockBinding {
    pub block_index: u32,
    pub observed_order: Option<ObservedRunOrder>,
    pub baseline_trial_receipt_sha256: String,
    pub candidate_trial_receipt_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalShadowReceipt {
    pub schema: String,
    pub scope: String,
    pub decision_eligible: bool,
    pub schedule_authority: String,
    pub adjudication: String,
    pub binding_sha256: String,
    pub request: PreservedObject,
    pub result: PreservedObject,
    pub observation_id: String,
    pub blocks: Vec<HistoricalBlockBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalShadowRecord {
    pub evidence_id: String,
    pub binding: PairedAdapterBinding,
    pub receipt: HistoricalShadowReceipt,
    pub result: DomainObservationResult,
    pub recorded_by: String,
    pub recorded_at: String,
}

pub fn record_historical_shadow(
    connection: &Connection,
    actor: &str,
    object_root: &Path,
    binding: &PairedAdapterBinding,
    domain_request: &Value,
) -> Result<(PairedEvidenceOutcome, HistoricalShadowRecord)> {
    validate_actor("paired evidence actor", actor)?;
    let verified = execute_paired_adapter(binding, domain_request)?;
    let binding_json = serde_json::to_vec(binding)?;
    let binding_sha256 = sha256(&binding_json);
    let request = preserve_object(object_root, &verified.request_bytes)?;
    let result = preserve_object(object_root, &verified.output_bytes)?;
    if request.sha256 != verified.result.request_sha256.0 {
        bail!("paired adapter request object differs from the domain result binding");
    }
    let blocks = verified
        .result
        .blocks
        .iter()
        .map(|block| {
            Ok(HistoricalBlockBinding {
                block_index: block.block_index,
                observed_order: observed_order(
                    &block.baseline.started_at,
                    &block.candidate.started_at,
                )?,
                baseline_trial_receipt_sha256: block.baseline.receipt_sha256.0.clone(),
                candidate_trial_receipt_sha256: block.candidate.receipt_sha256.0.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let receipt = HistoricalShadowReceipt {
        schema: HISTORICAL_SHADOW_RECEIPT_SCHEMA_V1.to_owned(),
        scope: "historical_shadow".to_owned(),
        decision_eligible: false,
        schedule_authority: "unavailable".to_owned(),
        adjudication: "prohibited".to_owned(),
        binding_sha256: binding_sha256.clone(),
        request: request.clone(),
        result: result.clone(),
        observation_id: verified.result.observation_id.clone(),
        blocks,
    };
    let receipt_bytes = serde_json::to_vec(&receipt)?;
    let receipt_object = preserve_object(object_root, &receipt_bytes)?;
    let evidence_id = receipt_object.sha256.clone();

    let transaction = begin_mutation(connection)?;
    if let Some(existing_receipt) = transaction
        .query_row(
            "SELECT receipt_sha256 FROM paired_adapter_observations WHERE evidence_id=?1",
            params![evidence_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        if existing_receipt != receipt_object.sha256 {
            bail!("historical shadow evidence identity conflicts with its durable receipt");
        }
        transaction.commit()?;
        let record = historical_shadow(connection, object_root, &evidence_id)?
            .context("historical shadow disappeared after idempotent replay")?;
        return Ok((PairedEvidenceOutcome::Existing, record));
    }
    record_artifact(&transaction, &request, "application/json")?;
    record_artifact(&transaction, &result, "application/json")?;
    record_artifact(&transaction, &receipt_object, "application/json")?;
    transaction.execute(
        "INSERT INTO paired_adapter_observations
         (evidence_id, scope, campaign_id, candidate_id, analysis_slot,
          binding_sha256, binding_json, request_sha256, result_sha256,
          receipt_sha256, decision_eligible, recorded_by, recorded_at)
         VALUES (?1, 'historical_shadow', NULL, NULL, NULL, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8)",
        params![
            evidence_id,
            binding_sha256,
            String::from_utf8(binding_json)?,
            request.sha256,
            result.sha256,
            receipt_object.sha256,
            actor,
            now(),
        ],
    )?;
    for (domain_block, receipt_block) in verified.result.blocks.iter().zip(&receipt.blocks) {
        for digest in [
            &receipt_block.baseline_trial_receipt_sha256,
            &receipt_block.candidate_trial_receipt_sha256,
        ] {
            if transaction
                .query_row(
                    "SELECT evidence_id FROM paired_domain_trial_receipts WHERE receipt_sha256=?1",
                    params![digest],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .is_some()
            {
                bail!("domain trial receipt {digest} is already claimed by paired evidence");
            }
        }
        transaction.execute(
            "INSERT INTO paired_adapter_blocks
             (evidence_id, block_index, observed_order, block_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                evidence_id,
                i64::from(receipt_block.block_index),
                receipt_block.observed_order.map(observed_order_label),
                serde_json::to_string(domain_block)?,
            ],
        )?;
        for (participant, digest) in [
            ("baseline", &receipt_block.baseline_trial_receipt_sha256),
            ("candidate", &receipt_block.candidate_trial_receipt_sha256),
        ] {
            transaction.execute(
                "INSERT INTO paired_domain_trial_receipts
                 (receipt_sha256, evidence_id, block_index, participant)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    digest,
                    evidence_id,
                    i64::from(receipt_block.block_index),
                    participant,
                ],
            )?;
        }
    }
    transaction.commit()?;
    let record = historical_shadow(connection, object_root, &evidence_id)?
        .context("historical shadow disappeared after recording")?;
    Ok((PairedEvidenceOutcome::Recorded, record))
}

pub fn historical_shadow(
    connection: &Connection,
    object_root: &Path,
    evidence_id: &str,
) -> Result<Option<HistoricalShadowRecord>> {
    let row = connection
        .query_row(
            "SELECT binding_sha256, binding_json, request_sha256, result_sha256,
                    receipt_sha256, decision_eligible, recorded_by, recorded_at
             FROM paired_adapter_observations
             WHERE evidence_id=?1 AND scope='historical_shadow'",
            params![evidence_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        binding_sha256,
        binding_json,
        request_sha256,
        result_sha256,
        receipt_sha256,
        decision_eligible,
        recorded_by,
        recorded_at,
    )) = row
    else {
        return Ok(None);
    };
    if evidence_id != receipt_sha256
        || decision_eligible != 0
        || sha256(binding_json.as_bytes()) != binding_sha256
    {
        bail!("historical shadow durable identity failed closed");
    }
    let binding: PairedAdapterBinding = serde_json::from_str(&binding_json)?;
    let request = artifact(connection, &request_sha256)?;
    let result_object = artifact(connection, &result_sha256)?;
    let receipt_object = artifact(connection, &receipt_sha256)?;
    let request_bytes = read_object(object_root, &request)?;
    let result_bytes = read_object(object_root, &result_object)?;
    let (receipt, receipt_bytes): (HistoricalShadowReceipt, Vec<u8>) =
        read_verified_json(object_root, &receipt_object, "historical-shadow receipt")?;
    let result = parse_historical_result(&result_bytes)?;
    if request.sha256 != result.request_sha256.0
        || result.adapter_executable_sha256 != binding.executable_sha256
        || receipt.request != request
        || receipt.result != result_object
        || receipt.binding_sha256 != binding_sha256
        || receipt.decision_eligible
        || receipt.scope != "historical_shadow"
        || receipt.schedule_authority != "unavailable"
        || receipt.adjudication != "prohibited"
        || sha256(&receipt_bytes) != evidence_id
    {
        bail!("historical shadow CAS receipt failed exact replay verification");
    }
    let expected_blocks = result
        .blocks
        .iter()
        .map(|block| {
            Ok(HistoricalBlockBinding {
                block_index: block.block_index,
                observed_order: observed_order(
                    &block.baseline.started_at,
                    &block.candidate.started_at,
                )?,
                baseline_trial_receipt_sha256: block.baseline.receipt_sha256.0.clone(),
                candidate_trial_receipt_sha256: block.candidate.receipt_sha256.0.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if receipt.blocks != expected_blocks {
        bail!("historical shadow receipt block bindings differ from its CAS result");
    }
    let durable_blocks = connection
        .prepare(
            "SELECT block_index, observed_order, block_json
             FROM paired_adapter_blocks WHERE evidence_id=?1 ORDER BY block_index",
        )?
        .query_map(params![evidence_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if durable_blocks.len() != result.blocks.len() {
        bail!("historical shadow durable block count differs from its CAS result");
    }
    for (((block_index, order, block_json), expected), expected_binding) in durable_blocks
        .iter()
        .zip(result.blocks.iter())
        .zip(expected_blocks.iter())
    {
        if u32::try_from(*block_index)? != expected.block_index
            || serde_json::from_str::<crate::adapter::DomainBlockMeasurement>(block_json)?
                != *expected
            || order.as_deref() != expected_binding.observed_order.map(observed_order_label)
        {
            bail!("historical shadow durable block index differs from its CAS result");
        }
    }
    let domain_receipts = result
        .blocks
        .iter()
        .flat_map(|block| {
            [
                block.baseline.receipt_sha256.0.as_str(),
                block.candidate.receipt_sha256.0.as_str(),
            ]
        })
        .collect::<BTreeSet<_>>();
    if domain_receipts.len() != result.blocks.len() * 2 {
        bail!("historical shadow result repeats a domain trial receipt");
    }
    let durable_receipts = connection
        .prepare(
            "SELECT receipt_sha256 FROM paired_domain_trial_receipts
             WHERE evidence_id=?1 ORDER BY receipt_sha256",
        )?
        .query_map(params![evidence_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    if durable_receipts
        != domain_receipts
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
    {
        bail!("historical shadow domain receipt index differs from its CAS result");
    }
    if request_bytes.is_empty() {
        bail!("historical shadow request object is empty");
    }
    Ok(Some(HistoricalShadowRecord {
        evidence_id: evidence_id.to_owned(),
        binding,
        receipt,
        result,
        recorded_by,
        recorded_at,
    }))
}

fn observed_order(
    baseline_started_at: &str,
    candidate_started_at: &str,
) -> Result<Option<ObservedRunOrder>> {
    let baseline = DateTime::parse_from_rfc3339(baseline_started_at)
        .context("parse baseline domain start time")?;
    let candidate = DateTime::parse_from_rfc3339(candidate_started_at)
        .context("parse candidate domain start time")?;
    Ok(match baseline.cmp(&candidate) {
        std::cmp::Ordering::Less => Some(ObservedRunOrder::BaselineFirst),
        std::cmp::Ordering::Greater => Some(ObservedRunOrder::CandidateFirst),
        std::cmp::Ordering::Equal => None,
    })
}

fn parse_historical_result(bytes: &[u8]) -> Result<DomainObservationResult> {
    let mut value: Value = serde_json::from_slice(bytes)?;
    let object = value
        .as_object_mut()
        .context("historical shadow result must be an object")?;
    if !object.contains_key("adapter_executable_sha256")
        && let Some(legacy) = object.remove("executor_executable_sha256")
    {
        object.insert("adapter_executable_sha256".to_owned(), legacy);
    }
    serde_json::from_value(value).context("parse historical shadow domain result")
}

fn observed_order_label(order: ObservedRunOrder) -> &'static str {
    match order {
        ObservedRunOrder::BaselineFirst => "baseline_first",
        ObservedRunOrder::CandidateFirst => "candidate_first",
    }
}
