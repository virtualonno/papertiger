//! Receipt-bound removal of project integration surfaces.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use serde::Serialize;

use super::filesystem::validate_destination;
use super::receipt::{InstallReceipt, load_install_receipt, managed_text_sha256};
use super::runtime_receipt::{
    build_runtime_install_receipt, runtime_receipt_bytes, runtime_receipt_relative_path,
};
use super::{INSTALL_RECEIPT_PATH, normalized_path};

#[derive(Debug)]
pub(crate) struct UninstallProjectRequest<'a> {
    pub(crate) project_root: &'a Path,
    /// Defaults to the running Papertiger executable. Tests can provide an
    /// isolated release fixture outside the consuming project.
    pub(crate) source_binary: Option<&'a Path>,
    pub(crate) dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UninstallOperation {
    Remove,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UninstallActionKind {
    Remove,
    Missing,
    ModifiedRefusal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UninstallAction {
    pub(crate) path: String,
    pub(crate) action: UninstallActionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct UninstallProjectResult {
    pub(crate) schema: &'static str,
    pub(crate) version: &'static str,
    pub(crate) project_root: String,
    pub(crate) authority_path: String,
    pub(crate) dry_run: bool,
    pub(crate) operation: UninstallOperation,
    pub(crate) actions: Vec<UninstallAction>,
    pub(crate) retained: Vec<String>,
    pub(crate) next_actions: Vec<String>,
}

#[derive(Debug)]
struct RemovalTarget {
    relative_path: String,
    expected: ExpectedContent,
}

#[derive(Debug)]
enum ExpectedContent {
    CanonicalText(String),
    ExactBytes(Vec<u8>),
    Receipt(Vec<u8>),
}

/// Remove only files whose ownership and current content are proven by the
/// project-install receipt and the matching external release binary.
pub(crate) fn uninstall_project(
    request: UninstallProjectRequest<'_>,
) -> Result<UninstallProjectResult> {
    let root = fs::canonicalize(request.project_root).with_context(|| {
        format!(
            "resolve uninstall-project root {}; create or restore the project directory first",
            request.project_root.display()
        )
    })?;
    if !root.is_dir() {
        return Err(anyhow!(
            "uninstall-project root {} is not an existing directory",
            root.display()
        ));
    }

    let receipt_path = root.join(INSTALL_RECEIPT_PATH);
    validate_destination(&root, Path::new(INSTALL_RECEIPT_PATH))?;
    let receipt = load_install_receipt(&receipt_path)?.ok_or_else(|| {
        anyhow!(
            "uninstall-project found no project-install receipt at {}; nothing was removed. Restore the receipt or review and remove unowned integration files manually",
            receipt_path.display()
        )
    })?;
    require_matching_release(&receipt)?;

    let source_binary = match request.source_binary {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe().context("resolve the running papertiger executable")?,
    };
    let runtime_relative = runtime_relative_path(&source_binary)?;
    let runtime_path = root.join(&runtime_relative);
    refuse_self_uninstall(&source_binary, &runtime_path)?;
    let source_bytes = fs::read(&source_binary)
        .with_context(|| format!("read release binary {}", source_binary.display()))?;
    let expected_runtime_receipt =
        build_runtime_install_receipt(Path::new(&runtime_relative), &source_bytes);
    let expected_runtime_receipt_bytes = runtime_receipt_bytes(&expected_runtime_receipt)?;
    let runtime_receipt_relative = normalized_path(&runtime_receipt_relative_path(Path::new(
        &runtime_relative,
    ))?);

    let receipt_bytes = fs::read(&receipt_path)
        .with_context(|| format!("read project-install receipt {}", receipt_path.display()))?;
    let mut targets = Vec::with_capacity(receipt.managed_files.len() + 3);
    let mut seen = HashSet::new();
    for file in &receipt.managed_files {
        if !seen.insert(file.path.clone()) {
            return Err(anyhow!(
                "project-install receipt repeats managed path {}; nothing was removed",
                file.path
            ));
        }
        targets.push(RemovalTarget {
            relative_path: file.path.clone(),
            expected: ExpectedContent::CanonicalText(file.sha256.clone()),
        });
    }
    if !seen.insert(runtime_relative.clone()) {
        return Err(anyhow!(
            "project-install receipt incorrectly records runtime path {runtime_relative} as text; nothing was removed"
        ));
    }
    targets.push(RemovalTarget {
        relative_path: runtime_relative,
        expected: ExpectedContent::ExactBytes(source_bytes),
    });
    if !seen.insert(runtime_receipt_relative.clone()) {
        return Err(anyhow!(
            "project-install receipt incorrectly records runtime receipt path {runtime_receipt_relative} as text; nothing was removed"
        ));
    }
    targets.push(RemovalTarget {
        relative_path: runtime_receipt_relative,
        expected: ExpectedContent::Receipt(expected_runtime_receipt_bytes),
    });
    targets.push(RemovalTarget {
        relative_path: INSTALL_RECEIPT_PATH.to_owned(),
        expected: ExpectedContent::Receipt(receipt_bytes),
    });

    let mut actions = Vec::with_capacity(targets.len());
    let mut blocked = false;
    for target in &targets {
        validate_destination(&root, Path::new(&target.relative_path))?;
        let action = inspect_target(&root.join(&target.relative_path), &target.expected)?;
        blocked |= action == UninstallActionKind::ModifiedRefusal;
        actions.push(UninstallAction {
            path: target.relative_path.clone(),
            action,
        });
    }

    let operation = if blocked {
        UninstallOperation::Blocked
    } else {
        UninstallOperation::Remove
    };
    if blocked && !request.dry_run {
        let paths = actions
            .iter()
            .filter(|action| action.action == UninstallActionKind::ModifiedRefusal)
            .map(|action| action.path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow!(
            "uninstall-project refuses modified or non-regular receipt-owned files: {paths}. Restore the receipt-owned content with `papertiger setup-project <project-root>`, or preserve and remove those paths manually; nothing was removed"
        ));
    }

    if !request.dry_run {
        // The receipt is deliberately last so an interrupted removal retains
        // the ownership record needed to retry or repair the installation.
        for (target, action) in targets.iter().zip(&actions) {
            if action.action != UninstallActionKind::Remove {
                continue;
            }
            let path = root.join(&target.relative_path);
            let current = inspect_target(&path, &target.expected)?;
            if current != UninstallActionKind::Remove {
                return Err(anyhow!(
                    "uninstall-project detected a concurrent change at {}; removal stopped with the project-install receipt retained when possible. Restore with `papertiger setup-project <project-root>` or review the remaining receipt-owned files",
                    path.display()
                ));
            }
            fs::remove_file(&path)
                .with_context(|| format!("remove receipt-owned file {}", path.display()))?;
        }
    }

    let retained = retained_surfaces(&receipt);
    let next_actions = if blocked {
        vec![
            "No files will be removed. Review every modified_refusal path; preserve repository-owned content or restore the receipt-owned release content before retrying."
                .to_owned(),
        ]
    } else if request.dry_run {
        vec![format!(
            "Preview is ready; apply with an external matching Papertiger {} binary: papertiger uninstall-project \"{}\"",
            env!("CARGO_PKG_VERSION"),
            normalized_path(&root)
        )]
    } else {
        vec![
            "Review and commit the removed receipt-owned integration files. The authority, sidecars, Mise state, repository guidance, unrelated skills, and .gitignore policy were retained."
                .to_owned(),
            "If the retained authority is no longer needed, archive or remove it in a separate explicit data-lifecycle decision."
                .to_owned(),
        ]
    };

    Ok(UninstallProjectResult {
        schema: "papertiger.project_uninstall.v2",
        version: env!("CARGO_PKG_VERSION"),
        project_root: normalized_path(&root),
        authority_path: receipt.authority_path.clone(),
        dry_run: request.dry_run,
        operation,
        actions,
        retained,
        next_actions,
    })
}

fn require_matching_release(receipt: &InstallReceipt) -> Result<()> {
    if receipt.papertiger_version != env!("CARGO_PKG_VERSION") {
        return Err(anyhow!(
            "uninstall-project requires a Papertiger {} project-install receipt, found version {}. Upgrade the managed integration with this release's `papertiger setup-project <project-root>` and review it before uninstalling, or use the matching Papertiger {} binary",
            env!("CARGO_PKG_VERSION"),
            receipt.papertiger_version,
            receipt.papertiger_version
        ));
    }
    Ok(())
}

fn runtime_relative_path(source_binary: &Path) -> Result<String> {
    let source_name = source_binary
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow!(
                "release binary has no UTF-8 file name: {}",
                source_binary.display()
            )
        })?;
    if source_name.eq_ignore_ascii_case("papertiger.exe") {
        Ok("tools/papertiger/bin/papertiger.exe".to_owned())
    } else if source_name == "papertiger" {
        Ok("tools/papertiger/bin/papertiger".to_owned())
    } else {
        Err(anyhow!(
            "uninstall-project release binary must be named papertiger or papertiger.exe, found {source_name:?}"
        ))
    }
}

fn refuse_self_uninstall(source_binary: &Path, runtime_path: &Path) -> Result<()> {
    if !runtime_path.exists() {
        return Ok(());
    }
    let source = fs::canonicalize(source_binary)
        .with_context(|| format!("resolve release binary {}", source_binary.display()))?;
    let runtime = fs::canonicalize(runtime_path)
        .with_context(|| format!("resolve installed runtime {}", runtime_path.display()))?;
    if source == runtime {
        return Err(anyhow!(
            "uninstall-project refuses to delete the running project-local binary {}; invoke an external Papertiger {} release binary instead",
            runtime.display(),
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(())
}

fn inspect_target(path: &Path, expected: &ExpectedContent) -> Result<UninstallActionKind> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UninstallActionKind::Missing);
        }
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Ok(UninstallActionKind::ModifiedRefusal);
        }
        Ok(_) => {}
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let matches = match expected {
        ExpectedContent::CanonicalText(sha256) => managed_text_sha256(&bytes) == *sha256,
        ExpectedContent::ExactBytes(expected) | ExpectedContent::Receipt(expected) => {
            bytes == *expected
        }
    };
    Ok(if matches {
        UninstallActionKind::Remove
    } else {
        UninstallActionKind::ModifiedRefusal
    })
}

