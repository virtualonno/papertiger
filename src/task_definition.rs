//! Creating tasks and editing their definition: fields, tags and dependencies.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, Transaction, params};
use std::collections::HashSet;

use crate::{
    MEANING_SOURCES, Priority, TASK_DEFINITION_REVISION_SCHEMA, TASK_KINDS, begin_mutation,
    find_cycle, get_task, kind_requires_result, now, plan_status, record_event_in_mutation,
    resolve_plan, start_task_in_mutation, task_graph, validate_assignable_task_kind, validate_tag,
    validate_task_title,
};

/// Everything a new task records. Start from [`TaskCreation::new`] and set
/// only the fields that differ from a proposed, untagged work task.
pub struct TaskCreation<'a> {
    pub title: &'a str,
    pub intent: &'a str,
    pub kind: &'a str,
    pub parent: Option<i64>,
    pub deps: &'a [i64],
    pub tags: &'a [String],
    pub priority: Priority,
    pub why: Option<&'a str>,
    pub start: bool,
    pub session: Option<&'a str>,
}

impl<'a> TaskCreation<'a> {
    /// A proposed task of `kind`; every task's kind is chosen deliberately.
    pub fn new(title: &'a str, kind: &'a str) -> Self {
        Self {
            title,
            intent: "",
            kind,
            parent: None,
            deps: &[],
            tags: &[],
            priority: Priority::Next,
            why: None,
            start: false,
            session: None,
        }
    }
}

/// The plan a new task joins.
#[derive(Clone, Copy, Debug)]
pub enum PlanSelector<'a> {
    Id(i64),
    Slug(&'a str),
    /// The single active plan; refused when none or several are active.
    Active,
}

impl From<i64> for PlanSelector<'_> {
    fn from(plan_id: i64) -> Self {
        Self::Id(plan_id)
    }
}

impl<'a> From<Option<&'a str>> for PlanSelector<'a> {
    fn from(slug: Option<&'a str>) -> Self {
        slug.map_or(Self::Active, Self::Slug)
    }
}

/// Create one task atomically, optionally starting it; returns its sequence.
pub fn add_task<'a>(
    conn: &Connection,
    actor: &str,
    plan: impl Into<PlanSelector<'a>>,
    creation: TaskCreation<'_>,
) -> Result<i64> {
    if creation.start && creation.why.is_none_or(|why| why.trim().is_empty()) {
        bail!("add --start requires --why or --why-file with a standalone rationale");
    }
    let tx = begin_mutation(conn)?;
    let plan_id = match plan.into() {
        PlanSelector::Id(plan_id) => plan_id,
        PlanSelector::Slug(slug) => resolve_plan(&tx, Some(slug))?.0,
        PlanSelector::Active => resolve_plan(&tx, None)?.0,
    };
    let seq = add_task_in_mutation(
        &tx,
        actor,
        plan_id,
        creation.title,
        creation.intent,
        creation.kind,
        creation.parent,
        creation.deps,
        creation.tags,
        creation.priority,
        creation.why,
    )?;
    if creation.start {
        start_task_in_mutation(&tx, actor, seq, creation.why, true, creation.session)?;
    }
    tx.commit()?;
    Ok(seq)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_task_in_mutation(
    tx: &Transaction<'_>,
    actor: &str,
    plan_id: i64,
    title: &str,
    intent: &str,
    kind: &str,
    parent: Option<i64>,
    deps: &[i64],
    tags: &[String],
    priority: Priority,
    why: Option<&str>,
) -> Result<i64> {
    validate_assignable_task_kind(kind)?;
    let title = validate_task_title(title)?;
    let status = plan_status(tx, plan_id)?;
    if matches!(status.as_str(), "done" | "retired") {
        bail!("plan is {status}; reactivate it before adding tasks");
    }
    let parent_id = match parent {
        Some(p) => {
            let parent = get_task(tx, p)?;
            if parent.plan_id != plan_id {
                bail!("parent #{p} belongs to a different plan");
            }
            if matches!(parent.status.as_str(), "done" | "retired" | "rejected") {
                bail!(
                    "parent #{p} is {}; reopen it before adding live children",
                    parent.status
                );
            }
            Some(parent.task_id)
        }
        None => None,
    };
    let seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM tasks", [], |r| r.get(0))?;
    let t = now();
    tx.execute(
        "INSERT INTO tasks
         (seq, plan_id, parent_id, title, intent, kind, status, priority, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'proposed', ?7, ?8, ?8)",
        params![seq, plan_id, parent_id, title, intent, kind, priority, t],
    )?;
    let id = tx.last_insert_rowid();
    record_event_in_mutation(
        tx,
        actor,
        "task",
        Some(id),
        "create",
        why,
        Some(&serde_json::json!({
            "seq": seq,
            "title": title,
            "kind": kind,
        })),
    )?;
    for d in deps {
        add_dep_inner(tx, actor, seq, *d, true, why)?;
    }
    for tag in tags {
        let tag = validate_tag(tag)?;
        tx.execute(
            "INSERT OR IGNORE INTO task_tags (task_id, tag) VALUES (?1, ?2)",
            params![id, tag],
        )?;
    }
    Ok(seq)
}

pub struct TaskEdit<'a> {
    pub title: Option<&'a str>,
    pub intent: Option<&'a str>,
    pub parent: Option<Option<i64>>,
    pub kind: Option<&'a str>,
    pub priority: Option<Priority>,
}

