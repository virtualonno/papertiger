use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::state::BudgetReservationStatus;
use crate::store::{begin_mutation, now, record_event_in_mutation, require_campaign, sql_amount};

pub type BudgetAmount = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetResource {
    Candidates,
    Trials,
    Failures,
    HoldoutDisclosures,
    WallTimeMilliseconds,
    CpuTimeMilliseconds,
    GpuTimeMilliseconds,
    DiskBytesWritten,
    NetworkBytes,
    ArtifactBytes,
    ModelTokens,
    CostMicrounits,
}

impl BudgetResource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidates => "candidates",
            Self::Trials => "trials",
            Self::Failures => "failures",
            Self::HoldoutDisclosures => "holdout_disclosures",
            Self::WallTimeMilliseconds => "wall_time_milliseconds",
            Self::CpuTimeMilliseconds => "cpu_time_milliseconds",
            Self::GpuTimeMilliseconds => "gpu_time_milliseconds",
            Self::DiskBytesWritten => "disk_bytes_written",
            Self::NetworkBytes => "network_bytes",
            Self::ArtifactBytes => "artifact_bytes",
            Self::ModelTokens => "model_tokens",
            Self::CostMicrounits => "cost_microunits",
        }
    }

    pub const fn unit(self) -> &'static str {
        match self {
            Self::Candidates | Self::Trials | Self::Failures | Self::HoldoutDisclosures => "count",
            Self::WallTimeMilliseconds | Self::CpuTimeMilliseconds | Self::GpuTimeMilliseconds => {
                "milliseconds"
            }
            Self::DiskBytesWritten | Self::NetworkBytes | Self::ArtifactBytes => "bytes",
            Self::ModelTokens => "tokens",
            Self::CostMicrounits => "micro-units",
        }
    }
}

