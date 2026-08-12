use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;

mod filesystem;
mod receipt;

use filesystem::{
    PreReceiptInstall, ensure_executable, inspect_pre_receipt_install, preflight_managed_file,
    preflight_retired_file, validate_destination, write_file, write_new_file,
};
#[cfg(test)]
use receipt::ManagedFileReceipt;
use receipt::{
    InstallReceipt, build_install_receipt, canonical_managed_text, load_install_receipt,
    preflight_receipt, receipt_bytes, receipt_hashes, refuse_release_downgrade,
    validate_receipt_managed_path,
};

const BASH_LAUNCHER_TEMPLATE: &str = include_str!("../../assets/project-launcher.sh");
const WINDOWS_LAUNCHER_TEMPLATE: &str = include_str!("../../assets/project-launcher.cmd");
const AGENT_INTEGRATION: &[u8] = include_bytes!("../../agent_integration.md");
const AGENT_SKILL: &[u8] = include_bytes!("../../templates/papertiger/SKILL.md");
const INSTALL_RECEIPT_PATH: &str = "tools/papertiger/project-install.json";
const DEFAULT_AUTHORITY_PATH: &str = "state/papertiger.sqlite";
pub(super) const PRE_RECEIPT_MANIFEST_PATH: &str = "tools/papertiger/README.md";
pub(super) const PRE_RECEIPT_BINARY_PATH: &str = "tools/papertiger/papertiger.exe";
pub(super) const PRE_RECEIPT_MISE_PATH: &str = "tools/papertiger/MISE.md";
const GITIGNORE_COMMENT: &str = "# Papertiger project-local runtime and authorities";
const GITIGNORE_END_COMMENT: &str = "# End Papertiger managed ignore block";
const BASE_GITIGNORE_ENTRIES: &[&str] = &[
    "/tools/papertiger/bin/",
    "/state/papertiger-mise.sqlite",
    "/state/papertiger-mise.sqlite-journal",
    "/state/papertiger-mise.sqlite-shm",
    "/state/papertiger-mise.sqlite-wal",
    "/state/papertiger-mise-objects/",
];
const PRE_RECEIPT_GITIGNORE_ENTRIES: &[&str] = &[
    "/tools/papertiger/bin/",
    "/state/papertiger.sqlite",
    "/state/papertiger.sqlite-journal",
    "/state/papertiger.sqlite-shm",
    "/state/papertiger.sqlite-wal",
    "/state/papertiger-mise.sqlite",
    "/state/papertiger-mise.sqlite-journal",
    "/state/papertiger-mise.sqlite-shm",
    "/state/papertiger-mise.sqlite-wal",
    "/state/papertiger-mise-objects/",
];

