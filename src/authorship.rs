//! Who wrote a planning event: the harness and session it came from, the
//! model that authored it, and how that model was reported.
//!
//! Harnesses name their sessions in the environment of the commands their
//! agents run, so harness and session are derived. No harness exposes the
//! model: it comes from `--model` (the agent's own report) or
//! `PAPERTIGER_MODEL` (set where the session is launched), and every event
//! records which. A write without a model is refused. The reserved model
//! `human` records the operator's manual use and is refused inside a
//! harness, so an agent that inherits the operator's shell profile can never
//! be recorded as the operator. Authorship is provenance, never ownership.

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// The model recorded for the operator's own manual use.
pub const HUMAN_MODEL: &str = "human";
/// Reserved for transitions a runtime performs itself; no caller supplies it.
pub const RUNTIME_MODEL: &str = "runtime";

/// A harness that names its sessions in its agents' command environment.
pub struct Harness {
    pub name: &'static str,
    /// Actor label prefix, e.g. `claude` in `claude-e2affada`.
    pub prefix: &'static str,
    pub session_variable: &'static str,
    /// The harness process id, where the harness exposes it; a pickup
    /// records it so a later reader can prove the owner has ended.
    pub pid_variable: Option<&'static str>,
    /// Configured reasoning effort, where the harness exposes it.
    pub effort_variable: Option<&'static str>,
    /// Executable names of the harness process (`codex` also covers its
    /// `codex-*` helpers); they place it in a command's process ancestry.
    pub host_executables: &'static [&'static str],
}

impl Harness {
    fn hosts(&self, executable: &str) -> bool {
        self.host_executables.iter().any(|host| {
            executable == *host
                || executable
                    .strip_prefix(host)
                    .is_some_and(|rest| rest.starts_with('-'))
        })
    }
}

pub const HARNESSES: &[Harness] = &[
    Harness {
        name: "Claude Code",
        prefix: "claude",
        session_variable: "CLAUDE_CODE_SESSION_ID",
        pid_variable: Some("CLAUDE_PID"),
        effort_variable: Some("CLAUDE_EFFORT"),
        host_executables: &["claude"],
    },
    Harness {
        name: "Codex",
        prefix: "codex",
        session_variable: "CODEX_THREAD_ID",
        pid_variable: None,
        effort_variable: None,
        host_executables: &["codex"],
    },
    Harness {
        name: "pi",
        prefix: "pi",
        session_variable: "PI_SESSION_ID",
        pid_variable: None,
        effort_variable: None,
        // pi runs as a Node.js script.
        host_executables: &["pi", "node"],
    },
    Harness {
        name: "OpenCode",
        prefix: "opencode",
        session_variable: "OPENCODE_SESSION_ID",
        pid_variable: Some("OPENCODE_PID"),
        effort_variable: None,
        host_executables: &["opencode"],
    },
    Harness {
        name: "Cursor",
        prefix: "cursor",
        session_variable: "CURSOR_CONVERSATION_ID",
        pid_variable: None,
        effort_variable: None,
        host_executables: &["cursor"],
    },
];

/// How the model of an event was reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// `--model`: the authoring agent's own report.
    Flag,
    /// `PAPERTIGER_MODEL`: configured where the session was launched.
    Environment,
    /// A transition the tool performs itself, recorded as the `runtime` model.
    Runtime,
}

impl ModelSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Environment => "environment",
            Self::Runtime => "runtime",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "flag" => Some(Self::Flag),
            "environment" => Some(Self::Environment),
            "runtime" => Some(Self::Runtime),
            _ => None,
        }
    }
}

/// The resolved author of every event a command writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Authorship {
    pub actor: String,
    pub harness: Option<String>,
    pub session: Option<String>,
    pub model: String,
    pub model_source: ModelSource,
    pub reasoning_effort: Option<String>,
    /// The harness process, observed when its harness names it; pickups
    /// record it as their owner.
    pub owner: Option<crate::PickupOwner>,
}

impl Authorship {
    /// A transition the tool performs itself, such as setup provisioning its
    /// own plan. Only library callers construct it; the CLI refuses a
    /// caller-supplied `runtime` model.
    pub fn runtime(actor: &str) -> Self {
        Self {
            actor: actor.to_owned(),
            harness: None,
            session: None,
            model: RUNTIME_MODEL.to_owned(),
            model_source: ModelSource::Runtime,
            reasoning_effort: None,
            owner: None,
        }
    }
}

