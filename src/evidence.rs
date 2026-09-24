use std::{
    fs::File,
    path::{Component, Path, PathBuf},
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
          WHERE gate.status='resolved' AND (?1 IS NULL OR task.seq=?1)
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
    let canonical = match resolve_file_evidence(root, value) {
        Ok(canonical) => canonical,
        Err(problem) => {
            result.status = problem.status.into();
            result.detail = Some(problem.detail);
            result.resolved_path = problem
                .resolved_path
                .and_then(|path| portable_absolute(&path).ok());
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
            Some("binding has no stored SHA-256; reopen and resolve it with --sha256".into());
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

/// Why a `file:` locator value does not name a regular file beneath the
/// project root, in `evidence verify` status vocabulary.
struct FileEvidenceProblem {
    status: &'static str,
    detail: String,
    resolved_path: Option<PathBuf>,
}

/// Resolve a `file:` locator value to its canonical regular file beneath an
/// already canonical project root. `evidence verify` and new gate and blocker
/// resolutions share this one path authority.
fn resolve_file_evidence(root: &Path, value: &str) -> Result<PathBuf, FileEvidenceProblem> {
    let problem = |status, detail: String, resolved_path| FileEvidenceProblem {
        status,
        detail,
        resolved_path,
    };
    let shown =
        |path: &Path| portable_absolute(path).unwrap_or_else(|_| path.display().to_string());
    let relative = Path::new(value);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return Err(problem(
            "invalid_path",
            "file: evidence must name a nonblank path relative to the project root".into(),
            None,
        ));
    }
    let mut path = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(component) => path.push(component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(problem(
                    "path_escape",
                    "file: evidence cannot contain parent, root, or platform-prefix components"
                        .into(),
                    None,
                ));
            }
        }
    }
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let detail = format!("evidence file does not exist: {}", shown(&path));
            return Err(problem("missing", detail, Some(path)));
        }
        Err(error) => {
            let detail = format!("inspect evidence file {}: {error}", shown(&path));
            return Err(problem("unreadable", detail, Some(path)));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(problem(
            "symlink",
            "file: evidence must be a regular file, not a symlink".into(),
            Some(path),
        ));
    }
    if !metadata.is_file() {
        return Err(problem(
            "not_regular_file",
            "file: evidence must be a regular file".into(),
            Some(path),
        ));
    }
    match std::fs::canonicalize(&path) {
        Ok(canonical) if canonical.starts_with(root) => Ok(canonical),
        Ok(canonical) => {
            let detail = format!(
                "resolved evidence path leaves the project root: {}",
                shown(&canonical)
            );
            Err(problem("path_escape", detail, Some(path)))
        }
        Err(error) => {
            let detail = format!("resolve evidence file {}: {error}", shown(&path));
            Err(problem("unreadable", detail, Some(path)))
        }
    }
}

/// Refuse a new `file:` evidence locator unless it names an existing regular
/// file beneath the project root of the authority it is written to and, when a
/// SHA-256 is supplied, the file's bytes match it. Other schemes pass unchanged. Import does not call this, so stored locators stay
/// restorable whether or not their files survive.
pub fn validate_new_file_evidence(
    locator: &str,
    sha256: Option<&str>,
    project_root: Option<&Path>,
    authority: &str,
) -> Result<()> {
    crate::validate_evidence_locator(locator)?;
    let Some((scheme, value)) = locator.split_once(':') else {
        return Ok(());
    };
    if !scheme.eq_ignore_ascii_case("file") {
        return Ok(());
    }
    let keep_text = "or keep the evidence text in the authority with `done <task> --result-file <path>` or `note --text-file <path> --task <task>`";
    let Some(project_root) = project_root else {
        bail!(
            "evidence {locator:?} names a file, but authority {authority} was selected without a project root (by --db, PAPERTIGER_DB, or the personal store), so the path cannot be checked; rerun with --db \"{authority}\" --project-root <project-root> for the project the path is relative to, {keep_text}"
        );
    };
    let root = std::fs::canonicalize(project_root).with_context(|| {
        format!(
            "resolve evidence project root {}; pass --project-root with an existing directory",
            project_root.display()
        )
    })?;
    if !root.is_dir() {
        bail!(
            "evidence project root {} is not a directory; pass --project-root with the project directory",
            project_root.display()
        );
    }
    let canonical = match resolve_file_evidence(&root, value) {
        Ok(canonical) => canonical,
        Err(problem) => bail!(
            "evidence {locator:?} is refused ({}): {}; pass --evidence file:<path> with a path relative to project root {} that names an existing regular file, {keep_text}",
            problem.status,
            problem.detail,
            portable_absolute(&root)?
        ),
    };
    let Some(expected) = sha256 else {
        return Ok(());
    };
    validate_sha256(expected, "sha256")?;
    let shown = portable_absolute(&canonical)?;
    let mut file = File::open(&canonical).with_context(|| format!("open evidence file {shown}"))?;
    let actual = crate::digest::sha256_reader(&mut file)
        .with_context(|| format!("read evidence file {shown}"))?;
    if actual != expected {
        bail!(
            "evidence {locator:?} is refused (digest_mismatch): the file's SHA-256 is {actual}, not {expected}; pass --sha256 {actual}, or omit --sha256"
        );
    }
    Ok(())
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
        "resolve".into(),
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