#[derive(Debug)]
pub(crate) struct SetupProjectRequest<'a> {
    pub(crate) project_root: &'a Path,
    /// Defaults to the running Papertiger executable. Tests can provide an
    /// isolated release fixture.
    pub(crate) source_binary: Option<&'a Path>,
    pub(crate) dry_run: bool,
    pub(crate) replace_managed: bool,
    pub(crate) authority_path: Option<&'a Path>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupOperation {
    Install,
    Repair,
    Upgrade,
    Unchanged,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupActionKind {
    Create,
    Replace,
    Unchanged,
    MakeExecutable,
    UpdateGitignore,
    RemoveRetired,
    ModifiedRefusal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SetupAction {
    pub(crate) path: String,
    pub(crate) action: SetupActionKind,
    pub(crate) requires_replace_managed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SetupProjectResult {
    pub(crate) schema: &'static str,
    pub(crate) version: &'static str,
    pub(crate) project_root: String,
    pub(crate) authority_path: String,
    pub(crate) dry_run: bool,
    pub(crate) operation: SetupOperation,
    pub(crate) actions: Vec<SetupAction>,
    pub(crate) agent_guidance_files_found: Vec<PathBuf>,
    pub(crate) next_actions: Vec<String>,
}

pub(super) struct ManagedFile {
    pub(super) relative_path: PathBuf,
    pub(super) content: Vec<u8>,
    pub(super) executable: bool,
    pub(super) content_kind: ManagedContentKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ManagedContentKind {
    RuntimeBinary,
    ReceiptText,
}

impl ManagedContentKind {
    fn is_receipt_text(self) -> bool {
        self == Self::ReceiptText
    }
}

/// Install a project-local release without touching an existing authority or
/// overwriting a divergent managed file. Every destination is preflighted
/// before the first write, including symlink traversal and `.gitignore`.
pub(crate) fn setup_project(request: SetupProjectRequest<'_>) -> Result<SetupProjectResult> {
    let root = fs::canonicalize(request.project_root).with_context(|| {
        format!(
            "resolve setup-project root {}; create the project directory first",
            request.project_root.display()
        )
    })?;
    if !root.is_dir() {
        return Err(anyhow!(
            "setup-project root {} is not an existing directory",
            root.display()
        ));
    }

    let source_binary = match request.source_binary {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe().context("resolve the running papertiger executable")?,
    };
    let source_name = source_binary
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow!(
                "source binary has no UTF-8 file name: {}",
                source_binary.display()
            )
        })?;
    let suffix = if source_name.eq_ignore_ascii_case("papertiger.exe") {
        ".exe"
    } else if source_name == "papertiger" {
        ""
    } else {
        return Err(anyhow!(
            "setup-project source binary must be named papertiger or papertiger.exe, found {source_name:?}"
        ));
    };
    let binary = fs::read(&source_binary)
        .with_context(|| format!("read source binary {}", source_binary.display()))?;

    let receipt_relative = Path::new(INSTALL_RECEIPT_PATH);
    validate_destination(&root, receipt_relative)?;
    let receipt_path = root.join(receipt_relative);
    let prior_receipt = load_install_receipt(&receipt_path)?;
    if let Some(receipt) = prior_receipt.as_ref() {
        refuse_release_downgrade(receipt)?;
    }
    let pre_receipt_install = if prior_receipt.is_none() {
        inspect_pre_receipt_install(&root)?
    } else {
        None
    };
    let authority_path = select_authority_path(request.authority_path, prior_receipt.as_ref())?;
    validate_destination(&root, Path::new(&authority_path)).with_context(|| {
        format!(
            "validate receipt-selected authority path {authority_path}; choose a regular path wholly inside the project"
        )
    })?;
    let authority_destination = root.join(Path::new(&authority_path));
    if authority_destination.exists() && !authority_destination.is_file() {
        return Err(anyhow!(
            "receipt-selected authority path {} exists but is not a regular file; choose a regular project-local database path or remove the conflicting object",
            authority_destination.display()
        ));
    }
    let bash_launcher = render_bash_launcher(&authority_path);
    let windows_launcher = render_windows_launcher(&authority_path);

    let managed = vec![
        ManagedFile {
            relative_path: PathBuf::from(format!("tools/papertiger/bin/papertiger{suffix}")),
            content: binary,
            executable: true,
            content_kind: ManagedContentKind::RuntimeBinary,
        },
        ManagedFile {
            relative_path: PathBuf::from("scripts/papertiger"),
            content: bash_launcher.into_bytes(),
            executable: true,
            content_kind: ManagedContentKind::ReceiptText,
        },
        ManagedFile {
            relative_path: PathBuf::from("scripts/papertiger.cmd"),
            content: windows_launcher.into_bytes(),
            executable: false,
            content_kind: ManagedContentKind::ReceiptText,
        },
        ManagedFile {
            relative_path: PathBuf::from("tools/papertiger/agent_integration.md"),
            content: canonical_managed_text(AGENT_INTEGRATION).into_owned(),
            executable: false,
            content_kind: ManagedContentKind::ReceiptText,
        },
        ManagedFile {
            relative_path: PathBuf::from(".agents/skills/papertiger/SKILL.md"),
            content: canonical_managed_text(AGENT_SKILL).into_owned(),
            executable: false,
            content_kind: ManagedContentKind::ReceiptText,
        },
        ManagedFile {
            relative_path: PathBuf::from(".claude/skills/papertiger/SKILL.md"),
            content: canonical_managed_text(AGENT_SKILL).into_owned(),
            executable: false,
            content_kind: ManagedContentKind::ReceiptText,
        },
    ];

    let desired_receipt = build_install_receipt(&authority_path, &managed);
    let desired_receipt_bytes = receipt_bytes(&desired_receipt)?;
    let mut prior_hashes = prior_receipt
        .as_ref()
        .map(receipt_hashes)
        .transpose()?
        .unwrap_or_default();
    if let Some(PreReceiptInstall::Verified { owned_hashes, .. }) = &pre_receipt_install {
        prior_hashes.extend(owned_hashes.clone());
    }
    let had_managed_files = managed
        .iter()
        .any(|file| root.join(&file.relative_path).exists())
        || receipt_path.exists()
        || root.join(PRE_RECEIPT_MANIFEST_PATH).exists();

    let mut managed_actions = Vec::with_capacity(managed.len());
    for file in &managed {
        validate_destination(&root, &file.relative_path)?;
        let destination = root.join(&file.relative_path);
        let prior_hash = prior_hashes.get(&normalized_path(&file.relative_path));
        let (action, requires_replace_managed) = preflight_managed_file(
            &destination,
            file,
            prior_hash.map(String::as_str),
            prior_receipt.is_some() && !file.content_kind.is_receipt_text(),
            request.dry_run,
            request.replace_managed,
        )?;
        managed_actions.push(SetupAction {
            path: normalized_path(&file.relative_path),
            action,
            requires_replace_managed,
        });
    }

    let current_receipt_paths = desired_receipt
        .managed_files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<HashSet<_>>();
    let mut retired_paths = Vec::new();
    let mut retired_actions = Vec::new();
    if let Some(prior) = &prior_receipt {
        for prior_file in &prior.managed_files {
            if current_receipt_paths.contains(prior_file.path.as_str()) {
                continue;
            }
            let relative = PathBuf::from(&prior_file.path);
            validate_receipt_managed_path(&relative)?;
            validate_destination(&root, &relative)?;
            if let Some(action) =
                preflight_retired_file(&root.join(&relative), &prior_file.sha256, request.dry_run)?
            {
                retired_paths.push(relative.clone());
                retired_actions.push(SetupAction {
                    path: normalized_path(&relative),
                    action,
                    requires_replace_managed: false,
                });
            }
        }
    }

    match &pre_receipt_install {
        Some(PreReceiptInstall::Verified {
            retired_paths: verified_retired,
            ..
        }) => {
            for relative in verified_retired {
                let relative = PathBuf::from(relative);
                validate_destination(&root, &relative)?;
                retired_paths.push(relative.clone());
                retired_actions.push(SetupAction {
                    path: normalized_path(&relative),
                    action: SetupActionKind::RemoveRetired,
                    requires_replace_managed: false,
                });
            }
        }
        Some(PreReceiptInstall::Unrecognized) => {
            let relative = PathBuf::from(PRE_RECEIPT_MANIFEST_PATH);
            validate_destination(&root, &relative)?;
            if request.dry_run {
                retired_paths.push(relative.clone());
                retired_actions.push(SetupAction {
                    path: PRE_RECEIPT_MANIFEST_PATH.to_owned(),
                    action: SetupActionKind::ModifiedRefusal,
                    requires_replace_managed: false,
                });
            } else {
                return Err(anyhow!(
                    "setup-project found unrecognized content at reserved pre-receipt install path {}; move that file or the old source tree aside deliberately, then rerun setup-project",
                    root.join(relative).display()
                ));
            }
        }
        None => {}
    }

    let gitignore_relative = Path::new(".gitignore");
    validate_destination(&root, gitignore_relative)?;
    let gitignore_path = root.join(gitignore_relative);
    let existing_gitignore = if gitignore_path.exists() {
        if !gitignore_path.is_file() {
            return Err(anyhow!(
                "setup-project cannot preserve non-file {}",
                gitignore_path.display()
            ));
        }
        Some(
            fs::read_to_string(&gitignore_path)
                .with_context(|| format!("read {} as UTF-8", gitignore_path.display()))?,
        )
    } else {
        None
    };
    let gitignore_entries = gitignore_entries(&authority_path);
    let updated_gitignore = gitignore_content(existing_gitignore.as_deref(), &gitignore_entries)?;
    let gitignore_action = if existing_gitignore.as_deref() == Some(updated_gitignore.as_str()) {
        SetupActionKind::Unchanged
    } else if existing_gitignore.is_some() {
        SetupActionKind::UpdateGitignore
    } else {
        SetupActionKind::Create
    };
    let gitignore_setup_action = SetupAction {
        path: normalized_path(gitignore_relative),
        action: gitignore_action,
        requires_replace_managed: false,
    };

    let (receipt_action, receipt_requires_replace) = preflight_receipt(
        &receipt_path,
        &desired_receipt_bytes,
        prior_receipt.is_some(),
        request.dry_run,
        request.replace_managed,
    )?;
    let receipt_setup_action = SetupAction {
        path: INSTALL_RECEIPT_PATH.to_owned(),
        action: receipt_action,
        requires_replace_managed: receipt_requires_replace,
    };

    let operation = setup_operation(
        prior_receipt.as_ref(),
        &desired_receipt,
        had_managed_files,
        &managed_actions,
        &retired_actions,
        gitignore_action,
        &receipt_setup_action,
    );
    let mut actions = managed_actions.clone();
    actions.extend(retired_actions.iter().cloned());
    actions.push(gitignore_setup_action);
    actions.push(receipt_setup_action);

    if !request.dry_run {
        for (file, action) in managed.iter().zip(&managed_actions) {
            let destination = root.join(&file.relative_path);
            match action.action {
                SetupActionKind::Create => write_new_file(&destination, &file.content)?,
                SetupActionKind::Replace => write_file(&destination, &file.content, false)?,
                SetupActionKind::MakeExecutable | SetupActionKind::Unchanged => {}
                SetupActionKind::UpdateGitignore => {
                    unreachable!("managed files never update .gitignore")
                }
                SetupActionKind::RemoveRetired | SetupActionKind::ModifiedRefusal => {
                    unreachable!("managed current files are never retired during apply")
                }
            }
            if file.executable {
                ensure_executable(&destination)?;
            }
        }
        for (relative, action) in retired_paths.iter().zip(&retired_actions) {
            if action.action == SetupActionKind::RemoveRetired {
                fs::remove_file(root.join(relative)).with_context(|| {
                    format!(
                        "remove retired managed file {}",
                        root.join(relative).display()
                    )
                })?;
            }
        }
        if !matches!(gitignore_action, SetupActionKind::Unchanged) {
            write_file(
                &gitignore_path,
                updated_gitignore.as_bytes(),
                existing_gitignore.is_none(),
            )?;
        }
        write_install_receipt(&receipt_path, &desired_receipt_bytes, receipt_action)?;
        verify_installation(&root, &managed, &receipt_path, &desired_receipt_bytes)?;
    }

    let agent_guidance_files_found = ["AGENTS.md", "CLAUDE.md"]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| root.join(path).is_file())
        .collect();
    let authority_exists = root.join(Path::new(&authority_path)).is_file();
    let next_actions = if request.dry_run {
        let mut apply_command = format!("papertiger setup-project \"{}\"", normalized_path(&root));
        if prior_receipt.is_none() && request.authority_path.is_some() {
            apply_command.push_str(&format!(" --authority-path {authority_path}"));
        }
        if request.replace_managed {
            apply_command.push_str(" --replace-managed");
        }
        if operation == SetupOperation::Unchanged {
            vec![
                "Preview is unchanged; no setup-project apply is needed.".to_owned(),
                "No authority was initialized or migrated.".to_owned(),
            ]
        } else if operation == SetupOperation::Blocked {
            let mut blocked = vec![
                "Preview is blocked; no files were written. Inspect modified_refusal actions and preserve or move repository-owned content before applying."
                    .to_owned(),
            ];
            if actions.iter().any(|action| action.requires_replace_managed) {
                let reviewed_apply_command = if request.replace_managed {
                    apply_command.clone()
                } else {
                    format!("{apply_command} --replace-managed")
                };
                blocked.push(format!(
                    "After reviewing every requires_replace_managed action, apply with: {reviewed_apply_command}"
                ));
            }
            if actions.iter().any(|action| {
                action.action == SetupActionKind::ModifiedRefusal
                    && !action.requires_replace_managed
            }) {
                blocked.push(
                    "A modified retired or unrecognized reserved file cannot be overridden; move or delete that exact path deliberately, then rerun the dry-run."
                        .to_owned(),
                );
            }
            blocked
        } else {
            vec![
                format!("Preview is ready; apply with: {apply_command}"),
                "Dry-run created no receipt and did not initialize or migrate authority."
                    .to_owned(),
            ]
        }
    } else {
        let mut applied = vec![
            format!(
                "Invoke the project-root scripts/papertiger from Bash or .\\scripts\\papertiger.cmd from Command Prompt/PowerShell (using a correct relative path when nested); both bind {authority_path} independently of the caller's current directory."
            ),
            "Review the installed Papertiger skill envelope and add only a concise pointer to tools/papertiger/agent_integration.md in repository-owned guidance; setup-project never edits AGENTS.md or CLAUDE.md."
                .to_owned(),
        ];
        if authority_exists {
            applied.push(
                "Run the project launcher with status, focus, and audit; setup-project never migrates or replaces the existing authority."
                    .to_owned(),
            );
        } else {
            applied.push(
                "If this project has never had a Papertiger authority, set PAPERTIGER_ACTOR and run the project launcher with init once; if prior work should exist, stop instead of creating a replacement authority."
                    .to_owned(),
            );
        }
        applied.push(format!(
            "Commit the launchers, project-install receipt, integration contract, skill envelopes, and additive .gitignore policy. Keep tools/papertiger/bin and {authority_path} host-local and outside Git; setup-project writes ignore rules but never changes existing index entries."
        ));
        applied
    };

    Ok(SetupProjectResult {
        schema: "papertiger.project_setup.v2",
        version: env!("CARGO_PKG_VERSION"),
        project_root: normalized_path(&root),
        authority_path,
        dry_run: request.dry_run,
        operation,
        actions,
        agent_guidance_files_found,
        next_actions,
    })
}