/// Explicit authorship inputs from command-line flags.
#[derive(Debug, Default, Clone, Copy)]
pub struct AuthorshipFlags<'a> {
    pub actor: Option<&'a str>,
    pub session: Option<&'a str>,
    pub model: Option<&'a str>,
    pub reasoning_effort: Option<&'a str>,
}

pub(crate) fn valid_identifier(value: &str, max_length: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
}

pub fn validate_model(model: &str) -> Result<()> {
    if !valid_identifier(model, 160) {
        bail!(
            "model must be a nonblank identifier of at most 160 ASCII letters, digits, '.', '_', ':', '/', or '-'; supply --model <model-id> or PAPERTIGER_MODEL"
        );
    }
    Ok(())
}

pub fn validate_reasoning_effort(effort: &str) -> Result<()> {
    if !valid_identifier(effort, 80) {
        bail!(
            "reasoning effort must be a nonblank identifier of at most 80 ASCII letters, digits, '.', '_', ':', '/', or '-'; supply --reasoning-effort <effort> or omit it"
        );
    }
    Ok(())
}

/// The harness running this command. A single visible harness session names
/// it. An agent launched inside another agent's shell also inherits the outer
/// agent's session variable, so when several are visible the nearest harness
/// host in the process ancestry decides; without one unique answer no harness
/// is recorded rather than the outer agent.
fn running_harness(
    env: &dyn Fn(&str) -> Option<String>,
    ancestry: &dyn Fn() -> Option<Vec<String>>,
) -> Option<&'static Harness> {
    let visible = HARNESSES
        .iter()
        .filter(|harness| {
            env(harness.session_variable).is_some_and(|value| !value.trim().is_empty())
        })
        .collect::<Vec<_>>();
    match visible.as_slice() {
        [] => None,
        [only] => Some(only),
        _ => {
            for executable in ancestry()? {
                let claims = visible
                    .iter()
                    .filter(|harness| harness.hosts(&executable))
                    .collect::<Vec<_>>();
                match claims.as_slice() {
                    [] => {}
                    [harness] => return Some(harness),
                    _ => return None,
                }
            }
            None
        }
    }
}

/// Resolve authorship from flags and an environment lookup. Explicit flags
/// win, then `PAPERTIGER_*` variables, then what the harness provides. With
/// several harness sessions visible and no process ancestry to place them,
/// no harness is recorded.
pub fn resolve_authorship(
    flags: AuthorshipFlags<'_>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Authorship> {
    resolve_authorship_within(flags, env, &|| None)
}

fn resolve_authorship_within(
    flags: AuthorshipFlags<'_>,
    env: &dyn Fn(&str) -> Option<String>,
    ancestry: &dyn Fn() -> Option<Vec<String>>,
) -> Result<Authorship> {
    let nonblank = |name: &str| env(name).filter(|value| !value.trim().is_empty());
    // Any visible harness session means an agent runs this command.
    let visible = HARNESSES
        .iter()
        .find(|harness| nonblank(harness.session_variable).is_some());
    let harness = running_harness(env, ancestry);
    let (model, model_source) = match (flags.model, nonblank("PAPERTIGER_MODEL")) {
        (Some(model), _) => (model.to_owned(), ModelSource::Flag),
        (None, Some(model)) => (model, ModelSource::Environment),
        (None, None) => bail!(
            "every write records the model that authored it; pass --model <model-id> (your own model) on this command, or set PAPERTIGER_MODEL in its environment{}",
            if visible.is_some() {
                ""
            } else {
                "; for manual operator use set PAPERTIGER_MODEL=human"
            }
        ),
    };
    let model = model.trim().to_ascii_lowercase();
    validate_model(&model)?;
    if model == RUNTIME_MODEL {
        bail!(
            "the model '{RUNTIME_MODEL}' is reserved for transitions a runtime performs itself; pass the model that authored this write"
        );
    }
    if model == HUMAN_MODEL
        && let Some(harness) = visible
    {
        bail!(
            "the model '{HUMAN_MODEL}' records the operator's manual use, but this command runs inside {} ({} is set); pass the agent's own model with --model <model-id>",
            harness.name,
            harness.session_variable
        );
    }
    let session = match flags.session {
        Some(session) => Some(session.to_owned()),
        None => nonblank("PAPERTIGER_SESSION")
            .or_else(|| harness.and_then(|harness| nonblank(harness.session_variable))),
    };
    if let Some(session) = &session {
        crate::validate_session(session)?;
    }
    let reasoning_effort = match flags.reasoning_effort {
        Some(effort) => Some(effort.to_owned()),
        None => nonblank("PAPERTIGER_REASONING_EFFORT").or_else(|| {
            harness
                .and_then(|harness| harness.effort_variable)
                .and_then(nonblank)
        }),
    };
    if let Some(effort) = &reasoning_effort {
        validate_reasoning_effort(effort)?;
    }
    let actor = match flags.actor {
        Some(actor) => actor.to_owned(),
        None => match (nonblank("PAPERTIGER_ACTOR"), harness, &session) {
            (Some(actor), _, _) => actor,
            (None, Some(harness), Some(session)) => {
                let short: String = session.chars().take(8).collect();
                format!("{}-{short}", harness.prefix)
            }
            _ if model == HUMAN_MODEL => "operator".to_owned(),
            _ => model.clone(),
        },
    };
    if actor.trim().is_empty() {
        bail!("--actor must be nonblank when given");
    }
    Ok(Authorship {
        actor,
        harness: harness.map(|harness| harness.name.to_owned()),
        session,
        model,
        model_source,
        reasoning_effort,
        owner: None,
    })
}

