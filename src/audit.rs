//! Advisory integrity findings over the whole authority.

use anyhow::{Result, anyhow};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::HashMap;

use crate::{
    ASSIGNABLE_TASK_KINDS, LEGACY_TASK_KIND, MAX_TAG_CHARS, MAX_TASK_TITLE_CHARS, MEANING_SOURCES,
    TASK_STATUSES, TaskPickup, external_references, get_task, history_recovery, mise_projection,
    pickup, plan_move, seq_handle, suggested_task_kind, task_graph, task_handle,
    valid_task_definition_revision_payload, validate_commit_oid, validate_new_evidence_locator,
    validate_optional_sha256, write_guard,
};

#[derive(Serialize)]
pub struct AuditFinding {
    pub kind: String,
    pub detail: String,
}

pub(crate) fn find_cycle(conn: &Connection, edge_sql: &str) -> Result<Option<Vec<i64>>> {
    let mut st = conn.prepare("SELECT task_id, seq FROM tasks ORDER BY seq")?;
    let tasks: Vec<(i64, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let index: HashMap<i64, usize> = tasks
        .iter()
        .enumerate()
        .map(|(position, (task_id, _))| (*task_id, position))
        .collect();
    let mut st = conn.prepare(edge_sql)?;
    let edges: Vec<(i64, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut adjacency = vec![Vec::new(); tasks.len()];
    for (from, to) in edges {
        if let (Some(&from), Some(&to)) = (index.get(&from), index.get(&to)) {
            adjacency[from].push(to);
        }
    }
    Ok(task_graph::first_cycle(&adjacency).map(|cycle| {
        cycle
            .into_iter()
            .map(|position| tasks[position].1)
            .collect()
    }))
}

pub(crate) fn replacement_repair_instruction(
    seq: i64,
    status: &str,
    replacement_seq: Option<i64>,
) -> String {
    let into = replacement_seq
        .map(|replacement| format!(" --into {replacement}"))
        .unwrap_or_default();
    let retire = format!("papertiger retire {seq}{into} --why <reason>");
    if matches!(status, "proposed" | "in_progress") {
        format!("run `{retire}`")
    } else {
        format!("run `papertiger reopen {seq} --why <reason>`, then `{retire}`")
    }
}

pub(crate) fn invalid_replacement_terminals(conn: &Connection) -> Result<Vec<(i64, i64, String)>> {
    let mut statement = conn.prepare(
        "SELECT source.seq, target.seq, target.status
           FROM tasks source
           JOIN tasks target ON target.task_id=source.replacement_task_id
          WHERE target.status='rejected'
             OR (target.status='retired' AND target.replacement_task_id IS NULL)
          ORDER BY source.seq",
    )?;
    Ok(statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn terminal_replacement_repair_instruction(target: i64) -> String {
    format!(
        "run `papertiger reopen {target} --why <reason>`, then `papertiger retire {target} --into <task> --why <reason>`"
    )
}

pub fn audit(conn: &Connection) -> Result<Vec<AuditFinding>> {
    history_recovery::validate(conn)?;
    let mut findings = Vec::new();
    findings.extend(history_recovery::audit_structure(conn)?);
    let drifted = write_guard::drift(conn)?;
    if !drifted.is_empty() {
        findings.push(AuditFinding {
            kind: "write_guard_drift".into(),
            detail: write_guard::drift_correction(&drifted),
        });
    }
    let mut push = |kind: &str, detail: String| {
        findings.push(AuditFinding {
            kind: kind.into(),
            detail,
        })
    };

    let mut pickups = conn.prepare("SELECT seq, status, pickup_session, pickup_at FROM tasks WHERE pickup_session IS NOT NULL OR pickup_at IS NOT NULL")?;
    let rows = pickups.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (seq, status, session, at) = row?;
        let validation = match at {
            Some(at) => pickup::validate_pickup(
                &status,
                Some(&TaskPickup {
                    session,
                    at,
                    owner: None,
                }),
            ),
            None => Err(anyhow!("pickup session requires a timestamp")),
        };
        if let Err(error) = validation {
            push(
                "invalid_task_pickup",
                format!(
                    "{seq}: {error}; restore an intact export",
                    seq = seq_handle(conn, seq)?
                ),
            );
        }
    }
    for (kind, detail) in mise_projection::audit_mise_projections(conn)? {
        push(&kind, detail);
    }

    let mut live_titles: HashMap<(String, String), Vec<i64>> = HashMap::new();
    let mut statement = conn.prepare(
        "SELECT plan.slug, task.seq, task.title, task.status
           FROM tasks task
           JOIN plans plan ON plan.plan_id=task.plan_id
          ORDER BY plan.slug, task.seq",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (plan, seq, title, status) in rows {
        let characters = title.chars().count();
        if characters > MAX_TASK_TITLE_CHARS {
            push(
                "oversized_task_title",
                format!(
                    "{seq} in plan '{plan}' has {characters} title characters (maximum {MAX_TASK_TITLE_CHARS})",
                    seq = seq_handle(conn, seq)?
                ),
            );
        }
        if matches!(status.as_str(), "proposed" | "in_progress") {
            live_titles
                .entry((plan, title.trim().to_lowercase()))
                .or_default()
                .push(seq);
        }
    }
    for ((plan, title), sequences) in live_titles {
        if sequences.len() > 1 {
            push(
                "duplicate_live_title",
                format!(
                    "plan '{plan}' has duplicate live title {title:?} on {}; inspect with `papertiger search {title:?} --plan {plan}`",
                    sequences
                        .iter()
                        .map(|seq| seq_handle(conn, *seq))
                        .collect::<Result<Vec<_>>>()?
                        .join(", ")
                ),
            );
        }
    }

    let mut statement = conn.prepare(
        "SELECT task.seq, tag.tag
           FROM task_tags tag
           JOIN tasks task ON task.task_id=tag.task_id
          ORDER BY task.seq, tag.tag",
    )?;
    let tags = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (seq, tag) in tags {
        if tag != tag.trim() || tag.is_empty() || tag.chars().count() > MAX_TAG_CHARS {
            push(
                "noncanonical_tag",
                format!(
                    "{seq} has noncanonical tag {tag:?}; remove it and add a trimmed tag of at most {MAX_TAG_CHARS} characters",
                    seq = seq_handle(conn, seq)?
                ),
            );
        }
    }

    let mut statement = conn.prepare(
        "SELECT task.seq, commit_association.repository, commit_association.commit_oid,
                commit_association.recorded_at
           FROM commit_associations commit_association
           JOIN tasks task ON task.task_id=commit_association.task_id
          ORDER BY task.seq, commit_association.commit_association_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (seq, repository, commit_oid, recorded_at) in rows {
        if repository.trim().is_empty() {
            push(
                "blank_commit_repository",
                format!(
                    "{seq} commit {commit_oid} has a blank repository label",
                    seq = seq_handle(conn, seq)?
                ),
            );
        } else if repository != repository.trim() {
            push(
                "noncanonical_commit_repository",
                format!(
                    "{seq} commit {commit_oid} has repository label '{repository}' with surrounding whitespace",
                    seq = seq_handle(conn, seq)?
                ),
            );
        }
        if validate_commit_oid(&commit_oid).is_err() {
            push(
                "malformed_commit_oid",
                format!(
                    "{seq} commit '{commit_oid}' in repository '{repository}'",
                    seq = seq_handle(conn, seq)?
                ),
            );
        }
        if recorded_at != recorded_at.trim()
            || chrono::DateTime::parse_from_rfc3339(&recorded_at).is_err()
        {
            push(
                "invalid_commit_recorded_at",
                format!(
                    "{seq} commit '{commit_oid}' in repository '{repository}' has noncanonical or invalid RFC3339 recorded_at '{recorded_at}'",
                    seq = seq_handle(conn, seq)?
                ),
            );
        }
    }

    let mut statement = conn.prepare(
        "SELECT event_id, at, entity, entity_seq, kind, payload FROM canonical_events ORDER BY event_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (event_id, at, entity, entity_seq, kind, payload) in rows {
        if chrono::DateTime::parse_from_rfc3339(&at).is_err() {
            push(
                "invalid_event_timestamp",
                format!("event {event_id} has non-RFC3339 timestamp '{at}'"),
            );
        }
        let parsed_payload = match payload.as_deref() {
            None => None,
            Some(raw) => match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(value) => Some(value),
                Err(error) => {
                    push(
                        "invalid_event_payload",
                        format!("event {event_id} payload is not valid JSON: {error}"),
                    );
                    None
                }
            },
        };
        let meaning_source_field = match (entity.as_str(), kind.as_str()) {
            ("task", "create") => Some("intent_source"),
            ("task", "status") => Some("result_source"),
            (_, "note") => Some("meaning_source"),
            _ => None,
        };
        if let Err(error) = crate::authorship::validate_payload_authorship(parsed_payload.as_ref())
        {
            push(
                "invalid_event_authorship",
                format!("event {event_id}: {error}"),
            );
        }
        if let Some(field) = meaning_source_field
            && let Some(value) = parsed_payload
                .as_ref()
                .and_then(|payload| payload.get(field))
            && !value.is_null()
            && !value
                .as_str()
                .is_some_and(|source| MEANING_SOURCES.contains(&source))
        {
            push(
                "invalid_meaning_source_event",
                format!("event {event_id} has invalid payload.{field}"),
            );
        }
        if entity == "task" && kind == "status" {
            let valid_target = parsed_payload
                .as_ref()
                .and_then(|value| {
                    value
                        .get("to")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .is_some_and(|target| TASK_STATUSES.contains(&target.as_str()));
            if !valid_target {
                let task = entity_seq
                    .map(|seq| format!("task #{seq}"))
                    .unwrap_or_else(|| "a task with no stable sequence".to_owned());
                push(
                    "invalid_task_status_event",
                    format!("event {event_id} for {task} lacks a canonical task-status payload.to"),
                );
            }
        }
        if entity == "task" && kind == "edit" {
            let parsed = parsed_payload.as_ref();
            let declares_revision = parsed
                .and_then(|value| value.get("revision_schema"))
                .is_some();
            if declares_revision && !parsed.is_some_and(valid_task_definition_revision_payload) {
                let task = entity_seq
                    .map(|seq| format!("task #{seq}"))
                    .unwrap_or_else(|| "a task with no stable sequence".to_owned());
                push(
                    "invalid_task_definition_revision",
                    format!(
                        "event {event_id} for {task} has a malformed or unsupported task-definition revision payload"
                    ),
                );
            }
        }
    }

    // Resolved gates with malformed or unknown-scheme evidence.
    let mut st = conn.prepare(
        "SELECT t.seq, g.name, g.evidence_locator FROM gates g JOIN tasks t ON t.task_id=g.task_id
         WHERE g.status='resolved'",
    )?;
    let rows: Vec<(i64, String, Option<String>)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, name, loc) in rows {
        match loc {
            None => push(
                "gate_no_evidence",
                format!(
                    "{seq} gate '{name}' resolved without locator",
                    seq = seq_handle(conn, seq)?
                ),
            ),
            Some(l) => {
                if validate_new_evidence_locator(&l).is_err() {
                    push(
                        "malformed_evidence_locator",
                        format!(
                            "{seq} gate '{name}' locator '{l}'",
                            seq = seq_handle(conn, seq)?
                        ),
                    );
                }
            }
        }
    }

    let mut st = conn.prepare(
        "SELECT t.seq, g.name, g.evidence_sha256 FROM gates g
         JOIN tasks t ON t.task_id=g.task_id WHERE g.evidence_sha256 IS NOT NULL",
    )?;
    let rows: Vec<(i64, String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, name, digest) in rows {
        if validate_optional_sha256(Some(&digest)).is_err() {
            push(
                "malformed_evidence_sha256",
                format!(
                    "{seq} gate '{name}' has noncanonical sha256 '{digest}'",
                    seq = seq_handle(conn, seq)?
                ),
            );
        }
    }

    let mut st = conn.prepare(
        "SELECT t.seq, g.name FROM gates g JOIN tasks t ON t.task_id=g.task_id
         WHERE t.status='done' AND g.status='open'",
    )?;
    let rows: Vec<(i64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, name) in rows {
        push(
            "done_with_open_gate",
            format!(
                "{seq} is done with open gate '{name}'",
                seq = seq_handle(conn, seq)?
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT t.seq, g.name FROM gates g JOIN tasks t ON t.task_id=g.task_id
         WHERE g.status='waived' AND (g.note IS NULL OR trim(g.note)='')",
    )?;
    let rows: Vec<(i64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, name) in rows {
        push(
            "waiver_without_reason",
            format!(
                "{seq} gate '{name}' is waived without a durable reason",
                seq = seq_handle(conn, seq)?
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT seq, kind FROM tasks
          WHERE status='done' AND kind IN ('probe','decision','bug')
            AND (result IS NULL OR trim(result)='')",
    )?;
    let rows: Vec<(i64, String)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, kind) in rows {
        push(
            "missing_task_result",
            format!(
                "{seq} is a completed {kind} without a durable result",
                seq = seq_handle(conn, seq)?
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT seq, title,
                EXISTS(SELECT 1 FROM tasks child WHERE child.parent_id=tasks.task_id)
           FROM tasks
          WHERE kind=?1 AND status IN ('proposed','in_progress')
          ORDER BY seq",
    )?;
    let rows: Vec<(i64, String, bool)> = st
        .query_map(params![LEGACY_TASK_KIND], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, title, has_children) in rows {
        // A task with children is a container whatever its title says.
        let choice = if has_children {
            "goal (suggested because it has children; confirm it fits the outcome)".to_owned()
        } else {
            suggested_task_kind(&title)
                .map(|kind| {
                    format!("{kind} (suggested from the title; confirm it fits the outcome)")
                })
                .unwrap_or_else(|| format!("one of {}", ASSIGNABLE_TASK_KINDS.join("|")))
        };
        let handle = task_handle(seq, LEGACY_TASK_KIND);
        push(
            "legacy_task_kind",
            format!(
                "{handle} is open with the legacy kind '{LEGACY_TASK_KIND}'; run `papertiger edit {handle} --kind <kind> --why <reason>` with {choice}"
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT task.seq, dependency.seq
           FROM deps
           JOIN tasks task ON task.task_id=deps.task_id
           JOIN tasks dependency ON dependency.task_id=deps.depends_on
          WHERE task.status='done' AND dependency.status<>'done'",
    )?;
    let rows: Vec<(i64, i64)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (task, dependency) in rows {
        push(
            "done_with_open_dependency",
            format!(
                "{task} is done while dependency {dependency} is not done",
                task = seq_handle(conn, task)?,
                dependency = seq_handle(conn, dependency)?
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT parent.seq, child.seq
           FROM tasks parent
           JOIN tasks child ON child.parent_id=parent.task_id
          WHERE parent.status='done' AND child.status IN ('proposed','in_progress')",
    )?;
    let rows: Vec<(i64, i64)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (parent, child) in rows {
        push(
            "done_with_live_child",
            format!(
                "{parent} is done while child {child} remains live",
                parent = seq_handle(conn, parent)?,
                child = seq_handle(conn, child)?
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT plan.slug, COUNT(task.task_id)
           FROM plans plan
           JOIN tasks task ON task.plan_id=plan.plan_id
          WHERE plan.status='done' AND task.status IN ('proposed','in_progress')
          GROUP BY plan.plan_id",
    )?;
    let rows: Vec<(String, i64)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (plan, count) in rows {
        push(
            "done_plan_with_live_tasks",
            format!("plan '{plan}' is done with {count} live task(s)"),
        );
    }

    let mut st = conn.prepare(
        "SELECT a.seq, b.seq FROM deps d JOIN tasks a ON a.task_id=d.task_id
         JOIN tasks b ON b.task_id=d.depends_on WHERE a.plan_id<>b.plan_id",
    )?;
    let rows: Vec<(i64, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (task, dependency) in rows {
        push(
            "cross_plan_dep",
            format!(
                "{task} depends on {dependency} from a different plan",
                task = seq_handle(conn, task)?,
                dependency = seq_handle(conn, dependency)?
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT c.seq, p.seq FROM tasks c JOIN tasks p ON p.task_id=c.parent_id
         WHERE c.plan_id<>p.plan_id",
    )?;
    let rows: Vec<(i64, i64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (child, parent) in rows {
        push(
            "cross_plan_parent",
            format!(
                "{child} has parent {parent} from a different plan",
                child = seq_handle(conn, child)?,
                parent = seq_handle(conn, parent)?
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT task.seq, task.status, task.replacement_task_id
           FROM tasks task
           LEFT JOIN tasks replacement ON replacement.task_id=task.replacement_task_id
          WHERE task.replacement_task_id IS NOT NULL AND replacement.task_id IS NULL",
    )?;
    let rows: Vec<(i64, String, i64)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (task, status, replacement_task_id) in rows {
        push(
            "dangling_replacement",
            format!(
                "{task} names missing replacement row id {replacement_task_id}; restore the referenced task or {} to clear the pointer",
                replacement_repair_instruction(task, &status, None),
                task = seq_handle(conn, task)?
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT task.seq, task.status, replacement.seq,
                task.plan_id=replacement.plan_id, replacement.status
           FROM tasks task
           JOIN tasks replacement ON replacement.task_id=task.replacement_task_id
          WHERE task.status<>'retired'",
    )?;
    let rows: Vec<(i64, String, i64, bool, String)> = st
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (task, status, replacement, same_plan, replacement_status) in rows {
        let reusable_replacement = (same_plan
            && matches!(
                replacement_status.as_str(),
                "proposed" | "in_progress" | "done"
            ))
        .then_some(replacement);
        push(
            "replacement_on_nonretired_task",
            format!(
                "{task} is {status} but names replacement {replacement}; valid replacement links require a retired source; {}",
                replacement_repair_instruction(task, &status, reusable_replacement),
                task = seq_handle(conn, task)?,
                replacement = seq_handle(conn, replacement)?
            ),
        );
    }

    let mut st = conn.prepare(
        "SELECT task.seq, task.status, replacement.seq
           FROM tasks task
           JOIN tasks replacement ON replacement.task_id=task.replacement_task_id
          WHERE task.plan_id<>replacement.plan_id",
    )?;
    let rows: Vec<(i64, String, i64)> = st
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (task, status, replacement) in rows {
        push(
            "cross_plan_replacement",
            format!(
                "{task} names replacement {replacement} from a different plan; {} to clear the pointer before choosing a same-plan replacement",
                replacement_repair_instruction(task, &status, None),
                task = seq_handle(conn, task)?,
                replacement = seq_handle(conn, replacement)?
            ),
        );
    }

    for (source, target, status) in invalid_replacement_terminals(conn)? {
        let terminal = if status == "retired" {
            "retired without its own replacement".to_owned()
        } else {
            status
        };
        push(
            "replacement_terminal_dead_end",
            format!(
                "{source} replacement chain terminates at {target}, which is {terminal}; {} to extend the chain through a live canonical task",
                terminal_replacement_repair_instruction(target),
                source = seq_handle(conn, source)?,
                target = seq_handle(conn, target)?
            ),
        );
    }

    if let Some(cycle) = find_cycle(conn, "SELECT task_id, depends_on FROM deps")? {
        push(
            "dependency_cycle",
            format!(
                "dependency cycle {}",
                cycle
                    .iter()
                    .map(|seq| seq_handle(conn, *seq))
                    .collect::<Result<Vec<_>>>()?
                    .join(" -> ")
            ),
        );
    }
    // Older releases admitted these, and import restores them unchanged.
    // Pure dependency loops are reported above as dependency_cycle.
    for (start, steps) in task_graph::WaitGraph::load_unfinished(conn)?.loops() {
        if steps
            .iter()
            .all(|step| step.edge == task_graph::WaitEdge::Dependency)
        {
            continue;
        }
        let removals = task_graph::dependency_removals(start, &steps);
        let correction = if removals.is_empty() {
            "move one of these tasks with `papertiger edit <task> --parent <task>`".to_string()
        } else {
            format!("remove one dependency with {}", removals.join(" or "))
        };
        push(
            "dependency_deadlock",
            format!(
                "{}, so none of these tasks can finish; {correction}",
                task_graph::describe_chain(conn, start, &steps)?
            ),
        );
    }
    if let Some(cycle) = find_cycle(
        conn,
        "SELECT task_id, parent_id FROM tasks WHERE parent_id IS NOT NULL",
    )? {
        push(
            "parent_cycle",
            format!(
                "parent cycle {}",
                cycle
                    .iter()
                    .map(|seq| seq_handle(conn, *seq))
                    .collect::<Result<Vec<_>>>()?
                    .join(" -> ")
            ),
        );
    }
    if let Some(cycle) = find_cycle(
        conn,
        "SELECT task_id, replacement_task_id FROM tasks WHERE replacement_task_id IS NOT NULL",
    )? {
        let repair_task = cycle[0];
        let repair_status = get_task(conn, repair_task)?.status;
        push(
            "replacement_cycle",
            format!(
                "replacement cycle {}; {} to break one edge",
                cycle
                    .iter()
                    .map(|seq| seq_handle(conn, *seq))
                    .collect::<Result<Vec<_>>>()?
                    .join(" -> "),
                replacement_repair_instruction(repair_task, &repair_status, None)
            ),
        );
    }

    // Live tasks depending on retired/rejected tasks.
    let mut st = conn.prepare(
        "SELECT a.seq, b.seq, b.status FROM deps d
         JOIN tasks a ON a.task_id=d.task_id JOIN tasks b ON b.task_id=d.depends_on
         WHERE a.status IN ('proposed','in_progress') AND b.status IN ('retired','rejected')",
    )?;
    let rows: Vec<(i64, i64, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (a, b, s) in rows {
        push(
            "dep_on_dead",
            format!(
                "{a} depends on {b} which is {s}",
                a = seq_handle(conn, a)?,
                b = seq_handle(conn, b)?
            ),
        );
    }

    // Parents lagging: all children done but parent not.
    let mut st = conn.prepare(
        "SELECT p.seq FROM tasks p WHERE p.status IN ('proposed','in_progress')
         AND EXISTS (SELECT 1 FROM tasks c WHERE c.parent_id=p.task_id)
         AND NOT EXISTS (SELECT 1 FROM tasks c WHERE c.parent_id=p.task_id
                         AND c.status IN ('proposed','in_progress'))",
    )?;
    let rows: Vec<i64> = st
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for seq in rows {
        push(
            "parent_lagging",
            format!(
                "{seq}: all children finished but parent is open",
                seq = seq_handle(conn, seq)?
            ),
        );
    }

    if let Err(error) = plan_move::audit_plan_history(conn) {
        push("invalid_event_plan_history", error.to_string());
    }
    let mut reference_tasks = conn.prepare("SELECT DISTINCT t.seq FROM external_references r JOIN tasks t ON t.task_id=r.task_id ORDER BY t.seq")?;
    for seq in reference_tasks.query_map([], |row| row.get::<_, i64>(0))? {
        let seq = seq?;
        if let Err(error) = external_references(conn, seq) {
            push(
                "invalid_external_reference",
                format!("{seq}: {error}", seq = seq_handle(conn, seq)?),
            );
        }
    }

    Ok(findings)
}

// ---- export / import ------------------------------------------------------