fn retained_surfaces(receipt: &InstallReceipt) -> Vec<String> {
    let authority = &receipt.authority_path;
    vec![
        format!("{authority} and its SQLite sidecars"),
        "state/papertiger-mise.sqlite, its SQLite sidecars, and state/papertiger-mise-objects/"
            .to_owned(),
        ".gitignore, including Papertiger authority and runtime protections".to_owned(),
        "AGENTS.md, CLAUDE.md, unrelated skills, and all other repository-owned files".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_setup::{SetupProjectRequest, SkillTargetRequest, setup_project};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEST_ROOT: AtomicUsize = AtomicUsize::new(0);

    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let serial = NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/project-uninstall-tests")
            .join(format!("{name}-{}-{serial}", std::process::id()));
        let release = root.join("release");
        let project = root.join("demo-project");
        fs::create_dir_all(&release).unwrap();
        fs::create_dir_all(&project).unwrap();
        let binary = release.join(format!("papertiger{}", std::env::consts::EXE_SUFFIX));
        fs::write(&binary, b"papertiger-binary").unwrap();
        (project, binary)
    }

    fn install(project: &Path, binary: &Path, target: SkillTargetRequest) {
        setup_project(SetupProjectRequest {
            project_root: project,
            source_binary: Some(binary),
            dry_run: false,
            replace_managed: false,
            authority_path: None,
            skill_target: Some(target),
        })
        .unwrap();
    }

    fn request<'a>(
        project: &'a Path,
        binary: &'a Path,
        dry_run: bool,
    ) -> UninstallProjectRequest<'a> {
        UninstallProjectRequest {
            project_root: project,
            source_binary: Some(binary),
            dry_run,
        }
    }

    fn cleanup(project: &Path) {
        fs::remove_dir_all(project.parent().unwrap()).unwrap();
    }

    #[test]
    fn dry_run_and_apply_remove_only_owned_files() {
        let (project, binary) = fixture("owned");
        fs::write(project.join("AGENTS.md"), "repository contract\n").unwrap();
        fs::create_dir(project.join("state")).unwrap();
        fs::write(project.join("state/papertiger.sqlite"), b"authority").unwrap();
        fs::write(project.join("state/papertiger.sqlite-wal"), b"planner-wal").unwrap();
        fs::write(project.join("state/papertiger-mise.sqlite"), b"mise").unwrap();
        fs::create_dir(project.join("state/papertiger-mise-objects")).unwrap();
        fs::write(
            project.join("state/papertiger-mise-objects/evidence"),
            b"evidence",
        )
        .unwrap();
        fs::create_dir_all(project.join(".agents/skills/unrelated")).unwrap();
        fs::write(
            project.join(".agents/skills/unrelated/SKILL.md"),
            "unrelated skill\n",
        )
        .unwrap();
        fs::write(project.join(".gitignore"), "target/\n").unwrap();
        install(&project, &binary, SkillTargetRequest::Agents);
        let ignore_before = fs::read(project.join(".gitignore")).unwrap();

        let preview = uninstall_project(request(&project, &binary, true)).unwrap();
        assert_eq!(preview.operation, UninstallOperation::Remove);
        assert!(preview.actions.iter().all(|action| {
            matches!(
                action.action,
                UninstallActionKind::Remove | UninstallActionKind::Missing
            )
        }));
        assert!(project.join(INSTALL_RECEIPT_PATH).is_file());

        let result = uninstall_project(request(&project, &binary, false)).unwrap();
        assert_eq!(result.operation, UninstallOperation::Remove);
        assert_eq!(result.schema, "papertiger.project_uninstall.v2");
        assert!(!project.join(INSTALL_RECEIPT_PATH).exists());
        assert!(
            !project
                .join(
                    runtime_receipt_relative_path(Path::new(
                        &runtime_relative_path(&binary).unwrap()
                    ))
                    .unwrap()
                )
                .exists()
        );
        assert!(
            !project
                .join("tools/papertiger/agent_integration.md")
                .exists()
        );
        assert!(!project.join(".agents/skills/papertiger/SKILL.md").exists());
        assert!(project.join("AGENTS.md").is_file());
        assert!(project.join("state/papertiger.sqlite").is_file());
        assert!(project.join("state/papertiger.sqlite-wal").is_file());
        assert!(project.join("state/papertiger-mise.sqlite").is_file());
        assert!(
            project
                .join("state/papertiger-mise-objects/evidence")
                .is_file()
        );
        assert!(project.join(".agents/skills/unrelated/SKILL.md").is_file());
        assert_eq!(fs::read(project.join(".gitignore")).unwrap(), ignore_before);
        cleanup(&project);
    }

    #[test]
    fn modified_text_blocks_all_removal() {
        let (project, binary) = fixture("modified-text");
        install(&project, &binary, SkillTargetRequest::Agents);
        let contract = project.join("tools/papertiger/agent_integration.md");
        fs::write(&contract, "repository edit\n").unwrap();

        let preview = uninstall_project(request(&project, &binary, true)).unwrap();
        assert_eq!(preview.operation, UninstallOperation::Blocked);
        assert!(preview.actions.iter().any(|action| {
            action.path == "tools/papertiger/agent_integration.md"
                && action.action == UninstallActionKind::ModifiedRefusal
        }));
        assert!(uninstall_project(request(&project, &binary, false)).is_err());
        assert!(project.join(INSTALL_RECEIPT_PATH).is_file());
        assert!(project.join(".agents/skills/papertiger/SKILL.md").is_file());
        cleanup(&project);
    }

    #[test]
    fn differing_runtime_blocks_all_removal() {
        let (project, binary) = fixture("modified-runtime");
        install(&project, &binary, SkillTargetRequest::None);
        let runtime = project.join(format!(
            "tools/papertiger/bin/papertiger{}",
            std::env::consts::EXE_SUFFIX
        ));
        fs::write(&runtime, b"different-binary").unwrap();

        let preview = uninstall_project(request(&project, &binary, true)).unwrap();
        assert_eq!(preview.operation, UninstallOperation::Blocked);
        assert!(uninstall_project(request(&project, &binary, false)).is_err());
        assert!(project.join(INSTALL_RECEIPT_PATH).is_file());
        assert!(
            project
                .join("tools/papertiger/agent_integration.md")
                .is_file()
        );
        cleanup(&project);
    }

    #[test]
    fn missing_receipt_refuses_unowned_cleanup() {
        let (project, binary) = fixture("no-receipt");
        let error = uninstall_project(request(&project, &binary, false)).unwrap_err();
        assert!(error.to_string().contains("no project-install receipt"));
        cleanup(&project);
    }

    #[test]
    fn receipt_version_mismatch_requires_matching_release_before_removal() {
        let (project, binary) = fixture("version-mismatch");
        install(&project, &binary, SkillTargetRequest::None);
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        value["papertiger_version"] = serde_json::Value::String("0.8.1".to_owned());
        fs::write(&receipt_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        let error = uninstall_project(request(&project, &binary, false)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("matching Papertiger 0.8.1 binary")
        );
        assert!(project.join(INSTALL_RECEIPT_PATH).is_file());
        assert!(
            project
                .join("tools/papertiger/agent_integration.md")
                .is_file()
        );
        cleanup(&project);
    }

    #[test]
    fn project_local_binary_cannot_uninstall_itself() {
        let (project, binary) = fixture("self-delete");
        install(&project, &binary, SkillTargetRequest::None);
        let runtime = project.join(format!(
            "tools/papertiger/bin/papertiger{}",
            std::env::consts::EXE_SUFFIX
        ));

        let error = uninstall_project(UninstallProjectRequest {
            project_root: &project,
            source_binary: Some(&runtime),
            dry_run: true,
        })
        .unwrap_err();
        assert!(error.to_string().contains("running project-local binary"));
        assert!(project.join(INSTALL_RECEIPT_PATH).is_file());
        cleanup(&project);
    }
}