/// The session of a read: an explicit flag, then `PAPERTIGER_SESSION`, then
/// the running harness's session, so focus recognizes this caller's own
/// pickups.
pub fn resolve_session(
    flag: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
    ancestry: &dyn Fn() -> Option<Vec<String>>,
) -> Result<Option<String>> {
    let nonblank = |name: &str| env(name).filter(|value| !value.trim().is_empty());
    let session = match flag {
        Some(session) => Some(session.to_owned()),
        None => nonblank("PAPERTIGER_SESSION").or_else(|| {
            running_harness(env, ancestry).and_then(|harness| nonblank(harness.session_variable))
        }),
    };
    if let Some(session) = &session {
        crate::validate_session(session)?;
    }
    Ok(session)
}

pub fn resolve_session_from_env(flag: Option<&str>) -> Result<Option<String>> {
    resolve_session(
        flag,
        &|name| std::env::var(name).ok(),
        &crate::process_identity::ancestor_executables,
    )
}

/// Resolve authorship from flags, the process environment and, when several
/// harness sessions are visible, the process ancestry.
pub fn resolve_authorship_from_env(flags: AuthorshipFlags<'_>) -> Result<Authorship> {
    let mut authorship = resolve_authorship_within(
        flags,
        &|name| std::env::var(name).ok(),
        &crate::process_identity::ancestor_executables,
    )?;
    // The harness process outlives each command, so a pickup can name it.
    authorship.owner = HARNESSES
        .iter()
        .filter(|harness| Some(harness.name) == authorship.harness.as_deref())
        .find_map(|harness| harness.pid_variable)
        .and_then(|variable| std::env::var(variable).ok())
        .and_then(|pid| pid.trim().parse::<u32>().ok())
        .and_then(crate::observe_owner);
    Ok(authorship)
}

/// Attribute every subsequent event written on this connection.
pub fn set_authorship(conn: &Connection, authorship: &Authorship) -> Result<()> {
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS papertiger_authorship(
           harness TEXT, session TEXT, model TEXT NOT NULL, model_source TEXT NOT NULL,
           reasoning_effort TEXT, owner_host TEXT, owner_pid INTEGER, owner_process TEXT);
         DELETE FROM temp.papertiger_authorship;",
    )?;
    conn.execute(
        "INSERT INTO temp.papertiger_authorship VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            authorship.harness,
            authorship.session,
            authorship.model,
            authorship.model_source.as_str(),
            authorship.reasoning_effort,
            authorship.owner.as_ref().map(|owner| &owner.host),
            authorship.owner.as_ref().map(|owner| owner.pid),
            authorship.owner.as_ref().map(|owner| &owner.process),
        ],
    )?;
    Ok(())
}

/// The owner process of the connection's authorship, if it was observed.
pub(crate) fn current_owner(conn: &Connection) -> Result<Option<crate::PickupOwner>> {
    let installed = conn
        .query_row(
            "SELECT 1 FROM sqlite_temp_master WHERE type='table' AND name='papertiger_authorship'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !installed {
        return Ok(None);
    }
    let owner = conn
        .query_row(
            "SELECT owner_host, owner_pid, owner_process FROM temp.papertiger_authorship",
            [],
            |row| {
                Ok(match (row.get(0)?, row.get(1)?, row.get(2)?) {
                    (Some(host), Some(pid), Some(process)) => {
                        Some(crate::PickupOwner { host, pid, process })
                    }
                    _ => None,
                })
            },
        )
        .optional()?;
    Ok(owner.flatten())
}

