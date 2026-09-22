//! Explicit, atomic relocation with unchanged identity and append-only history.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};

use crate::{
    Dump, EventDump, TASK_DEFINITION_REVISION_SCHEMA, begin_mutation, get_plan, get_task, now,
    record_event_in_mutation, resolve_plan,
};

pub fn move_tasks_to_plan(
    conn: &Connection,
    actor: &str,
    sequences: &[i64],
    destination: &str,
    why: &str,
) -> Result<()> {
    if sequences.is_empty() || why.trim().is_empty() {
        bail!(
            "moving tasks requires explicit task sequences, --plan <destination>, and --why <reason>"
        );
    }
    let selected = sequences.iter().copied().collect::<HashSet<_>>();
    if selected.len() != sequences.len() {
        bail!("move-plan contains repeated task sequences; supply each task once");
    }
    let tx = begin_mutation(conn)?;
    let (destination_id, _) = resolve_plan(&tx, Some(destination))?;
    if get_plan(&tx, destination_id)?.status != "active" {
        bail!(
            "destination plan '{destination}' is not active; use `papertiger plan set {destination} active --why <reason>` or select an active plan"
        );
    }
    let mut tasks = sequences
        .iter()
        .map(|seq| get_task(&tx, *seq))
        .collect::<Result<Vec<_>>>()?;
    tasks.sort_by_key(|task| task.seq);
    for task in &tasks {
        if task.plan_id == destination_id {
            bail!(
                "#{} already belongs to '{destination}'; omit it from move-plan",
                task.seq
            );
        }
    }
    // Every currently same-plan edge remains same-plan. Callers explicitly name
    // the complete related set; relocation never rewrites a dependency or parent.
    let mut statement = tx.prepare(
        "SELECT a.seq, b.seq FROM tasks a JOIN tasks b ON b.task_id=a.parent_id
         UNION SELECT a.seq, b.seq FROM tasks a JOIN tasks b ON b.task_id=a.replacement_task_id
         UNION SELECT a.seq, b.seq FROM deps d JOIN tasks a ON a.task_id=d.task_id JOIN tasks b ON b.task_id=d.depends_on")?;
    let edges = statement
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    // Return the entire closure, so the corrective command needs no guessing.
    let mut adjacency = HashMap::<i64, Vec<i64>>::new();
    for (a, b) in edges {
        adjacency.entry(a).or_default().push(b);
        adjacency.entry(b).or_default().push(a);
    }
    let mut closure = selected.clone();
    let mut pending = sequences.to_vec();
    while let Some(seq) = pending.pop() {
        for neighbor in adjacency.get(&seq).into_iter().flatten() {
            if closure.insert(*neighbor) {
                pending.push(*neighbor);
            }
        }
    }
    let mut missing = closure.difference(&selected).copied().collect::<Vec<_>>();
    if !missing.is_empty() {
        missing.sort_unstable();
        bail!(
            "move-plan would split parent, dependency, or replacement links; also supply related tasks {} in the same `papertiger move-plan ... --plan {destination} --why <reason>` command, or explicitly remove/reparent those links first",
            missing
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    let moved_at = now();
    for task in &tasks {
        tx.execute(
            "UPDATE tasks SET plan_id=?1, updated_at=?2 WHERE task_id=?3",
            params![destination_id, moved_at, task.task_id],
        )?;
    }
    for task in &tasks {
        let source = get_plan(&tx, task.plan_id)?;
        record_event_in_mutation(
            &tx,
            actor,
            "task",
            Some(task.task_id),
            "edit",
            Some(why),
            Some(&serde_json::json!({
                "seq": task.seq, "revision_schema": TASK_DEFINITION_REVISION_SCHEMA,
                "fields": ["plan"], "changes": {"plan": {"before": source.slug, "after": destination}},
            })),
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn validate_plan_history(dump: &Dump) -> Result<()> {
    let mut expected = dump
        .tasks
        .iter()
        .filter_map(|task| task.seq.map(|seq| (seq, task.plan.as_str())))
        .collect::<HashMap<_, _>>();
    let plans = dump
        .plans
        .iter()
        .map(|plan| plan.slug.as_str())
        .collect::<HashSet<_>>();
    for event in dump.events.iter().rev() {
        if !matches!(event.entity.as_str(), "task" | "dep" | "gate") {
            continue;
        }
        let seq = event
            .entity_seq
            .context("import task/dep/gate event lacks entity_seq")?;
        let current = expected
            .get_mut(&seq)
            .with_context(|| format!("event names task #{seq}, which is absent from the dump"))?;
        let plan = event
            .entity_plan
            .as_deref()
            .context("task event lacks entity_plan")?;
        if plan != *current {
            bail!(
                "event names plan '{plan}', but task #{seq} belongs to plan '{current}' at this event; restore a verified export with the complete move-plan history"
            );
        }
        if let Some(revision) = event
            .payload
            .as_ref()
            .and_then(|payload| payload.pointer("/changes/plan"))
        {
            if event.entity != "task"
                || event.kind != "edit"
                || !event
                    .payload
                    .as_ref()
                    .is_some_and(crate::valid_task_definition_revision_payload)
            {
                bail!("task #{seq} has an invalid plan revision; restore a verified export");
            }
            let before = revision["before"]
                .as_str()
                .context("plan revision requires before plan slug")?;
            if revision["after"].as_str() != Some(*current) || !plans.contains(before) {
                bail!(
                    "task #{seq} plan revision does not bind existing source and destination plans; restore a verified export"
                );
            }
            *current = before;
        }
    }
    Ok(())
}

pub(crate) fn audit_plan_history(conn: &Connection) -> Result<()> {
    // Reuse the transfer validator against a minimal projection. This is not a
    // second history interpretation and does not call export or audit recursively.
    let mut dump = Dump {
        schema: String::new(),
        plans: vec![],
        tasks: vec![],
        events: vec![],
        mise_projections: vec![],
    };
    let mut plans = conn.prepare("SELECT slug, title FROM plans")?;
    dump.plans = plans
        .query_map([], |row| {
            Ok(crate::PlanDump {
                slug: row.get(0)?,
                title: row.get(1)?,
                intent: String::new(),
                status: "active".into(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut tasks =
        conn.prepare("SELECT t.seq,p.slug FROM tasks t JOIN plans p ON p.plan_id=t.plan_id")?;
    for row in tasks.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })? {
        let (seq, plan) = row?;
        dump.tasks.push(serde_json::from_value(
            serde_json::json!({"seq":seq,"plan":plan,"title":"history identity"}),
        )?);
    }
    let mut events = conn.prepare(
        "SELECT entity, entity_seq, entity_plan, kind, payload FROM canonical_events ORDER BY event_id",
    )?;
    for row in events.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })? {
        let (entity, entity_seq, entity_plan, kind, payload) = row?;
        dump.events.push(EventDump {
            at: String::new(),
            actor: String::new(),
            entity,
            entity_seq,
            entity_plan,
            gate_name: None,
            kind,
            why: None,
            payload: payload.map(|raw| serde_json::from_str(&raw)).transpose()?,
        });
    }
    validate_plan_history(&dump)
}
