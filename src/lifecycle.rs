//! Task lifecycle transitions: start, complete, reopen, retire and reject.

use anyhow::{Result, bail};
use rusqlite::{Connection, Transaction, params};

use crate::{
    Task, TaskPickup, begin_mutation, entry_blockers, find_cycle, get_task, given_reason,
    kind_requires_result, now, pickup, plan_status, record_event_in_mutation, seq_handle,
    task_graph, validate_session,
};

pub fn start_task(
    conn: &Connection,
    actor: &str,
    seq: i64,
    why: Option<&str>,
    session: Option<&str>,
) -> Result<()> {
    let tx = begin_mutation(conn)?;
    start_task_in_mutation(&tx, actor, seq, why, false, session)?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn start_task_in_mutation(
    tx: &Transaction<'_>,
    actor: &str,
    seq: i64,
    why: Option<&str>,
    created_in_same_mutation: bool,
    session: Option<&str>,
) -> Result<()> {
    if let Some(session) = session {
        validate_session(session)?;
    }
    let task = get_task(tx, seq)?;
    let task_label = if created_in_same_mutation {
        "the new task".to_owned()
    } else {
        task.handle.clone()
    };
    match task.status.as_str() {
        "proposed" => {}
        "in_progress" => return pickup::record_pickup(tx, actor, &task, session, why),
        "done" | "retired" | "rejected" => {
            bail!(
                "{task_label} is {}; use `reopen` before starting it",
                task.status
            )
        }
        other => bail!("{task_label} has unknown status '{other}'"),
    }
    let status = plan_status(tx, task.plan_id)?;
    if status != "active" {
        if created_in_same_mutation {
            bail!("task was not created: plan is {status}; set it active before using add --start");
        }
        bail!(
            "plan is {status}; set it active before starting task {}",
            task.handle
        );
    }
    let blockers = entry_blockers(tx, &task)?;
    if !blockers.is_empty() {
        if created_in_same_mutation {
            bail!(
                "task was not created: it is not ready; resolve {} before using add --start",
                blockers.join(", ")
            );
        }
        bail!(
            "{} is not ready; resolve {} before starting it",
            task.handle,
            blockers.join(", ")
        );
    }
    let pickup = TaskPickup {
        session: session.map(str::to_owned),
        at: now(),
        owner: crate::authorship::current_owner(tx)?,
    };
    transition_task(
        tx,
        actor,
        &task,
        TaskTransition {
            status: "in_progress",
            why,
            result: None,
            replacement: None,
            pickup: Some(&pickup),
        },
    )?;
    Ok(())
}

/// What holds a task, with the command that clears it.
fn explain_blocker(handle: &str, blocker: &str) -> String {
    if let Some(dependency) = blocker.strip_prefix("dep:") {
        format!(
            "dependency {dependency} is not done (finish it, or `papertiger dep remove {handle} {dependency}`)"
        )
    } else if let Some(gate) = blocker.strip_prefix("external:") {
        format!(
            "external gate '{gate}' is open (`papertiger gate resolve {handle} {gate} --evidence <locator>` or `papertiger gate waive {handle} {gate} --why <reason>`)"
        )
    } else if let Some(disputed) = blocker.strip_prefix("disputed-result:") {
        format!(
            "{disputed}'s result is disputed (another session confirms it with `papertiger review {disputed} --result --verdict confirmed --why <reason>`, or `papertiger reopen {disputed} --why <reason>` to revise it)"
        )
    } else {
        blocker.to_owned()
    }
}

pub fn complete_task(conn: &Connection, actor: &str, seq: i64, result: Option<&str>) -> Result<()> {
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!("{} is already {}", task.handle, task.status);
    }
    let plan_status = plan_status(&tx, task.plan_id)?;
    if task.status == "proposed" && plan_status != "active" {
        bail!(
            "plan is {plan_status}; set it active before completing proposed task {}",
            task.handle
        );
    }
    let blockers = entry_blockers(&tx, &task)?;
    if !blockers.is_empty() {
        bail!(
            "{} cannot be completed yet: {}",
            task.handle,
            blockers
                .iter()
                .map(|blocker| explain_blocker(&task.handle, blocker))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    let open = open_gates(&tx, task.task_id)?;
    if !open.is_empty() {
        bail!(
            "{handle} has open gate(s): {}; resolve each with `papertiger gate resolve {seq} <name> --evidence <locator>` or waive it with `papertiger gate waive {seq} <name> --why <reason>`",
            open.join(", "),
            handle = task.handle
        );
    }
    let live_children = live_child_sequences(&tx, task.task_id)?;
    if !live_children.is_empty() {
        bail!(
            "{} has unfinished child task(s): {}; finish or disposition them first",
            task.handle,
            live_children
                .iter()
                .map(|child| seq_handle(&tx, *child))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        );
    }
    let result = result.map(str::trim).filter(|text| !text.is_empty());
    if kind_requires_result(&task.kind) && result.is_none() {
        bail!(
            "completing {} task {} requires --result or --result-file so the measured or selected outcome survives the session",
            task.kind,
            task.handle
        );
    }
    transition_task(
        &tx,
        actor,
        &task,
        TaskTransition {
            status: "done",
            why: result,
            result,
            replacement: None,
            pickup: None,
        },
    )?;
    tx.commit()?;
    Ok(())
}

pub fn reopen_task(conn: &Connection, actor: &str, seq: i64, why: Option<&str>) -> Result<()> {
    let why = given_reason(why);
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if !matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "{} is {}; only terminal tasks can be reopened",
            task.handle,
            task.status
        );
    }
    let status = plan_status(&tx, task.plan_id)?;
    if matches!(status.as_str(), "done" | "retired") {
        bail!(
            "plan is {status}; reactivate it before reopening task {}",
            task.handle
        );
    }
    if let Some(parent_id) = task.parent_id {
        let parent: (i64, String) = tx.query_row(
            "SELECT seq, status FROM tasks WHERE task_id=?1",
            params![parent_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let parent_handle = seq_handle(&tx, parent.0)?;
        if matches!(parent.1.as_str(), "done" | "retired" | "rejected") {
            bail!(
                "parent {parent_handle} is {}; reopen it before reopening child {}",
                parent.1,
                task.handle
            );
        }
        // Reopening makes the parent wait for this child again.
        if let Some(steps) = task_graph::waits_for(&tx, seq, parent.0)? {
            bail!(
                "reopening {handle} would deadlock: {handle} finishes only after its parent {parent_handle} ({}), which would wait for it again; move it first with `papertiger edit {seq} --parent <task>` or `papertiger edit {seq} --clear-parent`{}",
                task_graph::describe_chain(&tx, seq, &steps)?,
                task_graph::or_remove_dependency(seq, &steps),
                handle = task.handle
            );
        }
    }
    let completed_dependents = completed_dependent_sequences(&tx, task.task_id)?;
    if !completed_dependents.is_empty() {
        bail!(
            "{} supports completed dependent task(s): {}; reopen them first",
            task.handle,
            completed_dependents
                .iter()
                .map(|dependent| seq_handle(&tx, *dependent))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        );
    }
    transition_task(
        &tx,
        actor,
        &task,
        TaskTransition {
            status: "proposed",
            why,
            result: None,
            replacement: None,
            pickup: None,
        },
    )?;
    tx.commit()?;
    Ok(())
}

pub fn retire_task(
    conn: &Connection,
    actor: &str,
    seq: i64,
    replacement_seq: Option<i64>,
    why: Option<&str>,
) -> Result<()> {
    terminate_task(conn, actor, seq, "retired", replacement_seq, why)
}

pub fn reject_task(conn: &Connection, actor: &str, seq: i64, why: &str) -> Result<()> {
    terminate_task(conn, actor, seq, "rejected", None, Some(why))
}

fn terminate_task(
    conn: &Connection,
    actor: &str,
    seq: i64,
    status: &str,
    replacement_seq: Option<i64>,
    why: Option<&str>,
) -> Result<()> {
    if !matches!(status, "retired" | "rejected") {
        bail!("terminal disposition must be retired or rejected");
    }
    // Naming the replacement explains a retirement; any other disposition
    // needs its reason written down.
    let why = given_reason(why);
    if why.is_none() {
        if status == "rejected" {
            bail!("reject requires --why <reason>");
        }
        if replacement_seq.is_none() {
            bail!(
                "retire without --into requires --why <reason>; or name the replacement with --into <task>"
            );
        }
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if status == "rejected" && replacement_seq.is_some() {
        bail!(
            "reject does not accept a replacement task; use `papertiger retire {seq} --into <task> --why <reason>` when consolidating work"
        );
    }
    if task.status == status {
        bail!("{} is already {status}", task.handle);
    }
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "{} is {}; reopen it before changing its disposition",
            task.handle,
            task.status
        );
    }
    let completed_dependents = completed_dependent_sequences(&tx, task.task_id)?;
    if !completed_dependents.is_empty() {
        bail!(
            "{} supports completed dependent task(s): {}; reopen them before {status}",
            task.handle,
            completed_dependents
                .iter()
                .map(|dependent| seq_handle(&tx, *dependent))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        );
    }
    let live_children = live_child_sequences(&tx, task.task_id)?;
    if !live_children.is_empty() {
        bail!(
            "{} has unfinished child task(s): {}; finish, disposition, or reparent them before {status}",
            task.handle,
            live_children
                .iter()
                .map(|child| seq_handle(&tx, *child))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        );
    }
    let replacement_sources = inbound_replacement_sequences(&tx, task.task_id)?;
    if !replacement_sources.is_empty() && status == "rejected" {
        bail!(
            "{handle} is the canonical replacement for {}; rejection would strand that history; use `papertiger retire {seq} --into <task> --why <reason>` to extend the replacement chain, or reopen and re-disposition the referring tasks first",
            replacement_sources
                .iter()
                .map(|source| seq_handle(&tx, *source))
                .collect::<Result<Vec<_>>>()?
                .join(", "),
            handle = task.handle
        );
    }
    if !replacement_sources.is_empty() && status == "retired" && replacement_seq.is_none() {
        bail!(
            "{handle} is the canonical replacement for {}; bare retirement would strand that history; use `papertiger retire {seq} --into <task> --why <reason>` to extend the replacement chain",
            replacement_sources
                .iter()
                .map(|source| seq_handle(&tx, *source))
                .collect::<Result<Vec<_>>>()?
                .join(", "),
            handle = task.handle
        );
    }
    let replacement = replacement_seq
        .map(|replacement_seq| {
            if replacement_seq == seq {
                bail!("{} cannot replace itself", task.handle);
            }
            let replacement = get_task(&tx, replacement_seq)?;
            if replacement.plan_id != task.plan_id {
                bail!(
                    "{} and replacement {} belong to different plans",
                    task.handle,
                    replacement.handle
                );
            }
            if matches!(replacement.status.as_str(), "retired" | "rejected") {
                bail!(
                    "replacement {} is {}; choose a proposed, in_progress, or done same-plan task for --into",
                    replacement.handle,
                    replacement.status
                );
            }
            Ok(replacement)
        })
        .transpose()?;
    transition_task(
        &tx,
        actor,
        &task,
        TaskTransition {
            status,
            why,
            result: why,
            replacement: replacement.as_ref(),
            pickup: None,
        },
    )?;
    if let Some(cycle) = find_cycle(
        &tx,
        "SELECT task_id, replacement_task_id FROM tasks WHERE replacement_task_id IS NOT NULL",
    )? {
        bail!(
            "replacement would create cycle {}",
            cycle
                .iter()
                .map(|task| seq_handle(&tx, *task))
                .collect::<Result<Vec<_>>>()?
                .join(" -> ")
        );
    }
    tx.commit()?;
    Ok(())
}

struct TaskTransition<'a> {
    status: &'a str,
    why: Option<&'a str>,
    result: Option<&'a str>,
    replacement: Option<&'a Task>,
    pickup: Option<&'a TaskPickup>,
}

