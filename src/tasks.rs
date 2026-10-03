//! Task rows: the stored shape, validation, lookup, listing and task references.

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{
    ASSIGNABLE_TASK_KINDS, LEGACY_TASK_KIND, MAX_TAG_CHARS, MAX_TASK_TITLE_CHARS, MEANING_SOURCES,
    TASK_KINDS, TaskPickup, priority_recovery,
};

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    #[serde(skip_serializing)]
    pub task_id: i64,
    pub seq: i64,
    /// Kind letter and number, e.g. `F-924`; derived, never stored.
    pub handle: String,
    #[serde(skip_serializing)]
    pub plan_id: i64,
    #[serde(skip_serializing)]
    pub parent_id: Option<i64>,
    #[serde(skip_serializing)]
    pub replacement_task_id: Option<i64>,
    pub title: String,
    pub intent: String,
    pub kind: String,
    pub result: Option<String>,
    pub status: String,
    pub priority: i64,
    pub pickup: Option<TaskPickup>,
}

pub(crate) fn task_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let seq = r.get(1)?;
    let kind: String = r.get(7)?;
    Ok(Task {
        task_id: r.get(0)?,
        seq,
        handle: task_handle(seq, &kind),
        plan_id: r.get(2)?,
        parent_id: r.get(3)?,
        replacement_task_id: r.get(4)?,
        title: r.get(5)?,
        intent: r.get(6)?,
        kind,
        result: r.get(8)?,
        status: r.get(9)?,
        priority: priority_recovery::read_priority(r, 1, 10)?,
        pickup: match (
            r.get::<_, Option<String>>(11)?,
            r.get::<_, Option<String>>(12)?,
        ) {
            (None, None) => None,
            (session, Some(at)) => Some(TaskPickup { session, at }),
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
    })
}

pub(crate) const TASK_COLS: &str = "task_id, seq, plan_id, parent_id, replacement_task_id, title, intent, kind, result, status, priority, pickup_session, pickup_at";

/// Validate a stored kind, which may be the legacy `work`.
pub(crate) fn validate_task_kind(kind: &str) -> Result<()> {
    if !TASK_KINDS.contains(&kind) {
        bail!(
            "unknown task kind '{kind}' (expected {})",
            TASK_KINDS.join("|")
        );
    }
    Ok(())
}

/// Validate a kind being given to a task now; the legacy `work` is refused.
pub(crate) fn validate_assignable_task_kind(kind: &str) -> Result<()> {
    if kind == LEGACY_TASK_KIND {
        bail!(
            "'{LEGACY_TASK_KIND}' is the legacy kind of tasks recorded before typed kinds and cannot be assigned; choose --kind {}",
            ASSIGNABLE_TASK_KINDS.join("|")
        );
    }
    if !ASSIGNABLE_TASK_KINDS.contains(&kind) {
        bail!(
            "unknown task kind '{kind}'; choose --kind {}",
            ASSIGNABLE_TASK_KINDS.join("|")
        );
    }
    Ok(())
}

/// The letter a task handle shows for `kind`, or `None` for an unknown kind.
pub fn task_kind_letter(kind: &str) -> Option<char> {
    Some(match kind {
        "goal" => 'G',
        "feature" => 'F',
        "change" => 'C',
        "bug" => 'B',
        "probe" => 'P',
        "decision" => 'D',
        "release" => 'R',
        "work" => 'W',
        _ => return None,
    })
}

/// Probes, decisions and bugs record their outcome (the measurement, the
/// choice, the cause) before they can complete.
pub fn kind_requires_result(kind: &str) -> bool {
    matches!(kind, "probe" | "decision" | "bug")
}

