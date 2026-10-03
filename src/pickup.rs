//! Advisory pickup history. A pickup is neither a lock nor proof of liveness:
//! the owner process it records only lets a reader on the same host prove
//! that the owner has ended. Nothing is locked, reassigned or transitioned.

use anyhow::{Result, bail};
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{Task, now, record_event_in_mutation};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskPickup {
    pub session: Option<String>,
    pub at: String,
    /// The harness process that picked the task up, when the harness names
    /// it; absent for earlier pickups and harnesses without a process id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<PickupOwner>,
}

/// A process identified by host, PID and birth time, so PID reuse is told
/// apart from the original process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PickupOwner {
    pub host: String,
    pub pid: u32,
    pub process: String,
}

/// Whether a pickup's owner process still runs, observed now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerState {
    /// The same process still runs on this host.
    Running,
    /// Provably ended: on this host the PID is gone, has exited, or now
    /// names a newer process.
    Ended,
    /// Nothing can be proven: another host, no recorded process, or the
    /// process could not be inspected. Age never counts as proof.
    Unknown,
}

/// The name of this host, for comparing pickup owners.
pub fn host_name() -> Option<String> {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME")
            .ok()
            .filter(|name| !name.is_empty())
    }
    #[cfg(unix)]
    {
        let mut buffer = [0_u8; 256];
        // SAFETY: the buffer is writable for its full length, and the result
        // is read only up to its first NUL.
        let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
        if result != 0 {
            return None;
        }
        let end = buffer
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(buffer.len());
        String::from_utf8(buffer[..end].to_vec())
            .ok()
            .filter(|name| !name.is_empty())
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

/// Observe `pid` now and describe it as a pickup owner on this host.
pub fn observe_owner(pid: u32) -> Option<PickupOwner> {
    match crate::process_identity::observe_process(pid).ok()? {
        crate::process_identity::ProcessObservation::Active {
            process_birth_identity,
        } => Some(PickupOwner {
            host: host_name()?,
            pid,
            process: process_birth_identity,
        }),
        _ => None,
    }
}

/// Whether a pickup's owner still runs. Only an observation on the owner's
/// host can prove anything; everything else is `Unknown`.
pub fn owner_state(pickup: &TaskPickup) -> OwnerState {
    let Some(owner) = &pickup.owner else {
        return OwnerState::Unknown;
    };
    if host_name().as_deref() != Some(owner.host.as_str()) {
        return OwnerState::Unknown;
    }
    match crate::process_identity::observe_process(owner.pid) {
        Ok(crate::process_identity::ProcessObservation::Absent) => OwnerState::Ended,
        Ok(
            crate::process_identity::ProcessObservation::Exited {
                process_birth_identity,
            }
            | crate::process_identity::ProcessObservation::Active {
                process_birth_identity,
            },
        ) if process_birth_identity != owner.process => OwnerState::Ended,
        Ok(crate::process_identity::ProcessObservation::Exited { .. }) => OwnerState::Ended,
        Ok(crate::process_identity::ProcessObservation::Active { .. }) => OwnerState::Running,
        Err(_) => OwnerState::Unknown,
    }
}

// The nullable pair is populated only by pickup, never by guessing from an
// old event's actor. Old in-progress work retains unknown pickup context.
pub(crate) const SCHEMA: &str = r#"
ALTER TABLE tasks ADD COLUMN pickup_session TEXT;
ALTER TABLE tasks ADD COLUMN pickup_at TEXT
  CHECK ((pickup_session IS NULL AND pickup_at IS NULL) OR
         (pickup_at IS NOT NULL AND status = 'in_progress'));
"#;

pub fn validate_session(session: &str) -> Result<()> {
    if session.is_empty()
        || session.len() > 128
        || !session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        || !session.as_bytes()[0].is_ascii_alphanumeric()
    {
        bail!(
            "session identity must be 1..128 ASCII letters, digits, '.', '_', ':', or '-', starting with a letter or digit; supply --session <unique-session-id> or PAPERTIGER_SESSION"
        );
    }
    Ok(())
}

pub(crate) fn validate_pickup(status: &str, pickup: Option<&TaskPickup>) -> Result<()> {
    if let Some(pickup) = pickup {
        if let Some(session) = &pickup.session {
            validate_session(session)?;
        }
        if status != "in_progress" {
            bail!(
                "pickup context requires in_progress status; import a consistent Papertiger export"
            );
        }
        chrono::DateTime::parse_from_rfc3339(&pickup.at).map_err(|_| {
            anyhow::anyhow!(
                "pickup.at must be an RFC 3339 timestamp; import an intact Papertiger export"
            )
        })?;
    }
    Ok(())
}

/// Called under the same transaction as start/resume. Repeating start from the
/// same identified session is eventless; anonymous calls cannot prove identity.
pub(crate) fn record_pickup(
    tx: &Transaction<'_>,
    actor: &str,
    task: &Task,
    session: Option<&str>,
    why: Option<&str>,
) -> Result<()> {
    if let Some(session) = session {
        validate_session(session)?;
    }
    if session.is_some() && task.pickup.as_ref().and_then(|p| p.session.as_deref()) == session {
        return Ok(());
    }
    let pickup = TaskPickup {
        session: session.map(str::to_owned),
        at: now(),
        owner: crate::authorship::current_owner(tx)?,
    };
    tx.execute(
        "UPDATE tasks SET pickup_session=?1, pickup_at=?2, updated_at=?2,
                pickup_host=?4, pickup_pid=?5, pickup_process=?6
          WHERE task_id=?3",
        params![
            pickup.session,
            pickup.at,
            task.task_id,
            pickup.owner.as_ref().map(|owner| &owner.host),
            pickup.owner.as_ref().map(|owner| owner.pid),
            pickup.owner.as_ref().map(|owner| &owner.process),
        ],
    )?;
    record_event_in_mutation(
        tx,
        actor,
        "task",
        Some(task.task_id),
        "pickup",
        why,
        Some(&serde_json::json!({"seq": task.seq, "before": task.pickup, "after": pickup})),
    )?;
    Ok(())
}

pub(crate) fn readiness(task: &Task, session: Option<&str>, unblocked: bool) -> &'static str {
    match task.status.as_str() {
        "in_progress" => match task.pickup.as_ref().and_then(|p| p.session.as_deref()) {
            Some(last) if Some(last) == session => "mine",
            Some(_) => "picked_up_elsewhere",
            None => "in_progress",
        },
        "proposed" if unblocked => "ready",
        "proposed" => "blocked",
        _ => "unknown",
    }
}
