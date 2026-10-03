//! Opening the planning authority: connection policy, identity, write guards and the mutation transaction.

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{
    Connection, ErrorCode, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use std::path::Path;

use crate::{
    AUTHORITY_IDENTITY, AUTHORITY_IDENTITY_KEY, GuardDrift, SCHEMA_VERSION, SQLITE_LOCK_GRACE,
    history_recovery, write_guard,
};

pub(crate) fn configure_connection(conn: Connection) -> Result<Connection> {
    conn.busy_timeout(SQLITE_LOCK_GRACE)
        .context("configure papertiger SQLite lock grace")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// Replace any SQLite lock error in an anyhow context chain with Papertiger's
/// single operator-facing retry refusal. SQLite performs the bounded wait;
/// Papertiger never replays the command.
pub fn normalize_sqlite_lock_error(error: anyhow::Error) -> anyhow::Error {
    let locked = error.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code)
            .is_some_and(|code| matches!(code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked))
    });
    if locked {
        anyhow!(
            "papertiger SQLite lock admission refused after a {}ms grace: the authority is still locked; retry the command after the current database operation finishes",
            SQLITE_LOCK_GRACE.as_millis()
        )
    } else {
        error
    }
}

pub fn open_for_init(path: &str) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("open {path} for initialization"))?;
    configure_connection(conn)
}

pub fn open_existing(path: &str) -> Result<Connection> {
    require_existing_authority_path(path)?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("open existing papertiger database {path}"))?;
    let conn = configure_connection(conn)?;
    validate_existing(&conn, path, GuardPolicy::Enforce)?;
    Ok(conn)
}

/// Open a writable authority whose write guards or canonical history view may
/// have drifted, so `repair-guards` can reinstall them. Identity and schema
/// version checks still apply; history validation waits for the repaired view.
pub fn open_existing_for_guard_repair(path: &str) -> Result<Connection> {
    require_existing_authority_path(path)?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("open existing papertiger database {path}"))?;
    let conn = configure_connection(conn)?;
    validate_existing(&conn, path, GuardPolicy::Repair)?;
    Ok(conn)
}

/// Open an initialized Papertiger authority without granting mutation access.
/// Evidence consumers such as papertiger-mise use this to verify an exact
/// promotion gate while preserving Papertiger's independent ownership.
pub fn open_existing_read_only(path: &str) -> Result<Connection> {
    require_existing_authority_path(path)?;
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open existing papertiger database read-only {path}"))?;
    let conn = configure_connection(conn)?;
    // Guard drift blocks writes, never reads: audit and the CLI report it.
    validate_existing(&conn, path, GuardPolicy::Report)?;
    Ok(conn)
}

/// Guards that are missing or altered, plus foreign triggers; empty for an
/// intact authority.
pub fn write_guard_drift(conn: &Connection) -> Result<Vec<GuardDrift>> {
    write_guard::drift(conn)
}

/// Refuse an authority whose write guards drifted, for callers such as
/// promotion checks that must not rely on a read-only open alone.
pub fn verify_write_guards(conn: &Connection) -> Result<()> {
    write_guard::verify(conn)
}

/// Reinstall canonical write guards and drop foreign triggers with an audited
/// `repair_write_guards` event.
/// Returns the repaired drift; an intact authority records nothing.
pub fn repair_write_guards(conn: &Connection, actor: &str, why: &str) -> Result<Vec<GuardDrift>> {
    write_guard::repair(conn, actor, why)
}

fn require_existing_authority_path(path: &str) -> Result<()> {
    if !Path::new(path).exists() {
        bail!(
            "no Papertiger authority exists at {path}; for a genuinely new authority, rerun the same executable with `init` and the same authority selectors; if prior work existed, locate the original authority, or restore it from an export by running `init` and then `import <dump>` with the same selectors"
        );
    }
    Ok(())
}

pub(crate) fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1",
            params![name],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false))
}

pub(crate) fn authority_identity(conn: &Connection) -> Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM meta WHERE key=?1",
        params![AUTHORITY_IDENTITY_KEY],
        |row| row.get(0),
    )
    .optional()
    .context("read Papertiger authority identity from meta.authority")
}

