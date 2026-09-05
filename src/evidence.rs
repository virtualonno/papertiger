use std::{
    fs::File,
    path::{Component, Path},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::{get_task, portable_absolute, validate_sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClassification {
    Verified,
    Failed,
    Unsupported,
}

impl EvidenceClassification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Failed => "failed",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceBindingVerification {
    pub entity: String,
    pub task_seq: i64,
    pub task_status: String,
    pub name: String,
    pub locator: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    pub expected_sha256: Option<String>,
    pub status: String,
    pub classification: EvidenceClassification,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub corrective_commands: Vec<CorrectiveCommand>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorrectiveCommand {
    pub program: String,
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone)]
struct StoredBinding {
    entity: &'static str,
    task_seq: i64,
    name: String,
    locator: String,
    expected_sha256: Option<String>,
    task_status: String,
    task_kind: String,
    task_result: Option<String>,
    task_result_source: Option<String>,
    task_replacement_seq: Option<i64>,
}

pub(crate) fn verify_all_evidence(
    conn: &Connection,
    project_root: &Path,
    task_seq: Option<i64>,
) -> Result<(String, Vec<EvidenceBindingVerification>)> {
    let project_root = std::fs::canonicalize(project_root).with_context(|| {
        format!(
            "resolve evidence project root {}; pass --project-root with an existing directory",
            project_root.display()
        )
    })?;
    if !project_root.is_dir() {
        bail!(
            "evidence project root {} is not a directory; pass --project-root with the project directory",
            project_root.display()
        );
    }
    if let Some(seq) = task_seq {
        get_task(conn, seq)?;
    }

    let mut bindings = Vec::new();
    let mut statement = conn.prepare(
        "SELECT task.seq, gate.name, gate.evidence_locator, gate.evidence_sha256,
                task.status, task.kind, task.result, task.result_source,
                replacement.seq
           FROM gates gate
           JOIN tasks task ON task.task_id=gate.task_id
           LEFT JOIN tasks replacement ON replacement.task_id=task.replacement_task_id
          WHERE gate.status='closed' AND (?1 IS NULL OR task.seq=?1)
          ORDER BY task.seq, gate.gate_id",
    )?;
    bindings.extend(
        statement
            .query_map(params![task_seq], |row| {
                Ok(StoredBinding {
                    entity: "gate",
                    task_seq: row.get(0)?,
                    name: row.get(1)?,
                    locator: row.get(2)?,
                    expected_sha256: row.get(3)?,
                    task_status: row.get(4)?,
                    task_kind: row.get(5)?,
                    task_result: row.get(6)?,
                    task_result_source: row.get(7)?,
                    task_replacement_seq: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    let mut statement = conn.prepare(
        "SELECT task.seq, blocker.name, blocker.evidence_locator, blocker.evidence_sha256,
                task.status, task.kind, task.result, task.result_source,
                replacement.seq
           FROM task_blockers blocker
           JOIN tasks task ON task.task_id=blocker.task_id
           LEFT JOIN tasks replacement ON replacement.task_id=task.replacement_task_id
          WHERE blocker.status='resolved' AND (?1 IS NULL OR task.seq=?1)
          ORDER BY task.seq, blocker.blocker_id",
    )?;
    bindings.extend(
        statement
            .query_map(params![task_seq], |row| {
                Ok(StoredBinding {
                    entity: "blocker",
                    task_seq: row.get(0)?,
                    name: row.get(1)?,
                    locator: row.get(2)?,
                    expected_sha256: row.get(3)?,
                    task_status: row.get(4)?,
                    task_kind: row.get(5)?,
                    task_result: row.get(6)?,
                    task_result_source: row.get(7)?,
                    task_replacement_seq: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    bindings.sort_by(|left, right| {
        left.task_seq
            .cmp(&right.task_seq)
            .then_with(|| left.entity.cmp(right.entity))
            .then_with(|| left.name.cmp(&right.name))
    });

    let bindings = bindings
        .into_iter()
        .map(|binding| verify_binding(&project_root, binding))
        .collect::<Vec<_>>();
    Ok((portable_absolute(&project_root)?, bindings))
}

fn verify_binding(root: &Path, binding: StoredBinding) -> EvidenceBindingVerification {
    let mut result = EvidenceBindingVerification {
        entity: binding.entity.into(),
        task_seq: binding.task_seq,
        task_status: binding.task_status.clone(),
        name: binding.name.clone(),
        locator: binding.locator.clone(),
        scheme: None,
        expected_sha256: binding.expected_sha256.clone(),
        status: "unsupported_scheme".into(),
        classification: EvidenceClassification::Unsupported,
        resolved_path: None,
        actual_sha256: None,
        detail: None,
        corrective_commands: Vec::new(),
    };
    let Some((scheme, value)) = binding.locator.split_once(':') else {
        result.status = "invalid_locator".into();
        result.detail = Some("locator is not scheme:value".into());
        return add_corrective_commands(result, &binding);
    };
    result.scheme = Some(scheme.to_ascii_lowercase());
    if !scheme.eq_ignore_ascii_case("file") {
        result.detail = Some(format!(
            "scheme {scheme:?} has no local verifier; only file: bindings are resolved"
        ));
        return add_corrective_commands(result, &binding);
    }
    let relative = Path::new(value);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        result.status = "invalid_path".into();
        result.detail = Some(
            "file: evidence must name a nonblank project-relative path under --project-root".into(),
        );
        return add_corrective_commands(result, &binding);
    }
    let mut path = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(component) => path.push(component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                result.status = "path_escape".into();
                result.detail = Some(
                    "file: evidence cannot contain parent, root, or platform-prefix components"
                        .into(),
                );
                return add_corrective_commands(result, &binding);
            }
        }
    }
    result.resolved_path = portable_absolute(&path).ok();
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            result.status = "missing".into();
            result.detail = Some(format!("evidence file does not exist: {}", path.display()));
            return add_corrective_commands(result, &binding);
        }
        Err(error) => {
            result.status = "unreadable".into();
            result.detail = Some(format!("inspect evidence file {}: {error}", path.display()));
            return add_corrective_commands(result, &binding);
        }
    };
    if metadata.file_type().is_symlink() {
        result.status = "symlink".into();
        result.detail = Some("file: evidence must be a regular file, not a symlink".into());
        return add_corrective_commands(result, &binding);
    }
    if !metadata.is_file() {
        result.status = "not_regular_file".into();
        result.detail = Some("file: evidence must be a regular file".into());
        return add_corrective_commands(result, &binding);
    }
    let canonical = match std::fs::canonicalize(&path) {
        Ok(path) if path.starts_with(root) => path,
        Ok(path) => {
            result.status = "path_escape".into();
            result.detail = Some(format!(
                "resolved evidence path leaves the project root: {}",
                path.display()
            ));
            return add_corrective_commands(result, &binding);
        }
        Err(error) => {
            result.status = "unreadable".into();
            result.detail = Some(format!("resolve evidence file {}: {error}", path.display()));
            return add_corrective_commands(result, &binding);
        }
    };
    result.resolved_path = portable_absolute(&canonical).ok();
    let mut file = match File::open(&canonical) {
        Ok(file) => file,
        Err(error) => {
            result.status = "unreadable".into();
            result.detail = Some(format!(
                "open evidence file {}: {error}",
                canonical.display()
            ));
            return add_corrective_commands(result, &binding);
        }
    };
    let before = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            result.status = "unreadable".into();
            result.detail = Some(format!("inspect open evidence file: {error}"));
            return add_corrective_commands(result, &binding);
        }
    };
    let actual = match crate::digest::sha256_reader(&mut file) {
        Ok(actual) => actual,
        Err(error) => {
            result.status = "unreadable".into();
            result.detail = Some(format!("read evidence file: {error}"));
            return add_corrective_commands(result, &binding);
        }
    };
    let after = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            result.status = "unreadable".into();
            result.detail = Some(format!("reinspect open evidence file: {error}"));
            return add_corrective_commands(result, &binding);
        }
    };
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        result.status = "changed_during_read".into();
        result.detail =
            Some("evidence file metadata changed while its bytes were read; retry".into());
        return add_corrective_commands(result, &binding);
    }
    result.actual_sha256 = Some(actual.clone());
    let Some(expected) = binding.expected_sha256.as_deref() else {
        result.status = "unhashed".into();
        result.detail =
            Some("binding has no stored SHA-256; reopen and close it with --sha256".into());
        return add_corrective_commands(result, &binding);
    };
    if let Err(error) = validate_sha256(expected, "stored evidence SHA-256") {
        result.status = "invalid_digest".into();
        result.detail = Some(error.to_string());
        return add_corrective_commands(result, &binding);
    }
    if actual != expected {
        result.status = "digest_mismatch".into();
        result.detail = Some("file bytes do not match the stored SHA-256".into());
        return add_corrective_commands(result, &binding);
    }
    result.status = "verified".into();
    add_corrective_commands(result, &binding)
}

fn add_corrective_commands(
    mut result: EvidenceBindingVerification,
    binding: &StoredBinding,
) -> EvidenceBindingVerification {
    result.classification = match result.status.as_str() {
        "verified" => EvidenceClassification::Verified,
        "unsupported_scheme" => EvidenceClassification::Unsupported,
        _ => EvidenceClassification::Failed,
    };
    if result.status == "verified" || result.status == "unsupported_scheme" {
        return result;
    }
    let reason = "replace invalid evidence binding after papertiger evidence verify";
    let was_terminal = matches!(
        binding.task_status.as_str(),
        "done" | "retired" | "rejected"
    );
    if was_terminal {
        result.corrective_commands.push(command([
            "reopen".into(),
            binding.task_seq.to_string(),
            "--why".into(),
            reason.into(),
        ]));
    }
    result.corrective_commands.push(command([
        binding.entity.into(),
        "reopen".into(),
        binding.task_seq.to_string(),
        binding.name.clone(),
        "--why".into(),
        reason.into(),
    ]));
    let action = if binding.entity == "gate" {
        "close"
    } else {
        "resolve"
    };
    let replacement_locator = if matches!(
        result.status.as_str(),
        "invalid_locator" | "invalid_path" | "path_escape" | "not_regular_file"
    ) {
        "<project-relative-file-locator>".into()
    } else {
        binding.locator.clone()
    };
    result.corrective_commands.push(command([
        binding.entity.into(),
        action.into(),
        binding.task_seq.to_string(),
        binding.name.clone(),
        "--evidence".into(),
        replacement_locator,
        "--sha256".into(),
        result
            .actual_sha256
            .clone()
            .unwrap_or_else(|| "<lowercase-sha256-of-current-file>".into()),
    ]));
    if was_terminal {
        let arguments = match binding.task_status.as_str() {
            "retired" => {
                let mut arguments = vec!["retire".into(), binding.task_seq.to_string()];
                if let Some(replacement_seq) = binding.task_replacement_seq {
                    arguments.extend(["--into".into(), replacement_seq.to_string()]);
                }
                arguments.extend(["--why".into(), "restore prior disposition".into()]);
                arguments
            }
            "rejected" => vec![
                "reject".into(),
                binding.task_seq.to_string(),
                "--why".into(),
                "restore prior disposition".into(),
            ],
            _ => {
                let mut arguments = vec!["done".into(), binding.task_seq.to_string()];
                if let Some(task_result) = &binding.task_result {
                    arguments.extend(["--result".into(), task_result.clone()]);
                    if let Some(source) = &binding.task_result_source {
                        arguments.extend(["--result-source".into(), source.clone()]);
                    }
                } else if matches!(binding.task_kind.as_str(), "probe" | "decision") {
                    arguments.extend(["--result".into(), "<durable-outcome>".into()]);
                }
                arguments
            }
        };
        result.corrective_commands.push(command(arguments));
    }
    result
}

fn command(arguments: impl IntoIterator<Item = String>) -> CorrectiveCommand {
    CorrectiveCommand {
        program: "papertiger".into(),
        arguments: arguments.into_iter().collect(),
    }
}
