//! Claim outcomes per authoring model: evidence for which models are worth
//! running for filing and for execution. Counts only; it ranks nothing.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::authorship::payload_authorship;
use crate::review::{ClaimReview, ReviewState, normalized_model, task_review};
use crate::{TASK_COLS, Task, resolve_plan, task_from_row};

/// Groups below this many claims are labeled as small samples.
pub const SMALL_SAMPLE: usize = 10;
/// The group for claims recorded before authorship was required.
pub const UNKNOWN_MODEL: &str = "unknown";

#[derive(Debug, Clone, Default, Serialize)]
pub struct VerdictCounts {
    pub confirmed: usize,
    pub disputed: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClaimCounts {
    pub total: usize,
    /// Reviewed by a session running a different model.
    pub cross_model: VerdictCounts,
    /// Reviewed by another session running the same model.
    pub same_model: VerdictCounts,
    pub unreviewed: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FilingOutcomes {
    #[serde(flatten)]
    pub claims: ClaimCounts,
    /// Filings later started or rewritten by a different session.
    pub engaged: usize,
    pub done: usize,
    pub retired: usize,
    pub rejected: usize,
    pub open: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ResultOutcomes {
    #[serde(flatten)]
    pub claims: ClaimCounts,
    /// Completions by this model that were later reopened.
    pub reopened: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelOutcomes {
    /// Normalized model id, or `unknown` for claims recorded before
    /// authorship was required.
    pub model: String,
    pub filings: FilingOutcomes,
    pub results: ResultOutcomes,
    /// Fewer than [`SMALL_SAMPLE`] filings and results together.
    pub small_sample: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelReport {
    pub schema: &'static str,
    pub plan: Option<String>,
    pub since: Option<String>,
    pub models: Vec<ModelOutcomes>,
}

fn model_key(model: Option<&str>) -> String {
    model.map_or_else(|| UNKNOWN_MODEL.to_owned(), normalized_model)
}

fn count_verdict(counts: &mut ClaimCounts, claim: &ClaimReview) {
    let verdicts = if claim.same_model == Some(true) {
        &mut counts.same_model
    } else {
        &mut counts.cross_model
    };
    match claim.state {
        ReviewState::Confirmed => verdicts.confirmed += 1,
        ReviewState::Disputed => verdicts.disputed += 1,
        ReviewState::Unreviewed => counts.unreviewed += 1,
        ReviewState::Engaged => {}
    }
}

/// When each claim was made and which completions were reopened.
struct ClaimTimes {
    created_at: Option<String>,
    completed_at: Option<String>,
    /// Model keys of completions that were later reopened.
    reopened: Vec<String>,
}

fn claim_times(conn: &Connection, task: &Task) -> Result<ClaimTimes> {
    let mut statement = conn.prepare(
        "SELECT at, kind, payload FROM canonical_events
          WHERE entity='task' AND entity_seq=?1 AND kind IN ('create','status')
          ORDER BY event_id",
    )?;
    let rows = statement
        .query_map(params![task.seq], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut times = ClaimTimes {
        created_at: None,
        completed_at: None,
        reopened: Vec::new(),
    };
    let mut completion: Option<String> = None;
    for (at, kind, payload) in rows {
        let payload = payload
            .map(|raw| serde_json::from_str::<serde_json::Value>(&raw))
            .transpose()
            .with_context(|| {
                format!(
                    "task {} has an unreadable event payload; run `papertiger audit`",
                    task.handle
                )
            })?;
        if kind == "create" {
            times.created_at = Some(at);
            continue;
        }
        let to = payload
            .as_ref()
            .and_then(|payload| payload.get("to"))
            .and_then(serde_json::Value::as_str);
        match to {
            Some("done") => {
                completion = Some(model_key(
                    payload_authorship(payload.as_ref())?.model.as_deref(),
                ));
                times.completed_at = Some(at);
            }
            Some("proposed" | "in_progress") => {
                if let Some(model) = completion.take() {
                    times.reopened.push(model);
                }
                times.completed_at = None;
            }
            _ => {}
        }
    }
    Ok(times)
}

/// Claim outcomes per normalized model, optionally for one plan and for
/// claims made at or after `since` (an RFC3339 instant).
pub fn model_report(
    conn: &Connection,
    plan: Option<&str>,
    since: Option<&str>,
) -> Result<ModelReport> {
    if let Some(since) = since
        && chrono::DateTime::parse_from_rfc3339(since).is_err()
    {
        bail!("--since must be an RFC3339 instant such as 2026-10-01T00:00:00Z; got '{since}'");
    }
    let since_instant = since
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()?;
    let in_window = |at: Option<&str>| -> bool {
        match (since_instant, at) {
            (None, _) => true,
            (Some(since), Some(at)) => {
                chrono::DateTime::parse_from_rfc3339(at).is_ok_and(|at| at >= since)
            }
            (Some(_), None) => false,
        }
    };
    let plan_id = plan
        .map(|slug| resolve_plan(conn, Some(slug)))
        .transpose()?
        .map(|(id, _)| id);
    let mut statement = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks WHERE (?1 IS NULL OR plan_id=?1) ORDER BY seq"
    ))?;
    let tasks = statement
        .query_map(params![plan_id], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut models: BTreeMap<String, (FilingOutcomes, ResultOutcomes)> = BTreeMap::new();
    for task in tasks {
        let review = task_review(conn, &task)?;
        let times = claim_times(conn, &task)?;
        if in_window(times.created_at.as_deref()) {
            let entry = models
                .entry(model_key(review.filing.author_model.as_deref()))
                .or_default();
            let filings = &mut entry.0;
            filings.claims.total += 1;
            count_verdict(&mut filings.claims, &review.filing);
            if review.filing.state == ReviewState::Engaged {
                filings.engaged += 1;
            }
            match task.status.as_str() {
                "done" => filings.done += 1,
                "retired" => filings.retired += 1,
                "rejected" => filings.rejected += 1,
                _ => filings.open += 1,
            }
        }
        if let Some(result) = &review.result
            && in_window(times.completed_at.as_deref())
        {
            let results = &mut models
                .entry(model_key(result.author_model.as_deref()))
                .or_default()
                .1;
            results.claims.total += 1;
            count_verdict(&mut results.claims, result);
        }
        for model in times.reopened {
            models.entry(model).or_default().1.reopened += 1;
        }
    }
    Ok(ModelReport {
        schema: "papertiger.model_report.v1",
        plan: plan.map(str::to_owned),
        since: since.map(str::to_owned),
        models: models
            .into_iter()
            .map(|(model, (filings, results))| ModelOutcomes {
                small_sample: filings.claims.total + results.claims.total < SMALL_SAMPLE,
                model,
                filings,
                results,
            })
            .collect(),
    })
}