pub(crate) fn require_planner_identity(
    conn: &Connection,
    path: &str,
    allow_legacy: bool,
) -> Result<()> {
    if let Some(identity) = authority_identity(conn)? {
        if identity == AUTHORITY_IDENTITY {
            return Ok(());
        }
        if identity == "papertiger.mise" {
            bail!(
                "{path} is a papertiger-mise authority, not a Papertiger planning authority; use `papertiger-mise --db {path} status` or select the planning database with `papertiger --db <planner-authority>`"
            );
        }
        bail!(
            "{path} has foreign authority identity {identity:?}; select the Papertiger planning database with `papertiger --db <planner-authority>`"
        );
    }
    if has_table(conn, "campaigns")? {
        bail!(
            "{path} is a legacy papertiger-mise authority without typed identity; migrate it only with `papertiger-mise --db {path} init`"
        );
    }
    let planner_shape =
        has_table(conn, "plans")? && has_table(conn, "tasks")? && has_table(conn, "events")?;
    if allow_legacy && planner_shape {
        return Ok(());
    }
    if planner_shape {
        bail!(
            "{path} is a legacy Papertiger planning authority without typed identity; run `papertiger --db {path} init` explicitly to migrate it to schema v{SCHEMA_VERSION}"
        );
    }
    bail!(
        "{path} has Papertiger-style metadata but no recognized authority identity; restore a verified export or select the planning database with `papertiger --db <planner-authority>`"
    )
}

/// How an open treats drift in the tool's own schema boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GuardPolicy {
    /// Writable use: every guard and the canonical history view must be intact.
    Enforce,
    /// Reads: trigger drift is reported, but an altered history view refuses.
    Report,
    /// `repair-guards`: skip both so the boundary itself can be reinstalled.
    Repair,
}

fn validate_existing(conn: &Connection, path: &str, guards: GuardPolicy) -> Result<()> {
    let has_meta: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta'",
            [],
            |_| Ok(true),
        )
        .optional()
        .with_context(|| {
            format!(
                "{path} is not a readable SQLite database; select a Papertiger authority or create one with `papertiger --db <new-path> init`"
            )
        })?
        .unwrap_or(false);
    if !has_meta {
        let object_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .with_context(|| {
                format!(
                    "{path} is not a readable SQLite database; select a Papertiger authority or create one with `papertiger --db <new-path> init`"
                )
            })?;
        if object_count > 0 {
            bail!(
                "{path} is a SQLite database with {object_count} non-Papertiger object(s) and no Papertiger authority metadata; select the planning database with `papertiger --db <planner-authority>`"
            );
        }
        bail!(
            "{path} is an empty SQLite database without a Papertiger authority; run `papertiger --db {path} init` to initialize it"
        );
    }
    require_planner_identity(conn, path, false)?;
    let version = schema_version(conn, path)?;
    if version != SCHEMA_VERSION {
        bail!(
            "{path} uses papertiger schema v{version}; run `papertiger --db {path} init` explicitly to upgrade to v{SCHEMA_VERSION}"
        );
    }
    if guards == GuardPolicy::Enforce {
        write_guard::verify(conn)?;
    }
    if guards != GuardPolicy::Repair {
        history_recovery::validate(conn)?;
    }
    Ok(())
}

pub(crate) fn schema_version(conn: &Connection, path: &str) -> Result<i64> {
    let raw: String = conn
        .query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )
        .with_context(|| {
            format!(
                "{path} has no Papertiger meta.schema_version; restore a verified export or select the correct planning authority"
            )
        })?;
    raw.parse().map_err(|_| {
        anyhow!(
            "{path} has corrupt Papertiger meta.schema_version {raw:?}; restore a verified export or select the correct planning authority"
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitOutcome {
    Created,
    Migrated { from: i64, to: i64 },
    Current,
}

/// Reserve SQLite's writer slot within the connection-wide lock grace. The
/// command itself is never replayed. Dropping the returned transaction rolls
/// the logical mutation back and releases the reservation.
pub fn begin_mutation(conn: &Connection) -> Result<Transaction<'_>> {
    write_guard::admit(conn)?;
    Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .context("begin papertiger mutation")
}
