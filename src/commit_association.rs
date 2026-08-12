use super::{
    begin_mutation, get_task, now, record_event_in_mutation, require_nonblank, validate_commit_oid,
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitAssociation {
    pub repository: String,
    pub commit_oid: String,
    pub note: Option<String>,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitAssociationMatch {
    pub task_seq: i64,
    pub task_title: String,
    #[serde(flatten)]
    pub commit: CommitAssociation,
}

pub fn add_commit_association(
    conn: &Connection,
    actor: &str,
    seq: i64,
    repository: &str,
    commit_oid: &str,
    note: Option<&str>,
) -> Result<CommitAssociation> {
    let repository = require_nonblank("commit repository label", repository)?;
    validate_commit_oid(commit_oid)?;
    let note = note.map(str::trim).filter(|value| !value.is_empty());
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let recorded_at = now();
    match tx.execute(
        "INSERT INTO commit_associations (task_id, repository, commit_oid, note, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![task.task_id, repository, commit_oid, note, recorded_at],
    ) {
        Ok(_) => {}
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
        {
            bail!(
                "commit {commit_oid} in repository '{repository}' is already recorded on task #{seq}; inspect it with `papertiger commit list {seq}`"
            );
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("record commit {commit_oid} in repository '{repository}' on task #{seq}")
            });
        }
    }
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "commit_association_add",
        note,
        Some(&serde_json::json!({
            "seq": seq,
            "repository": repository,
            "commit_oid": commit_oid,
        })),
    )?;
    tx.commit()?;
    Ok(CommitAssociation {
        repository: repository.to_owned(),
        commit_oid: commit_oid.to_owned(),
        note: note.map(str::to_owned),
        recorded_at,
    })
}

pub fn remove_commit_association(
    conn: &Connection,
    actor: &str,
    seq: i64,
    repository: &str,
    commit_oid: &str,
    why: &str,
) -> Result<()> {
    let repository = require_nonblank("commit repository label", repository)?;
    validate_commit_oid(commit_oid)?;
    let why = require_nonblank("commit removal reason", why)?;
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let removed = tx.execute(
        "DELETE FROM commit_associations WHERE task_id=?1 AND repository=?2 AND commit_oid=?3",
        params![task.task_id, repository, commit_oid],
    )?;
    if removed == 0 {
        bail!(
            "task #{seq} has no commit {commit_oid} in repository '{repository}'; inspect it with `papertiger commit list {seq}`"
        );
    }
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "commit_association_remove",
        Some(why),
        Some(&serde_json::json!({
            "seq": seq,
            "repository": repository,
            "commit_oid": commit_oid,
        })),
    )?;
    tx.commit()?;
    Ok(())
}

pub fn commit_associations(conn: &Connection, seq: i64) -> Result<Vec<CommitAssociation>> {
    let task = get_task(conn, seq)?;
    let mut statement = conn.prepare(
        "SELECT repository, commit_oid, note, recorded_at
           FROM commit_associations WHERE task_id=?1 ORDER BY commit_association_id",
    )?;
    Ok(statement
        .query_map(params![task.task_id], |row| {
            Ok(CommitAssociation {
                repository: row.get(0)?,
                commit_oid: row.get(1)?,
                note: row.get(2)?,
                recorded_at: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn find_commit_associations(
    conn: &Connection,
    commit_oid: &str,
    repository: Option<&str>,
) -> Result<Vec<CommitAssociationMatch>> {
    validate_commit_oid(commit_oid)?;
    let repository = repository
        .map(|repository| require_nonblank("commit repository label", repository))
        .transpose()?;
    let (sql, values): (&str, Vec<rusqlite::types::Value>) = match repository {
        Some(repository) => (
            "SELECT t.seq, t.title, c.repository, c.commit_oid, c.note, c.recorded_at
               FROM commit_associations c JOIN tasks t ON t.task_id=c.task_id
              WHERE c.commit_oid=?1 AND c.repository=?2 ORDER BY t.seq",
            vec![commit_oid.to_owned().into(), repository.to_owned().into()],
        ),
        None => (
            "SELECT t.seq, t.title, c.repository, c.commit_oid, c.note, c.recorded_at
               FROM commit_associations c JOIN tasks t ON t.task_id=c.task_id
              WHERE c.commit_oid=?1 ORDER BY c.repository, t.seq",
            vec![commit_oid.to_owned().into()],
        ),
    };
    let mut statement = conn.prepare(sql)?;
    Ok(statement
        .query_map(rusqlite::params_from_iter(values), |row| {
            Ok(CommitAssociationMatch {
                task_seq: row.get(0)?,
                task_title: row.get(1)?,
                commit: CommitAssociation {
                    repository: row.get(2)?,
                    commit_oid: row.get(3)?,
                    note: row.get(4)?,
                    recorded_at: row.get(5)?,
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
