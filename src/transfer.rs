//! Export and import of papertiger dumps.

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::{
    CommitAssociation, ExternalReference, GATE_KINDS, MiseProjectionDump, TASK_COLS, TASK_STATUSES,
    Task, TaskPickup, add_dep_inner, begin_mutation, commit_associations, entry_blockers,
    external_reference, external_references, find_cycle, get_task, history_recovery,
    invalid_replacement_terminals, live_child_sequences, mise_projection, now, pickup, plan_move,
    record_event_in_mutation, replacement_repair_instruction, require_nonblank, task_from_row,
    terminal_replacement_repair_instruction, validate_commit_oid, validate_meaning_source,
    validate_new_evidence_locator, validate_optional_sha256, validate_tag, validate_task_kind,
    validate_task_title,
};

#[derive(Serialize, Deserialize)]
pub struct Dump {
    pub schema: String,
    pub plans: Vec<PlanDump>,
    pub tasks: Vec<TaskDump>,
    #[serde(default)]
    pub events: Vec<EventDump>,
    #[serde(default)]
    pub mise_projections: Vec<MiseProjectionDump>,
}

pub const DUMP_SCHEMA: &str = "papertiger.dump.v11";
const DUMP_SCHEMA_V10: &str = "papertiger.dump.v10";

/// Parse an exported dump, accepting the UTF-8 BOM emitted by Windows
/// PowerShell 5's common `Set-Content -Encoding utf8` workflow. A
/// `papertiger.dump.v10` document is converted by [`convert_dump_v10`] first.
pub fn parse_dump_json(text: &str) -> Result<Dump> {
    let mut value: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .context("parse papertiger dump")?;
    match value.get("schema").and_then(serde_json::Value::as_str) {
        Some(DUMP_SCHEMA_V10) => value = convert_dump_v10(value)?,
        Some(DUMP_SCHEMA) => {}
        other => bail!(
            "unsupported dump schema {}; `papertiger import <file>` reads {DUMP_SCHEMA} and converts {DUMP_SCHEMA_V10}; for older dumps, import into a temporary authority with the Papertiger release that produced it, run current `papertiger --db <temporary-authority> init`, then re-export {DUMP_SCHEMA}",
            other.map_or_else(|| "(missing)".to_owned(), |schema| format!("'{schema}'"))
        ),
    }
    serde_json::from_value(value).context("parse papertiger dump")
}

/// The only legacy dump conversion. v10 knew the task kinds `work`, `probe`
/// and `decision`, which v11 stores unchanged (`work` as the legacy kind),
/// and carried per-task `intent_source` and `result_source`, which v11
/// drops: the creation and completion events keep the recorded sources.
/// Integer priorities map to levels (positive `now`, zero `next`,
/// negative `later`). Each v10 blocker becomes an `external` gate with the
/// same condition,
/// outcome and evidence, renamed `blocker-<name>` if a gate holds its name.
/// Events and Mise projection documents are historical records and pass
/// through unchanged.
pub fn convert_dump_v10(mut value: serde_json::Value) -> Result<serde_json::Value> {
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(DUMP_SCHEMA_V10) {
        bail!(
            "convert_dump_v10 accepts only papertiger.dump.v10; re-export with the Papertiger release that produced the dump"
        );
    }
    let invalid = || {
        anyhow!(
            "papertiger.dump.v10 is malformed; re-export it with the Papertiger release that produced it"
        )
    };
    if let Some(tasks) = value
        .get_mut("tasks")
        .and_then(serde_json::Value::as_array_mut)
    {
        for task in tasks {
            let task = task.as_object_mut().ok_or_else(invalid)?;
            task.remove("intent_source");
            task.remove("result_source");
            if let Some(priority) = task.get("priority") {
                let level = crate::Priority::from_legacy(priority.as_i64().ok_or_else(invalid)?);
                task.insert("priority".into(), level.as_str().into());
            }
            let Some(blockers) = task.remove("blockers") else {
                continue;
            };
            let gates = task
                .entry("gates")
                .or_insert_with(|| serde_json::Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(invalid)?;
            for blocker in blockers.as_array().ok_or_else(invalid)? {
                let blocker = blocker.as_object().ok_or_else(invalid)?;
                let name = blocker
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(invalid)?;
                let taken = |candidate: &str| {
                    gates.iter().any(|gate| {
                        gate.get("name").and_then(serde_json::Value::as_str) == Some(candidate)
                    })
                };
                let name = if taken(name) {
                    let renamed = format!("blocker-{name}");
                    if taken(&renamed) {
                        bail!(
                            "papertiger.dump.v10 task blocker '{name}' cannot become an external gate: both '{name}' and '{renamed}' are gate names on that task; rename one with the release that produced the dump"
                        );
                    }
                    renamed
                } else {
                    name.to_owned()
                };
                let mut gate = serde_json::Map::new();
                gate.insert("name".into(), name.into());
                gate.insert("kind".into(), "external".into());
                gate.insert(
                    "requirement".into(),
                    blocker.get("condition").cloned().ok_or_else(invalid)?,
                );
                for key in [
                    "status",
                    "evidence_locator",
                    "evidence_sha256",
                    "note",
                    "resolved_at",
                ] {
                    if let Some(value) = blocker.get(key) {
                        gate.insert(key.into(), value.clone());
                    }
                }
                gates.push(serde_json::Value::Object(gate));
            }
        }
    }
    value["schema"] = DUMP_SCHEMA.into();
    Ok(value)
}

#[derive(Serialize, Deserialize)]
pub struct PlanDump {
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default = "default_active")]
    pub status: String,
}

