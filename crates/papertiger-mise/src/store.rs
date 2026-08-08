use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{
    Connection, ErrorCode, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::budget::{BudgetLimit, BudgetResource};
use crate::digest::{sha256, validate_sha256};
use crate::manifest::{CAMPAIGN_SCHEMA_V1, CampaignManifest};
use crate::validation::validate_nonblank;

pub const SCHEMA_VERSION: i64 = 7;

const MATERIALIZATION_RETRY_SCHEMA_V7: &str = r#"
DROP TRIGGER budget_reservation_uses_no_update;
DROP TRIGGER budget_reservation_uses_no_delete;
ALTER TABLE budget_reservation_uses RENAME TO budget_reservation_uses_v6;
CREATE TABLE budget_reservation_uses (
  campaign_id TEXT NOT NULL REFERENCES campaigns(campaign_id),
  reservation_id TEXT NOT NULL,
  use_kind TEXT NOT NULL,
  entity_key TEXT NOT NULL,
  bound_at TEXT NOT NULL,
  PRIMARY KEY (campaign_id, reservation_id)
);
INSERT INTO budget_reservation_uses
  (campaign_id, reservation_id, use_kind, entity_key, bound_at)
SELECT campaign_id, reservation_id, use_kind, entity_key, bound_at
FROM budget_reservation_uses_v6;
DROP TABLE budget_reservation_uses_v6;
CREATE INDEX idx_budget_reservation_uses_entity
  ON budget_reservation_uses(campaign_id, use_kind, entity_key);
CREATE TRIGGER budget_reservation_uses_no_update BEFORE UPDATE ON budget_reservation_uses
BEGIN SELECT RAISE(ABORT, 'budget reservation use is immutable'); END;
CREATE TRIGGER budget_reservation_uses_no_delete BEFORE DELETE ON budget_reservation_uses
BEGIN SELECT RAISE(ABORT, 'budget reservation use is immutable'); END;
"#;

const PAIRED_ANALYSIS_SCHEMA_V2: &str = r#"
CREATE TABLE paired_analysis_slots (
  campaign_id TEXT NOT NULL REFERENCES campaigns(campaign_id),
  slot INTEGER NOT NULL CHECK (slot > 0),
  candidate_id TEXT NOT NULL UNIQUE REFERENCES candidates(candidate_id),
  schedule_sha256 TEXT NOT NULL,
  order_seed_reveal BLOB NOT NULL,
  reserved_at TEXT NOT NULL,
  PRIMARY KEY (campaign_id, slot)
);
CREATE TRIGGER paired_analysis_slot_campaign_guard BEFORE INSERT ON paired_analysis_slots
WHEN NOT EXISTS (
  SELECT 1 FROM candidates c
  WHERE c.candidate_id=NEW.candidate_id AND c.campaign_id=NEW.campaign_id
)
BEGIN SELECT RAISE(ABORT, 'paired analysis slot candidate belongs to another campaign'); END;
CREATE TRIGGER paired_analysis_slot_identity_no_update
BEFORE UPDATE OF campaign_id, slot, candidate_id, schedule_sha256, order_seed_reveal, reserved_at
ON paired_analysis_slots
BEGIN SELECT RAISE(ABORT, 'paired analysis slot identity is immutable'); END;
CREATE TRIGGER paired_analysis_slots_no_delete BEFORE DELETE ON paired_analysis_slots
BEGIN SELECT RAISE(ABORT, 'paired analysis slot history is immutable'); END;
"#;

const PAIRED_EVIDENCE_SCHEMA_V3: &str = r#"
CREATE TABLE paired_adapter_observations (
  evidence_id TEXT PRIMARY KEY,
  scope TEXT NOT NULL CHECK (scope IN ('historical_shadow','live_campaign')),
  campaign_id TEXT REFERENCES campaigns(campaign_id),
  candidate_id TEXT REFERENCES candidates(candidate_id),
  analysis_slot INTEGER CHECK (analysis_slot IS NULL OR analysis_slot > 0),
  binding_sha256 TEXT NOT NULL,
  binding_json TEXT NOT NULL,
  request_sha256 TEXT NOT NULL REFERENCES artifacts(sha256),
  result_sha256 TEXT NOT NULL REFERENCES artifacts(sha256),
  receipt_sha256 TEXT NOT NULL UNIQUE REFERENCES artifacts(sha256),
  decision_eligible INTEGER NOT NULL CHECK (decision_eligible IN (0,1)),
  recorded_by TEXT NOT NULL,
  recorded_at TEXT NOT NULL,
  CHECK (
    (scope='historical_shadow' AND decision_eligible=0 AND campaign_id IS NULL
      AND candidate_id IS NULL AND analysis_slot IS NULL) OR
    (scope='live_campaign' AND decision_eligible=1 AND campaign_id IS NOT NULL
      AND candidate_id IS NOT NULL AND analysis_slot IS NOT NULL)
  )
);
CREATE TABLE paired_adapter_blocks (
  evidence_id TEXT NOT NULL REFERENCES paired_adapter_observations(evidence_id),
  block_index INTEGER NOT NULL CHECK (block_index >= 0),
  observed_order TEXT CHECK (observed_order IN ('baseline_first','candidate_first')),
  block_json TEXT NOT NULL,
  PRIMARY KEY (evidence_id, block_index)
);
CREATE TABLE paired_domain_trial_receipts (
  receipt_sha256 TEXT PRIMARY KEY,
  evidence_id TEXT NOT NULL REFERENCES paired_adapter_observations(evidence_id),
  block_index INTEGER NOT NULL,
  participant TEXT NOT NULL CHECK (participant IN ('baseline','candidate')),
  UNIQUE (evidence_id, block_index, participant),
  FOREIGN KEY (evidence_id, block_index)
    REFERENCES paired_adapter_blocks(evidence_id, block_index)
);
CREATE TRIGGER paired_adapter_observations_no_update BEFORE UPDATE ON paired_adapter_observations
BEGIN SELECT RAISE(ABORT, 'paired adapter evidence is immutable'); END;
CREATE TRIGGER paired_adapter_observations_no_delete BEFORE DELETE ON paired_adapter_observations
BEGIN SELECT RAISE(ABORT, 'paired adapter evidence is immutable'); END;
CREATE TRIGGER paired_adapter_blocks_no_update BEFORE UPDATE ON paired_adapter_blocks
BEGIN SELECT RAISE(ABORT, 'paired adapter blocks are immutable'); END;
CREATE TRIGGER paired_adapter_blocks_no_delete BEFORE DELETE ON paired_adapter_blocks
BEGIN SELECT RAISE(ABORT, 'paired adapter blocks are immutable'); END;
CREATE TRIGGER paired_domain_trial_receipts_no_update BEFORE UPDATE ON paired_domain_trial_receipts
BEGIN SELECT RAISE(ABORT, 'paired domain trial receipts are immutable'); END;
CREATE TRIGGER paired_domain_trial_receipts_no_delete BEFORE DELETE ON paired_domain_trial_receipts
BEGIN SELECT RAISE(ABORT, 'paired domain trial receipts are immutable'); END;
"#;

const DOMAIN_SHADOW_SCHEMA_V4: &str = r#"
CREATE TABLE domain_shadows (
  evidence_id TEXT PRIMARY KEY,
  binding_sha256 TEXT NOT NULL,
  binding_json TEXT NOT NULL,
  request_sha256 TEXT NOT NULL REFERENCES artifacts(sha256),
  result_sha256 TEXT NOT NULL REFERENCES artifacts(sha256),
  receipt_sha256 TEXT NOT NULL UNIQUE REFERENCES artifacts(sha256),
  state_before_sha256 TEXT NOT NULL,
  state_after_sha256 TEXT NOT NULL,
  decision_eligible INTEGER NOT NULL CHECK (decision_eligible=0),
  adjudication TEXT NOT NULL CHECK (adjudication='prohibited'),
  recorded_by TEXT NOT NULL,
  recorded_at TEXT NOT NULL,
  CHECK (state_before_sha256=state_after_sha256)
);
CREATE TRIGGER domain_shadows_no_update BEFORE UPDATE ON domain_shadows
BEGIN SELECT RAISE(ABORT, 'domain shadows are immutable'); END;
CREATE TRIGGER domain_shadows_no_delete BEFORE DELETE ON domain_shadows
BEGIN SELECT RAISE(ABORT, 'domain shadows are immutable'); END;
"#;

const SUCCESSOR_SCHEMA_V6: &str = r#"
CREATE TABLE successor_admissions (
  child_campaign_id TEXT PRIMARY KEY REFERENCES campaigns(campaign_id),
  parent_campaign_id TEXT NOT NULL REFERENCES campaigns(campaign_id),
  parent_manifest_sha256 TEXT NOT NULL,
  parent_nomination_id TEXT NOT NULL REFERENCES nominations(nomination_id),
  promoted_candidate_id TEXT NOT NULL REFERENCES candidates(candidate_id),
  promoted_source_tree TEXT NOT NULL,
  parent_promotion_proof_sha256 TEXT NOT NULL UNIQUE REFERENCES artifacts(sha256),
  parent_promotion_proof_locator TEXT NOT NULL UNIQUE,
  successor_manifest_sha256 TEXT NOT NULL,
  parent_runtime_generation INTEGER NOT NULL CHECK (parent_runtime_generation >= 0),
  successor_runtime_generation INTEGER NOT NULL CHECK (successor_runtime_generation > 0),
  parent_recursion_depth INTEGER NOT NULL CHECK (parent_recursion_depth >= 0),
  successor_recursion_depth INTEGER NOT NULL CHECK (successor_recursion_depth > 0),
  parent_budget_debit_json TEXT NOT NULL,
  papertiger_plan_slug TEXT NOT NULL,
  papertiger_task_seq INTEGER NOT NULL CHECK (papertiger_task_seq > 0),
  papertiger_gate_name TEXT NOT NULL,
  papertiger_gate_closed_at TEXT NOT NULL,
  admitted_at TEXT NOT NULL,
  admitted_by TEXT NOT NULL,
  CHECK (successor_runtime_generation = parent_runtime_generation + 1),
  CHECK (successor_recursion_depth = parent_recursion_depth + 1)
);
CREATE TRIGGER successor_admissions_no_update BEFORE UPDATE ON successor_admissions
BEGIN SELECT RAISE(ABORT, 'successor admission history is immutable'); END;
CREATE TRIGGER successor_admissions_no_delete BEFORE DELETE ON successor_admissions
BEGIN SELECT RAISE(ABORT, 'successor admission history is immutable'); END;
"#;

const PAIRED_RUNTIME_SCHEMA_V5: &str = r#"
CREATE TABLE paired_cohorts (
  cohort_id TEXT PRIMARY KEY,
  campaign_id TEXT NOT NULL REFERENCES campaigns(campaign_id),
  candidate_id TEXT NOT NULL REFERENCES candidates(candidate_id),
  cohort_kind TEXT NOT NULL CHECK (cohort_kind IN ('research','no_op_calibration','known_bad_calibration')),
  analysis_slot INTEGER CHECK (analysis_slot IS NULL OR analysis_slot > 0),
  schedule_sha256 TEXT NOT NULL,
  order_seed_reveal BLOB NOT NULL,
  participants_json TEXT NOT NULL,
  reservation_id TEXT NOT NULL,
  adapter_binding_sha256 TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'prepared'
    CHECK (status IN ('prepared','running','qualified','rejected','inconclusive','calibrated','infrastructure_failed','integrity_failed')),
  result_sha256 TEXT REFERENCES artifacts(sha256),
  receipt_sha256 TEXT UNIQUE REFERENCES artifacts(sha256),
  reason_code TEXT,
  recorded_by TEXT NOT NULL,
  created_at TEXT NOT NULL,
  finished_at TEXT,
  UNIQUE (campaign_id, cohort_kind, candidate_id),
  CHECK (
    (cohort_kind='research' AND analysis_slot IS NOT NULL) OR
    (cohort_kind!='research' AND analysis_slot IS NULL)
  ),
  CHECK (
    (status IN ('prepared','running') AND result_sha256 IS NULL AND receipt_sha256 IS NULL
      AND reason_code IS NULL AND finished_at IS NULL) OR
    (status NOT IN ('prepared','running') AND receipt_sha256 IS NOT NULL
      AND reason_code IS NOT NULL AND finished_at IS NOT NULL)
  )
);
CREATE TABLE paired_runs (
  execution_id TEXT PRIMARY KEY,
  cohort_id TEXT NOT NULL REFERENCES paired_cohorts(cohort_id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  block_index INTEGER NOT NULL CHECK (block_index >= 0),
  participant TEXT NOT NULL CHECK (participant IN ('baseline','candidate')),
  request_sha256 TEXT NOT NULL REFERENCES artifacts(sha256),
  status TEXT NOT NULL DEFAULT 'prepared'
    CHECK (status IN ('prepared','launched','succeeded','infrastructure_failed','integrity_failed')),
  pid INTEGER,
  process_birth_identity TEXT,
  heartbeat_at TEXT,
  adapter_result_sha256 TEXT REFERENCES artifacts(sha256),
  domain_receipt_sha256 TEXT UNIQUE REFERENCES artifacts(sha256),
  execution_receipt_sha256 TEXT UNIQUE REFERENCES artifacts(sha256),
  elapsed_ms INTEGER CHECK (elapsed_ms IS NULL OR elapsed_ms > 0),
  failure_code TEXT,
  created_at TEXT NOT NULL,
  finished_at TEXT,
  UNIQUE (cohort_id, ordinal),
  UNIQUE (cohort_id, block_index, participant),
  CHECK (
    (status='prepared' AND pid IS NULL AND process_birth_identity IS NULL AND heartbeat_at IS NULL
      AND adapter_result_sha256 IS NULL AND domain_receipt_sha256 IS NULL
      AND execution_receipt_sha256 IS NULL AND elapsed_ms IS NULL
      AND failure_code IS NULL AND finished_at IS NULL) OR
    (status='launched' AND pid IS NOT NULL AND process_birth_identity IS NOT NULL
      AND heartbeat_at IS NOT NULL AND adapter_result_sha256 IS NULL
      AND domain_receipt_sha256 IS NULL AND execution_receipt_sha256 IS NULL
      AND elapsed_ms IS NULL AND failure_code IS NULL AND finished_at IS NULL) OR
    (status='succeeded' AND pid IS NOT NULL AND process_birth_identity IS NOT NULL
      AND adapter_result_sha256 IS NOT NULL AND domain_receipt_sha256 IS NOT NULL
      AND execution_receipt_sha256 IS NOT NULL AND elapsed_ms IS NOT NULL
      AND failure_code IS NULL AND finished_at IS NOT NULL) OR
    (status IN ('infrastructure_failed','integrity_failed')
      AND execution_receipt_sha256 IS NOT NULL AND failure_code IS NOT NULL
      AND finished_at IS NOT NULL)
  )
);
CREATE TABLE paired_live_domain_receipts (
  receipt_sha256 TEXT PRIMARY KEY REFERENCES artifacts(sha256),
  execution_id TEXT NOT NULL UNIQUE REFERENCES paired_runs(execution_id)
);
CREATE TRIGGER paired_cohort_identity_no_update
BEFORE UPDATE OF cohort_id, campaign_id, candidate_id, cohort_kind, analysis_slot,
  schedule_sha256, order_seed_reveal, participants_json, reservation_id, adapter_binding_sha256,
  recorded_by, created_at ON paired_cohorts
BEGIN SELECT RAISE(ABORT, 'paired cohort identity is immutable'); END;
CREATE TRIGGER paired_cohort_transition_guard BEFORE UPDATE ON paired_cohorts
WHEN NOT (
  (OLD.status='prepared' AND NEW.status='running') OR
  (OLD.status='prepared' AND NEW.status IN ('infrastructure_failed','integrity_failed')) OR
  (OLD.status='running' AND NEW.status='running') OR
  (OLD.status='running' AND NEW.status NOT IN ('prepared','running'))
)
BEGIN SELECT RAISE(ABORT, 'invalid paired cohort lifecycle transition'); END;
CREATE TRIGGER paired_cohorts_no_delete BEFORE DELETE ON paired_cohorts
BEGIN SELECT RAISE(ABORT, 'paired cohort history is immutable'); END;
CREATE TRIGGER paired_run_identity_no_update
BEFORE UPDATE OF execution_id, cohort_id, ordinal, block_index, participant,
  request_sha256, created_at ON paired_runs
BEGIN SELECT RAISE(ABORT, 'paired run identity is immutable'); END;
CREATE TRIGGER paired_run_transition_guard BEFORE UPDATE ON paired_runs
WHEN NOT (
  (OLD.status='prepared' AND NEW.status='launched') OR
  (OLD.status='prepared' AND NEW.status IN ('infrastructure_failed','integrity_failed')) OR
  (OLD.status='launched' AND NEW.status='launched') OR
  (OLD.status='launched' AND NEW.status IN ('succeeded','infrastructure_failed','integrity_failed'))
)
BEGIN SELECT RAISE(ABORT, 'invalid paired run lifecycle transition'); END;
CREATE TRIGGER paired_runs_no_delete BEFORE DELETE ON paired_runs
BEGIN SELECT RAISE(ABORT, 'paired run history is immutable'); END;
CREATE TRIGGER paired_live_domain_receipts_no_update BEFORE UPDATE ON paired_live_domain_receipts
BEGIN SELECT RAISE(ABORT, 'paired live domain receipt claims are immutable'); END;
CREATE TRIGGER paired_live_domain_receipts_no_delete BEFORE DELETE ON paired_live_domain_receipts
BEGIN SELECT RAISE(ABORT, 'paired live domain receipt claims are immutable'); END;
CREATE TRIGGER paired_live_domain_receipt_global_guard BEFORE INSERT ON paired_live_domain_receipts
WHEN EXISTS (
  SELECT 1 FROM paired_domain_trial_receipts old WHERE old.receipt_sha256=NEW.receipt_sha256
)
BEGIN SELECT RAISE(ABORT, 'paired domain trial receipt is already claimed by shadow evidence'); END;
CREATE TRIGGER paired_shadow_domain_receipt_global_guard BEFORE INSERT ON paired_domain_trial_receipts
WHEN EXISTS (
  SELECT 1 FROM paired_live_domain_receipts live WHERE live.receipt_sha256=NEW.receipt_sha256
)
BEGIN SELECT RAISE(ABORT, 'paired domain trial receipt is already claimed by live evidence'); END;
"#;

#[derive(Debug, Clone)]
pub(crate) struct CampaignAdmission {
    campaign_id: String,
    manifest_schema: String,
    manifest_sha256: String,
    manifest_json: String,
    limits: Vec<BudgetLimit>,
}

impl CampaignAdmission {
    pub(crate) fn from_manifest(manifest: &CampaignManifest) -> Result<Self> {
        let canonical = manifest.canonical_bytes()?;
        let manifest_json = String::from_utf8(canonical)
            .context("canonical campaign manifest must be UTF-8 JSON")?;
        let canonical_manifest: CampaignManifest = serde_json::from_str(&manifest_json)?;
        Ok(Self {
            campaign_id: manifest.campaign_id.clone(),
            manifest_schema: CAMPAIGN_SCHEMA_V1.to_owned(),
            manifest_sha256: sha256(manifest_json.as_bytes()),
            manifest_json,
            limits: canonical_manifest.budgets.caps,
        })
    }

    pub(crate) fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CampaignRecord {
    pub campaign_id: String,
    pub manifest_schema: String,
    pub manifest_sha256: String,
    pub manifest_json: String,
    pub admitted_at: String,
    pub admitted_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CampaignSummary {
    pub campaign_id: String,
    pub manifest_sha256: String,
    pub admitted_at: String,
}

/// Bounded orientation over one initialized Mise authority. It deliberately
/// reports durable state rather than inferring whether any candidate should be
/// promoted or integrated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthorityStatus {
    pub schema: &'static str,
    pub schema_version: i64,
    pub campaign_count: i64,
    pub recent_campaigns: Vec<CampaignSummary>,
    pub recent_campaigns_truncated: bool,
    pub candidate_dispositions: BTreeMap<String, i64>,
    pub nomination_count: i64,
    pub open_reservation_count: i64,
    pub active_trial_count: i64,
    pub active_paired_cohort_count: i64,
    pub integrity_failure_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuccessorAdmissionRecord {
    pub child_campaign_id: String,
    pub parent_campaign_id: String,
    pub parent_manifest_sha256: String,
    pub parent_nomination_id: String,
    pub promoted_candidate_id: String,
    pub promoted_source_tree: String,
    pub parent_promotion_proof_sha256: String,
    pub parent_promotion_proof_locator: String,
    pub successor_manifest_sha256: String,
    pub parent_runtime_generation: u32,
    pub successor_runtime_generation: u32,
    pub parent_recursion_depth: u32,
    pub successor_recursion_depth: u32,
    pub parent_budget_debit: Vec<BudgetLimit>,
    pub papertiger_plan_slug: String,
    pub papertiger_task_seq: i64,
    pub papertiger_gate_name: String,
    pub papertiger_gate_closed_at: String,
    pub admitted_at: String,
    pub admitted_by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub event_id: i64,
    pub at: String,
    pub actor: String,
    pub entity: String,
    pub entity_key: String,
    pub kind: String,
    pub why: Option<String>,
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionOutcome {
    Admitted,
    Existing,
}

fn configure_connection(connection: Connection) -> Result<Connection> {
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.busy_timeout(Duration::ZERO)?;
    Ok(connection)
}

pub fn open_for_init(path: impl AsRef<Path>) -> Result<Connection> {
    let path = path.as_ref();
    let connection = Connection::open(path)
        .with_context(|| format!("open {} for papertiger-mise initialization", path.display()))?;
    configure_connection(connection)
}

pub fn open_existing(path: impl AsRef<Path>) -> Result<Connection> {
    open_existing_with_flags(path.as_ref(), OpenFlags::SQLITE_OPEN_READ_WRITE)
}

pub fn open_existing_read_only(path: impl AsRef<Path>) -> Result<Connection> {
    open_existing_with_flags(path.as_ref(), OpenFlags::SQLITE_OPEN_READ_ONLY)
}

fn open_existing_with_flags(path: &Path, flags: OpenFlags) -> Result<Connection> {
    let connection = Connection::open_with_flags(path, flags)
        .with_context(|| format!("open existing papertiger-mise database {}", path.display()))?;
    let connection = configure_connection(connection)?;
    let has_meta = connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='meta'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !has_meta {
        bail!(
            "{} is not an initialized papertiger-mise database; run `papertiger-mise --db {} init`",
            path.display(),
            path.display()
        );
    }
    let version = schema_version(&connection)?;
    if version != SCHEMA_VERSION {
        bail!(
            "{} uses papertiger-mise schema v{version}; run `papertiger-mise --db {} init` explicitly to upgrade to v{SCHEMA_VERSION}",
            path.display(),
            path.display()
        );
    }
    Ok(connection)
}

fn schema_version(connection: &Connection) -> Result<i64> {
    let raw: String = connection.query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    raw.parse()
        .map_err(|_| anyhow!("corrupt papertiger-mise schema_version '{raw}'"))
}

pub fn begin_mutation(connection: &Connection) -> Result<Transaction<'_>> {
    match Transaction::new_unchecked(connection, TransactionBehavior::Immediate) {
        Ok(transaction) => Ok(transaction),
        Err(error)
            if matches!(
                error.sqlite_error_code(),
                Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
            ) =>
        {
            bail!(
                "papertiger-mise mutation admission refused: the SQLite writer reservation is already held"
            )
        }
        Err(error) => Err(error).context("begin papertiger-mise mutation"),
    }
}

pub fn init(connection: &Connection) -> Result<()> {
    let has_meta = connection
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='meta'",
            [],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if has_meta {
        migrate(connection, schema_version(connection)?)?;
        return Ok(());
    }

    let initial_page_count: i64 =
        connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let transaction = begin_mutation(connection)?;
    let foreign_object: Option<(String, String)> = transaction
        .query_row(
            "SELECT type, name FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name
             LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((object_type, name)) = foreign_object {
        bail!(
            "refusing to initialize nonempty database without papertiger-mise metadata: found {object_type} '{name}'"
        );
    }
    if initial_page_count != 0 {
        bail!(
            "refusing to initialize nonempty database without papertiger-mise metadata: found {initial_page_count} allocated page(s)"
        );
    }

    transaction.execute_batch(
        r#"
CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE campaigns (
  campaign_id TEXT PRIMARY KEY,
  manifest_schema TEXT NOT NULL,
  manifest_sha256 TEXT NOT NULL,
  manifest_json TEXT NOT NULL,
  admitted_at TEXT NOT NULL,
  admitted_by TEXT NOT NULL
);
CREATE TABLE budget_limits (
  campaign_id TEXT NOT NULL REFERENCES campaigns(campaign_id),
  resource TEXT NOT NULL,
  unit TEXT NOT NULL,
  hard_limit INTEGER NOT NULL CHECK (hard_limit > 0),
  PRIMARY KEY (campaign_id, resource)
);
CREATE TABLE budget_balances (
  campaign_id TEXT NOT NULL,
  resource TEXT NOT NULL,
  reserved_amount INTEGER NOT NULL DEFAULT 0 CHECK (reserved_amount >= 0),
  spent_amount INTEGER NOT NULL DEFAULT 0 CHECK (spent_amount >= 0),
  PRIMARY KEY (campaign_id, resource),
  FOREIGN KEY (campaign_id, resource)
    REFERENCES budget_limits(campaign_id, resource)
);
CREATE TABLE budget_reservations (
  reservation_id TEXT NOT NULL,
  campaign_id TEXT NOT NULL,
  resource TEXT NOT NULL,
  unit TEXT NOT NULL,
  reserved_amount INTEGER NOT NULL CHECK (reserved_amount > 0),
  settled_amount INTEGER CHECK (settled_amount >= 0),
  status TEXT NOT NULL DEFAULT 'reserved'
    CHECK (status IN ('reserved','settled','charged')),
  created_at TEXT NOT NULL,
  settled_at TEXT,
  note TEXT,
  PRIMARY KEY (reservation_id, campaign_id, resource),
  FOREIGN KEY (campaign_id, resource)
    REFERENCES budget_limits(campaign_id, resource)
);
CREATE TABLE budget_reservation_uses (
  campaign_id TEXT NOT NULL REFERENCES campaigns(campaign_id),
  reservation_id TEXT NOT NULL,
  use_kind TEXT NOT NULL,
  entity_key TEXT NOT NULL,
  bound_at TEXT NOT NULL,
  PRIMARY KEY (campaign_id, reservation_id)
);
CREATE TABLE events (
  event_id INTEGER PRIMARY KEY,
  at TEXT NOT NULL,
  actor TEXT NOT NULL,
  entity TEXT NOT NULL,
  entity_key TEXT NOT NULL,
  kind TEXT NOT NULL,
  why TEXT,
  payload TEXT
);
CREATE TABLE artifacts (
  sha256 TEXT PRIMARY KEY,
  bytes INTEGER NOT NULL CHECK (bytes >= 0),
  locator TEXT NOT NULL UNIQUE,
  media_type TEXT NOT NULL,
  recorded_at TEXT NOT NULL
);
CREATE TABLE candidates (
  candidate_id TEXT PRIMARY KEY,
  campaign_id TEXT NOT NULL REFERENCES campaigns(campaign_id),
  proposal_json TEXT NOT NULL,
  patch_sha256 TEXT NOT NULL REFERENCES artifacts(sha256),
  negative_fingerprint TEXT NOT NULL,
  disposition TEXT NOT NULL DEFAULT 'proposed'
    CHECK (disposition IN ('proposed','materialized','evaluating','rejected','inconclusive','infrastructure_failed','nominated')),
  result_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE candidate_parents (
  candidate_id TEXT NOT NULL REFERENCES candidates(candidate_id),
  parent_candidate_id TEXT NOT NULL,
  PRIMARY KEY (candidate_id, parent_candidate_id)
);
CREATE TABLE candidate_artifacts (
  candidate_id TEXT NOT NULL REFERENCES candidates(candidate_id),
  role TEXT NOT NULL,
  sha256 TEXT NOT NULL REFERENCES artifacts(sha256),
  PRIMARY KEY (candidate_id, role)
);
CREATE TABLE materializations (
  candidate_id TEXT PRIMARY KEY REFERENCES candidates(candidate_id),
  reservation_id TEXT NOT NULL,
  receipt_sha256 TEXT NOT NULL UNIQUE REFERENCES artifacts(sha256),
  result_tree TEXT NOT NULL,
  worktree_locator TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL
);
CREATE TABLE trials (
  trial_id TEXT PRIMARY KEY,
  campaign_id TEXT NOT NULL REFERENCES campaigns(campaign_id),
  candidate_id TEXT NOT NULL REFERENCES candidates(candidate_id),
  materialization_receipt_sha256 TEXT NOT NULL,
  baseline_materialization_receipt_sha256 TEXT NOT NULL,
  result_tree TEXT NOT NULL,
  working_directory TEXT NOT NULL,
  reservation_id TEXT NOT NULL,
  tier TEXT NOT NULL,
  command_json TEXT NOT NULL,
  environment_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'owned'
    CHECK (status IN ('owned','launched','succeeded','rejected','infrastructure_failed','integrity_failed')),
  owner_uuid TEXT,
  pid INTEGER,
  process_birth_identity TEXT,
  supervisor_identity TEXT,
  heartbeat_at TEXT,
  outcome_json TEXT,
  created_at TEXT NOT NULL,
  finished_at TEXT
);
CREATE TABLE measurements (
  trial_id TEXT NOT NULL REFERENCES trials(trial_id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  objective TEXT NOT NULL,
  raw_json TEXT NOT NULL,
  PRIMARY KEY (trial_id, ordinal, objective)
);
CREATE TABLE negative_evidence (
  campaign_id TEXT NOT NULL REFERENCES campaigns(campaign_id),
  negative_fingerprint TEXT NOT NULL,
  candidate_id TEXT NOT NULL REFERENCES candidates(candidate_id),
  reason_code TEXT NOT NULL,
  evidence_sha256 TEXT NOT NULL REFERENCES artifacts(sha256),
  recorded_at TEXT NOT NULL,
  PRIMARY KEY (campaign_id, candidate_id)
);
CREATE INDEX idx_negative_fingerprint
  ON negative_evidence(campaign_id, negative_fingerprint);
CREATE TABLE nominations (
  nomination_id TEXT PRIMARY KEY,
  campaign_id TEXT NOT NULL REFERENCES campaigns(campaign_id),
  candidate_id TEXT NOT NULL UNIQUE REFERENCES candidates(candidate_id),
  receipt_sha256 TEXT NOT NULL UNIQUE,
  receipt_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX idx_events_entity ON events(entity, entity_key, event_id);
CREATE INDEX idx_budget_reservations_campaign
  ON budget_reservations(campaign_id, reservation_id);
CREATE INDEX idx_budget_reservation_uses_entity
  ON budget_reservation_uses(campaign_id, use_kind, entity_key);

CREATE TRIGGER campaigns_no_update BEFORE UPDATE ON campaigns
BEGIN SELECT RAISE(ABORT, 'campaign admissions are immutable'); END;
CREATE TRIGGER campaigns_no_delete BEFORE DELETE ON campaigns
BEGIN SELECT RAISE(ABORT, 'campaign admissions are immutable'); END;
CREATE TRIGGER budget_limits_no_update BEFORE UPDATE ON budget_limits
BEGIN SELECT RAISE(ABORT, 'campaign budget limits are immutable'); END;
CREATE TRIGGER budget_limits_no_delete BEFORE DELETE ON budget_limits
BEGIN SELECT RAISE(ABORT, 'campaign budget limits are immutable'); END;
CREATE TRIGGER budget_reservation_identity_no_update
BEFORE UPDATE OF reservation_id, campaign_id, resource, unit, reserved_amount, created_at
ON budget_reservations BEGIN SELECT RAISE(ABORT, 'budget reservation identity is immutable'); END;
CREATE TRIGGER budget_reservations_no_delete BEFORE DELETE ON budget_reservations
BEGIN SELECT RAISE(ABORT, 'budget reservation history is immutable'); END;
CREATE TRIGGER budget_reservation_transition_guard BEFORE UPDATE ON budget_reservations
WHEN NOT (
  OLD.status='reserved' AND NEW.status IN ('settled','charged') AND
  NEW.settled_amount IS NOT NULL AND NEW.settled_at IS NOT NULL
)
BEGIN SELECT RAISE(ABORT, 'invalid budget reservation transition'); END;
CREATE TRIGGER budget_reservation_uses_no_update BEFORE UPDATE ON budget_reservation_uses
BEGIN SELECT RAISE(ABORT, 'budget reservation use is immutable'); END;
CREATE TRIGGER budget_reservation_uses_no_delete BEFORE DELETE ON budget_reservation_uses
BEGIN SELECT RAISE(ABORT, 'budget reservation use is immutable'); END;
CREATE TRIGGER events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'papertiger-mise events are append-only'); END;
CREATE TRIGGER events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'papertiger-mise events are append-only'); END;
CREATE TRIGGER candidate_identity_no_update
BEFORE UPDATE OF candidate_id, campaign_id, proposal_json, patch_sha256, negative_fingerprint, created_at
ON candidates BEGIN SELECT RAISE(ABORT, 'candidate identity is immutable'); END;
CREATE TRIGGER candidates_no_delete BEFORE DELETE ON candidates
BEGIN SELECT RAISE(ABORT, 'candidate history is immutable'); END;
CREATE TRIGGER candidate_disposition_guard BEFORE UPDATE OF disposition, result_json ON candidates
WHEN NOT (
  (OLD.disposition='proposed' AND NEW.disposition='materialized' AND NEW.result_json IS NULL) OR
  (OLD.disposition IN ('proposed','materialized') AND NEW.disposition='evaluating' AND NEW.result_json IS NULL) OR
  (OLD.disposition='evaluating' AND NEW.disposition IN ('evaluating','rejected','inconclusive','infrastructure_failed','nominated') AND
    ((NEW.disposition='evaluating' AND NEW.result_json IS NULL) OR
     (NEW.disposition!='evaluating' AND NEW.result_json IS NOT NULL)))
)
BEGIN SELECT RAISE(ABORT, 'invalid candidate disposition transition'); END;
CREATE TRIGGER artifacts_no_update BEFORE UPDATE ON artifacts
BEGIN SELECT RAISE(ABORT, 'artifact identity is immutable'); END;
CREATE TRIGGER artifacts_no_delete BEFORE DELETE ON artifacts
BEGIN SELECT RAISE(ABORT, 'artifact identity is immutable'); END;
CREATE TRIGGER candidate_parents_no_update BEFORE UPDATE ON candidate_parents
BEGIN SELECT RAISE(ABORT, 'candidate ancestry is immutable'); END;
CREATE TRIGGER candidate_parents_no_delete BEFORE DELETE ON candidate_parents
BEGIN SELECT RAISE(ABORT, 'candidate ancestry is immutable'); END;
CREATE TRIGGER candidate_artifacts_no_update BEFORE UPDATE ON candidate_artifacts
BEGIN SELECT RAISE(ABORT, 'candidate artifact lineage is immutable'); END;
CREATE TRIGGER candidate_artifacts_no_delete BEFORE DELETE ON candidate_artifacts
BEGIN SELECT RAISE(ABORT, 'candidate artifact lineage is immutable'); END;
CREATE TRIGGER materializations_no_update BEFORE UPDATE ON materializations
BEGIN SELECT RAISE(ABORT, 'candidate materialization is immutable'); END;
CREATE TRIGGER materializations_no_delete BEFORE DELETE ON materializations
BEGIN SELECT RAISE(ABORT, 'candidate materialization is immutable'); END;
CREATE TRIGGER trial_identity_no_update
BEFORE UPDATE OF trial_id, campaign_id, candidate_id, materialization_receipt_sha256,
                 baseline_materialization_receipt_sha256, result_tree, working_directory,
                 reservation_id, tier, command_json, environment_json, created_at
ON trials BEGIN SELECT RAISE(ABORT, 'trial identity is immutable'); END;
CREATE TRIGGER trial_process_ownership_guard
BEFORE UPDATE OF owner_uuid, pid, process_birth_identity, supervisor_identity ON trials
WHEN NOT (
  OLD.status='owned' AND NEW.status='launched' AND
  NEW.owner_uuid=OLD.owner_uuid AND NEW.supervisor_identity=OLD.supervisor_identity AND
  OLD.pid IS NULL AND NEW.pid IS NOT NULL AND
  OLD.process_birth_identity IS NULL AND NEW.process_birth_identity IS NOT NULL
)
BEGIN SELECT RAISE(ABORT, 'invalid trial process-ownership transition'); END;
CREATE TRIGGER trials_no_delete BEFORE DELETE ON trials
BEGIN SELECT RAISE(ABORT, 'trial history is immutable'); END;
CREATE TRIGGER trial_status_guard BEFORE UPDATE OF status, outcome_json, finished_at ON trials
WHEN NOT (
  (OLD.status='owned' AND NEW.status='launched' AND NEW.outcome_json IS NULL) OR
  (OLD.status IN ('owned','launched') AND NEW.status='infrastructure_failed' AND NEW.outcome_json IS NOT NULL AND NEW.finished_at IS NOT NULL) OR
  (OLD.status='launched' AND NEW.status IN ('succeeded','rejected') AND NEW.outcome_json IS NOT NULL AND NEW.finished_at IS NOT NULL) OR
  (OLD.status IN ('owned','launched') AND NEW.status='integrity_failed' AND NEW.outcome_json IS NOT NULL AND NEW.finished_at IS NOT NULL)
)
BEGIN SELECT RAISE(ABORT, 'invalid trial status transition'); END;
CREATE TRIGGER measurements_no_update BEFORE UPDATE ON measurements
BEGIN SELECT RAISE(ABORT, 'raw measurements are immutable'); END;
CREATE TRIGGER measurements_no_delete BEFORE DELETE ON measurements
BEGIN SELECT RAISE(ABORT, 'raw measurements are immutable'); END;
CREATE TRIGGER negative_evidence_no_update BEFORE UPDATE ON negative_evidence
BEGIN SELECT RAISE(ABORT, 'negative evidence is immutable'); END;
CREATE TRIGGER negative_evidence_no_delete BEFORE DELETE ON negative_evidence
BEGIN SELECT RAISE(ABORT, 'negative evidence is immutable'); END;
CREATE TRIGGER nominations_no_update BEFORE UPDATE ON nominations
BEGIN SELECT RAISE(ABORT, 'nomination receipts are immutable'); END;
CREATE TRIGGER nominations_no_delete BEFORE DELETE ON nominations
BEGIN SELECT RAISE(ABORT, 'nomination receipts are immutable'); END;
CREATE TRIGGER nominations_require_qualified_candidate BEFORE INSERT ON nominations
WHEN NOT EXISTS (
  SELECT 1 FROM candidates c
  WHERE c.candidate_id=NEW.candidate_id AND c.campaign_id=NEW.campaign_id
    AND c.disposition='nominated' AND c.result_json IS NOT NULL
)
BEGIN SELECT RAISE(ABORT, 'nomination requires a qualified terminal candidate'); END;
"#,
    )?;
    transaction.execute_batch(PAIRED_ANALYSIS_SCHEMA_V2)?;
    transaction.execute_batch(PAIRED_EVIDENCE_SCHEMA_V3)?;
    transaction.execute_batch(DOMAIN_SHADOW_SCHEMA_V4)?;
    transaction.execute_batch(PAIRED_RUNTIME_SCHEMA_V5)?;
    transaction.execute_batch(SUCCESSOR_SCHEMA_V6)?;
    transaction.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn migrate(connection: &Connection, from: i64) -> Result<()> {
    if from > SCHEMA_VERSION {
        bail!(
            "database schema v{from} is newer than this papertiger-mise (v{SCHEMA_VERSION}); upgrade the tool"
        );
    }
    if from == SCHEMA_VERSION {
        return Ok(());
    }
    if from == 1 {
        let transaction = begin_mutation(connection)?;
        transaction.execute_batch(PAIRED_ANALYSIS_SCHEMA_V2)?;
        transaction.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            params![2_i64.to_string()],
        )?;
        transaction.commit()?;
        return migrate(connection, 2);
    }
    if from == 2 {
        let transaction = begin_mutation(connection)?;
        transaction.execute_batch(PAIRED_EVIDENCE_SCHEMA_V3)?;
        transaction.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            params![3_i64.to_string()],
        )?;
        transaction.commit()?;
        return migrate(connection, 3);
    }
    if from == 3 {
        let transaction = begin_mutation(connection)?;
        transaction.execute_batch(DOMAIN_SHADOW_SCHEMA_V4)?;
        transaction.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            params![4_i64.to_string()],
        )?;
        transaction.commit()?;
        return migrate(connection, 4);
    }
    if from == 4 {
        let transaction = begin_mutation(connection)?;
        transaction.execute_batch(PAIRED_RUNTIME_SCHEMA_V5)?;
        transaction.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            params![5_i64.to_string()],
        )?;
        transaction.commit()?;
        return migrate(connection, 5);
    }
    if from == 5 {
        let transaction = begin_mutation(connection)?;
        transaction.execute_batch(SUCCESSOR_SCHEMA_V6)?;
        transaction.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            params![6_i64.to_string()],
        )?;
        transaction.commit()?;
        return migrate(connection, 6);
    }
    if from == 6 {
        let transaction = begin_mutation(connection)?;
        transaction.execute_batch(MATERIALIZATION_RETRY_SCHEMA_V7)?;
        transaction.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            params![SCHEMA_VERSION.to_string()],
        )?;
        transaction.commit()?;
        return Ok(());
    }
    bail!("no papertiger-mise migration path from schema v{from} to v{SCHEMA_VERSION}")
}

pub(crate) fn admit_campaign(
    connection: &Connection,
    actor: &str,
    admission: &CampaignAdmission,
) -> Result<AdmissionOutcome> {
    let manifest = validate_campaign_admission(actor, admission)?;
    if manifest.generation.parent_campaign.is_some() || manifest.generation.recursion_depth != 0 {
        bail!(
            "descendant campaign admission requires an exact verified parent promotion proof; run `papertiger-mise campaign admit-successor <manifest> --parent-nomination <id> --gate-binding <binding.json>`"
        );
    }

    let transaction = begin_mutation(connection)?;
    if existing_campaign_matches(&transaction, admission)? {
        transaction.commit()?;
        return Ok(AdmissionOutcome::Existing);
    }
    insert_campaign_rows_in(&transaction, actor, admission)?;
    transaction.commit()?;
    Ok(AdmissionOutcome::Admitted)
}

fn validate_campaign_admission(
    actor: &str,
    admission: &CampaignAdmission,
) -> Result<CampaignManifest> {
    validate_nonblank("actor", actor)?;
    validate_nonblank("campaign_id", &admission.campaign_id)?;
    validate_nonblank("manifest_schema", &admission.manifest_schema)?;
    validate_sha256(&admission.manifest_sha256, "campaign manifest SHA-256")?;
    let manifest: CampaignManifest = serde_json::from_str(&admission.manifest_json)
        .context("campaign admission manifest_json must be a typed Mise campaign manifest")?;
    manifest.validate()?;
    if manifest.campaign_id != admission.campaign_id
        || manifest.schema != admission.manifest_schema
        || manifest.budgets.caps != admission.limits
    {
        bail!("campaign admission fields differ from the exact typed manifest");
    }
    let actual_sha256 = sha256(admission.manifest_json.as_bytes());
    if actual_sha256 != admission.manifest_sha256 {
        bail!(
            "campaign admission manifest digest differs from its exact stored JSON: expected {}, computed {actual_sha256}",
            admission.manifest_sha256
        );
    }
    validate_limits(&admission.limits)?;
    Ok(manifest)
}

fn existing_campaign_matches(
    transaction: &Transaction<'_>,
    admission: &CampaignAdmission,
) -> Result<bool> {
    let Some(existing) = campaign_in(transaction, &admission.campaign_id)? else {
        return Ok(false);
    };
    let existing_limits = budget_limits_in(transaction, &admission.campaign_id)?;
    let requested_limits = canonical_limits(&admission.limits);
    if existing.manifest_schema == admission.manifest_schema
        && existing.manifest_sha256 == admission.manifest_sha256
        && existing.manifest_json == admission.manifest_json
        && existing_limits == requested_limits
    {
        return Ok(true);
    }
    bail!(
        "campaign '{}' is already admitted with different immutable manifest or budget limits",
        admission.campaign_id
    );
}

fn insert_campaign_rows_in(
    transaction: &Transaction<'_>,
    actor: &str,
    admission: &CampaignAdmission,
) -> Result<()> {
    let admitted_at = now();
    transaction.execute(
        "INSERT INTO campaigns
         (campaign_id, manifest_schema, manifest_sha256, manifest_json, admitted_at, admitted_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            admission.campaign_id,
            admission.manifest_schema,
            admission.manifest_sha256,
            admission.manifest_json,
            admitted_at,
            actor
        ],
    )?;
    for limit in &admission.limits {
        let amount = sql_amount(limit.hard_limit)?;
        transaction.execute(
            "INSERT INTO budget_limits (campaign_id, resource, unit, hard_limit)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                admission.campaign_id,
                limit.resource.as_str(),
                limit.resource.unit(),
                amount
            ],
        )?;
        transaction.execute(
            "INSERT INTO budget_balances (campaign_id, resource) VALUES (?1, ?2)",
            params![admission.campaign_id, limit.resource.as_str()],
        )?;
    }
    record_event_in_mutation(
        transaction,
        actor,
        "campaign",
        &admission.campaign_id,
        "admitted",
        None,
        Some(&serde_json::json!({
            "manifest_schema": admission.manifest_schema,
            "manifest_sha256": admission.manifest_sha256,
            "budgets": admission.limits,
        })),
    )?;
    Ok(())
}