pub(crate) fn valid_task_definition_revision_payload(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object
        .get("revision_schema")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|schema| {
            schema != TASK_DEFINITION_REVISION_SCHEMA
                && schema != crate::TASK_DEFINITION_REVISION_SCHEMA_V1
        })
        || !object
            .get("seq")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|seq| seq > 0)
    {
        return false;
    }
    let Some(fields) = object.get("fields").and_then(serde_json::Value::as_array) else {
        return false;
    };
    let Some(changes) = object.get("changes").and_then(serde_json::Value::as_object) else {
        return false;
    };
    if fields.is_empty() || fields.len() != changes.len() {
        return false;
    }
    let field_names = fields
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<HashSet<_>>();
    if field_names.len() != fields.len()
        || field_names.len() != changes.len()
        || !changes
            .keys()
            .all(|field| field_names.contains(field.as_str()))
    {
        return false;
    }
    changes.iter().all(|(field, revision)| {
        let Some(revision) = revision.as_object() else {
            return false;
        };
        let (Some(before), Some(after)) = (revision.get("before"), revision.get("after")) else {
            return false;
        };
        if before == after {
            return false;
        }
        match field.as_str() {
            "title" => {
                before
                    .as_str()
                    .is_some_and(|value| !value.trim().is_empty())
                    && after.as_str().is_some_and(|value| !value.trim().is_empty())
            }
            "intent" => before.is_string() && after.is_string(),
            "intent_source" => [before, after].into_iter().all(|value| {
                value.is_null()
                    || value
                        .as_str()
                        .is_some_and(|source| MEANING_SOURCES.contains(&source))
            }),
            "kind" => {
                before
                    .as_str()
                    .is_some_and(|value| TASK_KINDS.contains(&value))
                    && after
                        .as_str()
                        .is_some_and(|value| TASK_KINDS.contains(&value))
            }
            "parent" => [before, after].into_iter().all(|value| {
                value.is_null() || value.as_i64().is_some_and(|parent_seq| parent_seq > 0)
            }),
            // Edits before priority levels recorded integers.
            "priority" => {
                (before.is_i64() && after.is_i64())
                    || [before, after].into_iter().all(|value| {
                        value
                            .as_str()
                            .is_some_and(|level| level.parse::<Priority>().is_ok())
                    })
            }
            "plan" => [before, after]
                .into_iter()
                .all(|value| value.as_str().is_some_and(|slug| !slug.trim().is_empty())),
            _ => false,
        }
    })
}

