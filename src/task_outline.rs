//! One-call decomposition: a versioned outline creates a parent's child tasks
//! and their dependencies atomically.
//!
//! Batch-local keys wire sibling dependencies and the plain output's
//! key-to-task lines. They are never stored, so no invented label survives
//! the call; the JSON receipt names children only by outline order.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use std::collections::{HashMap, HashSet};

use crate::task_graph::{WaitGraph, describe_chain, first_cycle, or_remove_dependency};
use crate::{
    begin_mutation, get_task, meaning_source_requires_text, parse_task_ref, plan_status,
    validate_meaning_source, validate_tag, validate_task_kind, validate_task_title,
};

pub const TASK_OUTLINE_SCHEMA: &str = "papertiger.task_outline.v1";
pub const MAX_OUTLINE_KEY_CHARS: usize = 64;
/// Largest outline document accepted, in bytes.
pub const MAX_OUTLINE_BYTES: usize = 1024 * 1024;
/// Most children one outline may create.
pub const MAX_OUTLINE_CHILDREN: usize = 256;

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
    /// Any integral JSON number, `2.0` included, as JSON Schema's `integer`.
    #[serde(default, deserialize_with = "integral_priority")]
    pub priority: i64,
    /// Strings name sibling keys; integers name existing task numbers.
    #[serde(default)]
    pub deps: Vec<serde_json::Value>,
}

fn default_kind() -> String {
    "work".into()
}

/// The integer a JSON number denotes, accepting an integral float such as
/// `3.0` the way JSON Schema's `integer` does.
fn integral(number: &serde_json::Number) -> Option<i64> {
    number.as_i64().or_else(|| {
        number
            .as_f64()
            .filter(|value| {
                value.fract() == 0.0 && *value >= i64::MIN as f64 && *value < i64::MAX as f64
            })
            .map(|value| value as i64)
    })
}

fn integral_priority<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let number = serde_json::Number::deserialize(deserializer)?;
    integral(&number)
        .ok_or_else(|| de::Error::custom(format!("priority {number} is not an integer")))
}

/// Walks any JSON value and refuses an object that repeats a key, which
/// `serde_json::Value` would otherwise resolve silently to the last copy.
struct UniqueKeys;

impl<'de> Deserialize<'de> for UniqueKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueKeysVisitor)
    }
}

struct UniqueKeysVisitor;