/// The authorship fields an event payload records.
pub fn current_authorship(conn: &Connection) -> Result<serde_json::Map<String, serde_json::Value>> {
    let installed = conn
        .query_row(
            "SELECT 1 FROM sqlite_temp_master WHERE type='table' AND name='papertiger_authorship'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let row = if installed {
        conn.query_row(
            "SELECT harness, session, model, model_source, reasoning_effort FROM temp.papertiger_authorship",
            [],
            |row| {
                Ok(PayloadAuthorship {
                    harness: row.get(0)?,
                    session: row.get(1)?,
                    model: row.get(2)?,
                    model_source: row.get(3)?,
                    reasoning_effort: row.get(4)?,
                })
            },
        )
        .optional()?
    } else {
        None
    };
    let Some(PayloadAuthorship {
        model: Some(model),
        model_source: Some(model_source),
        harness,
        session,
        reasoning_effort,
    }) = row
    else {
        bail!(
            "writes record their author; resolve one with `resolve_authorship` and install it with `set_authorship` before mutating (CLI: pass --model <model-id> or set PAPERTIGER_MODEL)"
        );
    };
    let mut fields = serde_json::Map::new();
    fields.insert("model".into(), model.into());
    fields.insert("model_source".into(), model_source.into());
    for (key, value) in [
        ("harness", harness),
        ("session", session),
        ("reasoning_effort", reasoning_effort),
    ] {
        if let Some(value) = value {
            fields.insert(key.into(), value.into());
        }
    }
    Ok(fields)
}

/// A stored payload's string field, refusing any other JSON type.
fn payload_text<'p>(payload: Option<&'p serde_json::Value>, key: &str) -> Result<Option<&'p str>> {
    match payload.and_then(|value| value.get(key)) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value)),
        Some(_) => bail!(
            "event {key} must be a string or null; run `papertiger audit` and restore a verified export"
        ),
    }
}

/// The stored authorship of one event, validated. Events recorded before
/// authorship was required carry none of it, or only a model.
#[derive(Debug, Clone, Default)]
pub(crate) struct PayloadAuthorship {
    pub model: Option<String>,
    pub model_source: Option<String>,
    pub harness: Option<String>,
    pub session: Option<String>,
    pub reasoning_effort: Option<String>,
}

pub(crate) fn payload_authorship(payload: Option<&serde_json::Value>) -> Result<PayloadAuthorship> {
    validate_payload_authorship(payload)?;
    let text = |key| payload_text(payload, key).map(|value| value.map(str::to_owned));
    Ok(PayloadAuthorship {
        model: text("model")?,
        model_source: text("model_source")?,
        harness: text("harness")?,
        session: text("session")?,
        reasoning_effort: text("reasoning_effort")?,
    })
}