/// A kind suggested by a title, for retyping legacy `work` tasks: its first
/// verb (after a short `Label:` prefix), else defect language anywhere in
/// it. Only a suggestion: the outcome, not the wording, decides the kind.
pub fn suggested_task_kind(title: &str) -> Option<&'static str> {
    let title = match title.split_once(": ") {
        Some((label, rest)) if label.split_whitespace().count() <= 3 => rest,
        _ => title,
    };
    let first = title
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .find(|word| !word.is_empty())?
        .to_ascii_lowercase();
    let by_verb = match first.as_str() {
        "fix" | "repair" | "correct" | "restore" | "stop" => Some("bug"),
        "decide" | "choose" | "select" | "ratify" | "settle" => Some("decision"),
        "probe" | "investigate" | "measure" | "evaluate" | "assess" | "diagnose" | "explore"
        | "compare" | "qualify" | "prove" | "verify" | "re-verify" | "audit" | "review" => {
            Some("probe")
        }
        "release" | "publish" | "ship" | "upgrade" | "refresh" | "vendor" | "install" | "roll"
        | "deploy" | "bump" => Some("release"),
        "refactor" | "split" | "cut" | "remove" | "rename" | "retire" | "migrate" | "replace"
        | "simplify" | "harden" | "keep" | "preserve" | "refuse" | "consolidate" | "move"
        | "drop" | "delete" | "trim" | "align" | "unify" | "reorganize" | "reduce" | "document" => {
            Some("change")
        }
        "add" | "build" | "implement" | "expose" | "support" | "create" | "enable" | "let"
        | "make" | "introduce" | "provide" | "record" | "show" | "report" | "model" | "emit"
        | "lower" | "admit" | "allow" | "attribute" | "establish" | "decompile" => Some("feature"),
        _ => None,
    };
    if by_verb.is_some() {
        return by_verb;
    }
    let lower = title.to_ascii_lowercase();
    [
        "cannot",
        "can never",
        "crash",
        "fails",
        "failed",
        "blocks",
        "drops",
        "silently",
        "misleading",
        "never",
        "stays",
        "is still",
        "wrong",
        "incorrect",
        "refuses",
        "breaks",
        "broken",
        "regress",
        "instead of",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    .then_some("bug")
}

pub fn validate_meaning_source(source: Option<&str>) -> Result<Option<&str>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let source = source.trim();
    if !MEANING_SOURCES.contains(&source) {
        bail!(
            "unknown meaning source '{source}' (expected {})",
            MEANING_SOURCES.join("|")
        );
    }
    Ok(Some(source))
}

pub(crate) fn validate_task_title(title: &str) -> Result<&str> {
    let title = require_nonblank("task title", title)?;
    let characters = title.chars().count();
    if characters > MAX_TASK_TITLE_CHARS {
        bail!(
            "task title has {characters} characters; shorten it to at most {MAX_TASK_TITLE_CHARS} characters"
        );
    }
    Ok(title)
}

pub(crate) fn validate_tag(tag: &str) -> Result<&str> {
    let tag = require_nonblank("tag", tag)?;
    let characters = tag.chars().count();
    if characters > MAX_TAG_CHARS {
        bail!("tag has {characters} characters; shorten it to at most {MAX_TAG_CHARS} characters");
    }
    Ok(tag)
}

pub(crate) fn require_nonblank<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} must not be blank");
    }
    Ok(value)
}

pub fn get_task(conn: &Connection, seq: i64) -> Result<Task> {
    conn.query_row(
        &format!("SELECT {TASK_COLS} FROM tasks WHERE seq=?1"),
        params![seq],
        task_from_row,
    )
    .optional()?
    .ok_or_else(|| anyhow!("no task #{seq}"))
}

