//! One-call decomposition: a versioned outline creates a parent's child tasks
//! and their dependencies atomically.
//!
//! Batch-local keys wire sibling dependencies and the receipt's key-to-task
//! mapping. They are never stored, so no invented label survives the call.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

use crate::{
    begin_mutation, get_task, meaning_source_requires_text, parse_task_ref, plan_status,
    validate_meaning_source, validate_tag, validate_task_kind, validate_task_title, waits_for,
};

pub const TASK_OUTLINE_SCHEMA: &str = "papertiger.task_outline.v1";
pub const MAX_OUTLINE_KEY_CHARS: usize = 64;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOutline {
    pub schema: String,
    pub children: Vec<OutlineEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlineEntry {
    /// Batch-local key: wires sibling dependencies and the receipt mapping.
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub intent_source: Option<String>,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub priority: i64,
    /// Strings name sibling keys; integers name existing task numbers.
    #[serde(default)]
    pub deps: Vec<serde_json::Value>,
}

fn default_kind() -> String {
    "work".into()
}

/// The task created for one outline key.
#[derive(Debug, Clone)]
pub struct OutlineChild {
    pub key: String,
    pub seq: i64,
    pub status: String,
}

/// Parse UTF-8 outline bytes (one leading BOM accepted) and check the
/// schema id before the document shape, so a wrong id is named exactly.
pub fn parse_task_outline(bytes: &[u8]) -> Result<TaskOutline> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).context(
        "task outline must be UTF-8 JSON; write the file as UTF-8 and pass it with --outline-file",
    )?;
    let value: serde_json::Value = serde_json::from_str(text).context(
        "task outline is not valid JSON; see `papertiger schema` for papertiger.task_outline.v1",
    )?;
    match value.get("schema").and_then(serde_json::Value::as_str) {
        Some(TASK_OUTLINE_SCHEMA) => {}
        Some(other) => bail!(
            "unsupported task outline schema '{other}'; set \"schema\": \"{TASK_OUTLINE_SCHEMA}\""
        ),
        None => bail!("task outline requires \"schema\": \"{TASK_OUTLINE_SCHEMA}\""),
    }
    serde_json::from_value(value).context(
        "task outline does not match papertiger.task_outline.v1; see `papertiger schema` for its fields",
    )
}