/// Validate the authorship fields a stored payload carries. Events recorded
/// before authorship was required carry none of them, or only a model.
pub(crate) fn validate_payload_authorship(payload: Option<&serde_json::Value>) -> Result<()> {
    let text = |key| payload_text(payload, key);
    if let Some(model) = text("model")? {
        validate_model(model)?;
    }
    if let Some(source) = text("model_source")?
        && ModelSource::parse(source).is_none()
    {
        bail!(
            "event model_source '{source}' is not flag, environment or runtime; run `papertiger audit` and restore a verified export"
        );
    }
    if let Some(effort) = text("reasoning_effort")? {
        validate_reasoning_effort(effort)?;
        if text("model")?.is_none() {
            bail!(
                "event reasoning effort requires a model identifier; run `papertiger audit` and restore a verified export"
            );
        }
    }
    if let Some(session) = text("session")? {
        crate::validate_session(session)?;
    }
    if let Some(harness) = text("harness")?
        && !HARNESSES.iter().any(|known| known.name == harness)
    {
        bail!(
            "event harness '{harness}' is not a known harness; run `papertiger audit` and restore a verified export"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn resolve(flags: AuthorshipFlags<'_>, env: &[(&str, &str)]) -> Result<Authorship> {
        let env: HashMap<String, String> = env
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        resolve_authorship(flags, &|name| env.get(name).cloned())
    }

    #[test]
    fn a_harness_supplies_session_actor_and_effort_but_never_the_model() {
        let claude = [
            ("CLAUDE_CODE_SESSION_ID", "e2affada-00fb-43a4"),
            ("CLAUDE_EFFORT", "medium"),
        ];
        let error = resolve(AuthorshipFlags::default(), &claude)
            .unwrap_err()
            .to_string();
        assert!(error.contains("pass --model <model-id>"), "{error}");
        assert!(!error.contains("PAPERTIGER_MODEL=human"), "{error}");
        let authorship = resolve(
            AuthorshipFlags {
                model: Some("Claude-Opus-5-5"),
                ..Default::default()
            },
            &claude,
        )
        .unwrap();
        assert_eq!(
            authorship,
            Authorship {
                actor: "claude-e2affada".into(),
                harness: Some("Claude Code".into()),
                session: Some("e2affada-00fb-43a4".into()),
                model: "claude-opus-5-5".into(),
                model_source: ModelSource::Flag,
                reasoning_effort: Some("medium".into()),
                owner: None,
            }
        );
    }

    #[test]
    fn human_is_refused_inside_a_harness_and_runtime_is_never_supplied() {
        let error = resolve(
            AuthorshipFlags::default(),
            &[("PAPERTIGER_MODEL", "human"), ("CODEX_THREAD_ID", "t1")],
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("runs inside Codex (CODEX_THREAD_ID is set)"),
            "{error}"
        );
        let operator =
            resolve(AuthorshipFlags::default(), &[("PAPERTIGER_MODEL", "human")]).unwrap();
        assert_eq!(operator.actor, "operator");
        assert_eq!(operator.model_source, ModelSource::Environment);
        assert!(operator.harness.is_none());
        let error = resolve(
            AuthorshipFlags {
                model: Some("runtime"),
                ..Default::default()
            },
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("reserved"), "{error}");
    }

    #[test]
    fn explicit_inputs_win_and_an_unharnessed_agent_is_named_by_its_model() {
        let authorship = resolve(
            AuthorshipFlags::default(),
            &[("PAPERTIGER_MODEL", "deepseek-v4.1-flash")],
        )
        .unwrap();
        assert_eq!(authorship.actor, "deepseek-v4.1-flash");
        let authorship = resolve(
            AuthorshipFlags {
                actor: Some("fleet-7"),
                session: Some("batch-1"),
                ..Default::default()
            },
            &[
                ("PAPERTIGER_MODEL", "deepseek-v4.1-flash"),
                ("PI_SESSION_ID", "0190"),
                ("PAPERTIGER_REASONING_EFFORT", "low"),
            ],
        )
        .unwrap();
        assert_eq!(authorship.actor, "fleet-7");
        assert_eq!(authorship.session.as_deref(), Some("batch-1"));
        assert_eq!(authorship.harness.as_deref(), Some("pi"));
        assert_eq!(authorship.reasoning_effort.as_deref(), Some("low"));
    }

    #[test]
    fn a_nested_agent_is_attributed_to_the_nearest_harness_in_its_ancestry() {
        let env: HashMap<String, String> = [
            ("PAPERTIGER_MODEL", "gpt-6.1-sol"),
            ("CLAUDE_CODE_SESSION_ID", "40fa0f64-outer"),
            ("PI_SESSION_ID", "0199-inner"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        let env = |name: &str| env.get(name).cloned();
        let within = |ancestry: Option<&[&str]>| {
            let ancestry =
                ancestry.map(|names| names.iter().map(|name| (*name).to_owned()).collect());
            resolve_authorship_within(AuthorshipFlags::default(), &env, &|| ancestry.clone())
                .unwrap()
        };
        // pi launched from a Claude Code shell runs papertiger from its own shell.
        let inner = within(Some(&["bash", "node", "bash", "claude"]));
        assert_eq!(inner.harness.as_deref(), Some("pi"));
        assert_eq!(inner.session.as_deref(), Some("0199-inner"));
        assert_eq!(inner.actor, "pi-0199-inn");
        // The reverse nesting is attributed to Claude Code.
        let reverse = within(Some(&["bash", "claude", "bash", "node"]));
        assert_eq!(reverse.harness.as_deref(), Some("Claude Code"));
        assert_eq!(reverse.session.as_deref(), Some("40fa0f64-outer"));
        // Without an ancestry that places one of them, neither is recorded.
        for ancestry in [None, Some(&["bash", "explorer"][..])] {
            let unplaced = within(ancestry);
            assert_eq!(unplaced.harness, None);
            assert_eq!(unplaced.session, None);
            assert_eq!(unplaced.actor, "gpt-6.1-sol");
        }
        assert_eq!(
            resolve_session(None, &env, &|| Some(vec!["node".into(), "claude".into()]))
                .unwrap()
                .as_deref(),
            Some("0199-inner")
        );
        // A harness's helper processes place it too.
        assert!(HARNESSES[1].hosts("codex-command-runner"));
        assert!(!HARNESSES[1].hosts("codexify"));
    }
}