fn default_active() -> String {
    "active".into()
}

#[derive(Serialize, Deserialize)]
pub struct TaskDump {
    pub pickup: Option<TaskPickup>,
    #[serde(default)]
    pub seq: Option<i64>,
    pub plan: String,
    pub title: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default = "default_work")]
    pub kind: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default = "default_proposed")]
    pub status: String,
    #[serde(default)]
    pub priority: crate::Priority,
    #[serde(default)]
    pub parent_seq: Option<i64>,
    #[serde(default)]
    pub replacement_seq: Option<i64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub deps: Vec<i64>,
    #[serde(default)]
    pub gates: Vec<GateDump>,
    #[serde(default)]
    pub commit_associations: Vec<CommitAssociation>,
    #[serde(default)]
    pub external_references: Vec<ExternalReference>,
}

fn default_work() -> String {
    "work".into()
}

fn default_proposed() -> String {
    "proposed".into()
}

#[derive(Serialize, Deserialize)]
pub struct GateDump {
    pub name: String,
    pub kind: String,
    pub requirement: String,
    #[serde(default = "default_open")]
    pub status: String,
    #[serde(default)]
    pub evidence_locator: Option<String>,
    #[serde(default)]
    pub evidence_sha256: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub resolved_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct EventDump {
    pub at: String,
    pub actor: String,
    pub entity: String,
    #[serde(default)]
    pub entity_seq: Option<i64>,
    #[serde(default)]
    pub entity_plan: Option<String>,
    #[serde(default)]
    pub gate_name: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
}

pub(crate) fn default_open() -> String {
    "open".into()
}

pub fn export(conn: &Connection, plan: Option<&str>) -> Result<Dump> {
    history_recovery::validate(conn)?;
    let mut plans = Vec::new();
    let mut st =
        conn.prepare("SELECT plan_id, slug, title, intent, status FROM plans ORDER BY plan_id")?;
    let all: Vec<(i64, String, String, String, String)> = st
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut tasks = Vec::new();
    for (plan_id, slug, title, intent, status) in all {
        if let Some(want) = plan
            && want != slug
        {
            continue;
        }
        plans.push(PlanDump {
            slug: slug.clone(),
            title,
            intent,
            status,
        });
        let mut st = conn.prepare(&format!(
            "SELECT {TASK_COLS} FROM tasks WHERE plan_id=?1 ORDER BY seq"
        ))?;
        let plan_tasks: Vec<Task> = st
            .query_map(params![plan_id], task_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        for t in plan_tasks {
            let parent_seq = match t.parent_id {
                Some(pid) => conn
                    .query_row(
                        "SELECT seq FROM tasks WHERE task_id=?1",
                        params![pid],
                        |r| r.get(0),
                    )
                    .optional()?,
                None => None,
            };
            let replacement_seq = match t.replacement_task_id {
                Some(replacement_task_id) => {
                    let replacement: (i64, String, Option<i64>) = conn
                        .query_row(
                        "SELECT seq, status, replacement_task_id FROM tasks WHERE task_id=?1",
                        params![replacement_task_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                        .optional()?
                        .ok_or_else(|| {
                            anyhow!(
                                "cannot export: #{} names missing replacement row id {}; {}; run `papertiger audit` to inspect every invalid replacement",
                                t.seq,
                                replacement_task_id,
                                replacement_repair_instruction(t.seq, &t.status, None)
                            )
                        })?;
                    if replacement.1 == "rejected"
                        || (replacement.1 == "retired" && replacement.2.is_none())
                    {
                        let terminal = if replacement.1 == "retired" {
                            "retired without its own replacement"
                        } else {
                            replacement.1.as_str()
                        };
                        bail!(
                            "cannot export: #{} replacement chain terminates at #{}, which is {}; {}; run `papertiger audit` to inspect every invalid replacement",
                            t.seq,
                            replacement.0,
                            terminal,
                            terminal_replacement_repair_instruction(replacement.0)
                        );
                    }
                    Some(replacement.0)
                }
                None => None,
            };
            let mut st = conn.prepare(
                "SELECT b.seq FROM deps d JOIN tasks b ON b.task_id=d.depends_on WHERE d.task_id=?1 ORDER BY b.seq",
            )?;
            let deps: Vec<i64> = st
                .query_map(params![t.task_id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut st = conn.prepare("SELECT tag FROM task_tags WHERE task_id=?1 ORDER BY tag")?;
            let tags: Vec<String> = st
                .query_map(params![t.task_id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut st = conn.prepare(
                "SELECT name, kind, requirement, status, evidence_locator, evidence_sha256, note, resolved_at
                 FROM gates WHERE task_id=?1 ORDER BY gate_id",
            )?;
            let gates: Vec<GateDump> = st
                .query_map(params![t.task_id], |r| {
                    Ok(GateDump {
                        name: r.get(0)?,
                        kind: r.get(1)?,
                        requirement: r.get(2)?,
                        status: r.get(3)?,
                        evidence_locator: r.get(4)?,
                        evidence_sha256: r.get(5)?,
                        note: r.get(6)?,
                        resolved_at: r.get(7)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            let commit_associations = commit_associations(conn, t.seq)?;
            let external_references = external_references(conn, t.seq)?;
            tasks.push(TaskDump {
                pickup: t.pickup,
                seq: Some(t.seq),
                plan: slug.clone(),
                title: t.title,
                intent: t.intent,
                kind: t.kind,
                result: t.result,
                status: t.status,
                priority: t.priority,
                parent_seq,
                replacement_seq,
                tags,
                deps,
                gates,
                commit_associations,
                external_references,
            });
        }
    }
    let selected_plans: HashSet<String> = plans.iter().map(|p| p.slug.clone()).collect();
    let selected_sequences = tasks
        .iter()
        .filter_map(|task| task.seq)
        .collect::<HashSet<_>>();
    let mut task_identities = conn.prepare("SELECT seq FROM tasks")?;
    let all_sequences = task_identities
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    let mut historical_plans = HashSet::new();
    let mut events = Vec::new();
    let mut st = conn.prepare(
        "SELECT at, actor, entity, entity_plan, entity_seq, gate_name, kind, why, payload
         FROM canonical_events ORDER BY event_id",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, Option<String>>(8)?,
        ))
    })?;
    for row in rows {
        let (at, actor, entity, entity_plan, entity_seq, gate_name, kind, why, payload) = row?;
        match entity.as_str() {
            "plan" if entity_plan.is_none() => {}
            "plan" | "task" | "dep" | "gate" => {}
            _ => continue,
        }
        if let Some(seq) = entity_seq {
            if !all_sequences.contains(&seq) {
                bail!(
                    "cannot export event for missing task #{seq}; run `papertiger audit` and restore a verified authority instead of omitting history"
                );
            }
            if !selected_sequences.contains(&seq) {
                continue;
            }
            if let Some(slug) = &entity_plan {
                historical_plans.insert(slug.clone());
            }
            if let Some(raw) = &payload {
                let value: serde_json::Value = serde_json::from_str(raw)?;
                if let Some(before) = value
                    .pointer("/changes/plan/before")
                    .and_then(serde_json::Value::as_str)
                {
                    historical_plans.insert(before.to_owned());
                }
            }
        } else if entity_plan
            .as_deref()
            .is_some_and(|slug| !selected_plans.contains(slug))
        {
            continue;
        }
        if matches!(entity.as_str(), "task" | "dep" | "gate")
            && (entity_plan.is_none() || entity_seq.is_none())
        {
            bail!("event history has a {entity} row without a stable plan/task reference");
        }
        if entity == "gate" && gate_name.is_none() {
            bail!("event history has a gate row without a stable gate name");
        }
        events.push(EventDump {
            at,
            actor,
            entity,
            entity_seq,
            entity_plan,
            gate_name,
            kind,
            why,
            payload: payload
                .map(|raw| serde_json::from_str(&raw).context("invalid stored event payload"))
                .transpose()?,
        });
    }

    let mut historical_plans = historical_plans
        .difference(&selected_plans)
        .collect::<Vec<_>>();
    historical_plans.sort();
    for slug in historical_plans {
        plans.push(conn.query_row(
            "SELECT slug, title, intent, status FROM plans WHERE slug=?1",
            params![slug],
            |row| {
                Ok(PlanDump {
                    slug: row.get(0)?,
                    title: row.get(1)?,
                    intent: row.get(2)?,
                    status: row.get(3)?,
                })
            },
        )?);
    }
    let selected_task_sequences = tasks
        .iter()
        .filter_map(|task| task.seq)
        .collect::<HashSet<_>>();
    let mise_projections =
        mise_projection::export_mise_projections(conn, &selected_task_sequences)?;

    Ok(Dump {
        schema: DUMP_SCHEMA.into(),
        plans,
        tasks,
        events,
        mise_projections,
    })
}

pub fn import(conn: &Connection, actor: &str, dump: &Dump) -> Result<(usize, usize)> {
    if dump.schema != DUMP_SCHEMA {
        bail!(
            "unsupported dump schema '{}'; `papertiger import <file>` reads {DUMP_SCHEMA} and converts {DUMP_SCHEMA_V10}; for older dumps, import into a temporary authority with the Papertiger release that produced it, run current `papertiger --db <temporary-authority> init`, then re-export {DUMP_SCHEMA}",
            dump.schema
        );
    }
    let tx = begin_mutation(conn)?;
    let dump_plan_slugs = dump
        .plans
        .iter()
        .map(|plan| plan.slug.as_str())
        .collect::<HashSet<_>>();
    if dump_plan_slugs.len() != dump.plans.len() {
        bail!("import repeats a plan slug; keep exactly one definition for each dump plan");
    }
    for p in &dump.plans {
        require_nonblank("import plan slug", &p.slug)?;
        require_nonblank("import plan title", &p.title)?;
        if !["active", "paused", "done", "retired"].contains(&p.status.as_str()) {
            bail!("import plan '{}' has unknown status '{}'", p.slug, p.status);
        }
        let existing: Option<(i64, String, String, String)> = tx
            .query_row(
                "SELECT plan_id, title, intent, status FROM plans WHERE slug=?1",
                params![p.slug],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((_, title, intent, status)) = existing {
            if title != p.title || intent != p.intent || status != p.status {
                bail!(
                    "import plan '{}' conflicts with the existing definition; import into an authority without that slug or make title, intent, and status identical",
                    p.slug
                );
            }
        } else {
            let t = now();
            tx.execute(
                "INSERT INTO plans (slug, title, intent, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![p.slug, p.title, p.intent, p.status, t],
            )?;
        }
    }

    // Allocate every sequence before writing anything. Explicit values are
    // stable; omitted values receive unused numbers above the current maximum.
    let mut used = HashSet::new();
    for td in &dump.tasks {
        if let Some(seq) = td.seq {
            if seq <= 0 {
                bail!("import task '{}' has nonpositive seq {seq}", td.title);
            }
            if !used.insert(seq) {
                bail!("import repeats task seq {seq}");
            }
        }
    }
    for seq in &used {
        let existing_title: Option<String> = tx
            .query_row(
                "SELECT title FROM tasks WHERE seq=?1",
                params![seq],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(title) = existing_title {
            bail!(
                "import task seq {seq} collides with existing task #{seq} '{title}'; import into an authority without that sequence"
            );
        }
    }
    let mut next_seq: i64 =
        tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM tasks", [], |r| r.get(0))?;
    if let Some(explicit_next) = used.iter().max().and_then(|seq| seq.checked_add(1)) {
        next_seq = next_seq.max(explicit_next);
    }
    let mut assigned = Vec::with_capacity(dump.tasks.len());
    for td in &dump.tasks {
        let seq = match td.seq {
            Some(seq) => seq,
            None => {
                while used.contains(&next_seq) {
                    next_seq += 1;
                }
                let seq = next_seq;
                used.insert(seq);
                next_seq += 1;
                seq
            }
        };
        assigned.push(seq);
    }
    let imported_plan_by_seq: HashMap<i64, &str> = assigned
        .iter()
        .copied()
        .zip(dump.tasks.iter().map(|task| task.plan.as_str()))
        .collect();

    // First pass: create tasks and gates.
    let mut created = 0usize;
    let mut imported_task_id_by_seq = HashMap::with_capacity(dump.tasks.len());
    for (td, seq) in dump.tasks.iter().zip(assigned.iter().copied()) {
        validate_task_title(&td.title).context("import task title is invalid")?;
        let plan_id: i64 = tx
            .query_row(
                "SELECT plan_id FROM plans WHERE slug=?1",
                params![td.plan],
                |r| r.get(0),
            )
            .with_context(|| {
                format!(
                    "import task '{0}' names missing plan '{1}'",
                    td.title, td.plan
                )
            })?;
        if !TASK_STATUSES.contains(&td.status.as_str()) {
            bail!("task '{}' has unknown status '{}'", td.title, td.status);
        }
        validate_task_kind(&td.kind)?;
        if td.status == "done" && td.gates.iter().any(|gate| gate.status == "open") {
            bail!("import: done task '{}' has an open gate", td.title);
        }
        if td.status == "done"
            && crate::kind_requires_result(&td.kind)
            && td
                .result
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
        {
            bail!(
                "import: completed {} task '{}' lacks a result",
                td.kind,
                td.title
            );
        }
        pickup::validate_pickup(&td.status, td.pickup.as_ref())?;
        let t = now();
        tx.execute(
            "INSERT INTO tasks
             (seq, plan_id, title, intent, kind, result, status, priority, created_at, updated_at, pickup_session, pickup_at,
              pickup_host, pickup_pid, pickup_process)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                seq,
                plan_id,
                td.title,
                td.intent,
                td.kind,
                td.result,
                td.status,
                td.priority,
                t,
                td.pickup.as_ref().and_then(|p| p.session.as_deref()),
                td.pickup.as_ref().map(|p| &p.at),
                td.pickup.as_ref().and_then(|p| p.owner.as_ref()).map(|o| &o.host),
                td.pickup.as_ref().and_then(|p| p.owner.as_ref()).map(|o| o.pid),
                td.pickup.as_ref().and_then(|p| p.owner.as_ref()).map(|o| &o.process),
            ],
        )
        .with_context(|| format!("import task seq {seq} '{}'", td.title))?;
        let id = tx.last_insert_rowid();
        imported_task_id_by_seq.insert(seq, id);
        for tag in &td.tags {
            let tag = validate_tag(tag)
                .with_context(|| format!("import task '{}' has invalid tag {tag:?}", td.title))?;
            tx.execute(
                "INSERT OR IGNORE INTO task_tags (task_id, tag) VALUES (?1, ?2)",
                params![id, tag],
            )?;
        }
        for g in &td.gates {
            require_nonblank("import gate name", &g.name)?;
            require_nonblank("import gate requirement", &g.requirement)?;
            if !GATE_KINDS.contains(&g.kind.as_str()) {
                bail!(
                    "import: gate '{}' on '{}' has unknown kind '{}'",
                    g.name,
                    td.title,
                    g.kind
                );
            }
            if !["open", "resolved", "waived"].contains(&g.status.as_str()) {
                bail!(
                    "import: gate '{}' on '{}' has unknown status '{}'",
                    g.name,
                    td.title,
                    g.status
                );
            }
            let resolved_at = match g.status.as_str() {
                "open" => {
                    if g.evidence_locator.is_some()
                        || g.evidence_sha256.is_some()
                        || g.resolved_at.is_some()
                    {
                        bail!(
                            "import: open gate '{}' on '{}' carries completion evidence or resolved_at",
                            g.name,
                            td.title
                        );
                    }
                    None
                }
                "resolved" => {
                    let locator = g.evidence_locator.as_deref().with_context(|| {
                        format!(
                            "import: resolved gate '{}' on '{}' lacks evidence_locator",
                            g.name, td.title
                        )
                    })?;
                    validate_new_evidence_locator(locator).with_context(|| {
                        format!(
                            "import resolved gate '{}' on '{}' has invalid evidence_locator {locator:?}",
                            g.name, td.title
                        )
                    })?;
                    validate_optional_sha256(g.evidence_sha256.as_deref())?;
                    let resolved_at = g.resolved_at.as_deref().with_context(|| {
                        format!(
                            "import: resolved gate '{}' on '{}' lacks resolved_at",
                            g.name, td.title
                        )
                    })?;
                    let resolved_at = resolved_at.trim();
                    chrono::DateTime::parse_from_rfc3339(resolved_at).with_context(|| {
                        format!(
                            "import: resolved gate '{}' on '{}' has invalid resolved_at '{}'",
                            g.name,
                            td.title,
                            g.resolved_at.as_deref().unwrap_or_default()
                        )
                    })?;
                    Some(resolved_at)
                }
                "waived" => {
                    if g.note.as_deref().map(str::trim).is_none_or(str::is_empty) {
                        bail!(
                            "import: waived gate '{}' on '{}' lacks a durable reason",
                            g.name,
                            td.title
                        );
                    }
                    if g.evidence_locator.is_some() || g.evidence_sha256.is_some() {
                        bail!(
                            "import: waived gate '{}' on '{}' carries completion evidence",
                            g.name,
                            td.title
                        );
                    }
                    let resolved_at = g.resolved_at.as_deref().with_context(|| {
                        format!(
                            "import: waived gate '{}' on '{}' lacks resolved_at",
                            g.name, td.title
                        )
                    })?;
                    let resolved_at = resolved_at.trim();
                    chrono::DateTime::parse_from_rfc3339(resolved_at).with_context(|| {
                        format!(
                            "import: waived gate '{}' on '{}' has invalid resolved_at '{}'",
                            g.name,
                            td.title,
                            g.resolved_at.as_deref().unwrap_or_default()
                        )
                    })?;
                    Some(resolved_at)
                }
                _ => unreachable!(),
            };
            tx.execute(
                "INSERT INTO gates (task_id, name, kind, requirement, status, evidence_locator, evidence_sha256, note, resolved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id, g.name, g.kind, g.requirement, g.status,
                    g.evidence_locator, g.evidence_sha256, g.note,
                    resolved_at
                ],
            )?;
        }
        for reference in &td.external_references {
            external_reference::insert_reference(&tx, id, reference)?;
        }
        for commit in &td.commit_associations {
            let repository =
                require_nonblank("import commit repository label", &commit.repository)?;
            validate_commit_oid(&commit.commit_oid)?;
            let recorded_at = commit.recorded_at.trim();
            chrono::DateTime::parse_from_rfc3339(recorded_at).with_context(|| {
                format!(
                    "import commit {} on task #{seq} has invalid recorded_at '{}'",
                    commit.commit_oid, commit.recorded_at
                )
            })?;
            tx.execute(
                "INSERT INTO commit_associations
                 (task_id, repository, commit_oid, note, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, repository, commit.commit_oid, commit.note, recorded_at],
            )?;
        }
        created += 1;
    }

    // Second pass: parents, replacements, and dependencies by the preassigned sequences.
    // References are self-contained within the dump and cannot accidentally
    // bind to an unrelated task already present in the destination database.
    let mut edges = 0usize;
    for (td, seq) in dump.tasks.iter().zip(assigned.iter().copied()) {
        if let Some(pseq) = td.parent_seq {
            let parent_plan = imported_plan_by_seq
                .get(&pseq)
                .with_context(|| format!("import: #{seq} has missing parent #{pseq}"))?;
            if *parent_plan != td.plan {
                bail!("import: #{seq} and parent #{pseq} belong to different plans");
            }
            let parent_id = imported_task_id_by_seq[&pseq];
            tx.execute(
                "UPDATE tasks SET parent_id=?1 WHERE seq=?2",
                params![parent_id, seq],
            )?;
        }
        if let Some(replacement_seq) = td.replacement_seq {
            if td.status != "retired" {
                bail!(
                    "import: #{seq} has replacement #{replacement_seq} but status '{}' is not retired",
                    td.status
                );
            }
            if replacement_seq == seq {
                bail!("import: #{seq} cannot replace itself");
            }
            let replacement_plan =
                imported_plan_by_seq
                    .get(&replacement_seq)
                    .with_context(|| {
                        format!("import: #{seq} has missing replacement #{replacement_seq}")
                    })?;
            if *replacement_plan != td.plan {
                bail!(
                    "import: #{seq} and replacement #{replacement_seq} belong to different plans"
                );
            }
            let replacement_id = imported_task_id_by_seq[&replacement_seq];
            tx.execute(
                "UPDATE tasks SET replacement_task_id=?1 WHERE seq=?2",
                params![replacement_id, seq],
            )?;
        }
        for on in &td.deps {
            let dependency_plan = imported_plan_by_seq
                .get(on)
                .with_context(|| format!("import: #{seq} depends on missing #{on}"))?;
            if *dependency_plan != td.plan {
                bail!("import: #{seq} and dependency #{on} belong to different plans");
            }
            add_dep_inner(&tx, actor, seq, *on, false, None)?;
            edges += 1;
        }
    }
    if let Some(cycle) = find_cycle(
        &tx,
        "SELECT task_id, parent_id FROM tasks WHERE parent_id IS NOT NULL",
    )? {
        bail!(
            "import creates parent cycle {}",
            cycle
                .iter()
                .map(|seq| format!("#{seq}"))
                .collect::<Vec<_>>()
                .join(" -> ")
        );
    }
    if let Some(cycle) = find_cycle(
        &tx,
        "SELECT task_id, replacement_task_id FROM tasks WHERE replacement_task_id IS NOT NULL",
    )? {
        bail!(
            "import creates replacement cycle {}",
            cycle
                .iter()
                .map(|seq| format!("#{seq}"))
                .collect::<Vec<_>>()
                .join(" -> ")
        );
    }
    if let Some((source, target, status)) = invalid_replacement_terminals(&tx)?.into_iter().next() {
        let terminal = if status == "retired" {
            "retired without its own replacement".to_owned()
        } else {
            status
        };
        bail!(
            "import: #{source} replacement chain terminates at #{target}, which is {terminal}; re-export after repairing the source authority: {}",
            terminal_replacement_repair_instruction(target)
        );
    }
    let mut statement =
        tx.prepare("SELECT task_id, seq FROM tasks WHERE status='done' ORDER BY seq")?;
    let done_tasks = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (task_id, seq) in done_tasks {
        let blockers = entry_blockers(&tx, &get_task(&tx, seq)?)?;
        if !blockers.is_empty() {
            bail!(
                "import: done task #{seq} retains open prerequisite(s): {}",
                blockers.join(", ")
            );
        }
        let children = live_child_sequences(&tx, task_id)?;
        if !children.is_empty() {
            bail!(
                "import: done task #{seq} has unfinished child task(s): {}",
                children
                    .iter()
                    .map(|child| format!("#{child}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    mise_projection::import_mise_projections(
        &tx,
        &dump.mise_projections,
        &imported_task_id_by_seq,
    )?;

    plan_move::validate_plan_history(dump)?;
    // Restore the append-only history using stable task sequences, plan slugs,
    // and gate names rather than database-local row ids.
    for event in &dump.events {
        if event.kind == "quarantine_event" {
            history_recovery::validate_envelope(event.payload.as_ref())?;
        }
        crate::authorship::validate_payload_authorship(event.payload.as_ref())?;
        let at = event.at.trim();
        if at.is_empty() || event.actor.trim().is_empty() || event.kind.trim().is_empty() {
            bail!("import event has a blank timestamp, actor, or kind");
        }
        chrono::DateTime::parse_from_rfc3339(at).with_context(|| {
            format!("import event timestamp '{}' is not valid RFC3339", event.at)
        })?;
        let entity_id: Option<i64> = match event.entity.as_str() {
            "plan" => event
                .entity_plan
                .as_deref()
                .map(|slug| {
                    if !dump_plan_slugs.contains(slug) {
                        bail!(
                            "import plan event names plan '{slug}', which is absent from the dump"
                        );
                    }
                    tx.query_row(
                        "SELECT plan_id FROM plans WHERE slug=?1",
                        params![slug],
                        |r| r.get(0),
                    )
                    .with_context(|| format!("import event names missing plan '{slug}'"))
                })
                .transpose()?,
            "task" | "dep" => {
                let seq = event
                    .entity_seq
                    .context("import task/dep event lacks entity_seq")?;
                let task_plan = imported_plan_by_seq.get(&seq).with_context(|| {
                    format!("import event names task #{seq}, which is absent from the dump")
                })?;
                let event_plan = event.entity_plan.as_deref().with_context(|| {
                    format!(
                        "import {} event for task #{seq} lacks entity_plan",
                        event.entity
                    )
                })?;
                let _ = (event_plan, task_plan); // validated by validate_plan_history
                Some(*imported_task_id_by_seq.get(&seq).with_context(|| {
                    format!("import event names task #{seq}, which is absent from the dump")
                })?)
            }
            "gate" => {
                let seq = event
                    .entity_seq
                    .context("import gate event lacks entity_seq")?;
                let task_plan = imported_plan_by_seq.get(&seq).with_context(|| {
                    format!("import gate event names task #{seq}, which is absent from the dump")
                })?;
                let event_plan = event.entity_plan.as_deref().with_context(|| {
                    format!("import gate event for task #{seq} lacks entity_plan")
                })?;
                let _ = (event_plan, task_plan); // validated by validate_plan_history
                let task_id = *imported_task_id_by_seq.get(&seq).with_context(|| {
                    format!("import gate event names task #{seq}, which is absent from the dump")
                })?;
                let name = event
                    .gate_name
                    .as_deref()
                    .context("import gate event lacks gate_name")?;
                tx.query_row(
                    "SELECT gate_id FROM gates WHERE task_id=?1 AND name=?2",
                    params![task_id, name],
                    |r| r.get(0),
                )
                .optional()?
            }
            other => bail!("import event has unknown entity '{other}'"),
        };
        if event.entity == "task" && event.kind == "status" {
            let target = event
                .payload
                .as_ref()
                .and_then(|payload| payload.get("to"))
                .and_then(serde_json::Value::as_str);
            if target.is_none_or(|target| !TASK_STATUSES.contains(&target)) {
                let seq = event.entity_seq.unwrap_or_default();
                bail!(
                    "import task status event for task #{seq} requires payload.to to be one of {}",
                    TASK_STATUSES.join("|")
                );
            }
        }
        let meaning_source_field = match (event.entity.as_str(), event.kind.as_str()) {
            ("task", "create") => Some("intent_source"),
            ("task", "status") => Some("result_source"),
            (_, "note") => Some("meaning_source"),
            _ => None,
        };
        if let Some(field) = meaning_source_field
            && let Some(value) = event
                .payload
                .as_ref()
                .and_then(|payload| payload.get(field))
            && !value.is_null()
        {
            let source = value.as_str().with_context(|| {
                format!("import event payload.{field} must be a string or null, got {value}")
            })?;
            validate_meaning_source(Some(source)).with_context(|| {
                format!("import event has invalid payload.{field} value {source:?}")
            })?;
        }
        tx.execute(
            "INSERT INTO events
             (at, actor, entity, entity_id, entity_plan, entity_seq, gate_name, kind, why, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                at,
                event.actor,
                event.entity,
                entity_id,
                event.entity_plan,
                event.entity_seq,
                event.gate_name,
                event.kind,
                event.why,
                event.payload.as_ref().map(serde_json::Value::to_string)
            ],
        )?;
    }
    record_event_in_mutation(
        &tx,
        actor,
        "plan",
        None,
        "import",
        None,
        Some(&serde_json::json!({
            "tasks": created,
            "deps": edges,
            "events": dump.events.len(),
            "mise_projections": dump.mise_projections.len()
        })),
    )?;
    tx.commit()?;
    Ok((created, edges))
}