impl<'de> Visitor<'de> for UniqueKeysVisitor {
    type Value = UniqueKeys;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }

    fn visit_i64<E>(self, _: i64) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }

    fn visit_u64<E>(self, _: u64) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }

    fn visit_f64<E>(self, _: f64) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }

    fn visit_str<E>(self, _: &str) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }

    fn visit_unit<E>(self) -> Result<UniqueKeys, E> {
        Ok(UniqueKeys)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<UniqueKeys, A::Error> {
        while seq.next_element::<UniqueKeys>()?.is_some() {}
        Ok(UniqueKeys)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<UniqueKeys, A::Error> {
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(de::Error::custom(format!(
                    "task outline repeats key {key:?} in one object; write each key once"
                )));
            }
            map.next_value::<UniqueKeys>()?;
        }
        Ok(UniqueKeys)
    }
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
/// A repeated key in any object refuses the outline.
pub fn parse_task_outline(bytes: &[u8]) -> Result<TaskOutline> {
    if bytes.len() > MAX_OUTLINE_BYTES {
        bail!(
            "task outline exceeds the {MAX_OUTLINE_BYTES}-byte limit; split it into several decompose calls"
        );
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).context(
        "task outline must be UTF-8 JSON; write the file as UTF-8 and pass it with --outline-file",
    )?;
    let value: serde_json::Value = serde_json::from_str(text).context(
        "task outline is not valid JSON; see `papertiger schema` for papertiger.task_outline.v1",
    )?;
    UniqueKeys::deserialize(&mut serde_json::Deserializer::from_str(text))
        .map_err(|error| anyhow::anyhow!("{error}"))?;
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

/// Create every outline child under `parent_seq` in one transaction, in
/// outline order, then their dependency edges, then (with `start_ready`) start
/// each child whose dependencies are all existing done tasks. A parent, plan
/// or outline size that cannot take children refuses first; otherwise every
/// problem found in the entries is listed and nothing is created.
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
    let parent_finished = matches!(parent.status.as_str(), "done" | "retired" | "rejected");
    let reopen_parent = format!("`papertiger reopen {parent_seq} --why <reason>`");
    // The plan comes first: a finished plan refuses the parent's reopen too.
    let status = plan_status(&tx, parent.plan_id)?;
    if matches!(status.as_str(), "done" | "retired") {
        let slug = crate::get_plan(&tx, parent.plan_id)?.slug;
        let then_reopen = if parent_finished {
            format!(" and then parent #{parent_seq} with {reopen_parent}")
        } else {
            String::new()
        };
        bail!(
            "plan '{slug}' is {status}; reactivate it with `papertiger plan set {slug} active --why <reason>`{then_reopen} before adding tasks"
        );
    }
    if parent_finished {
        bail!(
            "parent #{parent_seq} is {}; reopen it with {reopen_parent} before adding live children",
            parent.status
        );
    }
    if outline.children.is_empty() {
        bail!(
            "task outline has no children; list at least one entry under \"children\" or use `papertiger add`"
        );
    }
    if outline.children.len() > MAX_OUTLINE_CHILDREN {
        bail!(
            "task outline has {} children, more than the limit of {MAX_OUTLINE_CHILDREN}; group them under intermediate children and decompose each with its own outline",
            outline.children.len()
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
        let mut seen_tags = HashSet::new();
        for tag in &entry.tags {
            match validate_tag(tag) {
                Err(error) => problems.push(format!("{label}: {error}")),
                Ok(tag) if !seen_tags.insert(tag) => {
                    problems.push(format!("{label}: repeats tag {tag:?}; list each tag once"))
                }
                Ok(_) => {}
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

    // Children do not exist yet, so one snapshot answers every wait check.
    let waits = WaitGraph::load(&tx)?;
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
                    let Some(seq) = integral(number).filter(|seq| *seq > 0) else {
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
                    } else if let Some(steps) = waits.chain(seq, parent_seq) {
                        let chain = if steps.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", describe_chain(seq, &steps))
                        };
                        problems.push(format!(
                            "{label}: dependency #{seq} finishes only after parent #{parent_seq}, which waits for this child{chain}; depend on a task that does not wait for #{parent_seq}{}",
                            or_remove_dependency(seq, &steps)
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
    if let Some(cycle) = first_cycle(&sibling_edges) {
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
    fn repeated_keys_are_refused_at_any_depth() {
        for outline in [
            r#"{"schema":"papertiger.task_outline.v1","schema":"papertiger.task_outline.v1","children":[]}"#,
            r#"{"schema":"papertiger.task_outline.v1","children":[],"children":[{"key":"a","title":"A"}]}"#,
            r#"{"schema":"papertiger.task_outline.v1","children":[{"key":"a","title":"A","title":"B"}]}"#,
        ] {
            let error = parse_task_outline(outline.as_bytes())
                .unwrap_err()
                .to_string();
            assert!(error.contains("repeats key"), "{error}");
            assert!(error.contains("write each key once"), "{error}");
        }
    }

    #[test]
    fn integral_numbers_are_integers_and_oversized_input_is_refused() {
        let outline = parse_task_outline(
            br#"{"schema":"papertiger.task_outline.v1","children":[{"key":"a","title":"A","priority":2.0,"deps":[3.0]}]}"#,
        )
        .unwrap();
        assert_eq!(outline.children[0].priority, 2);
        let serde_json::Value::Number(dependency) = &outline.children[0].deps[0] else {
            panic!("dependency is a number");
        };
        assert_eq!(integral(dependency), Some(3));
        let fraction = parse_task_outline(
            br#"{"schema":"papertiger.task_outline.v1","children":[{"key":"a","title":"A","priority":2.5}]}"#,
        )
        .unwrap_err();
        assert!(format!("{fraction:#}").contains("priority 2.5 is not an integer"));
        let oversized = vec![b' '; MAX_OUTLINE_BYTES + 1];
        let error = parse_task_outline(&oversized).unwrap_err().to_string();
        assert!(
            error.contains("split it into several decompose calls"),
            "{error}"
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