fn transition_task(
    tx: &Transaction<'_>,
    actor: &str,
    task: &Task,
    transition: TaskTransition<'_>,
) -> Result<()> {
    let TaskTransition {
        status,
        why,
        result,
        replacement,
        pickup,
    } = transition;
    tx.execute(
        "UPDATE tasks
            SET status=?1, result=?2, replacement_task_id=?3, updated_at=?4, pickup_session=?6, pickup_at=?7,
                pickup_host=?8, pickup_pid=?9, pickup_process=?10
          WHERE task_id=?5",
        params![
            status,
            result,
            replacement.map(|task| task.task_id),
            now(),
            task.task_id,
            pickup.and_then(|p| p.session.as_deref()),
            pickup.map(|p| &p.at),
            pickup.and_then(|p| p.owner.as_ref()).map(|owner| &owner.host),
            pickup.and_then(|p| p.owner.as_ref()).map(|owner| owner.pid),
            pickup.and_then(|p| p.owner.as_ref()).map(|owner| &owner.process),
        ],
    )?;
    let mut payload = serde_json::json!({
        "seq": task.seq,
        "from": task.status,
        "to": status,
        "result": result,
        "previous_pickup": task.pickup,
        "pickup": pickup,
    });
    if let Some(replacement) = replacement {
        payload["replacement_seq"] = serde_json::json!(replacement.seq);
    }
    record_event_in_mutation(
        tx,
        actor,
        "task",
        Some(task.task_id),
        "status",
        why,
        Some(&payload),
    )?;
    Ok(())
}

pub(crate) fn open_gates(conn: &Connection, task_id: i64) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT name FROM gates WHERE task_id=?1 AND status='open'")?;
    Ok(st
        .query_map(params![task_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn live_child_sequences(conn: &Connection, task_id: i64) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT seq FROM tasks
          WHERE parent_id=?1 AND status IN ('proposed','in_progress')
          ORDER BY seq",
    )?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn inbound_replacement_sequences(conn: &Connection, task_id: i64) -> Result<Vec<i64>> {
    let mut statement =
        conn.prepare("SELECT seq FROM tasks WHERE replacement_task_id=?1 ORDER BY seq")?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn completed_dependent_sequences(conn: &Connection, task_id: i64) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT task.seq
           FROM deps
           JOIN tasks task ON task.task_id=deps.task_id
          WHERE deps.depends_on=?1 AND task.status='done'
          ORDER BY task.seq",
    )?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