fn select_authority_path(
    requested: Option<&Path>,
    prior_receipt: Option<&InstallReceipt>,
) -> Result<String> {
    if let Some(prior) = prior_receipt {
        if let Some(requested) = requested {
            let requested = normalize_authority_path(requested)?;
            if requested != prior.authority_path {
                return Err(anyhow!(
                    "project-install receipt already binds authority {}; setup-project will not rebind it to {requested}. Rerun without --authority-path, or perform a separate deliberate authority migration before creating a new installation receipt",
                    prior.authority_path
                ));
            }
        }
        return Ok(prior.authority_path.clone());
    }
    normalize_authority_path(requested.unwrap_or_else(|| Path::new(DEFAULT_AUTHORITY_PATH)))
}

fn normalize_authority_path(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(anyhow!(
                "--authority-path must be a normalized project-relative path without '.' or '..': {}",
                path.display()
            ));
        };
        let part = part.to_str().ok_or_else(|| {
            anyhow!(
                "--authority-path must contain portable UTF-8 path components: {}",
                path.display()
            )
        })?;
        let windows_stem = part.split('.').next().unwrap_or(part);
        let windows_device = matches!(
            windows_stem.to_ascii_uppercase().as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        );
        if part.is_empty()
            || part.ends_with('.')
            || windows_device
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(anyhow!(
                "--authority-path component {part:?} must be a portable non-device name using only ASCII letters, digits, '.', '_', or '-' with no trailing dot, so Bash and Windows launchers select the same file"
            ));
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(anyhow!(
            "--authority-path requires a project-relative database path such as {DEFAULT_AUTHORITY_PATH}"
        ));
    }
    let normalized = parts.join("/");
    let normalized_lower = normalized.to_ascii_lowercase();
    let reserved_top_level = normalized_lower.split('/').next().is_some_and(|part| {
        matches!(
            part,
            ".git" | ".gitignore" | "scripts" | "tools" | ".agents" | ".claude"
        )
    });
    let overlaps_mise = normalized_lower.starts_with("state/papertiger-mise.sqlite")
        || normalized_lower == "state/papertiger-mise-objects"
        || normalized_lower.starts_with("state/papertiger-mise-objects/");
    if reserved_top_level
        || overlaps_mise
        || normalized_lower == INSTALL_RECEIPT_PATH.to_ascii_lowercase()
        || normalized_lower == PRE_RECEIPT_MANIFEST_PATH.to_ascii_lowercase()
    {
        return Err(anyhow!(
            "--authority-path {normalized} overlaps setup-managed content or the separate Papertiger Mise authority; choose a dedicated planner database path such as {DEFAULT_AUTHORITY_PATH}"
        ));
    }
    Ok(normalized)
}

fn render_bash_launcher(authority_path: &str) -> String {
    render_launcher(
        BASH_LAUNCHER_TEMPLATE,
        "@PAPERTIGER_AUTHORITY_PATH@",
        authority_path,
    )
}

fn render_windows_launcher(authority_path: &str) -> String {
    render_launcher(
        WINDOWS_LAUNCHER_TEMPLATE,
        "@PAPERTIGER_AUTHORITY_PATH_WINDOWS@",
        &authority_path.replace('/', "\\"),
    )
}

fn render_launcher(template: &str, token: &str, value: &str) -> String {
    String::from_utf8(canonical_managed_text(template.as_bytes()).into_owned())
        .expect("embedded launcher template is UTF-8")
        .replace(token, value)
}

fn setup_operation(
    prior: Option<&InstallReceipt>,
    desired: &InstallReceipt,
    had_managed_files: bool,
    managed_actions: &[SetupAction],
    retired_actions: &[SetupAction],
    gitignore_action: SetupActionKind,
    receipt_action: &SetupAction,
) -> SetupOperation {
    if managed_actions
        .iter()
        .chain(retired_actions)
        .chain(std::iter::once(receipt_action))
        .any(|action| {
            action.action == SetupActionKind::ModifiedRefusal || action.requires_replace_managed
        })
    {
        return SetupOperation::Blocked;
    }
    let changed_owned_layout = managed_actions
        .iter()
        .any(|action| action.action == SetupActionKind::Replace)
        || retired_actions
            .iter()
            .any(|action| action.action == SetupActionKind::RemoveRetired);
    match prior {
        None if had_managed_files => SetupOperation::Upgrade,
        None => SetupOperation::Install,
        Some(prior)
            if prior != desired
                || changed_owned_layout
                || receipt_action.action == SetupActionKind::Replace =>
        {
            SetupOperation::Upgrade
        }
        Some(_)
            if managed_actions.iter().any(|action| {
                matches!(
                    action.action,
                    SetupActionKind::Create | SetupActionKind::MakeExecutable
                )
            }) || gitignore_action != SetupActionKind::Unchanged =>
        {
            SetupOperation::Repair
        }
        Some(_) => SetupOperation::Unchanged,
    }
}

