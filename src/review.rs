//! Review state of the two claims a task carries: its filing (title and
//! intent, made when the task is created) and its result (made when it is
//! completed). State is derived from the event log and never stored, so no
//! one has to move a task through a review status.
//!
//! A filing is `unreviewed` until a different author engages with it:
//! starting the task or rewriting its title or intent makes it `engaged`, and
//! a review records `confirmed` or `disputed`. A dispute stands until a later
//! confirmation or a rewrite by a different author. A result changes only
//! through reviews. Nothing here refuses work or ranks it lower, except that a
//! disputed result holds the tasks that build on it (see `focus`).

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::authorship::payload_authorship;
use crate::{Task, begin_mutation, get_task, record_event_in_mutation};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    /// Only the author has engaged with the claim.
    Unreviewed,
    /// A different author started or rewrote the task without a verdict.
    Engaged,
    Confirmed,
    Disputed,
}

impl ReviewState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unreviewed => "unreviewed",
            Self::Engaged => "engaged",
            Self::Confirmed => "confirmed",
            Self::Disputed => "disputed",
        }
    }

    /// Whether the claim still needs someone else's check.
    pub const fn needs_review(self) -> bool {
        matches!(self, Self::Unreviewed | Self::Disputed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimReview {
    pub state: ReviewState,
    /// Model that made the claim; absent for claims recorded before
    /// authorship was required.
    pub author_model: Option<String>,
    /// Model of the latest review or engagement, when there is one.
    pub reviewer_model: Option<String>,
    /// Whether that reviewer ran the same model as the author, compared by
    /// [`normalized_model`]; absent when either model is unknown.
    pub same_model: Option<bool>,
}

impl ClaimReview {
    /// A claim recorded before authorship existed, with no later review.
    pub fn is_legacy(&self) -> bool {
        self.author_model.is_none() && self.reviewer_model.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskReview {
    pub filing: ClaimReview,
    /// Present once the task is done.
    pub result: Option<ClaimReview>,
}

/// Which claim a review addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewTarget {
    Filing,
    Result,
}

impl ReviewTarget {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Filing => "filing",
            Self::Result => "result",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewVerdict {
    Confirmed,
    Disputed,
}

impl ReviewVerdict {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "confirmed" => Ok(Self::Confirmed),
            "disputed" => Ok(Self::Disputed),
            _ => bail!(
                "unknown review verdict '{value}'; use --verdict confirmed or --verdict disputed"
            ),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Disputed => "disputed",
        }
    }
}

/// Model ids that differ only by `.` or `-` between digits name the same
/// model (`claude-opus-5.5` and `claude-opus-5-5`); ids compare lowercased.
pub fn normalized_model(model: &str) -> String {
    let chars: Vec<char> = model.to_ascii_lowercase().chars().collect();
    chars
        .iter()
        .enumerate()
        .map(|(index, &c)| {
            let between_digits = index > 0
                && chars.get(index + 1).is_some_and(char::is_ascii_digit)
                && chars[index - 1].is_ascii_digit();
            if c == '.' && between_digits { '-' } else { c }
        })
        .collect()
}

/// Who made an event: compared by session when both are known, otherwise
/// by actor label.
#[derive(Debug, Clone)]
struct Author {
    actor: String,
    session: Option<String>,
    model: Option<String>,
}

impl Author {
    fn same_as(&self, other: &Self) -> bool {
        match (&self.session, &other.session) {
            (Some(left), Some(right)) => left == right,
            _ => self.actor == other.actor,
        }
    }
}

struct TaskEvent {
    author: Author,
    kind: String,
    payload: Option<serde_json::Value>,
}

fn task_events(conn: &Connection, seq: i64) -> Result<Vec<TaskEvent>> {
    let mut statement = conn.prepare(
        "SELECT actor, kind, payload FROM canonical_events
          WHERE entity='task' AND entity_seq=?1 ORDER BY event_id",
    )?;
    let rows = statement
        .query_map(params![seq], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(actor, kind, payload)| {
            let payload = payload
                .map(|raw| serde_json::from_str::<serde_json::Value>(&raw))
                .transpose()?;
            let authorship = payload_authorship(payload.as_ref())?;
            Ok(TaskEvent {
                author: Author {
                    actor,
                    session: authorship.session,
                    model: authorship.model,
                },
                kind,
                payload,
            })
        })
        .collect()
}

fn payload_str<'a>(payload: Option<&'a serde_json::Value>, key: &str) -> Option<&'a str> {
    payload?.get(key)?.as_str()
}

fn claim(author: &Author) -> ClaimReview {
    ClaimReview {
        state: ReviewState::Unreviewed,
        author_model: author.model.clone(),
        reviewer_model: None,
        same_model: None,
    }
}

fn note_reviewer(claim: &mut ClaimReview, reviewer: &Author) {
    claim.reviewer_model = reviewer.model.clone();
    claim.same_model = match (&claim.author_model, &reviewer.model) {
        (Some(author), Some(reviewer)) => {
            Some(normalized_model(author) == normalized_model(reviewer))
        }
        _ => None,
    };
}