pub(crate) fn task_tags_by_id(conn: &Connection, task_id: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare("SELECT tag FROM task_tags WHERE task_id=?1 ORDER BY tag")?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Query tasks through the canonical task projection. Optional filters are
/// applied without placeholder renumbering or a caller-owned row mapping.
pub fn list_tasks(
    conn: &Connection,
    plan_id: i64,
    status: Option<&str>,
    tag: Option<&str>,
    kind: Option<&str>,
) -> Result<Vec<Task>> {
    list_tasks_ordered(conn, plan_id, status, tag, kind, false)
}

pub fn list_tasks_by_activity(
    conn: &Connection,
    plan_id: i64,
    status: Option<&str>,
    tag: Option<&str>,
    kind: Option<&str>,
) -> Result<Vec<Task>> {
    list_tasks_ordered(conn, plan_id, status, tag, kind, true)
}

/// Refuse a kind filter that names no kind; `work` is a valid filter.
pub(crate) fn validate_kind_filter(kind: Option<&str>) -> Result<()> {
    match kind {
        Some(kind) => validate_task_kind(kind),
        None => Ok(()),
    }
}

fn list_tasks_ordered(
    conn: &Connection,
    plan_id: i64,
    status: Option<&str>,
    tag: Option<&str>,
    kind: Option<&str>,
    by_activity: bool,
) -> Result<Vec<Task>> {
    validate_kind_filter(kind)?;
    let order = if by_activity {
        "ORDER BY COALESCE((SELECT MAX(event_id) FROM canonical_events
                            WHERE entity_seq=tasks.seq
                              AND entity IN ('task','dep','gate')), 0) DESC, seq"
    } else {
        "ORDER BY seq"
    };
    let sql = format!(
        "SELECT {TASK_COLS} FROM tasks
          WHERE plan_id=?1
            AND (?2 IS NULL OR status=?2)
            AND (?3 IS NULL OR task_id IN (SELECT task_id FROM task_tags WHERE tag=?3))
            AND (?4 IS NULL OR kind=?4)
          {order}"
    );
    let parameters: Vec<rusqlite::types::Value> = vec![
        plan_id.into(),
        status.map(str::to_owned).into(),
        tag.map(str::to_owned).into(),
        kind.map(str::to_owned).into(),
    ];
    let mut statement = conn.prepare(&sql)?;
    Ok(statement
        .query_map(rusqlite::params_from_iter(parameters), task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Non-container tasks in one durable status, used by the compact status view.
pub fn leaf_tasks_with_status(conn: &Connection, plan_id: i64, status: &str) -> Result<Vec<Task>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks task WHERE plan_id=?1 AND status=?2
         AND NOT EXISTS (SELECT 1 FROM tasks child WHERE child.parent_id=task.task_id)
         ORDER BY seq"
    ))?;
    Ok(statement
        .query_map(params![plan_id, status], task_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Parse a task reference: `N`, `#N`, or a handle `F-N` whose letter names
/// the task's kind. Returns the number and the handle letter, if any; only
/// [`resolve_task_ref`] can check that letter against the authority.
pub(crate) fn parse_task_ref(task_ref: &str) -> Result<(i64, Option<char>)> {
    let invalid = || {
        anyhow!(
            "bad task reference '{task_ref}' (expected N, #N, or a handle such as F-N, with N a canonical positive ASCII decimal)"
        )
    };
    let (letter, digits) = match task_ref.split_once('-') {
        Some((letter, digits)) => {
            let mut chars = letter.chars();
            let (Some(letter), None) = (chars.next(), chars.next()) else {
                return Err(invalid());
            };
            let letter = letter.to_ascii_uppercase();
            if !TASK_KINDS
                .iter()
                .any(|kind| task_kind_letter(kind) == Some(letter))
            {
                bail!(
                    "bad task handle '{task_ref}': '{letter}' is not a kind letter (expected one of {})",
                    TASK_KINDS
                        .iter()
                        .filter_map(|kind| task_kind_letter(kind))
                        .collect::<String>()
                );
            }
            (Some(letter), digits)
        }
        None => (None, task_ref.strip_prefix('#').unwrap_or(task_ref)),
    };
    if digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    Ok((digits.parse::<i64>().map_err(|_| invalid())?, letter))
}

/// Resolve a task reference to its number. A handle's letter must match the
/// task's current kind, so a reference made before a retype is refused with
/// the current handle instead of silently naming different work.
pub fn resolve_task_ref(conn: &Connection, task_ref: &str) -> Result<i64> {
    let (seq, letter) = parse_task_ref(task_ref)?;
    if let Some(letter) = letter {
        let task = get_task(conn, seq)?;
        let current = task_handle(seq, &task.kind);
        if task_kind_letter(&task.kind) != Some(letter) {
            bail!(
                "task handle '{task_ref}' does not match #{seq}, which is a {} task ({current}); use {current} or {seq}",
                task.kind
            );
        }
    }
    Ok(seq)
}

/// A task's handle: its kind letter and number, e.g. `F-924`.
pub fn task_handle(seq: i64, kind: &str) -> String {
    match task_kind_letter(kind) {
        Some(letter) => format!("{letter}-{seq}"),
        None => format!("#{seq}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_suggestions_read_the_verb_after_a_label_then_defect_language() {
        assert_eq!(suggested_task_kind("Fix the empty plan crash"), Some("bug"));
        assert_eq!(
            suggested_task_kind("Dracomink: emit sequential facts"),
            Some("feature")
        );
        assert_eq!(
            suggested_task_kind("Native claims cannot express conditional delegation"),
            Some("bug")
        );
        assert_eq!(
            suggested_task_kind("Re-verify the framerate fixture"),
            Some("probe")
        );
        assert_eq!(suggested_task_kind("Plan templates for consumers"), None);
    }
}
