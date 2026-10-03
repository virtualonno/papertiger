//! Gates and the evidence locators they bind.

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    GATE_KINDS, begin_mutation, get_task, now, record_event_in_mutation, require_nonblank,
    validate_sha256,
};

pub fn add_gate(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    kind: &str,
    requirement: &str,
) -> Result<()> {
    let name = require_nonblank("gate name", name)?;
    let requirement = require_nonblank("gate requirement", requirement)?;
    if !GATE_KINDS.contains(&kind) {
        bail!(
            "unknown gate kind '{kind}' (expected {})",
            GATE_KINDS.join("|")
        );
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!("#{seq} is {}; reopen it before adding gates", task.status);
    }
    let exists = tx
        .query_row(
            "SELECT 1 FROM gates WHERE task_id=?1 AND name=?2",
            params![task.task_id, name],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        bail!("gate '{name}' already exists on #{seq}; choose a different gate name");
    }
    tx.execute(
        "INSERT INTO gates (task_id, name, kind, requirement) VALUES (?1, ?2, ?3, ?4)",
        params![task.task_id, name, kind, requirement],
    )
    .with_context(|| format!("store gate '{name}' on #{seq}"))?;
    record_event_in_mutation(
        &tx,
        actor,
        "gate",
        Some(tx.last_insert_rowid()),
        "create",
        None,
        Some(&serde_json::json!({"task": seq, "name": name, "kind": kind})),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn resolve_gate(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    evidence: &str,
    sha256: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    resolve_open_gate_then(
        conn,
        actor,
        seq,
        name,
        "resolved",
        Some(evidence),
        sha256,
        note,
        None,
    )
}

pub fn waive_gate(conn: &Connection, actor: &str, seq: i64, name: &str, why: &str) -> Result<()> {
    if why.trim().is_empty() {
        bail!("waiving a gate requires a nonblank reason");
    }
    resolve_open_gate_then(
        conn,
        actor,
        seq,
        name,
        "waived",
        None,
        None,
        None,
        Some(why),
    )
}

pub fn remove_gate(conn: &Connection, actor: &str, seq: i64, name: &str, why: &str) -> Result<()> {
    if why.trim().is_empty() {
        bail!("removing a gate requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let gate: Option<(i64, String, String)> = tx
        .query_row(
            "SELECT gate_id, kind, requirement FROM gates
             WHERE task_id=?1 AND name=?2 AND status='open'",
            params![task.task_id, name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (gate_id, kind, requirement) =
        gate.ok_or_else(|| anyhow!("no open gate '{name}' on #{seq}"))?;
    tx.execute("DELETE FROM gates WHERE gate_id=?1", params![gate_id])?;
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "gate_remove",
        Some(why),
        Some(&serde_json::json!({
            "seq": seq,
            "name": name,
            "kind": kind,
            "requirement": requirement,
        })),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn reopen_gate(conn: &Connection, actor: &str, seq: i64, name: &str, why: &str) -> Result<()> {
    if why.trim().is_empty() {
        bail!("reopening a gate requires a nonblank reason");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    if matches!(task.status.as_str(), "done" | "retired" | "rejected") {
        bail!(
            "#{seq} is {}; reopen the task before reopening gates",
            task.status
        );
    }
    let gate: Option<(i64, String)> = tx
        .query_row(
            "SELECT gate_id, status FROM gates WHERE task_id=?1 AND name=?2",
            params![task.task_id, name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (gate_id, status) = gate.ok_or_else(|| anyhow!("no gate '{name}' on #{seq}"))?;
    if status == "open" {
        bail!("gate '{name}' on #{seq} is already open");
    }
    tx.execute(
        "UPDATE gates
            SET status='open', evidence_locator=NULL, evidence_sha256=NULL,
                note=NULL, resolved_at=NULL
          WHERE gate_id=?1",
        params![gate_id],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "gate",
        Some(gate_id),
        "reopen",
        Some(why.trim()),
        Some(&serde_json::json!({
            "task": seq,
            "name": name,
            "from": status,
        })),
    )?;
    tx.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_open_gate_then(
    conn: &Connection,
    actor: &str,
    seq: i64,
    name: &str,
    to: &str,
    evidence: Option<&str>,
    sha256: Option<&str>,
    note: Option<&str>,
    why: Option<&str>,
) -> Result<()> {
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let gate_id: i64 = tx
        .query_row(
            "SELECT gate_id FROM gates WHERE task_id=?1 AND name=?2 AND status='open'",
            params![task.task_id, name],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("no open gate '{name}' on #{seq}"))?;
    if let Some(loc) = evidence {
        validate_new_evidence_locator(loc)?;
    }
    validate_optional_sha256(sha256)?;
    let stored_note = if to == "waived" { why } else { note };
    tx.execute(
        "UPDATE gates SET status=?1, evidence_locator=?2, evidence_sha256=?3, note=?4, resolved_at=?5
         WHERE gate_id=?6",
        params![to, evidence, sha256, stored_note, now(), gate_id],
    )?;
    record_event_in_mutation(
        &tx,
        actor,
        "gate",
        Some(gate_id),
        to,
        why,
        Some(&serde_json::json!({"task": seq, "name": name, "evidence": evidence})),
    )?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn validate_evidence_locator(locator: &str) -> Result<()> {
    let Some((scheme, value)) = locator.split_once(':') else {
        bail!(
            "evidence locator '{locator}' must be scheme:value (e.g. file:runtime/evidence/x.json)"
        );
    };
    if value.trim().is_empty() {
        bail!("evidence locator '{locator}' must have a nonblank scheme and value");
    }
    let mut chars = scheme.chars();
    let valid_scheme = chars.next().is_some_and(|ch| ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'));
    if !valid_scheme {
        bail!("evidence locator '{locator}' has an invalid scheme; use an RFC 3986-style name");
    }
    Ok(())
}

pub(crate) fn validate_new_evidence_locator(locator: &str) -> Result<()> {
    validate_evidence_locator(locator)?;
    if let Some((scheme, value)) = locator.split_once(':')
        && scheme.eq_ignore_ascii_case("commit")
    {
        validate_commit_oid(value)?;
    }
    Ok(())
}

pub fn validate_commit_oid(commit_oid: &str) -> Result<()> {
    let valid_length = matches!(commit_oid.len(), 40 | 64);
    if !valid_length || !commit_oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!(
            "commit object id '{commit_oid}' must be the full 40- or 64-character hexadecimal id; resolve it in the owning repository with `git rev-parse --verify 'HEAD^{{commit}}'`"
        );
    }
    if commit_oid.bytes().any(|byte| byte.is_ascii_uppercase()) {
        bail!(
            "commit object id '{commit_oid}' must use canonical lowercase hexadecimal; resolve it with `git rev-parse --verify 'HEAD^{{commit}}'`"
        );
    }
    Ok(())
}

pub(crate) fn validate_optional_sha256(sha256: Option<&str>) -> Result<()> {
    if let Some(digest) = sha256 {
        validate_sha256(digest, "sha256")?;
    }
    Ok(())
}