fn rewrites_definition(payload: Option<&serde_json::Value>) -> bool {
    payload
        .and_then(|payload| payload.get("fields"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|fields| {
            fields
                .iter()
                .any(|field| matches!(field.as_str(), Some("title" | "intent")))
        })
}

/// The review state of a task's filing and, once done, its result.
pub fn task_review(conn: &Connection, task: &Task) -> Result<TaskReview> {
    let events = task_events(conn, task.seq)?;
    // A task imported without its history has no creation event: its filing
    // author is unknown, so any later engagement is someone else's.
    let filing_author = events
        .iter()
        .find(|event| event.kind == "create")
        .map(|event| event.author.clone());
    let mut filing = match &filing_author {
        Some(author) => claim(author),
        None => ClaimReview {
            state: ReviewState::Unreviewed,
            author_model: None,
            reviewer_model: None,
            same_model: None,
        },
    };
    let by_other = |author: &Author| {
        filing_author
            .as_ref()
            .is_none_or(|filing_author| !author.same_as(filing_author))
    };
    let mut result: Option<(Author, ClaimReview)> = None;
    for event in &events {
        let payload = event.payload.as_ref();
        match event.kind.as_str() {
            "status" => match payload_str(payload, "to") {
                Some("in_progress") if by_other(&event.author) => {
                    if filing.state == ReviewState::Unreviewed {
                        filing.state = ReviewState::Engaged;
                        note_reviewer(&mut filing, &event.author);
                    }
                }
                Some("done") => result = Some((event.author.clone(), claim(&event.author))),
                // A reopened task's result no longer stands.
                Some("proposed" | "in_progress") => result = None,
                _ => {}
            },
            "edit" if rewrites_definition(payload) && by_other(&event.author) => {
                filing.state = ReviewState::Engaged;
                note_reviewer(&mut filing, &event.author);
            }
            "review" => {
                let verdict = match payload_str(payload, "verdict") {
                    Some("confirmed") => ReviewState::Confirmed,
                    Some("disputed") => ReviewState::Disputed,
                    _ => continue,
                };
                let target = match payload_str(payload, "target") {
                    Some("filing") => &mut filing,
                    Some("result") => match result.as_mut() {
                        Some((_, claim)) => claim,
                        None => continue,
                    },
                    _ => continue,
                };
                target.state = verdict;
                note_reviewer(target, &event.author);
            }
            _ => {}
        }
    }
    Ok(TaskReview {
        filing,
        result: result
            .filter(|_| task.status == "done")
            .map(|(_, claim)| claim),
    })
}

/// Whether a task carries a claim that still needs someone else's check: an
/// open filing that is unreviewed or disputed, or a done result that is,
/// unless that result predates authorship.
pub fn needs_review(conn: &Connection, task: &Task) -> Result<bool> {
    let review = task_review(conn, task)?;
    Ok(match task.status.as_str() {
        "proposed" | "in_progress" => review.filing.state.needs_review(),
        "done" => review
            .result
            .is_some_and(|result| result.state.needs_review() && !result.is_legacy()),
        _ => false,
    })
}

/// Whether `task` is done and its result is currently disputed.
pub(crate) fn result_disputed(conn: &Connection, task: &Task) -> Result<bool> {
    if task.status != "done" {
        return Ok(false);
    }
    Ok(task_review(conn, task)?
        .result
        .is_some_and(|result| result.state == ReviewState::Disputed))
}

/// Record a review of a task's filing or result by a different author.
pub fn review_task(
    conn: &Connection,
    actor: &str,
    seq: i64,
    target: ReviewTarget,
    verdict: ReviewVerdict,
    why: &str,
) -> Result<()> {
    if why.trim().is_empty() {
        bail!("a review records what was verified; pass --why <what you checked>");
    }
    let tx = begin_mutation(conn)?;
    let task = get_task(&tx, seq)?;
    let events = task_events(&tx, seq)?;
    let claim_author = match target {
        ReviewTarget::Filing => events
            .iter()
            .find(|event| event.kind == "create")
            .map(|event| event.author.clone()),
        ReviewTarget::Result => {
            if task.status != "done" {
                bail!(
                    "{} is {}; only a done task has a result to review (review its filing without --result)",
                    task.handle,
                    task.status
                );
            }
            events
                .iter()
                .rev()
                .find(|event| {
                    event.kind == "status"
                        && payload_str(event.payload.as_ref(), "to") == Some("done")
                })
                .map(|event| event.author.clone())
        }
    };
    let reviewer = Author {
        actor: actor.to_owned(),
        session: crate::authorship::current_authorship(&tx)?
            .get("session")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        model: None,
    };
    // A claim imported without its history has no known author to compare.
    if let Some(claim_author) = &claim_author
        && reviewer.same_as(claim_author)
    {
        bail!(
            "{}'s {} was made by this author ({}); a review must come from a different session",
            task.handle,
            target.as_str(),
            claim_author
                .session
                .as_deref()
                .map(|session| format!("session {session}"))
                .unwrap_or_else(|| format!("actor {}", claim_author.actor))
        );
    }
    record_event_in_mutation(
        &tx,
        actor,
        "task",
        Some(task.task_id),
        "review",
        Some(why),
        Some(&serde_json::json!({
            "seq": seq,
            "target": target.as_str(),
            "verdict": verdict.as_str(),
        })),
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_differing_by_a_dot_or_dash_between_digits_are_one_model() {
        assert_eq!(normalized_model("Claude-Opus-5.5"), "claude-opus-5-5");
        assert_eq!(normalized_model("claude-opus-5-5"), "claude-opus-5-5");
        assert_eq!(
            normalized_model("deepseek-v4.1-flash"),
            "deepseek-v4-1-flash"
        );
        assert_eq!(normalized_model("gpt.6"), "gpt.6");
    }
}