impl Display for BudgetResource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for BudgetResource {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "candidates" => Ok(Self::Candidates),
            "trials" => Ok(Self::Trials),
            "failures" => Ok(Self::Failures),
            "holdout_disclosures" => Ok(Self::HoldoutDisclosures),
            "wall_time_milliseconds" => Ok(Self::WallTimeMilliseconds),
            "cpu_time_milliseconds" => Ok(Self::CpuTimeMilliseconds),
            "gpu_time_milliseconds" => Ok(Self::GpuTimeMilliseconds),
            "disk_bytes_written" => Ok(Self::DiskBytesWritten),
            "network_bytes" => Ok(Self::NetworkBytes),
            "artifact_bytes" => Ok(Self::ArtifactBytes),
            "model_tokens" => Ok(Self::ModelTokens),
            "cost_microunits" => Ok(Self::CostMicrounits),
            _ => bail!("unknown budget resource '{value}'"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetLimit {
    pub resource: BudgetResource,
    pub hard_limit: BudgetAmount,
}

impl BudgetLimit {
    pub fn new(resource: BudgetResource, hard_limit: BudgetAmount) -> Result<Self> {
        if hard_limit == 0 {
            bail!("budget limit '{resource}' must be nonzero");
        }
        sql_amount(hard_limit)?;
        Ok(Self {
            resource,
            hard_limit,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetRequest {
    pub resource: BudgetResource,
    pub amount: BudgetAmount,
}

impl BudgetRequest {
    pub fn new(resource: BudgetResource, amount: BudgetAmount) -> Result<Self> {
        if amount == 0 {
            bail!("budget reservation '{resource}' must be nonzero");
        }
        sql_amount(amount)?;
        Ok(Self { resource, amount })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetSettlement {
    pub resource: BudgetResource,
    pub actual_amount: BudgetAmount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettlementMode {
    Measured,
    ChargeReservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BudgetBalance {
    pub resource: BudgetResource,
    pub unit: String,
    pub hard_limit: BudgetAmount,
    pub reserved_amount: BudgetAmount,
    pub spent_amount: BudgetAmount,
    pub available_amount: BudgetAmount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationOutcome {
    Reserved,
    Existing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementOutcome {
    Settled,
    Existing,
}

pub fn reserve_budget(
    connection: &Connection,
    actor: &str,
    campaign_id: &str,
    reservation_id: &str,
    requests: &[BudgetRequest],
) -> Result<ReservationOutcome> {
    let transaction = begin_mutation(connection)?;
    let outcome = reserve_budget_in(&transaction, actor, campaign_id, reservation_id, requests)?;
    transaction.commit()?;
    Ok(outcome)
}

pub(crate) fn reserve_budget_in(
    transaction: &Transaction<'_>,
    actor: &str,
    campaign_id: &str,
    reservation_id: &str,
    requests: &[BudgetRequest],
) -> Result<ReservationOutcome> {
    validate_request_identity(actor, campaign_id, reservation_id)?;
    let canonical = canonical_requests(requests)?;
    require_campaign(transaction, campaign_id)?;

    let existing = reservation_rows(transaction, campaign_id, reservation_id)?;
    if !existing.is_empty() {
        if existing
            .iter()
            .all(|row| row.status == BudgetReservationStatus::Reserved)
            && existing_requests(&existing) == canonical
        {
            return Ok(ReservationOutcome::Existing);
        }
        bail!(
            "budget reservation '{reservation_id}' already exists with different resources or terminal state"
        );
    }

    for (resource, amount) in &canonical {
        let balance = budget_balance_in(transaction, campaign_id, *resource)?
            .with_context(|| format!("campaign '{campaign_id}' has no '{resource}' budget"))?;
        let committed = balance
            .reserved_amount
            .checked_add(balance.spent_amount)
            .and_then(|value| value.checked_add(*amount))
            .context("budget cumulative amount overflow")?;
        if committed > balance.hard_limit {
            bail!(
                "budget reservation '{reservation_id}' exceeds campaign '{campaign_id}' resource '{resource}': {} reserved + {} spent + {amount} requested > {} hard limit",
                balance.reserved_amount,
                balance.spent_amount,
                balance.hard_limit
            );
        }
    }

    let created_at = now();
    for (resource, amount) in &canonical {
        transaction.execute(
            "INSERT INTO budget_reservations
             (reservation_id, campaign_id, resource, unit, reserved_amount, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                reservation_id,
                campaign_id,
                resource.as_str(),
                resource.unit(),
                sql_amount(*amount)?,
                created_at
            ],
        )?;
        transaction.execute(
            "UPDATE budget_balances
             SET reserved_amount=reserved_amount + ?3
             WHERE campaign_id=?1 AND resource=?2",
            params![campaign_id, resource.as_str(), sql_amount(*amount)?],
        )?;
    }
    record_event_in_mutation(
        transaction,
        actor,
        "campaign",
        campaign_id,
        "budget-reserved",
        None,
        Some(&json!({
            "reservation_id": reservation_id,
            "resources": canonical.iter().map(|(resource, amount)| json!({
                "resource": resource,
                "unit": resource.unit(),
                "amount": amount,
            })).collect::<Vec<_>>(),
        })),
    )?;
    Ok(ReservationOutcome::Reserved)
}

pub fn settle_budget(
    connection: &Connection,
    actor: &str,
    campaign_id: &str,
    reservation_id: &str,
    mode: SettlementMode,
    settlements: &[BudgetSettlement],
    note: Option<&str>,
) -> Result<SettlementOutcome> {
    settle_budget_with_binding(
        connection,
        actor,
        SettlementTarget {
            campaign_id,
            reservation_id,
            expected_binding: None,
        },
        mode,
        settlements,
        note,
    )
}

pub(crate) struct BoundReservation<'a> {
    pub campaign_id: &'a str,
    pub reservation_id: &'a str,
    pub use_kind: &'a str,
    pub entity_key: &'a str,
}

pub(crate) fn settle_bound_budget(
    connection: &Connection,
    actor: &str,
    binding: BoundReservation<'_>,
    mode: SettlementMode,
    settlements: &[BudgetSettlement],
    note: Option<&str>,
) -> Result<SettlementOutcome> {
    settle_budget_with_binding(
        connection,
        actor,
        SettlementTarget {
            campaign_id: binding.campaign_id,
            reservation_id: binding.reservation_id,
            expected_binding: Some((binding.use_kind, binding.entity_key)),
        },
        mode,
        settlements,
        note,
    )
}

pub(crate) fn settle_bound_budget_in(
    transaction: &Transaction<'_>,
    actor: &str,
    binding: BoundReservation<'_>,
    mode: SettlementMode,
    settlements: &[BudgetSettlement],
    note: Option<&str>,
) -> Result<SettlementOutcome> {
    settle_budget_in(
        transaction,
        actor,
        SettlementTarget {
            campaign_id: binding.campaign_id,
            reservation_id: binding.reservation_id,
            expected_binding: Some((binding.use_kind, binding.entity_key)),
        },
        mode,
        settlements,
        note,
    )
}

struct SettlementTarget<'a> {
    campaign_id: &'a str,
    reservation_id: &'a str,
    expected_binding: Option<(&'a str, &'a str)>,
}

fn settle_budget_with_binding(
    connection: &Connection,
    actor: &str,
    target: SettlementTarget<'_>,
    mode: SettlementMode,
    settlements: &[BudgetSettlement],
    note: Option<&str>,
) -> Result<SettlementOutcome> {
    let transaction = begin_mutation(connection)?;
    let outcome = settle_budget_in(&transaction, actor, target, mode, settlements, note)?;
    transaction.commit()?;
    Ok(outcome)
}

fn settle_budget_in(
    transaction: &Transaction<'_>,
    actor: &str,
    target: SettlementTarget<'_>,
    mode: SettlementMode,
    settlements: &[BudgetSettlement],
    note: Option<&str>,
) -> Result<SettlementOutcome> {
    let campaign_id = target.campaign_id;
    let reservation_id = target.reservation_id;
    validate_request_identity(actor, campaign_id, reservation_id)?;
    let measured = canonical_settlements(settlements)?;
    if mode == SettlementMode::ChargeReservation && !measured.is_empty() {
        bail!("charge-reservation settlement cannot also declare measured amounts");
    }
    require_campaign(transaction, campaign_id)?;
    let durable_binding = transaction
        .query_row(
            "SELECT use_kind, entity_key FROM budget_reservation_uses
             WHERE campaign_id=?1 AND reservation_id=?2",
            params![campaign_id, reservation_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    match (target.expected_binding, durable_binding.as_ref()) {
        (None, Some(_)) => {
            bail!("generic settlement refuses a lifecycle-bound budget reservation")
        }
        (Some((kind, key)), Some((durable_kind, durable_key)))
            if kind == durable_kind && key == durable_key => {}
        (Some(_), _) => bail!("bound settlement does not match the reservation's exact entity"),
        (None, None) => {}
    }
    let rows = reservation_rows(transaction, campaign_id, reservation_id)?;
    if rows.is_empty() {
        bail!("unknown budget reservation '{reservation_id}' for campaign '{campaign_id}'");
    }

    if rows
        .iter()
        .all(|row| row.status != BudgetReservationStatus::Reserved)
    {
        if terminal_settlement_matches(&rows, mode, &measured, note) {
            return Ok(SettlementOutcome::Existing);
        }
        bail!("budget reservation '{reservation_id}' was already settled differently");
    }
    if rows
        .iter()
        .any(|row| row.status != BudgetReservationStatus::Reserved)
    {
        bail!("budget reservation '{reservation_id}' has mixed terminal state");
    }

    if mode == SettlementMode::Measured {
        let reserved_resources: BTreeSet<_> = rows.iter().map(|row| row.resource).collect();
        let measured_resources: BTreeSet<_> = measured.keys().copied().collect();
        if reserved_resources != measured_resources {
            bail!("measured settlement must name every reserved resource exactly once");
        }
    }

    let settled_at = now();
    let status = match mode {
        SettlementMode::Measured => BudgetReservationStatus::Settled,
        SettlementMode::ChargeReservation => BudgetReservationStatus::Charged,
    };
    let mut charged = Vec::with_capacity(rows.len());
    for row in &rows {
        let actual = match mode {
            SettlementMode::Measured => measured[&row.resource],
            SettlementMode::ChargeReservation => row.reserved_amount,
        };
        if actual > row.reserved_amount {
            bail!(
                "measured '{}' use {actual} exceeds reservation {} for '{reservation_id}'",
                row.resource,
                row.reserved_amount
            );
        }
        transaction.execute(
            "UPDATE budget_balances
             SET reserved_amount=reserved_amount - ?3,
                 spent_amount=spent_amount + ?4
             WHERE campaign_id=?1 AND resource=?2",
            params![
                campaign_id,
                row.resource.as_str(),
                sql_amount(row.reserved_amount)?,
                sql_amount(actual)?
            ],
        )?;
        transaction.execute(
            "UPDATE budget_reservations
             SET settled_amount=?4, status=?5, settled_at=?6, note=?7
             WHERE reservation_id=?1 AND campaign_id=?2 AND resource=?3",
            params![
                reservation_id,
                campaign_id,
                row.resource.as_str(),
                sql_amount(actual)?,
                status.as_str(),
                settled_at,
                note
            ],
        )?;
        charged.push(json!({
            "resource": row.resource,
            "unit": row.resource.unit(),
            "reserved_amount": row.reserved_amount,
            "settled_amount": actual,
        }));
    }
    record_event_in_mutation(
        transaction,
        actor,
        "campaign",
        campaign_id,
        "budget-settled",
        note,
        Some(&json!({
            "reservation_id": reservation_id,
            "mode": mode,
            "resources": charged,
        })),
    )?;
    Ok(SettlementOutcome::Settled)
}

pub fn budget_balances(connection: &Connection, campaign_id: &str) -> Result<Vec<BudgetBalance>> {
    let mut statement = connection.prepare(
        "SELECT l.resource, l.unit, l.hard_limit, b.reserved_amount, b.spent_amount
         FROM budget_limits l
         JOIN budget_balances b
           ON b.campaign_id=l.campaign_id AND b.resource=l.resource
         WHERE l.campaign_id=?1 ORDER BY l.resource",
    )?;
    let rows = statement
        .query_map(params![campaign_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(
            |(resource, unit, hard_limit, reserved_amount, spent_amount)| {
                let resource: BudgetResource = resource.parse()?;
                if unit != resource.unit() {
                    bail!(
                        "stored budget '{}' has unit '{unit}', expected '{}'",
                        resource,
                        resource.unit()
                    );
                }
                let hard_limit = u64::try_from(hard_limit).context("negative hard budget limit")?;
                let reserved_amount =
                    u64::try_from(reserved_amount).context("negative reserved budget amount")?;
                let spent_amount =
                    u64::try_from(spent_amount).context("negative spent budget amount")?;
                let available_amount = hard_limit
                    .checked_sub(reserved_amount)
                    .and_then(|amount| amount.checked_sub(spent_amount))
                    .context("stored budget balance exceeds its hard limit")?;
                Ok(BudgetBalance {
                    resource,
                    unit,
                    hard_limit,
                    reserved_amount,
                    spent_amount,
                    available_amount,
                })
            },
        )
        .collect()
}

#[derive(Debug)]
struct ReservationRow {
    resource: BudgetResource,
    reserved_amount: u64,
    settled_amount: Option<u64>,
    status: BudgetReservationStatus,
    note: Option<String>,
}

fn reservation_rows(
    connection: &Connection,
    campaign_id: &str,
    reservation_id: &str,
) -> Result<Vec<ReservationRow>> {
    let mut statement = connection.prepare(
        "SELECT resource, unit, reserved_amount, settled_amount, status, note
         FROM budget_reservations
         WHERE campaign_id=?1 AND reservation_id=?2 ORDER BY resource",
    )?;
    let rows = statement
        .query_map(params![campaign_id, reservation_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(
            |(resource, unit, reserved_amount, settled_amount, status, note)| {
                let resource: BudgetResource = resource.parse()?;
                if unit != resource.unit() {
                    bail!(
                        "reservation resource '{}' has unit '{unit}', expected '{}'",
                        resource,
                        resource.unit()
                    );
                }
                Ok(ReservationRow {
                    resource,
                    reserved_amount: u64::try_from(reserved_amount)
                        .context("negative reservation amount")?,
                    settled_amount: settled_amount
                        .map(|amount| {
                            u64::try_from(amount).context("negative settled reservation amount")
                        })
                        .transpose()?,
                    status: BudgetReservationStatus::parse_column(
                        "budget_reservations.status",
                        &status,
                    )?,
                    note,
                })
            },
        )
        .collect()
}

fn terminal_settlement_matches(
    rows: &[ReservationRow],
    mode: SettlementMode,
    measured: &BTreeMap<BudgetResource, u64>,
    note: Option<&str>,
) -> bool {
    let expected_status = match mode {
        SettlementMode::Measured => BudgetReservationStatus::Settled,
        SettlementMode::ChargeReservation => BudgetReservationStatus::Charged,
    };
    rows.iter().all(|row| {
        let expected_amount = match mode {
            SettlementMode::Measured => measured.get(&row.resource).copied(),
            SettlementMode::ChargeReservation => Some(row.reserved_amount),
        };
        row.status == expected_status
            && row.settled_amount == expected_amount
            && row.note.as_deref() == note
    }) && match mode {
        SettlementMode::Measured => measured.len() == rows.len(),
        SettlementMode::ChargeReservation => measured.is_empty(),
    }
}

fn budget_balance_in(
    connection: &Connection,
    campaign_id: &str,
    resource: BudgetResource,
) -> Result<Option<BudgetBalance>> {
    connection
        .query_row(
            "SELECT l.unit, l.hard_limit, b.reserved_amount, b.spent_amount
             FROM budget_limits l
             JOIN budget_balances b
               ON b.campaign_id=l.campaign_id AND b.resource=l.resource
             WHERE l.campaign_id=?1 AND l.resource=?2",
            params![campaign_id, resource.as_str()],
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
        .map(|(unit, hard_limit, reserved_amount, spent_amount)| {
            let hard_limit = u64::try_from(hard_limit).context("negative hard budget limit")?;
            let reserved_amount =
                u64::try_from(reserved_amount).context("negative reserved budget amount")?;
            let spent_amount =
                u64::try_from(spent_amount).context("negative spent budget amount")?;
            let available_amount = hard_limit
                .checked_sub(reserved_amount)
                .and_then(|amount| amount.checked_sub(spent_amount))
                .context("stored budget balance exceeds its hard limit")?;
            Ok(BudgetBalance {
                resource,
                unit,
                hard_limit,
                reserved_amount,
                spent_amount,
                available_amount,
            })
        })
        .transpose()
}

fn canonical_requests(requests: &[BudgetRequest]) -> Result<BTreeMap<BudgetResource, u64>> {
    if requests.is_empty() {
        bail!("budget reservation requires at least one resource");
    }
    let mut canonical = BTreeMap::new();
    for request in requests {
        if request.amount == 0 {
            bail!("budget reservation '{}' must be nonzero", request.resource);
        }
        sql_amount(request.amount)?;
        if canonical.insert(request.resource, request.amount).is_some() {
            bail!("budget reservation repeats resource '{}'", request.resource);
        }
    }
    Ok(canonical)
}

fn canonical_settlements(
    settlements: &[BudgetSettlement],
) -> Result<BTreeMap<BudgetResource, u64>> {
    let mut canonical = BTreeMap::new();
    for settlement in settlements {
        sql_amount(settlement.actual_amount)?;
        if canonical
            .insert(settlement.resource, settlement.actual_amount)
            .is_some()
        {
            bail!(
                "budget settlement repeats resource '{}'",
                settlement.resource
            );
        }
    }
    Ok(canonical)
}

fn existing_requests(rows: &[ReservationRow]) -> BTreeMap<BudgetResource, u64> {
    rows.iter()
        .map(|row| (row.resource, row.reserved_amount))
        .collect()
}

fn validate_request_identity(actor: &str, campaign_id: &str, reservation_id: &str) -> Result<()> {
    for (field, value) in [
        ("actor", actor),
        ("campaign_id", campaign_id),
        ("reservation_id", reservation_id),
    ] {
        if value.trim().is_empty() {
            bail!("{field} must not be blank");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::store::{CampaignAdmission, admit_campaign, campaign_events, init};

    fn prepared() -> Connection {
        let connection = Connection::open_in_memory().expect("database");
        init(&connection).expect("initialize");
        let mut manifest = crate::manifest::tests::valid_manifest();
        manifest.campaign_id = "budget-test".to_owned();
        manifest.execution_limits.maximum_trial_wall_time_ms = 1_000;
        manifest
            .budgets
            .caps
            .iter_mut()
            .find(|cap| cap.resource == BudgetResource::WallTimeMilliseconds)
            .expect("wall cap")
            .hard_limit = 1_000;
        let admission = CampaignAdmission::from_manifest(&manifest).expect("admission");
        admit_campaign(&connection, "agent", &admission).expect("admit");
        connection
    }

    #[test]
    fn cumulative_reservation_and_measured_settlement_are_atomic_and_idempotent() {
        let connection = prepared();
        let requests = [
            BudgetRequest::new(BudgetResource::Candidates, 1).expect("request"),
            BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 600).expect("request"),
        ];
        assert_eq!(
            reserve_budget(
                &connection,
                "agent",
                "budget-test",
                "candidate-1",
                &requests
            )
            .expect("reserve"),
            ReservationOutcome::Reserved
        );
        assert_eq!(
            reserve_budget(
                &connection,
                "agent",
                "budget-test",
                "candidate-1",
                &requests
            )
            .expect("reserve replay"),
            ReservationOutcome::Existing
        );
        assert!(
            reserve_budget(
                &connection,
                "agent",
                "budget-test",
                "too-large",
                &[BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 500).expect("request")],
            )
            .is_err()
        );

        let settlements = [
            BudgetSettlement {
                resource: BudgetResource::Candidates,
                actual_amount: 1,
            },
            BudgetSettlement {
                resource: BudgetResource::WallTimeMilliseconds,
                actual_amount: 450,
            },
        ];
        assert_eq!(
            settle_budget(
                &connection,
                "agent",
                "budget-test",
                "candidate-1",
                SettlementMode::Measured,
                &settlements,
                Some("completed"),
            )
            .expect("settle"),
            SettlementOutcome::Settled
        );
        assert_eq!(
            settle_budget(
                &connection,
                "agent",
                "budget-test",
                "candidate-1",
                SettlementMode::Measured,
                &settlements,
                Some("completed"),
            )
            .expect("settlement replay"),
            SettlementOutcome::Existing
        );
        let balances = budget_balances(&connection, "budget-test").expect("balances");
        let wall = balances
            .iter()
            .find(|balance| balance.resource == BudgetResource::WallTimeMilliseconds)
            .expect("wall balance");
        assert_eq!(wall.reserved_amount, 0);
        assert_eq!(wall.spent_amount, 450);
        assert_eq!(wall.available_amount, 550);
        assert_eq!(
            campaign_events(&connection, "budget-test")
                .expect("events")
                .len(),
            3
        );
    }

    #[test]
    fn crash_settlement_charges_the_full_reservation() {
        let connection = prepared();
        reserve_budget(
            &connection,
            "agent",
            "budget-test",
            "candidate-crash",
            &[
                BudgetRequest::new(BudgetResource::Candidates, 1).expect("request"),
                BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 700).expect("request"),
            ],
        )
        .expect("reserve");
        settle_budget(
            &connection,
            "recovery-agent",
            "budget-test",
            "candidate-crash",
            SettlementMode::ChargeReservation,
            &[],
            Some("worker process disappeared before usage reconciliation"),
        )
        .expect("charge reservation");

        let balances = budget_balances(&connection, "budget-test").expect("balances");
        let wall = balances
            .iter()
            .find(|balance| balance.resource == BudgetResource::WallTimeMilliseconds)
            .expect("wall balance");
        assert_eq!(wall.reserved_amount, 0);
        assert_eq!(wall.spent_amount, 700);
        assert_eq!(wall.available_amount, 300);
        assert!(
            reserve_budget(
                &connection,
                "agent",
                "budget-test",
                "after-crash",
                &[BudgetRequest::new(BudgetResource::WallTimeMilliseconds, 301).expect("request")],
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_durable_reservation_status_fails_closed_with_column_and_value() {
        let connection = prepared();
        reserve_budget(
            &connection,
            "agent",
            "budget-test",
            "corrupt-status",
            &[BudgetRequest::new(BudgetResource::Candidates, 1).unwrap()],
        )
        .unwrap();
        connection
            .execute_batch(
                "PRAGMA ignore_check_constraints=ON;
                 DROP TRIGGER budget_reservation_transition_guard;
                 UPDATE budget_reservations SET status='banana'
                 WHERE reservation_id='corrupt-status';",
            )
            .unwrap();

        let error = reserve_budget(
            &connection,
            "agent",
            "budget-test",
            "corrupt-status",
            &[BudgetRequest::new(BudgetResource::Candidates, 1).unwrap()],
        )
        .expect_err("unknown durable status must refuse");
        assert_eq!(
            error.to_string(),
            "unknown stored budget_reservations.status value 'banana'"
        );
    }
}
