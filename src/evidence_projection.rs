//! Bounded, filterable projection over one complete live evidence observation.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, anyhow, bail};
use rusqlite::Connection;
use serde::Serialize;

use crate::evidence::{
    CorrectiveCommand, EvidenceBindingVerification, EvidenceClassification, verify_all_evidence,
};
use crate::{sha256, validate_sha256};

pub const DEFAULT_EVIDENCE_PAGE: usize = 50;
pub const MAX_EVIDENCE_PAGE: usize = 500;
const EVIDENCE_CURSOR_PREFIX: &str = "evidence-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOutcomeFilter {
    All,
    Incomplete,
    Verified,
    Failed,
    Unsupported,
}

impl EvidenceOutcomeFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Incomplete => "incomplete",
            Self::Verified => "verified",
            Self::Failed => "failed",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTaskStateFilter {
    All,
    Open,
    Terminal,
}

impl EvidenceTaskStateFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Open => "open",
            Self::Terminal => "terminal",
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvidenceVerificationOptions {
    pub task_seq: Option<i64>,
    pub outcome: EvidenceOutcomeFilter,
    pub task_state: EvidenceTaskStateFilter,
    pub limit: usize,
    pub after_cursor: Option<String>,
}

impl Default for EvidenceVerificationOptions {
    fn default() -> Self {
        Self {
            task_seq: None,
            outcome: EvidenceOutcomeFilter::Incomplete,
            task_state: EvidenceTaskStateFilter::All,
            limit: DEFAULT_EVIDENCE_PAGE,
            after_cursor: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceVerificationReport {
    pub schema: String,
    pub project_root: String,
    pub task_seq: Option<i64>,
    pub summary: EvidenceVerificationSummary,
    pub projection: EvidenceVerificationProjection,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceVerificationSummary {
    pub binding_count: usize,
    pub verified_count: usize,
    pub failed_count: usize,
    pub unsupported_count: usize,
    pub verification_complete: bool,
    pub status_counts: BTreeMap<String, usize>,
    pub unsupported_scheme_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceVerificationProjection {
    pub scope: String,
    pub ordering: String,
    pub outcome: EvidenceOutcomeFilter,
    pub task_state: EvidenceTaskStateFilter,
    pub eligible_count: usize,
    pub page_start: usize,
    pub returned_count: usize,
    pub omitted_count: usize,
    pub remaining_count: usize,
    pub complete: bool,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation_command: Option<CorrectiveCommand>,
    pub bindings: Vec<EvidenceBindingVerification>,
}

pub fn verify_evidence(
    conn: &Connection,
    project_root: &Path,
    options: &EvidenceVerificationOptions,
) -> Result<EvidenceVerificationReport> {
    if !(1..=MAX_EVIDENCE_PAGE).contains(&options.limit) {
        bail!("evidence verify --limit must be between 1 and {MAX_EVIDENCE_PAGE}");
    }
    let (project_root, bindings) = verify_all_evidence(conn, project_root, options.task_seq)?;
    let verified_count = bindings
        .iter()
        .filter(|binding| binding.classification == EvidenceClassification::Verified)
        .count();
    let unsupported_count = bindings
        .iter()
        .filter(|binding| binding.classification == EvidenceClassification::Unsupported)
        .count();
    let failed_count = bindings.len() - verified_count - unsupported_count;
    let mut status_counts = BTreeMap::new();
    let mut unsupported_scheme_counts = BTreeMap::new();
    for binding in &bindings {
        *status_counts.entry(binding.status.clone()).or_insert(0) += 1;
        if binding.classification == EvidenceClassification::Unsupported {
            *unsupported_scheme_counts
                .entry(binding.scheme.clone().unwrap_or_else(|| "<missing>".into()))
                .or_insert(0) += 1;
        }
    }
    let filtered = bindings
        .into_iter()
        .filter(|binding| binding_matches(binding, options))
        .collect::<Vec<_>>();
    let page_start = options
        .after_cursor
        .as_deref()
        .map(|cursor| validate_evidence_cursor(cursor, &project_root, options, &filtered))
        .transpose()?
        .unwrap_or(0);
    let eligible_count = filtered.len();
    let page = filtered
        .iter()
        .skip(page_start)
        .take(options.limit)
        .cloned()
        .collect::<Vec<_>>();
    let returned_count = page.len();
    let next_start = page_start.saturating_add(returned_count);
    let remaining_count = eligible_count.saturating_sub(next_start);
    let omitted_count = eligible_count.saturating_sub(returned_count);
    let has_more = remaining_count > 0;
    let next_cursor = has_more
        .then(|| evidence_cursor(&project_root, options, &filtered, next_start))
        .transpose()?;
    let task_scope = options
        .task_seq
        .map(|seq| format!(" on task #{seq}"))
        .unwrap_or_default();
    Ok(EvidenceVerificationReport {
        schema: "papertiger.evidence_verification.v2".into(),
        project_root,
        task_seq: options.task_seq,
        summary: EvidenceVerificationSummary {
            binding_count: verified_count + failed_count + unsupported_count,
            verified_count,
            failed_count,
            unsupported_count,
            verification_complete: failed_count == 0 && unsupported_count == 0,
            status_counts,
            unsupported_scheme_counts,
        },
        projection: EvidenceVerificationProjection {
            scope: format!(
                "resolved gates and blockers{task_scope}; outcome={}; task_state={}",
                options.outcome.as_str(),
                options.task_state.as_str()
            ),
            ordering: "task_seq asc, entity asc, name asc".into(),
            outcome: options.outcome,
            task_state: options.task_state,
            eligible_count,
            page_start,
            returned_count,
            omitted_count,
            remaining_count,
            complete: omitted_count == 0,
            has_more,
            next_cursor,
            continuation_command: None,
            bindings: page,
        },
    })
}

fn binding_matches(
    binding: &EvidenceBindingVerification,
    options: &EvidenceVerificationOptions,
) -> bool {
    let outcome_matches = match options.outcome {
        EvidenceOutcomeFilter::All => true,
        EvidenceOutcomeFilter::Incomplete => {
            binding.classification != EvidenceClassification::Verified
        }
        EvidenceOutcomeFilter::Verified => {
            binding.classification == EvidenceClassification::Verified
        }
        EvidenceOutcomeFilter::Failed => binding.classification == EvidenceClassification::Failed,
        EvidenceOutcomeFilter::Unsupported => {
            binding.classification == EvidenceClassification::Unsupported
        }
    };
    let task_state_matches = match options.task_state {
        EvidenceTaskStateFilter::All => true,
        EvidenceTaskStateFilter::Open => {
            matches!(binding.task_status.as_str(), "proposed" | "in_progress")
        }
        EvidenceTaskStateFilter::Terminal => {
            matches!(
                binding.task_status.as_str(),
                "done" | "retired" | "rejected"
            )
        }
    };
    outcome_matches && task_state_matches
}

fn evidence_cursor(
    project_root: &str,
    options: &EvidenceVerificationOptions,
    filtered: &[EvidenceBindingVerification],
    page_start: usize,
) -> Result<String> {
    let bytes = serde_json::to_vec(&(
        project_root,
        options.task_seq,
        options.outcome,
        options.task_state,
        filtered,
        page_start,
    ))?;
    Ok(format!(
        "{EVIDENCE_CURSOR_PREFIX}:{page_start}:{}",
        sha256(&bytes)
    ))
}

fn validate_evidence_cursor(
    token: &str,
    project_root: &str,
    options: &EvidenceVerificationOptions,
    filtered: &[EvidenceBindingVerification],
) -> Result<usize> {
    let mut parts = token.split(':');
    let prefix = parts.next();
    let page_start = parts.next();
    let digest = parts.next();
    if prefix != Some(EVIDENCE_CURSOR_PREFIX)
        || page_start.is_none()
        || digest.is_none()
        || parts.next().is_some()
    {
        return Err(invalid_cursor());
    }
    let page_start = page_start.unwrap_or_default();
    if page_start.is_empty()
        || page_start.starts_with('0')
        || !page_start.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_cursor());
    }
    let page_start = page_start.parse::<usize>().map_err(|_| invalid_cursor())?;
    if page_start == 0 || page_start > filtered.len() {
        return Err(invalid_cursor());
    }
    let digest = digest.unwrap_or_default();
    validate_sha256(digest, "evidence cursor digest").map_err(|_| invalid_cursor())?;
    let actual = evidence_cursor(project_root, options, filtered, page_start)?;
    if actual != token {
        return Err(invalid_cursor());
    }
    Ok(page_start)
}

fn invalid_cursor() -> anyhow::Error {
    anyhow!(
        "evidence cursor is invalid for this live verification scope; discard it and rerun `papertiger evidence verify --json` with the intended authority and filters"
    )
}