pub fn edit_task(
    conn: &Connection,
    actor: &str,
    seq: i64,
    edit: TaskEdit<'_>,
    why: &str,
) -> Result<Vec<&'static str>> {
    if why.trim().is_empty() {
        bail!("editing a task requires a nonblank reason");
    }
    if edit.title.is_none()
        && edit.intent.is_none()
        && edit.parent.is_none()
        && edit.kind.is_none()
        && edit.priority.is_none()
    {
        bail!(
            "task edit requires at least one of --title, --intent, --parent, --clear-parent, --kind, or --priority"
        );
    }
    if let Some(kind) = edit.kind {
        validate_assignable_task_kind(kind)?;
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let edited_at = now();
    let mut changed = Vec::new();
    let mut changes = serde_json::Map::new();
    if let Some(new) = edit.title {
        let new = validate_task_title(new)?;
        if new != task.title {
            tx.execute(
                "UPDATE tasks SET title=?1, updated_at=?2 WHERE task_id=?3",
                params![new, edited_at, task.task_id],
            )?;
            changed.push("title");
            changes.insert(
                "title".into(),
                serde_json::json!({"before": task.title, "after": new}),
            );
        }
    }
    if let Some(new) = edit.intent
        && new != task.intent
    {
        tx.execute(
            "UPDATE tasks SET intent=?1, updated_at=?2 WHERE task_id=?3",
            params![new, edited_at, task.task_id],
        )?;
        changed.push("intent");
        changes.insert(
            "intent".into(),
            serde_json::json!({"before": task.intent, "after": new}),
        );
    }
    if let Some(new) = edit.kind
        && new != task.kind
    {
        tx.execute(
            "UPDATE tasks SET kind=?1, updated_at=?2 WHERE task_id=?3",
            params![new, edited_at, task.task_id],
        )?;
        changed.push("kind");
        changes.insert(
            "kind".into(),
            serde_json::json!({"before": task.kind, "after": new}),
        );
    }
    if let Some(parent) = edit.parent {
        let old_parent_seq = task
            .parent_id
            .map(|parent_id| {
                tx.query_row(
                    "SELECT seq FROM tasks WHERE task_id=?1",
                    params![parent_id],
                    |row| row.get::<_, i64>(0),
                )
            })
            .transpose()?;
        let (parent_id, parent_seq) = match parent {
            Some(parent_seq) => {
                let new_parent = get_task(&tx, parent_seq)?;
                if new_parent.task_id == task.task_id {
                    bail!("#{seq} cannot be its own parent");
                }
                if new_parent.plan_id != task.plan_id {
                    bail!("#{seq} and parent #{parent_seq} belong to different plans");
                }
                if matches!(task.status.as_str(), "proposed" | "in_progress")
                    && matches!(new_parent.status.as_str(), "done" | "retired" | "rejected")
                {
                    bail!(
                        "parent #{parent_seq} is {}; reopen it before assigning live child #{seq}",
                        new_parent.status
                    );
                }
                (Some(new_parent.task_id), Some(new_parent.seq))
            }
            None => (None, None),
        };
        if parent_id != task.parent_id {
            tx.execute(
                "UPDATE tasks SET parent_id=?1, updated_at=?2 WHERE task_id=?3",
                params![parent_id, edited_at, task.task_id],
            )?;
            if let Some(cycle) = find_cycle(
                &tx,
                "SELECT task_id, parent_id FROM tasks WHERE parent_id IS NOT NULL",
            )? {
                bail!(
                    "parent change would create cycle {}",
                    cycle
                        .iter()
                        .map(|task| format!("#{task}"))
                        .collect::<Vec<_>>()
                        .join(" -> ")
                );
            }
            if let Some(parent_seq) = parent_seq
                && matches!(task.status.as_str(), "proposed" | "in_progress")
                && let Some(steps) = task_graph::waits_for(&tx, seq, parent_seq)?
            {
                bail!(
                    "parent change would deadlock: #{seq} finishes only after its new parent #{parent_seq} ({}); choose another parent{}",
                    task_graph::describe_chain(seq, &steps),
                    task_graph::or_remove_dependency(seq, &steps)
                );
            }
            changed.push("parent");
            changes.insert(
                "parent".into(),
                serde_json::json!({"before": old_parent_seq, "after": parent_seq}),
            );
        }
    }
    if let Some(new) = edit.priority
        && new != task.priority
    {
        tx.execute(
            "UPDATE tasks SET priority=?1, updated_at=?2 WHERE task_id=?3",
            params![new, edited_at, task.task_id],
        )?;
        changed.push("priority");
        changes.insert(
            "priority".into(),
            serde_json::json!({"before": task.priority, "after": new}),
        );
    }
    if changed.is_empty() {
        bail!("#{seq} edit made no changes; omit unchanged values or choose different values");
    }
    let edited = get_task(&tx, seq)?;
    if edited.status == "done"
        && kind_requires_result(&edited.kind)
        && edited
            .result
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    {
        bail!(
            "completed {} task #{seq} requires a durable result; reopen it before changing kind",
            edited.kind
        );
    }
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "edit",
        Some(why),
        Some(&serde_json::json!({
            "seq": task.seq,
            "revision_schema": TASK_DEFINITION_REVISION_SCHEMA,
            "fields": changed,
            "changes": changes,
        })),
    )?;
    tx.commit()?;
    Ok(changed)
}

/// Tag and dependency edits record a reason only when one is given; a reason
/// that is given must be nonblank.
fn optional_reason<'a>(why: Option<&'a str>, action: &str) -> Result<Option<&'a str>> {
    match why {
        Some(why) if why.trim().is_empty() => {
            bail!("{action}: --why was given without text; pass a reason or omit --why")
        }
        why => Ok(why),
    }
}

