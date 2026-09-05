//! Bounded discovery of recorded campaign work, without execution or CAS claims.

use std::collections::BTreeMap;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::manifest::CampaignManifest;
use crate::{BudgetBalance, budget_balances, campaign, sha256};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InspectionSection {
    Candidates,
    Trials,
    Cohorts,
    Reservations,
}

impl InspectionSection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidates => "candidates",
            Self::Trials => "trials",
            Self::Cohorts => "cohorts",
            Self::Reservations => "reservations",
        }
    }

    fn query(self) -> (&'static str, &'static str) {
        match self {
            Self::Candidates => ("candidates", "candidate_id"),
            Self::Trials => ("trials", "trial_id"),
            Self::Cohorts => ("paired_cohorts", "cohort_id"),
            Self::Reservations => ("budget_reservations", "reservation_id"),
        }
    }
}

impl FromStr for InspectionSection {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "candidates" => Ok(Self::Candidates),
            "trials" => Ok(Self::Trials),
            "cohorts" => Ok(Self::Cohorts),
            "reservations" => Ok(Self::Reservations),
            _ => bail!("pass --section candidates, trials, cohorts, or reservations"),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CampaignInspection {
    pub schema: &'static str,
    pub campaign_id: String,
    pub manifest_sha256: String,
    pub evidence_check: &'static str,
    pub execution_mode: &'static str,
    pub counts: BTreeMap<String, u64>,
    pub recorded_budgets: Vec<BudgetBalance>,
    pub section: InspectionSection,
    pub ordering: &'static str,
    pub offset: u64,
    pub total: u64,
    pub returned: u64,
    pub omitted: u64,
    pub next_offset: Option<u64>,
    pub items: Vec<Value>,
}

/// Each response is one SQLite read snapshot. Pages are live discovery, not a
/// frozen evidence receipt; callers should restart discovery after concurrent
/// changes. The CLI opens the authority read-only before entering this API.
pub fn inspect_campaign(
    connection: &Connection,
    campaign_id: &str,
    section: InspectionSection,
    limit: u64,
    offset: u64,
) -> Result<CampaignInspection> {
    if !(1..=100).contains(&limit) || offset > i64::MAX as u64 {
        bail!(
            "campaign inspect requires --limit 1..100 and --offset 0..{}",
            i64::MAX
        );
    }
    let snapshot = connection.unchecked_transaction()?;
    let record = campaign(&snapshot, campaign_id)?.with_context(|| {
        format!(
            "unknown campaign '{campaign_id}'; run `status --json` and pass an admitted campaign ID"
        )
    })?;
    let manifest: CampaignManifest = serde_json::from_str(&record.manifest_json).context(
        "invalid stored manifest; restore the campaign authority from verified recovery evidence",
    )?;
    if manifest.campaign_id != campaign_id
        || manifest.schema != record.manifest_schema
        || sha256(record.manifest_json.as_bytes()) != record.manifest_sha256
    {
        bail!(
            "stored campaign identity mismatch; restore the campaign authority from verified recovery evidence"
        );
    }
    let mut counts = BTreeMap::new();
    for kind in [
        InspectionSection::Candidates,
        InspectionSection::Trials,
        InspectionSection::Cohorts,
        InspectionSection::Reservations,
    ] {
        let (table, identity) = kind.query();
        let count: i64 = snapshot.query_row(
            &format!("SELECT COUNT(DISTINCT {identity}) FROM {table} WHERE campaign_id=?1"),
            [campaign_id],
            |row| row.get(0),
        )?;
        counts.insert(kind.as_str().to_owned(), u64::try_from(count)?);
    }
    let (table, identity) = section.query();
    let mut statement = snapshot.prepare(&format!(
        "SELECT DISTINCT {identity} FROM {table} WHERE campaign_id=?1
         ORDER BY {identity} LIMIT ?2 OFFSET ?3"
    ))?;
    let ids = statement
        .query_map(
            params![campaign_id, i64::try_from(limit)?, i64::try_from(offset)?],
            |row| row.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let items = ids
        .iter()
        .map(|id| inspect_item(&snapshot, &manifest, section, id))
        .collect::<Result<Vec<_>>>()?;
    let total = counts[section.as_str()];
    let returned = u64::try_from(items.len())?;
    let next = offset
        .checked_add(returned)
        .context("inspection page offset overflow; reduce --offset")?;
    Ok(CampaignInspection {
        schema: "papertiger-mise.campaign-inspection.v1",
        campaign_id: campaign_id.to_owned(),
        manifest_sha256: record.manifest_sha256,
        evidence_check: "recorded_state_only_cas_not_reopened",
        execution_mode: if manifest.paired_analysis.is_some() {
            "paired"
        } else {
            "deterministic"
        },
        recorded_budgets: budget_balances(&snapshot, campaign_id)?,
        counts,
        section,
        ordering: "identity_ascending_live_pages",
        offset,
        total,
        returned,
        omitted: total.saturating_sub(returned),
        next_offset: (next < total).then_some(next),
        items,
    })
}

fn inspect_item(
    connection: &Connection,
    manifest: &CampaignManifest,
    section: InspectionSection,
    id: &str,
) -> Result<Value> {
    let campaign_id = &manifest.campaign_id;
    Ok(match section {
        InspectionSection::Candidates => {
            let record = crate::candidate(connection, id)?
                .context("candidate disappeared within inspection snapshot")?;
            let role = if record.material_sha256 == manifest.calibration.no_op_material_sha256().0 {
                "no_op_calibration"
            } else if record.material_sha256 == manifest.calibration.known_bad_material_sha256().0 {
                "known_bad_calibration"
            } else {
                "research"
            };
            let nomination_id: Option<String> = connection.query_row(
                "SELECT nomination_id FROM nominations WHERE campaign_id=?1 AND candidate_id=?2",
                params![campaign_id, id], |row| row.get(0),
            ).optional()?;
            json!({"candidate_id": id, "disposition": record.disposition, "role": role,
                "nomination_id": nomination_id,
                "inspect_arguments": ["candidate", "show", id]})
        }
        InspectionSection::Trials => {
            let record = crate::trial(connection, id)?
                .context("trial disappeared within inspection snapshot")?;
            json!({"trial_id": id, "candidate_id": record.candidate_id, "status": record.status,
                "tier": record.tier, "inspect_arguments": ["trial", "show", id]})
        }
        InspectionSection::Cohorts => {
            let record = crate::paired_cohort(connection, id)?
                .context("cohort disappeared within inspection snapshot")?;
            json!({"cohort_id": id, "candidate_id": record.candidate_id, "status": record.status,
                "inspect_arguments": ["paired", "show-cohort", id]})
        }
        InspectionSection::Reservations => {
            let mut statement = connection.prepare(
                "SELECT resource, reserved_amount, settled_amount, status, u.use_kind, u.entity_key
                 FROM budget_reservations r LEFT JOIN budget_reservation_uses u
                 ON u.campaign_id=r.campaign_id AND u.reservation_id=r.reservation_id
                 WHERE r.campaign_id=?1 AND r.reservation_id=?2 ORDER BY resource",
            )?;
            let rows = statement
                .query_map(params![campaign_id, id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let resources = rows
                .into_iter()
                .map(|(resource, reserved, settled, status, use_kind, entity)| {
                    let resource: crate::BudgetResource = resource.parse()?;
                    let status = crate::BudgetReservationStatus::parse_column(
                        "budget_reservations.status",
                        &status,
                    )?;
                    let reserved = u64::try_from(reserved)
                        .context("negative reservation; restore verified authority")?;
                    let settled = settled
                        .map(u64::try_from)
                        .transpose()
                        .context("negative settlement; restore verified authority")?;
                    Ok(
                        json!({"resource": resource, "reserved": reserved, "settled": settled,
                    "status": status, "use_kind": use_kind, "entity_key": entity}),
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            json!({"reservation_id": id, "resources": resources,
                "inspect_arguments": ["budget", "show", campaign_id]})
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{CampaignAdmission, admit_campaign};

    #[test]
    fn inspection_pages_distinct_reservations_and_never_mutates_authority() {
        let connection = Connection::open_in_memory().unwrap();
        crate::init(&connection).unwrap();
        let manifest = crate::manifest::tests::valid_manifest();
        admit_campaign(
            &connection,
            "test",
            &CampaignAdmission::from_manifest(&manifest).unwrap(),
        )
        .unwrap();
        for id in ["c", "a", "b"] {
            crate::reserve_budget(
                &connection,
                "test",
                &manifest.campaign_id,
                id,
                &[crate::BudgetRequest {
                    resource: crate::BudgetResource::Candidates,
                    amount: 1,
                }],
            )
            .unwrap();
        }
        let changes = connection.total_changes();
        connection.pragma_update(None, "query_only", true).unwrap();
        let first = inspect_campaign(
            &connection,
            &manifest.campaign_id,
            InspectionSection::Reservations,
            2,
            0,
        )
        .unwrap();
        assert_eq!(
            (
                first.total,
                first.returned,
                first.omitted,
                first.next_offset
            ),
            (3, 2, 1, Some(2))
        );
        assert_eq!(first.items[0]["reservation_id"], "a");
        assert_eq!(first.items[1]["reservation_id"], "b");
        let second = inspect_campaign(
            &connection,
            &manifest.campaign_id,
            InspectionSection::Reservations,
            2,
            2,
        )
        .unwrap();
        assert_eq!(second.items[0]["reservation_id"], "c");
        assert_eq!(second.next_offset, None);
        for section in [
            InspectionSection::Candidates,
            InspectionSection::Trials,
            InspectionSection::Cohorts,
        ] {
            let empty =
                inspect_campaign(&connection, &manifest.campaign_id, section, 20, 0).unwrap();
            assert_eq!(empty.total, 0);
            assert!(empty.items.is_empty());
        }
        assert!(
            inspect_campaign(&connection, "missing", InspectionSection::Candidates, 20, 0).is_err()
        );
        assert!(
            inspect_campaign(
                &connection,
                &manifest.campaign_id,
                InspectionSection::Candidates,
                0,
                0
            )
            .is_err()
        );
        assert_eq!(connection.total_changes(), changes);
    }
}