fn write_install_receipt(path: &Path, content: &[u8], action: SetupActionKind) -> Result<()> {
    match action {
        SetupActionKind::Create => write_new_file(path, content)?,
        SetupActionKind::Replace => write_file(path, content, false)?,
        SetupActionKind::Unchanged => {}
        _ => unreachable!("receipt apply action must create, replace, or remain unchanged"),
    }
    let installed = fs::read(path)
        .with_context(|| format!("verify project-install receipt {}", path.display()))?;
    if receipt::managed_text_sha256(&installed) != receipt::managed_text_sha256(content) {
        return Err(anyhow!(
            "project-install receipt verification failed at {}; rerun setup-project after checking the filesystem",
            path.display()
        ));
    }
    Ok(())
}

fn verify_installation(
    root: &Path,
    managed: &[ManagedFile],
    receipt_path: &Path,
    receipt_bytes: &[u8],
) -> Result<()> {
    for file in managed {
        let installed = fs::read(root.join(&file.relative_path)).with_context(|| {
            format!(
                "verify installed managed file {}",
                root.join(&file.relative_path).display()
            )
        })?;
        let content_matches = if file.content_kind.is_receipt_text() {
            receipt::managed_text_sha256(&installed) == receipt::managed_text_sha256(&file.content)
        } else {
            installed == file.content
        };
        if !content_matches {
            return Err(anyhow!(
                "setup-project verification found unexpected content at {}; rerun setup-project after checking the filesystem",
                root.join(&file.relative_path).display()
            ));
        }
    }
    if receipt::managed_text_sha256(&fs::read(receipt_path)?)
        != receipt::managed_text_sha256(receipt_bytes)
    {
        return Err(anyhow!(
            "setup-project verification found an unexpected project-install receipt at {}",
            receipt_path.display()
        ));
    }
    Ok(())
}

fn gitignore_entries(authority_path: &str) -> Vec<String> {
    let mut entries = BASE_GITIGNORE_ENTRIES
        .iter()
        .map(|entry| (*entry).to_owned())
        .collect::<Vec<_>>();
    let authority = format!("/{authority_path}");
    entries.extend([
        authority.clone(),
        format!("{authority}-journal"),
        format!("{authority}-shm"),
        format!("{authority}-wal"),
    ]);
    entries
}

fn gitignore_content(existing: Option<&str>, entries: &[String]) -> Result<String> {
    let existing = existing.unwrap_or("");
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut managed_block = String::new();
    managed_block.push_str(GITIGNORE_COMMENT);
    managed_block.push_str(newline);
    for entry in entries {
        managed_block.push_str(entry);
        managed_block.push_str(newline);
    }
    managed_block.push_str(GITIGNORE_END_COMMENT);
    managed_block.push_str(newline);

    let starts = marker_line_ranges(existing, GITIGNORE_COMMENT);
    let ends = marker_line_ranges(existing, GITIGNORE_END_COMMENT);
    let repository_content = match (starts.as_slice(), ends.as_slice()) {
        ([], []) => existing.to_owned(),
        ([(start, start_end)], []) if is_pre_receipt_gitignore_suffix(&existing[*start_end..]) => {
            existing[..*start].to_owned()
        }
        ([(start, _)], [(_, end)]) if start < end => {
            let mut content = existing[..*start].to_owned();
            content.push_str(&existing[*end..]);
            content
        }
        _ => {
            return Err(anyhow!(
                "setup-project refuses malformed or duplicate Papertiger managed markers in .gitignore; keep either no Papertiger marker lines or exactly one ordered '{}' / '{}' pair, then rerun setup-project",
                GITIGNORE_COMMENT,
                GITIGNORE_END_COMMENT
            ));
        }
    };

    let mut output = repository_content;
    if !output.is_empty() && !output.ends_with('\n') {
        output.push_str(newline);
    }
    if !output.is_empty() && !output.ends_with(&format!("{newline}{newline}")) {
        output.push_str(newline);
    }
    output.push_str(&managed_block);
    Ok(output)
}

fn marker_line_ranges(content: &str, marker: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        let line_end = offset + line.len();
        if line.trim_end_matches(['\r', '\n']) == marker {
            ranges.push((offset, line_end));
        }
        offset = line_end;
    }
    ranges
}

fn is_pre_receipt_gitignore_suffix(content: &str) -> bool {
    let mut prior_index = None;
    let mut found = false;
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let Some(index) = PRE_RECEIPT_GITIGNORE_ENTRIES
            .iter()
            .position(|entry| line == *entry)
        else {
            return false;
        };
        if prior_index.is_some_and(|prior| index <= prior) {
            return false;
        }
        prior_index = Some(index);
        found = true;
    }
    found
}