pub(crate) fn admit_successor_campaign(
    connection: &Connection,
    actor: &str,
    admission: &CampaignAdmission,
    proof: &crate::successor::ParentPromotionProof,
    proof_object: &crate::object::PreservedObject,
    gate: &crate::successor::VerifiedParentPromotionGate,
) -> Result<AdmissionOutcome> {
    let manifest = validate_campaign_admission(actor, admission)?;
    let parent = manifest
        .generation
        .parent_campaign
        .as_ref()
        .context("successor admission requires a non-root campaign manifest")?;
    let proof_bytes = proof.canonical_bytes()?;
    let proof_sha256 = proof.sha256()?;
    if proof_object.sha256 != proof_sha256
        || proof_object.bytes != u64::try_from(proof_bytes.len())?
        || proof.successor_campaign_id != admission.campaign_id
        || proof.successor_manifest_sha256 != admission.manifest_sha256
        || proof.parent_campaign_id != parent.campaign_id
        || proof.parent_manifest_sha256 != parent.manifest_sha256.0
        || proof.parent_runtime_generation != parent.runtime_generation
        || proof.parent_recursion_depth != parent.recursion_depth
        || canonical_limits(&proof.successor_budget_debit) != canonical_limits(&admission.limits)
    {
        bail!("successor admission fields differ from the exact parent promotion proof");
    }
    if gate.parent_nomination_id != proof.parent_nomination_id
        || gate.parent_campaign_id != proof.parent_campaign_id
        || gate.promoted_candidate_id != proof.promoted_candidate_id
        || gate.successor_campaign_id != proof.successor_campaign_id
        || gate.parent_promotion_proof_sha256 != proof_sha256
        || gate.evidence_locator != proof.evidence_locator()?
        || gate.evidence_sha256 != proof_sha256
    {
        bail!("verified Papertiger gate differs from the exact parent promotion proof");
    }

    let transaction = begin_mutation(connection)?;
    if existing_campaign_matches(&transaction, admission)? {
        let existing = successor_admission_in(&transaction, &admission.campaign_id)?
            .context("existing descendant campaign has no immutable successor admission receipt")?;
        if successor_record_matches(&existing, proof, gate)? {
            transaction.commit()?;
            return Ok(AdmissionOutcome::Existing);
        }
        bail!(
            "campaign '{}' already has a different immutable successor admission receipt",
            admission.campaign_id
        );
    }

    let durable_parent = campaign_in(&transaction, &proof.parent_campaign_id)?
        .context("parent campaign is not admitted in this Mise authority")?;
    if durable_parent.manifest_sha256 != proof.parent_manifest_sha256 {
        bail!("parent campaign manifest differs from the promotion proof");
    }
    let durable_nomination = transaction
        .query_row(
            "SELECT campaign_id, candidate_id, receipt_sha256 FROM nominations WHERE nomination_id=?1",
            params![proof.parent_nomination_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .context("parent nomination disappeared before successor admission")?;
    if durable_nomination.0 != proof.parent_campaign_id
        || durable_nomination.1 != proof.promoted_candidate_id
        || durable_nomination.2 != proof.parent_nomination_sha256
    {
        bail!("durable parent nomination differs from the promotion proof");
    }
    let durable_materialization = transaction
        .query_row(
            "SELECT receipt_sha256, result_tree FROM materializations WHERE candidate_id=?1",
            params![proof.promoted_candidate_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .context("promoted candidate materialization disappeared before successor admission")?;
    if durable_materialization.0 != proof.promoted_materialization_receipt_sha256
        || durable_materialization.1 != proof.promoted_source_tree
    {
        bail!("durable promoted materialization differs from the promotion proof");
    }

    for debit in &proof.successor_budget_debit {
        let row = transaction
            .query_row(
                "SELECT l.unit, l.hard_limit, b.reserved_amount, b.spent_amount
                 FROM budget_limits l
                 JOIN budget_balances b
                   ON b.campaign_id=l.campaign_id AND b.resource=l.resource
                 WHERE l.campaign_id=?1 AND l.resource=?2",
                params![proof.parent_campaign_id, debit.resource.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?
            .with_context(|| {
                format!(
                    "parent campaign '{}' has no '{}' budget required by its successor",
                    proof.parent_campaign_id, debit.resource
                )
            })?;
        if row.0 != debit.resource.unit() {
            bail!("parent budget unit differs from the typed successor debit");
        }
        let hard_limit = u64::try_from(row.1).context("parent budget limit is negative")?;
        let reserved = u64::try_from(row.2).context("parent reserved budget is negative")?;
        let spent = u64::try_from(row.3).context("parent spent budget is negative")?;
        let committed = reserved
            .checked_add(spent)
            .and_then(|value| value.checked_add(debit.hard_limit))
            .context("parent cumulative successor debit overflow")?;
        if committed > hard_limit {
            bail!(
                "successor '{}' budget debit exceeds parent '{}' resource '{}': {reserved} reserved + {spent} spent + {} child envelope > {hard_limit} hard limit",
                proof.successor_campaign_id,
                proof.parent_campaign_id,
                debit.resource,
                debit.hard_limit
            );
        }
    }

    let admitted_at = now();
    let reservation_id = format!("successor-admission/{}", proof.successor_campaign_id);
    for debit in &proof.successor_budget_debit {
        transaction.execute(
            "INSERT INTO budget_reservations
             (reservation_id, campaign_id, resource, unit, reserved_amount, settled_amount,
              status, created_at, settled_at, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, 'charged', ?6, ?6,
                     'successor-budget-envelope')",
            params![
                reservation_id,
                proof.parent_campaign_id,
                debit.resource.as_str(),
                debit.resource.unit(),
                sql_amount(debit.hard_limit)?,
                admitted_at,
            ],
        )?;
        transaction.execute(
            "UPDATE budget_balances SET spent_amount=spent_amount + ?3
             WHERE campaign_id=?1 AND resource=?2",
            params![
                proof.parent_campaign_id,
                debit.resource.as_str(),
                sql_amount(debit.hard_limit)?,
            ],
        )?;
    }
    transaction.execute(
        "INSERT INTO budget_reservation_uses
         (campaign_id, reservation_id, use_kind, entity_key, bound_at)
         VALUES (?1, ?2, 'successor_admission', ?3, ?4)",
        params![
            proof.parent_campaign_id,
            reservation_id,
            proof.successor_campaign_id,
            admitted_at,
        ],
    )?;
    crate::object::record_indexed_object(
        &transaction,
        proof_object,
        "application/vnd.papertiger-mise.parent-promotion-proof+json",
    )?;
    insert_campaign_rows_in(&transaction, actor, admission)?;
    let debit_json = serde_json::to_string(&proof.successor_budget_debit)?;
    transaction.execute(
        "INSERT INTO successor_admissions
         (child_campaign_id, parent_campaign_id, parent_manifest_sha256,
          parent_nomination_id, promoted_candidate_id, promoted_source_tree,
          parent_promotion_proof_sha256, parent_promotion_proof_locator,
          successor_manifest_sha256, parent_runtime_generation,
          successor_runtime_generation, parent_recursion_depth, successor_recursion_depth,
          parent_budget_debit_json, papertiger_plan_slug, papertiger_task_seq,
          papertiger_gate_name, papertiger_gate_closed_at, admitted_at, admitted_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        params![
            proof.successor_campaign_id,
            proof.parent_campaign_id,
            proof.parent_manifest_sha256,
            proof.parent_nomination_id,
            proof.promoted_candidate_id,
            proof.promoted_source_tree,
            proof_sha256,
            proof.evidence_locator()?,
            proof.successor_manifest_sha256,
            i64::from(proof.parent_runtime_generation),
            i64::from(proof.successor_runtime_generation),
            i64::from(proof.parent_recursion_depth),
            i64::from(proof.successor_recursion_depth),
            debit_json,
            gate.plan_slug,
            gate.task_seq,
            gate.gate_name,
            gate.closed_at,
            admitted_at,
            actor,
        ],
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "campaign",
        &proof.parent_campaign_id,
        "successor-budget-debited",
        Some("exact child envelope charged before descendant admission"),
        Some(&serde_json::json!({
            "child_campaign_id": proof.successor_campaign_id,
            "parent_promotion_proof_sha256": proof_sha256,
            "reservation_id": reservation_id,
            "resources": proof.successor_budget_debit,
        })),
    )?;
    record_event_in_mutation(
        &transaction,
        actor,
        "campaign",
        &proof.successor_campaign_id,
        "successor-admitted",
        Some("verified parent promotion gate and atomic parent budget debit"),
        Some(&serde_json::json!({
            "parent_campaign_id": proof.parent_campaign_id,
            "parent_nomination_id": proof.parent_nomination_id,
            "parent_promotion_proof_sha256": proof_sha256,
            "papertiger_plan_slug": gate.plan_slug,
            "papertiger_task_seq": gate.task_seq,
            "papertiger_gate_name": gate.gate_name,
        })),
    )?;
    transaction.commit()?;
    Ok(AdmissionOutcome::Admitted)
}

pub fn successor_admission(
    connection: &Connection,
    child_campaign_id: &str,
) -> Result<Option<SuccessorAdmissionRecord>> {
    successor_admission_in(connection, child_campaign_id)
}

fn successor_admission_in(
    connection: &Connection,
    child_campaign_id: &str,
) -> Result<Option<SuccessorAdmissionRecord>> {
    let row = connection
        .query_row(
            "SELECT child_campaign_id, parent_campaign_id, parent_manifest_sha256,
                    parent_nomination_id, promoted_candidate_id, promoted_source_tree,
                    parent_promotion_proof_sha256, parent_promotion_proof_locator,
                    successor_manifest_sha256, parent_runtime_generation,
                    successor_runtime_generation, parent_recursion_depth,
                    successor_recursion_depth, parent_budget_debit_json,
                    papertiger_plan_slug, papertiger_task_seq, papertiger_gate_name,
                    papertiger_gate_closed_at, admitted_at, admitted_by
             FROM successor_admissions WHERE child_campaign_id=?1",
            params![child_campaign_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, String>(17)?,
                    row.get::<_, String>(18)?,
                    row.get::<_, String>(19)?,
                ))
            },
        )
        .optional()?;
    row.map(|row| {
        Ok(SuccessorAdmissionRecord {
            child_campaign_id: row.0,
            parent_campaign_id: row.1,
            parent_manifest_sha256: row.2,
            parent_nomination_id: row.3,
            promoted_candidate_id: row.4,
            promoted_source_tree: row.5,
            parent_promotion_proof_sha256: row.6,
            parent_promotion_proof_locator: row.7,
            successor_manifest_sha256: row.8,
            parent_runtime_generation: u32::try_from(row.9)?,
            successor_runtime_generation: u32::try_from(row.10)?,
            parent_recursion_depth: u32::try_from(row.11)?,
            successor_recursion_depth: u32::try_from(row.12)?,
            parent_budget_debit: serde_json::from_str(&row.13)?,
            papertiger_plan_slug: row.14,
            papertiger_task_seq: row.15,
            papertiger_gate_name: row.16,
            papertiger_gate_closed_at: row.17,
            admitted_at: row.18,
            admitted_by: row.19,
        })
    })
    .transpose()
}

fn successor_record_matches(
    record: &SuccessorAdmissionRecord,
    proof: &crate::successor::ParentPromotionProof,
    gate: &crate::successor::VerifiedParentPromotionGate,
) -> Result<bool> {
    Ok(record.child_campaign_id == proof.successor_campaign_id
        && record.parent_campaign_id == proof.parent_campaign_id
        && record.parent_manifest_sha256 == proof.parent_manifest_sha256
        && record.parent_nomination_id == proof.parent_nomination_id
        && record.promoted_candidate_id == proof.promoted_candidate_id
        && record.promoted_source_tree == proof.promoted_source_tree
        && record.parent_promotion_proof_sha256 == proof.sha256()?
        && record.parent_promotion_proof_locator == proof.evidence_locator()?
        && record.successor_manifest_sha256 == proof.successor_manifest_sha256
        && record.parent_runtime_generation == proof.parent_runtime_generation
        && record.successor_runtime_generation == proof.successor_runtime_generation
        && record.parent_recursion_depth == proof.parent_recursion_depth
        && record.successor_recursion_depth == proof.successor_recursion_depth
        && canonical_limits(&record.parent_budget_debit)
            == canonical_limits(&proof.successor_budget_debit)
        && record.papertiger_plan_slug == gate.plan_slug
        && record.papertiger_task_seq == gate.task_seq
        && record.papertiger_gate_name == gate.gate_name
        && record.papertiger_gate_closed_at == gate.closed_at)
}

pub fn campaign(connection: &Connection, campaign_id: &str) -> Result<Option<CampaignRecord>> {
    campaign_in(connection, campaign_id)
}

pub fn authority_status(connection: &Connection, recent_limit: usize) -> Result<AuthorityStatus> {
    let campaign_count = connection.query_row("SELECT COUNT(*) FROM campaigns", [], |row| {
        row.get::<_, i64>(0)
    })?;
    let mut recent_statement = connection.prepare(
        "SELECT campaign_id, manifest_sha256, admitted_at FROM campaigns
         ORDER BY admitted_at DESC, campaign_id LIMIT ?1",
    )?;
    let recent_campaigns = recent_statement
        .query_map([i64::try_from(recent_limit)?], |row| {
            Ok(CampaignSummary {
                campaign_id: row.get(0)?,
                manifest_sha256: row.get(1)?,
                admitted_at: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut disposition_statement = connection.prepare(
        "SELECT disposition, COUNT(*) FROM candidates GROUP BY disposition ORDER BY disposition",
    )?;
    let candidate_dispositions = disposition_statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    let nomination_count =
        connection.query_row("SELECT COUNT(*) FROM nominations", [], |row| row.get(0))?;
    let open_reservation_count = connection.query_row(
        "SELECT COUNT(*) FROM (
           SELECT campaign_id, reservation_id FROM budget_reservations
           WHERE status='reserved' GROUP BY campaign_id, reservation_id
         )",
        [],
        |row| row.get(0),
    )?;
    let active_trial_count = connection.query_row(
        "SELECT COUNT(*) FROM trials WHERE status IN ('owned','launched')",
        [],
        |row| row.get(0),
    )?;
    let active_paired_cohort_count = connection.query_row(
        "SELECT COUNT(*) FROM paired_cohorts WHERE status IN ('prepared','running')",
        [],
        |row| row.get(0),
    )?;
    let integrity_failure_count = connection.query_row(
        "SELECT
           (SELECT COUNT(*) FROM trials WHERE status='integrity_failed') +
           (SELECT COUNT(*) FROM paired_cohorts WHERE status='integrity_failed') +
           (SELECT COUNT(*) FROM paired_runs WHERE status='integrity_failed')",
        [],
        |row| row.get(0),
    )?;
    Ok(AuthorityStatus {
        schema: "papertiger-mise.authority-status.v1",
        schema_version: SCHEMA_VERSION,
        campaign_count,
        recent_campaigns_truncated: campaign_count > i64::try_from(recent_campaigns.len())?,
        recent_campaigns,
        candidate_dispositions,
        nomination_count,
        open_reservation_count,
        active_trial_count,
        active_paired_cohort_count,
        integrity_failure_count,
    })
}

pub(crate) fn require_campaign(connection: &Connection, campaign_id: &str) -> Result<()> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM campaigns WHERE campaign_id=?1",
            params![campaign_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !exists {
        bail!("unknown campaign '{campaign_id}'");
    }
    Ok(())
}

fn campaign_in(connection: &Connection, campaign_id: &str) -> Result<Option<CampaignRecord>> {
    connection
        .query_row(
            "SELECT campaign_id, manifest_schema, manifest_sha256, manifest_json,
                    admitted_at, admitted_by
             FROM campaigns WHERE campaign_id=?1",
            params![campaign_id],
            |row| {
                Ok(CampaignRecord {
                    campaign_id: row.get(0)?,
                    manifest_schema: row.get(1)?,
                    manifest_sha256: row.get(2)?,
                    manifest_json: row.get(3)?,
                    admitted_at: row.get(4)?,
                    admitted_by: row.get(5)?,
                })
            },
        )
        .optional()
        .context("query papertiger-mise campaign")
}

pub fn append_campaign_event(
    connection: &Connection,
    actor: &str,
    campaign_id: &str,
    kind: &str,
    why: Option<&str>,
    payload: Option<&Value>,
) -> Result<()> {
    validate_nonblank("actor", actor)?;
    validate_nonblank("event kind", kind)?;
    let transaction = begin_mutation(connection)?;
    if campaign_in(&transaction, campaign_id)?.is_none() {
        bail!("unknown campaign '{campaign_id}'");
    }
    record_event_in_mutation(
        &transaction,
        actor,
        "campaign",
        campaign_id,
        kind,
        why,
        payload,
    )?;
    transaction.commit()?;
    Ok(())
}

pub fn campaign_events(connection: &Connection, campaign_id: &str) -> Result<Vec<EventRecord>> {
    let mut statement = connection.prepare(
        "SELECT event_id, at, actor, entity, entity_key, kind, why, payload
         FROM events WHERE entity='campaign' AND entity_key=?1 ORDER BY event_id",
    )?;
    statement
        .query_map(params![campaign_id], |row| {
            let payload = row.get::<_, Option<String>>(7)?;
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                payload,
            ))
        })?
        .map(|row| {
            let (event_id, at, actor, entity, entity_key, kind, why, payload) = row?;
            Ok(EventRecord {
                event_id,
                at,
                actor,
                entity,
                entity_key,
                kind,
                why,
                payload: payload
                    .map(|text| serde_json::from_str(&text).context("invalid stored event payload"))
                    .transpose()?,
            })
        })
        .collect()
}

pub(crate) fn record_event_in_mutation(
    transaction: &Transaction<'_>,
    actor: &str,
    entity: &str,
    entity_key: &str,
    kind: &str,
    why: Option<&str>,
    payload: Option<&Value>,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO events (at, actor, entity, entity_key, kind, why, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            now(),
            actor,
            entity,
            entity_key,
            kind,
            why,
            payload.map(serde_json::to_string).transpose()?
        ],
    )?;
    Ok(())
}

pub(crate) fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

pub(crate) fn sql_amount(amount: u64) -> Result<i64> {
    i64::try_from(amount).context("budget amount exceeds SQLite's signed integer range")
}

fn validate_limits(limits: &[BudgetLimit]) -> Result<()> {
    if limits.is_empty() {
        bail!("campaign admission requires at least one cumulative budget limit");
    }
    let mut seen = BTreeMap::new();
    for limit in limits {
        if limit.hard_limit == 0 {
            bail!("budget limit '{}' must be nonzero", limit.resource);
        }
        sql_amount(limit.hard_limit)?;
        if seen.insert(limit.resource, limit.hard_limit).is_some() {
            bail!("campaign repeats budget resource '{}'", limit.resource);
        }
    }
    Ok(())
}

fn canonical_limits(limits: &[BudgetLimit]) -> BTreeMap<BudgetResource, u64> {
    limits
        .iter()
        .map(|limit| (limit.resource, limit.hard_limit))
        .collect()
}

fn budget_limits_in(
    connection: &Connection,
    campaign_id: &str,
) -> Result<BTreeMap<BudgetResource, u64>> {
    let mut statement = connection.prepare(
        "SELECT resource, unit, hard_limit FROM budget_limits
         WHERE campaign_id=?1 ORDER BY resource",
    )?;
    let rows = statement
        .query_map(params![campaign_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut limits = BTreeMap::new();
    for (resource, unit, amount) in rows {
        let resource: BudgetResource = resource.parse()?;
        if unit != resource.unit() {
            bail!(
                "stored budget resource '{}' has unit '{}', expected '{}'",
                resource,
                unit,
                resource.unit()
            );
        }
        limits.insert(
            resource,
            u64::try_from(amount).context("stored budget limit is negative")?,
        );
    }
    Ok(limits)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::manifest::tests::valid_manifest;

    fn admission(id: &str) -> CampaignAdmission {
        let mut manifest = valid_manifest();
        manifest.campaign_id = id.to_owned();
        CampaignAdmission::from_manifest(&manifest).expect("admission")
    }

    struct SuccessorFixture {
        parent: CampaignManifest,
        child: CampaignManifest,
        nomination_id: String,
        candidate_id: String,
        proof: crate::successor::ParentPromotionProof,
        proof_object: crate::object::PreservedObject,
        gate: crate::successor::VerifiedParentPromotionGate,
    }

    fn successor_fixture(connection: &Connection, child_id: &str) -> SuccessorFixture {
        let mut parent = valid_manifest();
        parent.campaign_id = "papertiger-lineage-root-a01".to_owned();
        let parent_admission = CampaignAdmission::from_manifest(&parent).expect("parent admission");
        admit_campaign(connection, "outer-judge", &parent_admission).expect("admit parent");

        let patch_sha256 = "a".repeat(64);
        let materialization_sha256 = "b".repeat(64);
        let nomination_id = "c".repeat(64);
        let candidate_id = "d".repeat(64);
        let promoted_source_tree = "e".repeat(40);
        for (digest, locator, media_type) in [
            (
                patch_sha256.as_str(),
                format!("sha256/aa/{patch_sha256}"),
                "text/x-diff",
            ),
            (
                materialization_sha256.as_str(),
                format!("sha256/bb/{materialization_sha256}"),
                "application/json",
            ),
        ] {
            connection
                .execute(
                    "INSERT INTO artifacts (sha256, bytes, locator, media_type, recorded_at)
                     VALUES (?1, 1, ?2, ?3, 'now')",
                    params![digest, locator, media_type],
                )
                .expect("fixture artifact");
        }
        connection
            .execute(
                "INSERT INTO candidates
                 (candidate_id, campaign_id, proposal_json, patch_sha256,
                  negative_fingerprint, disposition, result_json, created_at, updated_at)
                 VALUES (?1, ?2, '{}', ?3, 'fixture-negative', 'nominated', '{}', 'now', 'now')",
                params![candidate_id, parent.campaign_id, patch_sha256],
            )
            .expect("fixture candidate");
        connection
            .execute(
                "INSERT INTO materializations
                 (candidate_id, reservation_id, receipt_sha256, result_tree,
                  worktree_locator, created_at)
                 VALUES (?1, 'fixture-materialization', ?2, ?3, 'C:/fixture/worktree', 'now')",
                params![candidate_id, materialization_sha256, promoted_source_tree],
            )
            .expect("fixture materialization");
        connection
            .execute(
                "INSERT INTO nominations
                 (nomination_id, campaign_id, candidate_id, receipt_sha256,
                  receipt_json, created_at)
                 VALUES (?1, ?2, ?3, ?1, '{}', 'now')",
                params![nomination_id, parent.campaign_id, candidate_id],
            )
            .expect("fixture nomination");

        let mut child = parent.clone();
        child.campaign_id = child_id.to_owned();
        child.source.base_commit = "f".repeat(40);
        child.source.base_tree = promoted_source_tree.clone();
        child.generation.runtime_generation += 1;
        child.generation.recursion_depth += 1;
        child.generation.parent_campaign = Some(crate::manifest::ParentCampaignBinding {
            campaign_id: parent.campaign_id.clone(),
            manifest_sha256: crate::manifest::Sha256Digest(
                parent.sha256().expect("parent manifest digest"),
            ),
            runtime_generation: parent.generation.runtime_generation,
            recursion_depth: parent.generation.recursion_depth,
        });
        child.validate().expect("child manifest");
        let proof = crate::successor::ParentPromotionProof {
            schema: crate::successor::PARENT_PROMOTION_PROOF_SCHEMA_V1.to_owned(),
            scope: crate::successor::SUCCESSOR_ADMISSION_SCOPE_V1.to_owned(),
            parent_campaign_id: parent.campaign_id.clone(),
            parent_manifest_sha256: parent.sha256().expect("parent digest"),
            parent_runtime_generation: parent.generation.runtime_generation,
            parent_recursion_depth: parent.generation.recursion_depth,
            parent_nomination_id: nomination_id.clone(),
            parent_nomination_sha256: nomination_id.clone(),
            promoted_candidate_id: candidate_id.clone(),
            promoted_candidate_result_sha256: "1".repeat(64),
            promoted_materialization_receipt_sha256: materialization_sha256,
            promoted_source_tree,
            parent_evidence_grade: crate::state::EvidenceGrade::DeterministicDevelopment,
            relied_upon_trial_ids: vec!["root-no-op".to_owned(), "root-known-bad".to_owned()],
            relied_upon_paired_cohort_ids: Vec::new(),
            successor_campaign_id: child.campaign_id.clone(),
            successor_manifest_sha256: child.sha256().expect("child digest"),
            successor_runtime_generation: child.generation.runtime_generation,
            successor_recursion_depth: child.generation.recursion_depth,
            successor_outer_judge_executable_locator: child
                .generation
                .outer_judge_executable_locator
                .clone(),
            successor_outer_judge_executable_sha256: child
                .generation
                .outer_judge_executable_sha256
                .0
                .clone(),
            promoted_judge_build_trial_receipts: Vec::new(),
            promoted_judge_executable_sha256: None,
            successor_budget_debit: child.budgets.caps.clone(),
        };
        let proof_bytes = proof.canonical_bytes().expect("proof bytes");
        let proof_sha256 = proof.sha256().expect("proof digest");
        let proof_object = crate::object::PreservedObject {
            sha256: proof_sha256.clone(),
            bytes: u64::try_from(proof_bytes.len()).expect("proof size"),
            locator: format!("sha256/{}/{proof_sha256}", &proof_sha256[..2]),
        };
        let gate = crate::successor::VerifiedParentPromotionGate {
            parent_nomination_id: nomination_id.clone(),
            parent_campaign_id: parent.campaign_id.clone(),
            promoted_candidate_id: candidate_id.clone(),
            successor_campaign_id: child.campaign_id.clone(),
            parent_promotion_proof_sha256: proof_sha256.clone(),
            plan_slug: "papertiger-mise".to_owned(),
            task_seq: 42,
            task_title: "Authorize exact successor lineage".to_owned(),
            task_status: "in_progress".to_owned(),
            gate_name: "parent-promotion-proof".to_owned(),
            evidence_locator: proof.evidence_locator().expect("proof locator"),
            evidence_sha256: proof_sha256,
            closed_at: "2026-08-02T00:00:00Z".to_owned(),
        };
        SuccessorFixture {
            parent,
            child,
            nomination_id,
            candidate_id,
            proof,
            proof_object,
            gate,
        }
    }

    #[test]
    fn initialization_is_explicit_and_refuses_foreign_databases() {
        let directory = tempdir().expect("tempdir");
        let absent = directory.path().join("absent.sqlite");
        assert!(open_existing(&absent).is_err());
        assert!(!absent.exists());

        let foreign_path = directory.path().join("foreign.sqlite");
        let foreign = Connection::open(&foreign_path).expect("foreign database");
        foreign
            .execute("CREATE TABLE foreign_table (value TEXT)", [])
            .expect("foreign table");
        assert!(init(&foreign).is_err());

        let path = directory.path().join("mise.sqlite");
        let connection = open_for_init(&path).expect("initialization connection");
        init(&connection).expect("initialize");
        init(&connection).expect("idempotent initialize");
        drop(connection);
        let read_only = open_existing_read_only(&path).expect("read-only existing database");
        assert!(
            read_only
                .execute("CREATE TABLE forbidden_read_only_write (value TEXT)", [])
                .is_err(),
            "read-only authority must reject mutation by construction"
        );
    }

    #[test]
    fn schema_one_migrates_explicitly_through_all_later_authorities() {
        let connection = Connection::open_in_memory().expect("database");
        init(&connection).expect("initialize current schema");
        let fresh_family_schema = trust_boundary_family_schema(&connection);
        connection
            .execute_batch(
                "DROP TRIGGER successor_admissions_no_delete;
                 DROP TRIGGER successor_admissions_no_update;
                 DROP TABLE successor_admissions;
                 DROP TRIGGER paired_shadow_domain_receipt_global_guard;
                 DROP TRIGGER paired_live_domain_receipt_global_guard;
                 DROP TRIGGER paired_live_domain_receipts_no_delete;
                 DROP TRIGGER paired_live_domain_receipts_no_update;
                 DROP TRIGGER paired_runs_no_delete;
                 DROP TRIGGER paired_run_transition_guard;
                 DROP TRIGGER paired_run_identity_no_update;
                 DROP TRIGGER paired_cohorts_no_delete;
                 DROP TRIGGER paired_cohort_transition_guard;
                 DROP TRIGGER paired_cohort_identity_no_update;
                 DROP TABLE paired_live_domain_receipts;
                 DROP TABLE paired_runs;
                 DROP TABLE paired_cohorts;
                 DROP TRIGGER domain_shadows_no_delete;
                 DROP TRIGGER domain_shadows_no_update;
                 DROP TABLE domain_shadows;
                 DROP TRIGGER paired_domain_trial_receipts_no_delete;
                 DROP TRIGGER paired_domain_trial_receipts_no_update;
                 DROP TRIGGER paired_adapter_blocks_no_delete;
                 DROP TRIGGER paired_adapter_blocks_no_update;
                 DROP TRIGGER paired_adapter_observations_no_delete;
                 DROP TRIGGER paired_adapter_observations_no_update;
                 DROP TABLE paired_domain_trial_receipts;
                 DROP TABLE paired_adapter_blocks;
                 DROP TABLE paired_adapter_observations;
                 DROP TABLE paired_analysis_slots;
                 UPDATE meta SET value='1' WHERE key='schema_version';",
            )
            .expect("construct exact prior schema boundary");
        assert_eq!(schema_version(&connection).unwrap(), 1);

        init(&connection).expect("explicit migration");
        assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
        assert_eq!(
            trust_boundary_family_schema(&connection),
            fresh_family_schema,
            "fresh initialization and migration must install byte-identical trust-boundary DDL"
        );
        let table: String = connection
            .query_row(
                "SELECT name FROM sqlite_schema WHERE type='table' AND name='paired_analysis_slots'",
                [],
                |row| row.get(0),
            )
            .expect("paired slot table");
        assert_eq!(table, "paired_analysis_slots");
        let evidence_table: String = connection
            .query_row(
                "SELECT name FROM sqlite_schema
                 WHERE type='table' AND name='paired_adapter_observations'",
                [],
                |row| row.get(0),
            )
            .expect("paired evidence table");
        assert_eq!(evidence_table, "paired_adapter_observations");
        let shadow_table: String = connection
            .query_row(
                "SELECT name FROM sqlite_schema
                 WHERE type='table' AND name='domain_shadows'",
                [],
                |row| row.get(0),
            )
            .expect("domain shadow table");
        assert_eq!(shadow_table, "domain_shadows");
        let successor_table: String = connection
            .query_row(
                "SELECT name FROM sqlite_schema
                 WHERE type='table' AND name='successor_admissions'",
                [],
                |row| row.get(0),
            )
            .expect("successor admission table");
        assert_eq!(successor_table, "successor_admissions");
    }

    fn trust_boundary_family_schema(connection: &Connection) -> Vec<(String, String, String)> {
        connection
            .prepare(
                "SELECT type, name, sql FROM sqlite_schema
                 WHERE name LIKE 'paired_analysis_%'
                    OR name LIKE 'paired_adapter_%'
                    OR name LIKE 'paired_domain_%'
                    OR name LIKE 'domain_shadows%'
                 ORDER BY type, name",
            )
            .expect("prepare trust-boundary schema query")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query trust-boundary schema")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect trust-boundary schema")
    }

    #[test]
    fn domain_shadow_schema_requires_unchanged_state_and_refuses_decision_authority() {
        let connection = Connection::open_in_memory().expect("database");
        init(&connection).expect("initialize");
        let request = "a".repeat(64);
        let result = "b".repeat(64);
        let receipt = "c".repeat(64);
        for (digest, locator) in [
            (&request, "sha256/aa/domain-request"),
            (&result, "sha256/bb/domain-result"),
            (&receipt, "sha256/cc/domain-receipt"),
        ] {
            connection
                .execute(
                    "INSERT INTO artifacts
                     (sha256, bytes, locator, media_type, recorded_at)
                     VALUES (?1, 1, ?2, 'application/json', 'now')",
                    params![digest, locator],
                )
                .expect("artifact");
        }
        let insert = |id: &str, after: &str, eligible: i64, adjudication: &str| {
            connection.execute(
                "INSERT INTO domain_shadows
                 (evidence_id, binding_sha256, binding_json, request_sha256,
                  result_sha256, receipt_sha256, state_before_sha256, state_after_sha256,
                  decision_eligible, adjudication, recorded_by, recorded_at)
                 VALUES (?1, ?2, '{}', ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'agent', 'now')",
                params![
                    id,
                    "d".repeat(64),
                    request,
                    result,
                    receipt,
                    "e".repeat(64),
                    after,
                    eligible,
                    adjudication,
                ],
            )
        };
        assert!(insert("drift", &"f".repeat(64), 0, "prohibited").is_err());
        assert!(insert("eligible", &"e".repeat(64), 1, "prohibited").is_err());
        assert!(insert("judged", &"e".repeat(64), 0, "allowed").is_err());
        insert("shadow", &"e".repeat(64), 0, "prohibited").expect("domain shadow");
        assert!(
            connection
                .execute(
                    "UPDATE domain_shadows SET decision_eligible=1 WHERE evidence_id='shadow'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn historical_shadow_schema_cannot_be_made_decision_eligible_or_reused() {
        let connection = Connection::open_in_memory().expect("database");
        init(&connection).expect("initialize");
        let request = "a".repeat(64);
        let result = "b".repeat(64);
        let receipt = "c".repeat(64);
        for (digest, locator) in [
            (&request, "sha256/aa/request"),
            (&result, "sha256/bb/result"),
            (&receipt, "sha256/cc/receipt"),
        ] {
            connection
                .execute(
                    "INSERT INTO artifacts
                     (sha256, bytes, locator, media_type, recorded_at)
                     VALUES (?1, 1, ?2, 'application/json', 'now')",
                    params![digest, locator],
                )
                .expect("artifact");
        }
        let insert = |id: &str, eligible: i64| {
            connection.execute(
                "INSERT INTO paired_adapter_observations
                 (evidence_id, scope, binding_sha256, binding_json,
                  request_sha256, result_sha256, receipt_sha256,
                  decision_eligible, recorded_by, recorded_at)
                 VALUES (?1, 'historical_shadow', ?2, '{}', ?3, ?4, ?5, ?6, 'agent', 'now')",
                params![id, "d".repeat(64), request, result, receipt, eligible],
            )
        };
        assert!(insert("unsafe", 1).is_err());
        insert("shadow", 0).expect("decision-ineligible shadow");
        connection
            .execute(
                "INSERT INTO paired_adapter_blocks
                 (evidence_id, block_index, observed_order, block_json)
                 VALUES ('shadow', 0, 'baseline_first', '{}')",
                [],
            )
            .expect("block");
        let domain_receipt = "e".repeat(64);
        connection
            .execute(
                "INSERT INTO paired_domain_trial_receipts
                 (receipt_sha256, evidence_id, block_index, participant)
                 VALUES (?1, 'shadow', 0, 'baseline')",
                params![domain_receipt],
            )
            .expect("domain receipt");
        assert!(
            connection
                .execute(
                    "UPDATE paired_adapter_observations SET decision_eligible=1
                     WHERE evidence_id='shadow'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO paired_domain_trial_receipts
                     (receipt_sha256, evidence_id, block_index, participant)
                     VALUES (?1, 'shadow', 0, 'candidate')",
                    params![domain_receipt],
                )
                .is_err(),
            "one domain receipt cannot represent both paired executions"
        );
    }

    #[test]
    fn campaign_admission_is_immutable_and_evented_once() {
        let connection = Connection::open_in_memory().expect("database");
        init(&connection).expect("initialize");
        let admission = admission("campaign-a");
        assert_eq!(
            admit_campaign(&connection, "agent", &admission).expect("admit"),
            AdmissionOutcome::Admitted
        );
        assert_eq!(
            admit_campaign(&connection, "agent", &admission).expect("replay"),
            AdmissionOutcome::Existing
        );
        assert_eq!(
            campaign_events(&connection, "campaign-a")
                .expect("events")
                .len(),
            1
        );

        let mut changed_manifest = valid_manifest();
        changed_manifest.campaign_id = "campaign-a".to_owned();
        changed_manifest.objectives[1].minimum_practical_change = 2.0;
        let different = CampaignAdmission::from_manifest(&changed_manifest).expect("different");
        assert!(admit_campaign(&connection, "agent", &different).is_err());
        assert!(
            connection
                .execute(
                    "UPDATE campaigns SET admitted_by='intruder' WHERE campaign_id='campaign-a'",
                    [],
                )
                .is_err()
        );
        assert!(connection.execute("DELETE FROM events", []).is_err());

        append_campaign_event(
            &connection,
            "agent",
            "campaign-a",
            "observed",
            Some("bounded probe"),
            Some(&json!({"result": "negative"})),
        )
        .expect("append event");
        assert_eq!(
            campaign_events(&connection, "campaign-a")
                .expect("events")
                .len(),
            2
        );
    }

    #[test]
    fn descendant_admission_fails_closed_without_verified_parent_promotion() {
        let connection = Connection::open_in_memory().expect("database");
        init(&connection).expect("initialize");
        let mut parent = valid_manifest();
        parent.campaign_id = "generation-one".to_owned();
        let parent_admission = CampaignAdmission::from_manifest(&parent).expect("parent");
        admit_campaign(&connection, "outer-judge", &parent_admission).expect("admit parent");

        let mut child = parent.clone();
        child.campaign_id = "generation-two".to_owned();
        child.generation.runtime_generation = parent.generation.runtime_generation + 1;
        child.generation.recursion_depth = 1;
        child.generation.parent_campaign = Some(crate::manifest::ParentCampaignBinding {
            campaign_id: parent.campaign_id.clone(),
            manifest_sha256: crate::manifest::Sha256Digest(parent.sha256().expect("parent digest")),
            runtime_generation: parent.generation.runtime_generation,
            recursion_depth: parent.generation.recursion_depth,
        });
        let child_admission = CampaignAdmission::from_manifest(&child).expect("child");
        let error =
            admit_campaign(&connection, "outer-judge", &child_admission).expect_err("refuse child");
        assert!(error.to_string().contains("promotion proof"), "{error:#}");
        for limit in &parent.budgets.caps {
            let (reserved, spent): (i64, i64) = connection
                .query_row(
                    "SELECT reserved_amount, spent_amount FROM budget_balances
                     WHERE campaign_id=?1 AND resource=?2",
                    params![parent.campaign_id, limit.resource.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("parent balance");
            assert_eq!(reserved, 0);
            assert_eq!(spent, 0);
        }
        assert!(
            campaign(&connection, "generation-two")
                .expect("child query")
                .is_none(),
            "unverified descendant must not become durable"
        );
    }

    #[test]
    fn verified_successor_atomically_debits_parent_and_replays_without_double_charge() {
        let connection = Connection::open_in_memory().expect("database");
        init(&connection).expect("initialize");
        let fixture = successor_fixture(&connection, "papertiger-lineage-child-a01");
        let child_admission =
            CampaignAdmission::from_manifest(&fixture.child).expect("child admission");

        assert_eq!(
            admit_successor_campaign(
                &connection,
                "operator",
                &child_admission,
                &fixture.proof,
                &fixture.proof_object,
                &fixture.gate,
            )
            .expect("admit successor"),
            AdmissionOutcome::Admitted
        );
        let first_balances =
            crate::budget::budget_balances(&connection, &fixture.parent.campaign_id)
                .expect("parent balances");
        for debit in &fixture.proof.successor_budget_debit {
            let balance = first_balances
                .iter()
                .find(|balance| balance.resource == debit.resource)
                .expect("debited parent resource");
            assert_eq!(balance.reserved_amount, 0);
            assert_eq!(balance.spent_amount, debit.hard_limit);
        }
        let record = successor_admission(&connection, &fixture.child.campaign_id)
            .expect("successor query")
            .expect("successor receipt");
        assert_eq!(record.parent_nomination_id, fixture.nomination_id);
        assert_eq!(record.promoted_candidate_id, fixture.candidate_id);
        assert_eq!(
            record.parent_promotion_proof_sha256,
            fixture.proof_object.sha256
        );

        assert_eq!(
            admit_successor_campaign(
                &connection,
                "operator",
                &child_admission,
                &fixture.proof,
                &fixture.proof_object,
                &fixture.gate,
            )
            .expect("replay successor"),
            AdmissionOutcome::Existing
        );
        assert_eq!(
            crate::budget::budget_balances(&connection, &fixture.parent.campaign_id)
                .expect("replayed balances"),
            first_balances,
            "idempotent replay must not debit the parent twice"
        );
        assert!(
            connection
                .execute(
                    "UPDATE successor_admissions SET admitted_by='intruder'
                     WHERE child_campaign_id=?1",
                    params![fixture.child.campaign_id],
                )
                .is_err(),
            "successor admission receipt must be immutable"
        );
    }

    #[test]
    fn successor_refusals_leave_parent_ledger_and_child_admission_unchanged() {
        let connection = Connection::open_in_memory().expect("database");
        init(&connection).expect("initialize");
        let fixture = successor_fixture(&connection, "papertiger-lineage-child-a02");
        let child_admission =
            CampaignAdmission::from_manifest(&fixture.child).expect("child admission");
        let before = crate::budget::budget_balances(&connection, &fixture.parent.campaign_id)
            .expect("parent balances");

        let mut fabricated_gate = fixture.gate.clone();
        fabricated_gate.evidence_sha256 = "9".repeat(64);
        let error = admit_successor_campaign(
            &connection,
            "operator",
            &child_admission,
            &fixture.proof,
            &fixture.proof_object,
            &fabricated_gate,
        )
        .expect_err("fabricated gate must fail closed");
        assert!(error.to_string().contains("Papertiger gate"), "{error:#}");
        assert!(
            campaign(&connection, &fixture.child.campaign_id)
                .expect("child query")
                .is_none()
        );
        assert_eq!(
            crate::budget::budget_balances(&connection, &fixture.parent.campaign_id)
                .expect("unchanged balances"),
            before
        );

        let reservation_id = "preexisting-parent-use";
        let first = fixture.parent.budgets.caps.first().expect("parent budget");
        crate::budget::reserve_budget(
            &connection,
            "operator",
            &fixture.parent.campaign_id,
            reservation_id,
            &[crate::budget::BudgetRequest {
                resource: first.resource,
                amount: 1,
            }],
        )
        .expect("consume one unit of parent capacity");
        let after_reservation =
            crate::budget::budget_balances(&connection, &fixture.parent.campaign_id)
                .expect("reserved parent balances");
        let error = admit_successor_campaign(
            &connection,
            "operator",
            &child_admission,
            &fixture.proof,
            &fixture.proof_object,
            &fixture.gate,
        )
        .expect_err("insufficient parent envelope must fail closed");
        assert!(error.to_string().contains("exceeds parent"), "{error:#}");
        assert!(
            campaign(&connection, &fixture.child.campaign_id)
                .expect("child query")
                .is_none()
        );
        assert!(
            successor_admission(&connection, &fixture.child.campaign_id)
                .expect("successor query")
                .is_none()
        );
        assert_eq!(
            crate::budget::budget_balances(&connection, &fixture.parent.campaign_id)
                .expect("failed admission balances"),
            after_reservation,
            "failed admission must roll back every attempted debit"
        );
    }

    #[test]
    fn second_writer_fails_without_hidden_wait() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("mise.sqlite");
        let first = open_for_init(&path).expect("first connection");
        init(&first).expect("initialize");
        let second = open_existing(&path).expect("second connection");
        let transaction = begin_mutation(&first).expect("writer reservation");
        let error = begin_mutation(&second).expect_err("second writer must fail");
        assert!(
            error
                .to_string()
                .contains("writer reservation is already held")
        );
        drop(transaction);
        begin_mutation(&second)
            .expect("reservation after rollback")
            .rollback()
            .expect("rollback");
        drop(second);
        drop(first);
        fs::remove_file(path).expect("database can close cleanly");
    }

    #[test]
    fn authority_status_is_bounded_and_reports_open_work_without_judging_it() {
        let connection = Connection::open_in_memory().expect("database");
        init(&connection).expect("initialize");
        let first = admission("project-improvement-a01");
        let second = admission("project-improvement-a02");
        admit_campaign(&connection, "agent", &first).expect("first campaign");
        admit_campaign(&connection, "agent", &second).expect("second campaign");

        let status = authority_status(&connection, 1).expect("status");
        assert_eq!(status.schema, "papertiger-mise.authority-status.v1");
        assert_eq!(status.schema_version, SCHEMA_VERSION);
        assert_eq!(status.campaign_count, 2);
        assert_eq!(status.recent_campaigns.len(), 1);
        assert!(status.recent_campaigns_truncated);
        assert!(status.candidate_dispositions.is_empty());
        assert_eq!(status.nomination_count, 0);
        assert_eq!(status.open_reservation_count, 0);
        assert_eq!(status.active_trial_count, 0);
        assert_eq!(status.active_paired_cohort_count, 0);
        assert_eq!(status.integrity_failure_count, 0);
    }
}