fn valid_key(key: &str) -> bool {
    key.len() <= MAX_OUTLINE_KEY_CHARS
        && key.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn title_identity(title: &str) -> String {
    title.trim().to_lowercase()
}

enum Dependency {
    Sibling(usize),
    Existing(i64),
}

fn sibling_cycle(edges: &[Vec<usize>]) -> Option<Vec<usize>> {
    fn visit(
        node: usize,
        edges: &[Vec<usize>],
        state: &mut [u8],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        state[node] = 1;
        stack.push(node);
        for &next in &edges[node] {
            match state[next] {
                0 => {
                    if let Some(cycle) = visit(next, edges, state, stack) {
                        return Some(cycle);
                    }
                }
                1 => {
                    let start = stack.iter().position(|&n| n == next).unwrap_or(0);
                    let mut cycle = stack[start..].to_vec();
                    cycle.push(next);
                    return Some(cycle);
                }
                _ => {}
            }
        }
        stack.pop();
        state[node] = 2;
        None
    }
    let mut state = vec![0; edges.len()];
    for node in 0..edges.len() {
        if state[node] == 0
            && let Some(cycle) = visit(node, edges, &mut state, &mut Vec::new())
        {
            return Some(cycle);
        }
    }
    None
}

/// Create every outline child under `parent_seq` in one transaction, in
/// outline order, then their dependency edges, then (with `start_ready`) start
/// each child whose dependencies are all existing done tasks. Any invalid
/// entry refuses the whole outline and names every problem found.
pub fn decompose_task(
    conn: &Connection,
    actor: &str,
    parent_seq: i64,
    outline: &TaskOutline,
    start_ready: bool,
    session: Option<&str>,
) -> Result<Vec<OutlineChild>> {
    let tx = begin_mutation(conn)?;
    let parent = get_task(&tx, parent_seq)?;
    if matches!(parent.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "parent #{parent_seq} is {}; reopen it before adding live children",
            parent.status
        );
    }
    let status = plan_status(&tx, parent.plan_id)?;
    if matches!(status.as_str(), "done" | "retired") {
        bail!("plan is {status}; reactivate it before adding tasks");
    }
    if outline.children.is_empty() {
        bail!(
            "task outline has no children; list at least one entry under \"children\" or use `papertiger add`"
        );
    }

    let mut problems = Vec::new();
    let name = |index: usize| {
        format!(
            "child {} ({:?})",
            index + 1,
            outline.children[index].key.as_str()
        )
    };
    let mut keys = HashMap::new();
    for (index, entry) in outline.children.iter().enumerate() {
        if !valid_key(&entry.key) {
            problems.push(format!(
                "{}: key must be 1-{MAX_OUTLINE_KEY_CHARS} ASCII letters, digits, '-' or '_', starting with a letter",
                name(index)
            ));
        } else if let Some(first) = keys.insert(entry.key.as_str(), index) {
            problems.push(format!(
                "{}: key repeats {}; give every child its own key",
                name(index),
                name(first)
            ));
        }
    }

    let mut live_titles = HashMap::new();
    let mut statement = tx.prepare(
        "SELECT seq, title FROM tasks WHERE parent_id=?1 AND status IN ('proposed','in_progress')",
    )?;
    for row in statement.query_map(params![parent.task_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })? {
        let (seq, title) = row?;
        live_titles.insert(title_identity(&title), seq);
    }
    drop(statement);
    let mut outline_titles = HashMap::new();
    for (index, entry) in outline.children.iter().enumerate() {
        let label = name(index);
        match validate_task_title(&entry.title) {
            Err(error) => problems.push(format!("{label}: {error}")),
            Ok(title) => {
                let identity = title_identity(title);
                if let Some(seq) = live_titles.get(&identity) {
                    problems.push(format!(
                        "{label}: title duplicates live child #{seq} of #{parent_seq}; if this outline was already applied, inspect `papertiger show {parent_seq}` instead of replaying it"
                    ));
                } else if let Some(first) = outline_titles.insert(identity, index) {
                    problems.push(format!(
                        "{label}: title repeats {} (titles compare trimmed and case-insensitive)",
                        name(first)
                    ));
                }
            }
        }
        if let Err(error) = validate_task_kind(&entry.kind) {
            problems.push(format!("{label}: {error}"));
        }
        match validate_meaning_source(entry.intent_source.as_deref()) {
            Err(error) => problems.push(format!("{label}: {error}")),
            Ok(source) if !meaning_source_requires_text(Some(&entry.intent), source) => {
                problems.push(format!("{label}: intent_source requires a nonblank intent"))
            }
            Ok(_) => {}
        }
        for tag in &entry.tags {
            if let Err(error) = validate_tag(tag) {
                problems.push(format!("{label}: {error}"));
            }
        }
        if entry
            .why
            .as_deref()
            .is_some_and(|why| why.trim().is_empty())
        {
            problems.push(format!(
                "{label}: why must be nonblank; omit it or state the rationale"
            ));
        }
    }

    let mut dependencies: Vec<Vec<Dependency>> = Vec::new();
    for (index, entry) in outline.children.iter().enumerate() {
        let label = name(index);
        let mut resolved = Vec::new();
        let mut seen_keys = HashSet::new();
        let mut seen_tasks = HashSet::new();
        for dependency in &entry.deps {
            match dependency {
                serde_json::Value::String(key) => match keys.get(key.as_str()) {
                    Some(&target) if target == index => {
                        problems.push(format!("{label}: cannot depend on itself"))
                    }
                    Some(&target) => {
                        if seen_keys.insert(target) {
                            resolved.push(Dependency::Sibling(target));
                        } else {
                            problems.push(format!("{label}: repeats dependency {key:?}"));
                        }
                    }
                    None if parse_task_ref(key).is_ok() => problems.push(format!(
                        "{label}: dependency {key:?} is a string; write an existing task as the integer {}",
                        key.trim_start_matches('#')
                    )),
                    None => problems.push(format!(
                        "{label}: dependency {key:?} names no key in this outline"
                    )),
                },
                serde_json::Value::Number(number) => {
                    let Some(seq) = number.as_i64().filter(|seq| *seq > 0) else {
                        problems.push(format!(
                            "{label}: dependency {number} is not a positive task number"
                        ));
                        continue;
                    };
                    if !seen_tasks.insert(seq) {
                        problems.push(format!("{label}: repeats dependency #{seq}"));
                        continue;
                    }
                    let exists = tx
                        .query_row("SELECT 1 FROM tasks WHERE seq=?1", params![seq], |_| Ok(()))
                        .optional()?
                        .is_some();
                    if !exists {
                        problems.push(format!("{label}: dependency #{seq} does not exist"));
                        continue;
                    }
                    // An unreadable existing task refuses with its own corrective error.
                    let task = get_task(&tx, seq)?;
                    if task.plan_id != parent.plan_id {
                        problems.push(format!(
                            "{label}: dependency #{seq} belongs to a different plan than parent #{parent_seq}"
                        ));
                    } else if matches!(task.status.as_str(), "retired" | "rejected") {
                        problems.push(format!(
                            "{label}: dependency #{seq} is {}; reopen it or choose a viable prerequisite",
                            task.status
                        ));
                    } else if waits_for(&tx, task.task_id, parent.task_id)? {
                        problems.push(format!(
                            "{label}: dependency #{seq} finishes only after parent #{parent_seq}, which waits for this child"
                        ));
                    } else {
                        resolved.push(Dependency::Existing(seq));
                    }
                }
                other => problems.push(format!(
                    "{label}: dependency {other} must be a sibling key string or an existing task number"
                )),
            }
        }
        dependencies.push(resolved);
    }
    let sibling_edges = dependencies
        .iter()
        .map(|resolved| {
            resolved
                .iter()
                .filter_map(|dependency| match dependency {
                    Dependency::Sibling(target) => Some(*target),
                    Dependency::Existing(_) => None,
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if let Some(cycle) = sibling_cycle(&sibling_edges) {
        problems.push(format!(
            "sibling dependencies form a cycle: {}",
            cycle
                .iter()
                .map(|&index| format!("{:?}", outline.children[index].key.as_str()))
                .collect::<Vec<_>>()
                .join(" -> ")
        ));
    }

    let mut ready = vec![false; outline.children.len()];
    if start_ready {
        if status != "active" {
            problems.push(format!(
                "--start-ready needs an active plan, but the plan is {status}; set it active or omit --start-ready"
            ));
        }
        for (index, resolved) in dependencies.iter().enumerate() {
            let mut satisfied = true;
            for dependency in resolved {
                satisfied &= match dependency {
                    Dependency::Sibling(_) => false,
                    Dependency::Existing(seq) => get_task(&tx, *seq)?.status == "done",
                };
            }
            ready[index] = satisfied;
            if satisfied && outline.children[index].why.is_none() {
                problems.push(format!(
                    "{}: --start-ready would start it, so it needs a \"why\" like add --start",
                    name(index)
                ));
            }
        }
    }
    if !problems.is_empty() {
        bail!(
            "task outline refused; nothing was created:\n- {}",
            problems.join("\n- ")
        );
    }

    let mut sequences = Vec::with_capacity(outline.children.len());
    for entry in &outline.children {
        sequences.push(crate::add_task_in_mutation(
            &tx,
            actor,
            parent.plan_id,
            &entry.title,
            &entry.intent,
            entry.intent_source.as_deref(),
            &entry.kind,
            Some(parent_seq),
            &[],
            &entry.tags,
            entry.priority,
            entry.why.as_deref(),
        )?);
    }
    for (index, resolved) in dependencies.iter().enumerate() {
        for dependency in resolved {
            let on = match dependency {
                Dependency::Sibling(target) => sequences[*target],
                Dependency::Existing(seq) => *seq,
            };
            crate::add_dep_inner(
                &tx,
                actor,
                sequences[index],
                on,
                true,
                outline.children[index].why.as_deref(),
            )?;
        }
    }
    let mut children = Vec::with_capacity(outline.children.len());
    for (index, entry) in outline.children.iter().enumerate() {
        if ready[index] {
            crate::start_task_in_mutation(
                &tx,
                actor,
                sequences[index],
                entry.why.as_deref(),
                true,
                session,
            )?;
        }
        children.push(OutlineChild {
            key: entry.key.clone(),
            seq: sequences[index],
            status: if ready[index] {
                "in_progress"
            } else {
                "proposed"
            }
            .into(),
        });
    }
    tx.commit()?;
    Ok(children)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_ascii_identifiers_starting_with_a_letter() {
        for key in [
            "store",
            "cli-flag",
            "a_1",
            &"k".repeat(MAX_OUTLINE_KEY_CHARS),
        ] {
            assert!(valid_key(key), "{key}");
        }
        for key in ["", "1st", "#4", "has space", "dot.key", &"k".repeat(65)] {
            assert!(!valid_key(key), "{key}");
        }
    }

    #[test]
    fn sibling_cycles_are_reported_as_a_closed_path() {
        assert_eq!(sibling_cycle(&[vec![1], vec![2], vec![]]), None);
        assert_eq!(
            sibling_cycle(&[vec![1], vec![2], vec![0]]),
            Some(vec![0, 1, 2, 0])
        );
    }

    #[test]
    fn schema_id_is_checked_before_the_document_shape() {
        let wrong = parse_task_outline(br#"{"schema":"papertiger.task_outline.v0","children":7}"#)
            .unwrap_err()
            .to_string();
        assert!(wrong.contains("set \"schema\": \"papertiger.task_outline.v1\""));
        let unknown = parse_task_outline(
            br#"{"schema":"papertiger.task_outline.v1","children":[{"key":"a","title":"A","depends_on":[]}]}"#,
        )
        .unwrap_err();
        assert!(format!("{unknown:#}").contains("unknown field `depends_on`"));
        let with_bom = [
            b"\xef\xbb\xbf".as_slice(),
            br#"{"schema":"papertiger.task_outline.v1","children":[]}"#,
        ]
        .concat();
        assert!(parse_task_outline(&with_bom).unwrap().children.is_empty());
    }
}