fn normalized_path(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = path.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = path.strip_prefix("//?/") {
        rest.to_owned()
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_TEST_ROOT: AtomicUsize = AtomicUsize::new(0);

    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let serial = NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/project-setup-tests")
            .join(format!("{name}-{}-{serial}", std::process::id()));
        let release = root.join("release");
        let project = root.join("demo-project");
        fs::create_dir_all(&release).unwrap();
        fs::create_dir_all(&project).unwrap();
        let suffix = std::env::consts::EXE_SUFFIX;
        let binary = release.join(format!("papertiger{suffix}"));
        fs::write(&binary, b"papertiger-binary").unwrap();
        (project, binary)
    }

    fn request<'a>(project: &'a Path, binary: &'a Path) -> SetupProjectRequest<'a> {
        SetupProjectRequest {
            project_root: project,
            source_binary: Some(binary),
            dry_run: false,
            replace_managed: false,
            authority_path: None,
        }
    }

    fn cleanup(project: &Path) {
        fs::remove_dir_all(project.parent().unwrap()).unwrap();
    }

    fn write_pre_receipt_install(project: &Path) {
        let directory = project.join("tools/papertiger");
        fs::create_dir_all(&directory).unwrap();
        let binary = b"pre-receipt-papertiger-binary";
        let contract = b"pre-receipt agent contract\n";
        let mise = b"pre-receipt Mise contract\n";
        fs::write(project.join(PRE_RECEIPT_BINARY_PATH), binary).unwrap();
        fs::write(
            project.join("tools/papertiger/agent_integration.md"),
            contract,
        )
        .unwrap();
        fs::write(project.join(PRE_RECEIPT_MISE_PATH), mise).unwrap();
        let manifest = format!(
            "# Vendored Papertiger\n\nThis directory vendors the project-generic Papertiger planning client used by this project.\n\n- Canonical planning database: `state/papertiger.sqlite` at the repo root\n\nUse `tools/papertiger/papertiger.exe` from the repo root.\n\n- Binary SHA-256: `{}`\n- Agent contract SHA-256: `{}`\n- Mise contract SHA-256: `{}`\n",
            papertiger::sha256(binary),
            papertiger::sha256(contract),
            papertiger::sha256(mise),
        );
        fs::write(project.join(PRE_RECEIPT_MANIFEST_PATH), manifest).unwrap();
    }

    #[test]
    fn dry_run_reports_without_writing() {
        let (project, binary) = fixture("dry-run");
        let mut request = request(&project, &binary);
        request.dry_run = true;
        let result = setup_project(request).unwrap();
        assert!(result.dry_run);
        assert!(
            result
                .actions
                .iter()
                .all(|action| action.action == SetupActionKind::Create)
        );
        assert!(!project.join("scripts/papertiger").exists());
        assert!(!project.join(".gitignore").exists());
        cleanup(&project);
    }

    #[test]
    fn ready_dry_run_replays_reviewed_authority_and_replacement_flags() {
        let (project, binary) = fixture("dry-run-replay");
        fs::create_dir_all(project.join("scripts")).unwrap();
        fs::write(project.join("scripts/papertiger"), b"pre-receipt launcher").unwrap();
        fs::create_dir_all(project.join("plans")).unwrap();
        fs::write(
            project.join("plans/papertiger.sqlite"),
            b"existing-authority",
        )
        .unwrap();

        let mut preview_request = request(&project, &binary);
        preview_request.dry_run = true;
        preview_request.replace_managed = true;
        preview_request.authority_path = Some(Path::new("plans/papertiger.sqlite"));
        let preview = setup_project(preview_request).unwrap();
        assert_ne!(preview.operation, SetupOperation::Blocked);
        assert!(preview.next_actions.iter().any(|action| {
            action
                == &format!(
                    "Preview is ready; apply with: papertiger setup-project \"{}\" --authority-path plans/papertiger.sqlite --replace-managed",
                    normalized_path(&project)
                )
        }));
        assert_eq!(
            fs::read(project.join("scripts/papertiger")).unwrap(),
            b"pre-receipt launcher"
        );

        let mut apply_request = request(&project, &binary);
        apply_request.replace_managed = true;
        apply_request.authority_path = Some(Path::new("plans/papertiger.sqlite"));
        setup_project(apply_request).unwrap();
        assert_eq!(
            fs::read(project.join("plans/papertiger.sqlite")).unwrap(),
            b"existing-authority"
        );
        cleanup(&project);
    }

    #[test]
    fn public_paths_remove_windows_verbatim_prefixes() {
        assert_eq!(
            normalized_path(Path::new(r"\\?\C:\projects\consumer")),
            "C:/projects/consumer"
        );
        assert_eq!(
            normalized_path(Path::new(r"\\?\UNC\server\share\consumer")),
            "//server/share/consumer"
        );
    }

    #[test]
    fn install_is_idempotent_and_preserves_repository_owned_files_and_authority() {
        let (project, binary) = fixture("idempotent");
        fs::write(project.join("AGENTS.md"), "repository contract\n").unwrap();
        fs::write(project.join("CLAUDE.md"), "other repository contract\n").unwrap();
        fs::write(project.join(".gitignore"), "target/\n").unwrap();
        fs::create_dir(project.join("state")).unwrap();
        fs::write(
            project.join("state/papertiger.sqlite"),
            b"existing-authority",
        )
        .unwrap();

        let first = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(first.schema, "papertiger.project_setup.v2");
        assert_eq!(first.operation, SetupOperation::Install);
        assert_eq!(first.authority_path, DEFAULT_AUTHORITY_PATH);
        assert_eq!(first.agent_guidance_files_found.len(), 2);
        assert!(
            first
                .next_actions
                .iter()
                .any(|action| action.contains(".\\scripts\\papertiger.cmd"))
        );
        assert!(first.next_actions.iter().any(|action| {
            action.contains("using a correct relative path when nested")
                && action.contains("independently of the caller's current directory")
        }));
        assert!(first.next_actions.iter().any(|action| {
            action.contains("tools/papertiger/bin")
                && action.contains(DEFAULT_AUTHORITY_PATH)
                && action.contains("never changes existing index entries")
        }));
        assert_eq!(
            fs::read_to_string(project.join("AGENTS.md")).unwrap(),
            "repository contract\n"
        );
        assert_eq!(
            fs::read(project.join("state/papertiger.sqlite")).unwrap(),
            b"existing-authority"
        );
        let ignore = fs::read_to_string(project.join(".gitignore")).unwrap();
        assert!(ignore.starts_with("target/\n"));
        for entry in gitignore_entries(DEFAULT_AUTHORITY_PATH) {
            assert_eq!(
                ignore.lines().filter(|line| line.trim() == entry).count(),
                1
            );
        }
        assert!(!project.join("AGENTS.md.papertiger").exists());
        assert!(
            !project
                .join("tools/papertiger/bin/papertiger-mise")
                .exists()
        );
        assert_eq!(
            fs::read(project.join(".agents/skills/papertiger/SKILL.md")).unwrap(),
            AGENT_SKILL
        );
        assert_eq!(
            fs::read(project.join(".claude/skills/papertiger/SKILL.md")).unwrap(),
            AGENT_SKILL
        );
        let receipt = load_install_receipt(&project.join(INSTALL_RECEIPT_PATH))
            .unwrap()
            .unwrap();
        assert_eq!(receipt.authority_path, DEFAULT_AUTHORITY_PATH);
        assert_eq!(receipt.managed_files.len(), 5);

        let second = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(second.operation, SetupOperation::Unchanged);
        assert!(
            second
                .actions
                .iter()
                .all(|action| action.action == SetupActionKind::Unchanged)
        );
        cleanup(&project);
    }

    #[test]
    fn divergent_managed_file_refuses_before_any_write() {
        let (project, binary) = fixture("divergent");
        setup_project(request(&project, &binary)).unwrap();
        fs::write(
            project.join("scripts/papertiger"),
            b"repository-owned replacement",
        )
        .unwrap();
        fs::remove_file(project.join("tools/papertiger/agent_integration.md")).unwrap();

        let error = setup_project(request(&project, &binary)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("modified or unowned managed file")
        );
        assert!(
            !project
                .join("tools/papertiger/agent_integration.md")
                .exists()
        );
        cleanup(&project);
    }

    #[test]
    fn dry_run_discloses_and_explicit_replacement_applies_upgrade() {
        let (project, binary) = fixture("replace");
        setup_project(request(&project, &binary)).unwrap();
        fs::write(project.join("scripts/papertiger"), b"older release").unwrap();

        let mut dry_run = request(&project, &binary);
        dry_run.dry_run = true;
        let result = setup_project(dry_run).unwrap();
        assert_eq!(result.operation, SetupOperation::Blocked);
        assert!(result.actions.iter().any(|action| {
            action.path == "scripts/papertiger"
                && action.action == SetupActionKind::ModifiedRefusal
                && action.requires_replace_managed
        }));
        assert_eq!(
            fs::read(project.join("scripts/papertiger")).unwrap(),
            b"older release"
        );

        let mut replace = request(&project, &binary);
        replace.replace_managed = true;
        setup_project(replace).unwrap();
        assert_eq!(
            fs::read(project.join("scripts/papertiger")).unwrap(),
            render_bash_launcher(DEFAULT_AUTHORITY_PATH).as_bytes()
        );
        cleanup(&project);
    }

    #[test]
    fn receipt_owned_content_upgrades_without_requiring_a_replacement_flag() {
        let (project, binary) = fixture("receipt-upgrade");
        setup_project(request(&project, &binary)).unwrap();
        let old_launcher = b"#!/usr/bin/env bash\necho old release\n";
        fs::write(project.join("scripts/papertiger"), old_launcher).unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut receipt = load_install_receipt(&receipt_path).unwrap().unwrap();
        receipt.papertiger_version = "0.4.0".to_owned();
        receipt
            .managed_files
            .iter_mut()
            .find(|file| file.path == "scripts/papertiger")
            .unwrap()
            .sha256 = papertiger::sha256(old_launcher);
        fs::write(&receipt_path, receipt_bytes(&receipt).unwrap()).unwrap();

        let upgraded = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(upgraded.operation, SetupOperation::Upgrade);
        assert!(upgraded.actions.iter().any(|action| {
            action.path == "scripts/papertiger"
                && action.action == SetupActionKind::Replace
                && !action.requires_replace_managed
        }));
        assert_eq!(
            fs::read(project.join("scripts/papertiger")).unwrap(),
            render_bash_launcher(DEFAULT_AUTHORITY_PATH).as_bytes()
        );
        assert_eq!(
            load_install_receipt(&receipt_path)
                .unwrap()
                .unwrap()
                .papertiger_version,
            env!("CARGO_PKG_VERSION")
        );
        cleanup(&project);
    }

    #[test]
    fn newer_receipt_version_refuses_downgrade_even_with_replacement_authority() {
        let (project, binary) = fixture("receipt-downgrade");
        setup_project(request(&project, &binary)).unwrap();
        let launcher_path = project.join("scripts/papertiger");
        let launcher_before = fs::read(&launcher_path).unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut receipt = load_install_receipt(&receipt_path).unwrap().unwrap();
        receipt.papertiger_version = "999.0.0".to_owned();
        let future_receipt = receipt_bytes(&receipt).unwrap();
        fs::write(&receipt_path, &future_receipt).unwrap();

        let mut downgrade = request(&project, &binary);
        downgrade.replace_managed = true;
        let error = setup_project(downgrade).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("refuses to downgrade"));
        assert!(message.contains("verified Papertiger 999.0.0 or newer"));
        assert_eq!(fs::read(&receipt_path).unwrap(), future_receipt);
        assert_eq!(fs::read(&launcher_path).unwrap(), launcher_before);
        cleanup(&project);
    }

    #[test]
    fn non_semantic_receipt_version_refuses_before_any_write() {
        let (project, binary) = fixture("receipt-invalid-version");
        setup_project(request(&project, &binary)).unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut receipt = load_install_receipt(&receipt_path).unwrap().unwrap();
        receipt.papertiger_version = "release-next".to_owned();
        let invalid_receipt = receipt_bytes(&receipt).unwrap();
        fs::write(&receipt_path, &invalid_receipt).unwrap();

        let error = setup_project(request(&project, &binary)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must be a canonical semantic version")
        );
        assert_eq!(fs::read(&receipt_path).unwrap(), invalid_receipt);
        cleanup(&project);
    }

    #[test]
    fn receipt_retires_only_hash_matching_prior_managed_content() {
        let (project, binary) = fixture("receipt-retired-path");
        setup_project(request(&project, &binary)).unwrap();
        let retired = b"retired Papertiger documentation\n";
        let retired_path = project.join(PRE_RECEIPT_MANIFEST_PATH);
        fs::write(&retired_path, retired).unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut receipt = load_install_receipt(&receipt_path).unwrap().unwrap();
        receipt.managed_files.push(ManagedFileReceipt {
            path: PRE_RECEIPT_MANIFEST_PATH.to_owned(),
            sha256: papertiger::sha256(retired),
        });
        fs::write(&receipt_path, receipt_bytes(&receipt).unwrap()).unwrap();

        let upgraded = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(upgraded.operation, SetupOperation::Upgrade);
        assert!(!retired_path.exists());
        assert!(upgraded.actions.iter().any(|action| {
            action.path == PRE_RECEIPT_MANIFEST_PATH
                && action.action == SetupActionKind::RemoveRetired
        }));
        cleanup(&project);
    }

    #[test]
    fn modified_retired_receipt_content_refuses_instead_of_deleting() {
        let (project, binary) = fixture("modified-retired-path");
        setup_project(request(&project, &binary)).unwrap();
        let retired_path = project.join(PRE_RECEIPT_MANIFEST_PATH);
        fs::write(&retired_path, b"repository-owned content\n").unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut receipt = load_install_receipt(&receipt_path).unwrap().unwrap();
        receipt.managed_files.push(ManagedFileReceipt {
            path: PRE_RECEIPT_MANIFEST_PATH.to_owned(),
            sha256: papertiger::sha256(b"older release content\n"),
        });
        fs::write(&receipt_path, receipt_bytes(&receipt).unwrap()).unwrap();

        let mut dry_run = request(&project, &binary);
        dry_run.dry_run = true;
        let preview = setup_project(dry_run).unwrap();
        assert_eq!(preview.operation, SetupOperation::Blocked);
        assert!(preview.actions.iter().any(|action| {
            action.path == PRE_RECEIPT_MANIFEST_PATH
                && action.action == SetupActionKind::ModifiedRefusal
        }));
        let error = setup_project(request(&project, &binary)).unwrap_err();
        assert!(error.to_string().contains("modified retired managed file"));
        assert_eq!(
            fs::read(&retired_path).unwrap(),
            b"repository-owned content\n"
        );
        cleanup(&project);
    }

    #[test]
    fn hash_bound_pre_receipt_install_cuts_over_without_guessing_ownership() {
        let (project, binary) = fixture("pre-receipt-install");
        write_pre_receipt_install(&project);
        fs::write(
            project.join("tools/papertiger/agent_integration.md"),
            b"pre-receipt agent contract\r\n",
        )
        .unwrap();

        let mut dry_run = request(&project, &binary);
        dry_run.dry_run = true;
        let preview = setup_project(dry_run).unwrap();
        assert_eq!(preview.operation, SetupOperation::Upgrade);
        assert!(preview.actions.iter().any(|action| {
            action.path == PRE_RECEIPT_MANIFEST_PATH
                && action.action == SetupActionKind::RemoveRetired
                && !action.requires_replace_managed
        }));
        assert!(preview.actions.iter().any(|action| {
            action.path == "tools/papertiger/agent_integration.md"
                && action.action == SetupActionKind::Replace
                && !action.requires_replace_managed
        }));

        setup_project(request(&project, &binary)).unwrap();
        assert!(!project.join(PRE_RECEIPT_MANIFEST_PATH).exists());
        assert!(!project.join(PRE_RECEIPT_BINARY_PATH).exists());
        assert!(!project.join(PRE_RECEIPT_MISE_PATH).exists());
        assert_eq!(
            fs::read(project.join("tools/papertiger/agent_integration.md")).unwrap(),
            AGENT_INTEGRATION
        );
        assert!(project.join(INSTALL_RECEIPT_PATH).is_file());
        cleanup(&project);
    }

    #[test]
    fn changed_pre_receipt_bundle_refuses_before_writing() {
        let (project, binary) = fixture("changed-pre-receipt-install");
        write_pre_receipt_install(&project);
        fs::write(
            project.join(PRE_RECEIPT_MISE_PATH),
            b"repository-owned change\n",
        )
        .unwrap();

        let mut preview = request(&project, &binary);
        preview.dry_run = true;
        let error = setup_project(preview).unwrap_err();
        assert!(error.to_string().contains("pre-receipt Mise contract"));
        assert!(error.to_string().contains("restore the recorded file"));
        assert!(!project.join("scripts/papertiger").exists());
        assert!(project.join(PRE_RECEIPT_MANIFEST_PATH).exists());
        cleanup(&project);
    }

    #[test]
    fn unrecognized_reserved_source_tree_is_never_removed_by_replacement_flag() {
        let (project, binary) = fixture("unrecognized-pre-receipt-tree");
        fs::create_dir_all(project.join("tools/papertiger")).unwrap();
        fs::write(
            project.join(PRE_RECEIPT_MANIFEST_PATH),
            b"# papertiger\n\nLean project-generic planning source tree.\n",
        )
        .unwrap();

        let mut preview = request(&project, &binary);
        preview.dry_run = true;
        preview.replace_managed = true;
        let result = setup_project(preview).unwrap();
        assert_eq!(result.operation, SetupOperation::Blocked);
        assert!(result.actions.iter().any(|action| {
            action.path == PRE_RECEIPT_MANIFEST_PATH
                && action.action == SetupActionKind::ModifiedRefusal
                && !action.requires_replace_managed
        }));

        let mut apply = request(&project, &binary);
        apply.replace_managed = true;
        let error = setup_project(apply).unwrap_err();
        assert!(error.to_string().contains("unrecognized content"));
        assert!(project.join(PRE_RECEIPT_MANIFEST_PATH).exists());
        assert!(!project.join("scripts/papertiger").exists());
        cleanup(&project);
    }

    #[test]
    fn custom_authority_path_survives_repair_and_binary_upgrade() {
        let (project, binary) = fixture("custom-authority");
        fs::create_dir(project.join("plans")).unwrap();
        fs::write(
            project.join("plans/papertiger.sqlite"),
            b"existing-custom-authority",
        )
        .unwrap();
        let mut first_request = request(&project, &binary);
        first_request.authority_path = Some(Path::new("plans/papertiger.sqlite"));
        let first = setup_project(first_request).unwrap();
        assert_eq!(first.authority_path, "plans/papertiger.sqlite");
        assert!(
            fs::read_to_string(project.join("scripts/papertiger"))
                .unwrap()
                .contains("$root/plans/papertiger.sqlite")
        );
        assert!(
            fs::read_to_string(project.join("scripts/papertiger.cmd"))
                .unwrap()
                .contains(r"%PAPERTIGER_ROOT%\plans\papertiger.sqlite")
        );
        let ignore = fs::read_to_string(project.join(".gitignore")).unwrap();
        assert!(
            ignore
                .lines()
                .any(|line| line == "/plans/papertiger.sqlite")
        );

        fs::remove_file(project.join(".agents/skills/papertiger/SKILL.md")).unwrap();
        let repaired = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(repaired.operation, SetupOperation::Repair);
        assert_eq!(repaired.authority_path, "plans/papertiger.sqlite");
        fs::write(&binary, b"papertiger-binary-v2").unwrap();
        let upgraded = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(upgraded.operation, SetupOperation::Upgrade);
        assert_eq!(upgraded.authority_path, "plans/papertiger.sqlite");
        assert_eq!(
            fs::read(project.join("plans/papertiger.sqlite")).unwrap(),
            b"existing-custom-authority"
        );
        cleanup(&project);
    }

    #[test]
    fn authority_path_refuses_nonportable_or_escaping_values() {
        let (project, binary) = fixture("authority-path-refusal");
        for invalid in [
            "../outside.sqlite",
            "plans/space name.sqlite",
            "/root.sqlite",
            "SCRIPTS/PAPERTIGER",
            "tools/papertiger/bin/papertiger.exe",
            ".agents/skills/papertiger/state.sqlite",
            ".git/papertiger.sqlite",
            "state/papertiger-mise.sqlite",
            "state/papertiger-mise-objects/object",
            "plans/con.sqlite",
            "plans/trailing.",
        ] {
            let mut invalid_request = request(&project, &binary);
            invalid_request.authority_path = Some(Path::new(invalid));
            let error = setup_project(invalid_request).unwrap_err();
            assert!(error.to_string().contains("--authority-path"), "{error:#}");
        }
        assert!(!project.join("scripts").exists());
        cleanup(&project);
    }

    #[test]
    fn existing_non_file_authority_path_refuses_before_setup_writes() {
        let (project, binary) = fixture("authority-directory");
        fs::create_dir(project.join("plans")).unwrap();
        let mut invalid = request(&project, &binary);
        invalid.authority_path = Some(Path::new("plans"));
        let error = setup_project(invalid).unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
        assert!(!project.join("scripts").exists());
        cleanup(&project);
    }

    #[test]
    fn canonical_text_hashes_accept_crlf_checkout_conversion() {
        fn as_crlf(text: &str) -> String {
            text.replace("\r\n", "\n").replace('\n', "\r\n")
        }

        let (project, binary) = fixture("crlf-checkout");
        setup_project(request(&project, &binary)).unwrap();
        let receipt = load_install_receipt(&project.join(INSTALL_RECEIPT_PATH))
            .unwrap()
            .unwrap();
        let mut original_bytes = Vec::new();
        for file in &receipt.managed_files {
            let path = project.join(&file.path);
            let text = fs::read_to_string(&path).unwrap();
            fs::write(path, as_crlf(&text)).unwrap();
            original_bytes.push((
                file.path.clone(),
                fs::read(project.join(&file.path)).unwrap(),
            ));
        }
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let receipt_text = fs::read_to_string(&receipt_path).unwrap();
        fs::write(&receipt_path, as_crlf(&receipt_text)).unwrap();
        let original_receipt = fs::read(&receipt_path).unwrap();

        let current = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(current.operation, SetupOperation::Unchanged);
        assert!(!current.actions.iter().any(|action| {
            action.action == SetupActionKind::ModifiedRefusal || action.requires_replace_managed
        }));
        for (path, bytes) in original_bytes {
            assert_eq!(fs::read(project.join(path)).unwrap(), bytes);
        }
        assert_eq!(fs::read(receipt_path).unwrap(), original_receipt);
        cleanup(&project);
    }

    #[test]
    fn managed_text_equivalence_never_weakens_binary_identity() {
        let (project, _) = fixture("binary-line-ending-identity");
        let destination = project.join("tools/papertiger/bin/papertiger.exe");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"binary\r\npayload").unwrap();

        let (text_action, _) = preflight_managed_file(
            &destination,
            &ManagedFile {
                relative_path: PathBuf::from("scripts/papertiger.cmd"),
                content: b"binary\npayload".to_vec(),
                executable: false,
                content_kind: ManagedContentKind::ReceiptText,
            },
            None,
            false,
            true,
            false,
        )
        .unwrap();
        assert_eq!(text_action, SetupActionKind::Unchanged);

        let (binary_action, requires_replacement) = preflight_managed_file(
            &destination,
            &ManagedFile {
                relative_path: PathBuf::from("tools/papertiger/bin/papertiger.exe"),
                content: b"binary\npayload".to_vec(),
                executable: false,
                content_kind: ManagedContentKind::RuntimeBinary,
            },
            None,
            false,
            true,
            false,
        )
        .unwrap();
        assert_eq!(binary_action, SetupActionKind::ModifiedRefusal);
        assert!(requires_replacement);
        cleanup(&project);
    }

    #[test]
    fn managed_text_rendering_is_checkout_line_ending_independent() {
        let cases = [
            (
                BASH_LAUNCHER_TEMPLATE,
                "@PAPERTIGER_AUTHORITY_PATH@",
                "plans/papertiger.sqlite",
            ),
            (
                WINDOWS_LAUNCHER_TEMPLATE,
                "@PAPERTIGER_AUTHORITY_PATH_WINDOWS@",
                "plans\\papertiger.sqlite",
            ),
        ];
        for (template, token, value) in cases {
            let lf = template.replace("\r\n", "\n");
            let crlf = lf.replace('\n', "\r\n");
            let from_lf = render_launcher(&lf, token, value);
            let from_crlf = render_launcher(&crlf, token, value);
            assert_eq!(from_lf, from_crlf);
            assert!(!from_lf.contains('\r'));
        }

        for guidance in [AGENT_INTEGRATION, AGENT_SKILL] {
            let lf = String::from_utf8_lossy(guidance).replace("\r\n", "\n");
            let crlf = lf.replace('\n', "\r\n");
            assert_eq!(
                canonical_managed_text(lf.as_bytes()),
                canonical_managed_text(crlf.as_bytes())
            );
        }
    }

    #[test]
    fn gitignore_managed_block_reasserts_policy_after_later_negation() {
        let (project, binary) = fixture("gitignore-negation");
        setup_project(request(&project, &binary)).unwrap();
        let gitignore_path = project.join(".gitignore");
        let mut gitignore = fs::read_to_string(&gitignore_path).unwrap();
        gitignore.push_str("!/state/papertiger.sqlite\n");
        fs::write(&gitignore_path, gitignore).unwrap();

        let repaired = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(repaired.operation, SetupOperation::Repair);
        assert!(repaired.actions.iter().any(|action| {
            action.path == ".gitignore" && action.action == SetupActionKind::UpdateGitignore
        }));
        let updated = fs::read_to_string(&gitignore_path).unwrap();
        assert!(updated.ends_with(&format!("{GITIGNORE_END_COMMENT}\n")));
        assert_eq!(
            updated
                .lines()
                .filter(|line| *line == GITIGNORE_COMMENT)
                .count(),
            1
        );
        assert_eq!(
            updated
                .lines()
                .filter(|line| *line == GITIGNORE_END_COMMENT)
                .count(),
            1
        );
        assert!(
            updated.rfind("/state/papertiger.sqlite\n").unwrap()
                > updated.rfind("!/state/papertiger.sqlite\n").unwrap()
        );
        assert_eq!(
            setup_project(request(&project, &binary)).unwrap().operation,
            SetupOperation::Unchanged
        );
        cleanup(&project);
    }

    #[test]
    fn gitignore_managed_block_replaces_owned_entries_during_upgrade() {
        let existing = format!(
            "target/\n\n{GITIGNORE_COMMENT}\n/obsolete-papertiger-state.sqlite\n{GITIGNORE_END_COMMENT}\n"
        );
        let entries = gitignore_entries("plans/papertiger.sqlite");
        let updated = gitignore_content(Some(&existing), &entries).unwrap();

        assert!(updated.starts_with("target/\n\n"));
        assert!(!updated.contains("obsolete-papertiger-state"));
        assert!(updated.contains("/plans/papertiger.sqlite\n"));
        assert_eq!(updated.matches(GITIGNORE_COMMENT).count(), 1);
        assert_eq!(updated.matches(GITIGNORE_END_COMMENT).count(), 1);
        assert_eq!(
            gitignore_content(Some(&updated), &entries).unwrap(),
            updated
        );
    }

    #[test]
    fn pre_receipt_gitignore_suffix_migrates_to_one_bounded_current_block() {
        let legacy_entries = PRE_RECEIPT_GITIGNORE_ENTRIES.join("\n");
        let existing = format!("target/\n\n{GITIGNORE_COMMENT}\n{legacy_entries}\n");
        let entries = gitignore_entries("plans/papertiger.sqlite");
        let updated = gitignore_content(Some(&existing), &entries).unwrap();

        assert!(updated.starts_with("target/\n\n"));
        assert!(updated.contains("/plans/papertiger.sqlite\n"));
        assert!(
            !updated
                .lines()
                .any(|line| line == "/state/papertiger.sqlite")
        );
        assert_eq!(updated.matches(GITIGNORE_COMMENT).count(), 1);
        assert_eq!(updated.matches(GITIGNORE_END_COMMENT).count(), 1);
        assert!(updated.ends_with(&format!("{GITIGNORE_END_COMMENT}\n")));
    }

    #[test]
    fn malformed_or_duplicate_gitignore_markers_refuse_before_setup_writes() {
        let malformed = [
            format!("{GITIGNORE_COMMENT}\n/repository-owned-rule\n"),
            format!("{GITIGNORE_END_COMMENT}\n"),
            format!("{GITIGNORE_END_COMMENT}\n{GITIGNORE_COMMENT}\n"),
            format!(
                "{GITIGNORE_COMMENT}\n{GITIGNORE_END_COMMENT}\n{GITIGNORE_COMMENT}\n{GITIGNORE_END_COMMENT}\n"
            ),
        ];
        for (index, content) in malformed.iter().enumerate() {
            let (project, binary) = fixture(&format!("malformed-gitignore-{index}"));
            fs::write(project.join(".gitignore"), content).unwrap();

            let error = setup_project(request(&project, &binary)).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("malformed or duplicate Papertiger managed markers"),
                "{error:#}"
            );
            assert!(!project.join("scripts").exists());
            cleanup(&project);
        }
    }

    #[test]
    fn receipt_bound_authority_cannot_be_silently_rebound() {
        let (project, binary) = fixture("authority-rebind-refusal");
        setup_project(request(&project, &binary)).unwrap();
        let receipt_before = fs::read(project.join(INSTALL_RECEIPT_PATH)).unwrap();
        let launcher_before = fs::read(project.join("scripts/papertiger")).unwrap();

        let mut rebind = request(&project, &binary);
        rebind.authority_path = Some(Path::new("plans/papertiger.sqlite"));
        let error = setup_project(rebind).unwrap_err();
        assert!(error.to_string().contains("will not rebind"));
        assert_eq!(
            fs::read(project.join(INSTALL_RECEIPT_PATH)).unwrap(),
            receipt_before
        );
        assert_eq!(
            fs::read(project.join("scripts/papertiger")).unwrap(),
            launcher_before
        );
        cleanup(&project);
    }

    #[cfg(unix)]
    #[test]
    fn authority_path_refuses_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let (project, binary) = fixture("authority-symlink");
        let external = project.parent().unwrap().join("external");
        fs::create_dir(&external).unwrap();
        symlink(&external, project.join("plans")).unwrap();
        let mut symlinked = request(&project, &binary);
        symlinked.authority_path = Some(Path::new("plans/papertiger.sqlite"));
        let error = setup_project(symlinked).unwrap_err();
        assert!(
            format!("{error:#}").contains("refuses symlinked managed path"),
            "{error:#}"
        );
        assert!(!project.join("scripts").exists());
        cleanup(&project);
    }

    #[test]
    fn non_file_gitignore_refuses_before_managed_writes() {
        let (project, binary) = fixture("gitignore-directory");
        fs::create_dir(project.join(".gitignore")).unwrap();
        let error = setup_project(request(&project, &binary)).unwrap_err();
        assert!(error.to_string().contains("cannot preserve non-file"));
        assert!(!project.join("scripts").exists());
        cleanup(&project);
    }

    #[test]
    fn non_directory_managed_parent_refuses_before_any_write() {
        let (project, binary) = fixture("managed-parent-file");
        fs::write(project.join("scripts"), b"not a directory").unwrap();

        let error = setup_project(request(&project, &binary)).unwrap_err();
        assert!(error.to_string().contains("parent path is not a directory"));
        assert!(!project.join("tools").exists());
        assert_eq!(
            fs::read(project.join("scripts")).unwrap(),
            b"not a directory"
        );
        cleanup(&project);
    }
}