pub fn add_tag(
    conn: &Connection,
    actor: &str,
    seq: i64,
    tag: &str,
    why: Option<&str>,
) -> Result<()> {
    let tag = validate_tag(tag).context("adding a tag requires a valid tag")?;
    let why = optional_reason(why, "adding a tag")?;
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let changed = tx.execute(
        "INSERT OR IGNORE INTO task_tags (task_id, tag) VALUES (?1, ?2)",
        params![task.task_id, tag],
    )?;
    if changed == 0 {
        bail!("#{seq} already has tag '{tag}'");
    }
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "tag_add",
        why,
        Some(&serde_json::json!({"seq": seq, "tag": tag})),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn remove_tag(
    conn: &Connection,
    actor: &str,
    seq: i64,
    tag: &str,
    why: Option<&str>,
) -> Result<()> {
    let tag = validate_tag(tag).context("removing a tag requires a valid tag")?;
    let why = optional_reason(why, "removing a tag")?;
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let changed = tx.execute(
        "DELETE FROM task_tags WHERE task_id=?1 AND tag=?2",
        params![task.task_id, tag],
    )?;
    if changed == 0 {
        bail!("#{seq} does not have tag '{tag}'");
    }
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "tag_remove",
        why,
        Some(&serde_json::json!({"seq": seq, "tag": tag})),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn add_dep(
    conn: &Connection,
    actor: &str,
    seq: i64,
    on_seq: i64,
    why: Option<&str>,
) -> Result<()> {
    let why = optional_reason(why, "adding a dependency")?;
    let tx = begin_mutation(conn)?;
    add_dep_inner(&tx, actor, seq, on_seq, true, why)?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn add_dep_inner(
    tx: &Transaction<'_>,
    actor: &str,
    seq: i64,
    on_seq: i64,
    event: bool,
    why: Option<&str>,
) -> Result<()> {
    let task = get_task(tx, seq)?;
    let on = get_task(tx, on_seq)?;
    if task.task_id == on.task_id {
        bail!("#{seq} cannot depend on itself");
    }
    if task.plan_id != on.plan_id {
        bail!("#{seq} and dependency #{on_seq} belong to different plans");
    }
    if event && matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "#{seq} is {}; reopen it before adding dependencies",
            task.status
        );
    }
    if event && matches!(on.status.as_str(), "retired" | "rejected") {
        bail!(
            "dependency #{on_seq} is {}; reopen it or choose a viable prerequisite",
            on.status
        );
    }
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM deps WHERE task_id=?1 AND depends_on=?2)",
        params![task.task_id, on.task_id],
        |r| r.get(0),
    )?;
    if exists {
        bail!("#{seq} already depends on #{on_seq}");
    }
    // Reject cycles: is `task` reachable from `on` via deps?
    let mut stack = vec![on.task_id];
    let mut seen = HashSet::new();
    while let Some(cur) = stack.pop() {
        if cur == task.task_id {
            bail!("dependency #{seq} -> #{on_seq} would create a cycle");
        }
        if !seen.insert(cur) {
            continue;
        }
        let mut st = tx.prepare("SELECT depends_on FROM deps WHERE task_id=?1")?;
        let next: Vec<i64> = st
            .query_map(params![cur], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        stack.extend(next);
    }
    // Import restores edges that earlier releases admitted; it never adds new waits.
    if event && let Some(steps) = task_graph::waits_for(tx, on.seq, task.seq)? {
        bail!(
            "dependency #{seq} -> #{on_seq} would deadlock: #{on_seq} finishes only after #{seq} ({}); choose a prerequisite that does not wait for #{seq}{}",
            task_graph::describe_chain(on.seq, &steps),
            task_graph::or_remove_dependency(on.seq, &steps)
        );
    }
    tx.execute(
        "INSERT INTO deps (task_id, depends_on) VALUES (?1, ?2)",
        params![task.task_id, on.task_id],
    )?;
    if event {
        record_event_in_mutation(
            tx,
            actor,
            "dep",
            Some(task.task_id),
            "add",
            why,
            Some(&serde_json::json!({"on": on_seq})),
        )?;
    }
    Ok(())
}

pub fn remove_dep(
    conn: &Connection,
    actor: &str,
    seq: i64,
    on_seq: i64,
    why: Option<&str>,
) -> Result<()> {
    let why = optional_reason(why, "removing a dependency")?;
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let on = get_task(&tx, on_seq)?;
    let n = tx.execute(
        "DELETE FROM deps WHERE task_id=?1 AND depends_on=?2",
        params![task.task_id, on.task_id],
    )?;
    if n == 0 {
        bail!("#{seq} does not depend on #{on_seq}");
    }
    record_event_in_mutation(
        &tx,
        actor,
        "dep",
        Some(task.task_id),
        "remove",
        why,
        Some(&serde_json::json!({"on": on_seq})),
    )?;
    tx.commit()?;
    Ok(())
}
