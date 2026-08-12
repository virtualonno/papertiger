//! Deterministic, field-aware task retrieval without a second index authority.

use std::collections::HashSet;

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{
    ActivityEvent, Plan, TASK_COLS, TASK_STATUSES, Task, get_plan, resolve_plan, task_activity,
    task_from_row, task_tags_by_id,
};

const MAX_SEARCH_RESULTS: usize = 200;
const EXCERPT_CHARS: usize = 240;

#[derive(Debug, Clone, Serialize)]
pub struct SearchExcerpt {
    pub field: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub plan: String,
    pub task: Task,
    pub score: i64,
    pub matched_fields: Vec<String>,
    pub excerpt: SearchExcerpt,
    pub last_event: Option<ActivityEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub schema: String,
    pub query: String,
    pub terms: Vec<String>,
    pub plan: Option<Plan>,
    pub status: Option<String>,
    pub total_matches: usize,
    pub truncated: bool,
    pub results: Vec<SearchHit>,
}

#[derive(Debug)]
struct SearchField {
    name: &'static str,
    text: String,
    normalized: String,
    weight: i64,
    phrase_bonus: i64,
}

pub fn search_tasks(
    conn: &Connection,
    query: &str,
    plan: Option<&str>,
    status: Option<&str>,
    limit: usize,
) -> Result<SearchResponse> {
    let query = query.trim();
    let terms = analyze(query);
    if terms.is_empty() {
        bail!("search query must contain at least one letter or digit");
    }
    if !(1..=MAX_SEARCH_RESULTS).contains(&limit) {
        bail!("search --limit must be between 1 and {MAX_SEARCH_RESULTS}");
    }
    if let Some(status) = status
        && !TASK_STATUSES.contains(&status)
    {
        bail!(
            "unknown task status '{status}' (expected {})",
            TASK_STATUSES.join("|")
        );
    }
    let selected_plan = plan
        .map(|slug| {
            let (plan_id, _) = resolve_plan(conn, Some(slug))?;
            get_plan(conn, plan_id)
        })
        .transpose()?;
    let plan_id = selected_plan.as_ref().map(|plan| plan.plan_id);
    let mut statement = conn.prepare(&format!(
        "SELECT {TASK_COLS},
                (SELECT slug FROM plans WHERE plan_id=tasks.plan_id)
           FROM tasks
          WHERE (?1 IS NULL OR plan_id=?1)
            AND (?2 IS NULL OR status=?2)
          ORDER BY seq"
    ))?;
    let rows = statement
        .query_map(params![plan_id, status], |row| {
            Ok((task_from_row(row)?, row.get::<_, String>(11)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let phrase = terms.join(" ");
    let mut hits = Vec::new();
    for (task, plan_slug) in rows {
        let tags = task_tags_by_id(conn, task.task_id)?;
        let rationale = task_rationale(conn, task.seq)?;
        let fields = fields_for(&task, &tags, &rationale);
        if terms.iter().any(|term| {
            !fields
                .iter()
                .any(|field| contains_term(&field.normalized, term))
        }) {
            continue;
        }
        let mut score = 0;
        let mut matched_fields = Vec::new();
        for field in &fields {
            let term_matches = terms
                .iter()
                .filter(|term| contains_term(&field.normalized, term))
                .count();
            if term_matches == 0 {
                continue;
            }
            matched_fields.push(field.name.to_owned());
            score += field.weight * i64::try_from(term_matches)?;
            if field.normalized.contains(&phrase) {
                score += field.phrase_bonus;
            }
        }
        if normalize(&task.title) == phrase {
            score += 240;
        }
        if tags.iter().any(|tag| normalize(tag) == phrase) {
            score += 120;
        }
        let excerpt = select_excerpt(&fields, &terms, &phrase);
        let last_event = task_activity(conn, task.seq)?.last_event;
        hits.push(SearchHit {
            plan: plan_slug,
            task,
            score,
            matched_fields,
            excerpt,
            last_event,
        });
    }
    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| {
                right
                    .last_event
                    .as_ref()
                    .map(|event| event.event_id)
                    .cmp(&left.last_event.as_ref().map(|event| event.event_id))
            })
            .then_with(|| left.task.seq.cmp(&right.task.seq))
    });
    let total_matches = hits.len();
    hits.truncate(limit);
    Ok(SearchResponse {
        schema: "papertiger.search.v1".into(),
        query: query.to_owned(),
        terms,
        plan: selected_plan,
        status: status.map(str::to_owned),
        total_matches,
        truncated: total_matches > hits.len(),
        results: hits,
    })
}

fn task_rationale(conn: &Connection, seq: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT why
           FROM events
          WHERE entity_seq=?1
            AND entity IN ('task','dep','gate')
            AND why IS NOT NULL
          ORDER BY event_id DESC",
    )?;
    Ok(statement
        .query_map(params![seq], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn fields_for(task: &Task, tags: &[String], rationale: &[String]) -> Vec<SearchField> {
    [
        ("title", task.title.clone(), 24, 140),
        ("tags", tags.join(" "), 18, 90),
        ("intent", task.intent.clone(), 10, 55),
        ("result", task.result.clone().unwrap_or_default(), 9, 50),
        ("rationale", rationale.join("\n"), 5, 30),
    ]
    .into_iter()
    .map(|(name, text, weight, phrase_bonus)| SearchField {
        normalized: normalize(&text),
        name,
        text,
        weight,
        phrase_bonus,
    })
    .collect()
}

fn analyze(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    normalize(text)
        .split_whitespace()
        .filter_map(|term| {
            let term = term.to_owned();
            seen.insert(term.clone()).then_some(term)
        })
        .collect()
}

fn normalize(text: &str) -> String {
    let mut normalized = String::new();
    let mut separated = true;
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            normalized.push(character);
            separated = false;
        } else if !separated {
            normalized.push(' ');
            separated = true;
        }
    }
    normalized.trim_end().to_owned()
}

fn contains_term(normalized: &str, term: &str) -> bool {
    normalized.split_whitespace().any(|token| token == term)
}

fn select_excerpt(fields: &[SearchField], terms: &[String], phrase: &str) -> SearchExcerpt {
    let field = fields
        .iter()
        .find(|field| field.normalized.contains(phrase))
        .or_else(|| {
            fields.iter().find(|field| {
                terms
                    .iter()
                    .any(|term| contains_term(&field.normalized, term))
            })
        })
        .expect("a matched task has at least one matched field");
    let match_term = terms
        .iter()
        .find(|term| contains_term(&field.normalized, term))
        .map(String::as_str)
        .unwrap_or(phrase);
    SearchExcerpt {
        field: field.name.into(),
        text: bounded_excerpt(&field.text, match_term),
    }
}

fn bounded_excerpt(text: &str, match_term: &str) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.len() <= EXCERPT_CHARS {
        return text.to_owned();
    }
    let lowercase = text.to_lowercase();
    let match_character = lowercase
        .find(match_term)
        .map(|byte| lowercase[..byte].chars().count())
        .unwrap_or(0)
        .min(characters.len());
    let start = match_character.saturating_sub(EXCERPT_CHARS / 3);
    let end = (start + EXCERPT_CHARS).min(characters.len());
    let mut excerpt = characters[start..end].iter().collect::<String>();
    if start > 0 {
        excerpt.insert(0, '…');
    }
    if end < characters.len() {
        excerpt.push('…');
    }
    excerpt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_is_literal_unicode_and_deduplicated() {
        assert_eq!(
            analyze(" Object_store  café OBJECT-store "),
            vec!["object", "store", "café"]
        );
    }

    #[test]
    fn term_matching_does_not_expand_to_substrings() {
        assert!(contains_term("start activity", "start"));
        assert!(!contains_term("start activity", "art"));
    }

    #[test]
    fn excerpts_are_bounded_around_a_match() {
        let text = format!("{}needle{}", "a".repeat(300), "b".repeat(300));
        let excerpt = bounded_excerpt(&text, "needle");
        assert!(excerpt.contains("needle"));
        assert!(excerpt.starts_with('…'));
        assert!(excerpt.ends_with('…'));
        assert!(excerpt.chars().count() <= EXCERPT_CHARS + 2);
    }
}
