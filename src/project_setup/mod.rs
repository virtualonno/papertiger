use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::ValueEnum;
use serde::Serialize;

mod filesystem;
mod receipt;
mod runtime_receipt;
mod uninstall;

pub(crate) use uninstall::{UninstallProjectRequest, uninstall_project};

use filesystem::{
    canonical_text, content_matches, ensure_executable, preflight_managed_file,
    preflight_text_file, text_matches, validate_destination, write_file, write_new_file,
};
use receipt::{
    InstallReceipt, SkillTarget, build_install_receipt, load_install_receipt, receipt_bytes,
    refuse_release_downgrade,
};
use runtime_receipt::{
    RuntimeInstallReceipt, build_runtime_install_receipt, current_host_binary_path,
    load_runtime_install_receipt, preflight_runtime_receipt, runtime_receipt_bytes,
    runtime_receipt_relative_path, verify_runtime_installation, write_runtime_receipt,
};

const AGENT_INTEGRATION: &[u8] = include_bytes!("../../templates/agent_integration.md");
const AGENT_SKILL: &[u8] = include_bytes!("../../templates/skills/papertiger/SKILL.md");
const AGENT_INTEGRATION_PATH: &str = "tools/papertiger/agent_integration.md";
const INSTALL_RECEIPT_PATH: &str = "tools/papertiger/project-install.json";
const DEFAULT_AUTHORITY_PATH: &str = "state/papertiger.sqlite";
// Compatibility is anchored in the shared `.agents/skills` contract. The
// bootstrap catalog only selects that shared residence for common compatible
// harnesses before `.agents` exists; it never creates harness-native copies.
const SHARED_AGENT_SKILLS_DIRECTORIES: &[&str] = &[".agents"];
const COMMON_AGENT_SKILLS_BOOTSTRAP_DIRECTORIES: &[&str] =
    &[".codex", ".cursor", ".pi", ".omp", ".opencode"];
const COMMON_AGENT_SKILLS_BOOTSTRAP_FILES: &[&str] =
    &["AGENTS.md", "opencode.json", "opencode.jsonc"];
const CLAUDE_HARNESS_DIRECTORIES: &[&str] = &[".claude"];
const CLAUDE_HARNESS_FILES: &[&str] = &["CLAUDE.md"];
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

#[derive(Debug)]
pub(crate) struct SetupProjectRequest<'a> {
    pub(crate) project_root: &'a Path,
    /// Defaults to the running Papertiger executable. Tests can provide an
    /// isolated release fixture.
    pub(crate) source_binary: Option<&'a Path>,
    pub(crate) dry_run: bool,
    pub(crate) authority_path: Option<&'a Path>,
    pub(crate) skill_target: Option<SkillTargetRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SkillTargetRequest {
    Auto,
    Agents,
    Claude,
    Both,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupOperation {
    Install,
    Repair,
    Upgrade,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupActionKind {
    Create,
    Replace,
    Unchanged,
    MakeExecutable,
    UpdateGitignore,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SetupAction {
    pub(crate) path: String,
    pub(crate) action: SetupActionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SetupProjectResult {
    pub(crate) schema: &'static str,
    pub(crate) version: &'static str,
    pub(crate) project_root: String,
    pub(crate) authority_path: String,
    pub(crate) skill_targets: Vec<SkillTarget>,
    pub(crate) runtime_receipt_path: String,
    pub(crate) runtime_install: RuntimeInstallReceipt,
    pub(crate) dry_run: bool,
    pub(crate) operation: SetupOperation,
    pub(crate) actions: Vec<SetupAction>,
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
    Text,
}

/// Install a project-local release without touching an existing authority.
/// The binary, reference, and selected skills are release-owned and written
/// unconditionally; every destination is preflighted before the first write,
/// including symlink traversal and `.gitignore`.
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
    let binary_relative = PathBuf::from(format!("tools/papertiger/bin/papertiger{suffix}"));
    let desired_runtime_receipt = build_runtime_install_receipt(&binary_relative, &binary);
    let desired_runtime_receipt_bytes = runtime_receipt_bytes(&desired_runtime_receipt)?;

    let receipt_relative = Path::new(INSTALL_RECEIPT_PATH);
    validate_destination(&root, receipt_relative)?;
    let receipt_path = root.join(receipt_relative);
    let runtime_receipt_relative = runtime_receipt_relative_path(&binary_relative)?;
    validate_destination(&root, &runtime_receipt_relative)?;
    let runtime_receipt_path = root.join(&runtime_receipt_relative);
    let prior_receipt = load_install_receipt(&receipt_path)?;
    if let Some(receipt) = prior_receipt.as_ref() {
        refuse_release_downgrade(receipt)?;
    }
    let authority_path = select_authority_path(request.authority_path, prior_receipt.as_ref())?;
    let skill_targets = select_skill_targets(&root, request.skill_target, prior_receipt.as_ref());
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
    let mut managed = vec![
        ManagedFile {
            relative_path: binary_relative,
            content: binary,
            executable: true,
            content_kind: ManagedContentKind::RuntimeBinary,
        },
        ManagedFile {
            relative_path: PathBuf::from(AGENT_INTEGRATION_PATH),
            content: canonical_text(AGENT_INTEGRATION).into_owned(),
            executable: false,
            content_kind: ManagedContentKind::Text,
        },
    ];
    for target in &skill_targets {
        managed.push(ManagedFile {
            relative_path: PathBuf::from(target.managed_path()),
            content: canonical_text(AGENT_SKILL).into_owned(),
            executable: false,
            content_kind: ManagedContentKind::Text,
        });
    }

    let desired_receipt = build_install_receipt(&authority_path, &skill_targets);
    let desired_receipt_bytes = receipt_bytes(&desired_receipt)?;
    let had_managed_files = managed
        .iter()
        .any(|file| root.join(&file.relative_path).exists())
        || receipt_path.exists()
        || runtime_receipt_path.exists();

    let mut managed_actions = Vec::with_capacity(managed.len());
    for file in &managed {
        validate_destination(&root, &file.relative_path)?;
        let action = preflight_managed_file(&root.join(&file.relative_path), file)?;
        managed_actions.push(SetupAction {
            path: normalized_path(&file.relative_path),
            action,
        });
    }

    // A skill target the prior receipt selected but this run deselects is
    // release-owned, so its file is removed with the selection.
    let mut deselected = Vec::new();
    if let Some(prior) = &prior_receipt {
        for target in &prior.skill_targets {
            if skill_targets.contains(target) {
                continue;
            }
            let relative = Path::new(target.managed_path());
            validate_destination(&root, relative)?;
            let destination = root.join(relative);
            if !destination.exists() {
                continue;
            }
            if !destination.is_file() {
                return Err(anyhow!(
                    "deselected skill path is not a file: {}; move it aside, then rerun setup-project",
                    destination.display()
                ));
            }
            deselected.push(SetupAction {
                path: normalized_path(relative),
                action: SetupActionKind::Remove,
            });
        }
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

    let receipt_action = preflight_text_file(&receipt_path, &desired_receipt_bytes)?;
    let runtime_receipt_action =
        preflight_runtime_receipt(&runtime_receipt_path, &desired_runtime_receipt_bytes)?;

    let mut actions = managed_actions.clone();
    actions.extend(deselected.iter().cloned());
    actions.push(SetupAction {
        path: normalized_path(gitignore_relative),
        action: gitignore_action,
    });
    actions.push(SetupAction {
        path: INSTALL_RECEIPT_PATH.to_owned(),
        action: receipt_action,
    });
    actions.push(SetupAction {
        path: normalized_path(&runtime_receipt_relative),
        action: runtime_receipt_action,
    });
    let operation = setup_operation(
        prior_receipt.as_ref(),
        &desired_receipt,
        had_managed_files,
        managed_actions[0].action,
        &actions,
    );

    if !request.dry_run {
        for (file, action) in managed.iter().zip(&managed_actions) {
            let destination = root.join(&file.relative_path);
            match action.action {
                SetupActionKind::Create => write_new_file(&destination, &file.content)?,
                SetupActionKind::Replace => write_file(&destination, &file.content, false)?,
                SetupActionKind::MakeExecutable | SetupActionKind::Unchanged => {}
                SetupActionKind::UpdateGitignore | SetupActionKind::Remove => {
                    unreachable!("selected managed files are only created or replaced")
                }
            }
            if file.executable {
                ensure_executable(&destination)?;
            }
        }
        for action in &deselected {
            let destination = root.join(&action.path);
            fs::remove_file(&destination).with_context(|| {
                format!("remove deselected skill file {}", destination.display())
            })?;
        }
        if !matches!(gitignore_action, SetupActionKind::Unchanged) {
            write_file(
                &gitignore_path,
                updated_gitignore.as_bytes(),
                existing_gitignore.is_none(),
            )?;
        }
        write_install_receipt(&receipt_path, &desired_receipt_bytes, receipt_action)?;
        verify_installation(&root, &managed)?;
        // This ignored receipt is the final installation commit marker. If a
        // crash occurs before it is written, ordinary commands refuse the
        // incomplete installation and direct the operator back to setup.
        write_runtime_receipt(
            &runtime_receipt_path,
            &desired_runtime_receipt_bytes,
            runtime_receipt_action,
        )?;
        verify_runtime_installation(&root, &desired_runtime_receipt)?;
    }

    let authority_exists = root.join(Path::new(&authority_path)).is_file();
    let next_actions = if request.dry_run {
        let mut apply_command = format!("papertiger setup-project \"{}\"", normalized_path(&root));
        if prior_receipt.is_none() && request.authority_path.is_some() {
            apply_command.push_str(&format!(" --authority-path {authority_path}"));
        }
        apply_command.push_str(&format!(
            " --skill-target {}",
            skill_target_label(&skill_targets)
        ));
        if operation == SetupOperation::Unchanged {
            vec![
                "Preview is unchanged; no setup-project apply is needed.".to_owned(),
                "No authority was initialized or migrated.".to_owned(),
            ]
        } else {
            vec![
                format!("Preview is ready; apply with: {apply_command}"),
                "Dry-run created no receipt and did not initialize or migrate authority."
                    .to_owned(),
            ]
        }
    } else {
        let mut applied = vec![format!(
            "Invoke the installed native binary at tools/papertiger/bin/papertiger (papertiger.exe on Windows). It discovers this project receipt and binds {authority_path} when called from the project root or any nested directory; no shell launcher is required."
        )];
        if skill_targets.is_empty() {
            applied.push(
                "No skill envelope was selected. Use setup-user for personal discovery or rerun setup-project with --skill-target agents for a project skill. Neither requires edits to AGENTS.md or CLAUDE.md."
                    .to_owned(),
            );
        } else {
            applied.push(format!(
                "Start a fresh agent session and verify the installed Papertiger skill for {} is listed. Skills-capable harnesses need no project guidance edit; setup-project never edits AGENTS.md or CLAUDE.md.",
                skill_target_label(&skill_targets)
            ));
        }
        if authority_exists {
            applied.push(
                "Run the installed Papertiger binary with status, focus, and audit; setup-project never migrates or replaces the existing authority."
                    .to_owned(),
            );
        } else {
            applied.push(
                "If this project has never had a Papertiger authority, set PAPERTIGER_ACTOR and run the installed Papertiger binary with init once; if prior work should exist, stop instead of creating a replacement authority."
                    .to_owned(),
            );
        }
        applied.push(format!(
            "Commit the project-install receipt, integration contract, selected skill envelopes, and additive .gitignore policy. Keep tools/papertiger/bin and {authority_path} host-local and outside Git; setup-project writes ignore rules but never changes existing index entries."
        ));
        applied
    };

    Ok(SetupProjectResult {
        schema: "papertiger.project_install_result.v7",
        version: env!("CARGO_PKG_VERSION"),
        project_root: normalized_path(&root),
        authority_path,
        skill_targets,
        runtime_receipt_path: normalized_path(&runtime_receipt_relative),
        runtime_install: desired_runtime_receipt,
        dry_run: request.dry_run,
        operation,
        actions,
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

fn select_skill_targets(
    root: &Path,
    requested: Option<SkillTargetRequest>,
    prior_receipt: Option<&InstallReceipt>,
) -> Vec<SkillTarget> {
    let mut targets = match requested {
        None => prior_receipt
            .map(|receipt| receipt.skill_targets.clone())
            .unwrap_or_else(|| detect_skill_targets(root)),
        Some(SkillTargetRequest::Auto) => detect_skill_targets(root),
        Some(SkillTargetRequest::Agents) => vec![SkillTarget::Agents],
        Some(SkillTargetRequest::Claude) => vec![SkillTarget::Claude],
        Some(SkillTargetRequest::Both) => vec![SkillTarget::Agents, SkillTarget::Claude],
        Some(SkillTargetRequest::None) => Vec::new(),
    };
    if targets.contains(&SkillTarget::Claude) && !targets.contains(&SkillTarget::Agents) {
        targets.insert(0, SkillTarget::Agents);
    }
    targets
}

fn detect_skill_targets(root: &Path) -> Vec<SkillTarget> {
    let has_agents = has_directory_marker(root, SHARED_AGENT_SKILLS_DIRECTORIES)
        || has_directory_marker(root, COMMON_AGENT_SKILLS_BOOTSTRAP_DIRECTORIES)
        || has_file_marker(root, COMMON_AGENT_SKILLS_BOOTSTRAP_FILES);
    let has_claude = has_directory_marker(root, CLAUDE_HARNESS_DIRECTORIES)
        || has_file_marker(root, CLAUDE_HARNESS_FILES);
    match (has_agents, has_claude) {
        (true, true) => vec![SkillTarget::Agents, SkillTarget::Claude],
        (true, false) => vec![SkillTarget::Agents],
        (false, true) => vec![SkillTarget::Claude],
        (false, false) => Vec::new(),
    }
}

fn has_directory_marker(root: &Path, markers: &[&str]) -> bool {
    markers.iter().any(|marker| root.join(marker).is_dir())
}

fn has_file_marker(root: &Path, markers: &[&str]) -> bool {
    markers.iter().any(|marker| root.join(marker).is_file())
}

fn skill_target_label(skill_targets: &[SkillTarget]) -> &'static str {
    match skill_targets {
        [] => "none",
        [SkillTarget::Agents] => "agents",
        [SkillTarget::Claude] => "claude",
        [SkillTarget::Agents, SkillTarget::Claude] => "both",
        _ => unreachable!("validated skill target selection has canonical order"),
    }
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
                "--authority-path component {part:?} must be a portable non-device name using only ASCII letters, digits, '.', '_', or '-' with no trailing dot, so every native binary selects the same file"
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
            ".git" | ".gitignore" | "tools" | ".agents" | ".claude"
        )
    });
    let overlaps_mise = normalized_lower.starts_with("state/papertiger-mise.sqlite")
        || normalized_lower == "state/papertiger-mise-objects"
        || normalized_lower.starts_with("state/papertiger-mise-objects/");
    if reserved_top_level
        || overlaps_mise
        || normalized_lower == INSTALL_RECEIPT_PATH.to_ascii_lowercase()
    {
        return Err(anyhow!(
            "--authority-path {normalized} overlaps setup-managed content or the separate Papertiger Mise authority; choose a dedicated planner database path such as {DEFAULT_AUTHORITY_PATH}"
        ));
    }
    Ok(normalized)
}

/// Resolve the nearest setup-managed project authority from `start`, walking
/// upward like Git. Explicit `--db` and `PAPERTIGER_DB` remain higher-priority
/// caller overrides in the CLI.
pub(crate) fn discover_project_authority(start: &Path) -> Result<Option<PathBuf>> {
    let start = fs::canonicalize(start)
        .with_context(|| format!("resolve current directory {}", start.display()))?;
    for root in start.ancestors() {
        let Some(receipt) = load_running_project_receipt(root)? else {
            if crate::project_bundle::verify(root)? {
                validate_destination(root, Path::new(DEFAULT_AUTHORITY_PATH))?;
                return Ok(Some(root.join(DEFAULT_AUTHORITY_PATH)));
            }
            continue;
        };
        return receipt_authority(root, &receipt).map(Some);
    }
    Ok(None)
}

/// Resolve the authority at one exact project root: its project-install
/// receipt, else its release bundle, else an existing default authority file.
/// Unlike upward discovery, this never walks into a parent project. The
/// receipt and bundle branches may name an authority `init` has yet to create;
/// the fallback branch selects only a database that already exists.
pub(crate) fn project_authority(project_root: &Path) -> Result<PathBuf> {
    let root = fs::canonicalize(project_root)
        .with_context(|| format!("resolve explicit project root {}", project_root.display()))?;
    if !root.is_dir() {
        return Err(anyhow!(
            "explicit project root is not a directory: {}; pass the installed project directory",
            root.display()
        ));
    }

    let receipt_path = root.join(INSTALL_RECEIPT_PATH);
    if !receipt_path.exists() {
        let default_authority = Path::new(DEFAULT_AUTHORITY_PATH);
        if crate::project_bundle::verify(&root)? {
            validate_destination(&root, default_authority)?;
            return Ok(root.join(default_authority));
        }
        validate_destination(&root, default_authority).with_context(|| {
            format!(
                "validate {DEFAULT_AUTHORITY_PATH} beneath explicit project root {}",
                root.display()
            )
        })?;
        let authority = root.join(default_authority);
        if authority.is_file() {
            return Ok(authority);
        }
        if authority.exists() {
            return Err(anyhow!(
                "{} exists but is not a regular file; restore the project authority, or pass --db <path> for a database elsewhere",
                authority.display()
            ));
        }
        return Err(anyhow!(
            "no project-install receipt, release bundle, or existing {DEFAULT_AUTHORITY_PATH} was found at {}; pass the exact installed project root, restore its authority if prior planning existed, or inspect a first installation with: papertiger setup-project \"{}\" --dry-run --json",
            root.display(),
            root.display()
        ));
    }
    let receipt = load_running_project_receipt(&root)?.ok_or_else(|| {
        anyhow!(
            "project-install receipt {} disappeared while it was read; rerun the command",
            receipt_path.display()
        )
    })?;
    receipt_authority(&root, &receipt)
}

pub(crate) fn discover_project_root(start: &Path) -> Result<Option<PathBuf>> {
    let start = fs::canonicalize(start)
        .with_context(|| format!("resolve current directory {}", start.display()))?;
    for root in start.ancestors() {
        if load_running_project_receipt(root)?.is_some() || crate::project_bundle::verify(root)? {
            return Ok(Some(root.to_path_buf()));
        }
    }
    Ok(None)
}

fn load_running_project_receipt(root: &Path) -> Result<Option<InstallReceipt>> {
    let receipt_path = root.join(INSTALL_RECEIPT_PATH);
    let Some(receipt) = load_install_receipt(&receipt_path)? else {
        return Ok(None);
    };
    // A complete overlay replaces release files, not the project's authority selection.
    if crate::project_bundle::verify(root)? {
        return Ok(Some(receipt));
    }
    let running = env!("CARGO_PKG_VERSION");
    if receipt.papertiger_version != running {
        return Err(anyhow!(
            "project-install receipt at {} requires Papertiger {}, but the running binary is {running}; upgrade the project deliberately with: papertiger setup-project \"{}\"",
            receipt_path.display(),
            receipt.papertiger_version,
            root.display()
        ));
    }
    let runtime_receipt_path =
        root.join(runtime_receipt_relative_path(&current_host_binary_path())?);
    let runtime_receipt = load_runtime_install_receipt(&runtime_receipt_path).with_context(|| {
        format!(
            "validate host-local identity for project-install receipt {}; repair this installation with: papertiger setup-project \"{}\"",
            receipt_path.display(),
            root.display()
        )
    })?;
    verify_runtime_installation(root, &runtime_receipt).with_context(|| {
        format!(
            "validate host-local identity for project-install receipt {}; repair this installation with: papertiger setup-project \"{}\"",
            receipt_path.display(),
            root.display()
        )
    })?;
    Ok(Some(receipt))
}

fn receipt_authority(root: &Path, receipt: &InstallReceipt) -> Result<PathBuf> {
    let receipt_path = root.join(INSTALL_RECEIPT_PATH);
    let authority = Path::new(&receipt.authority_path);
    validate_destination(root, authority).with_context(|| {
        format!(
            "validate authority {} selected by project-install receipt {}",
            receipt.authority_path,
            receipt_path.display()
        )
    })?;
    Ok(root.join(authority))
}

fn setup_operation(
    prior: Option<&InstallReceipt>,
    desired: &InstallReceipt,
    had_managed_files: bool,
    binary_action: SetupActionKind,
    actions: &[SetupAction],
) -> SetupOperation {
    match prior {
        None if had_managed_files => SetupOperation::Upgrade,
        None => SetupOperation::Install,
        Some(prior) if prior != desired || binary_action == SetupActionKind::Replace => {
            SetupOperation::Upgrade
        }
        Some(_)
            if actions
                .iter()
                .any(|action| action.action != SetupActionKind::Unchanged) =>
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
    if !text_matches(&installed, content) {
        return Err(anyhow!(
            "project-install receipt verification failed at {}; rerun setup-project after checking the filesystem",
            path.display()
        ));
    }
    Ok(())
}

fn verify_installation(root: &Path, managed: &[ManagedFile]) -> Result<()> {
    for file in managed {
        let path = root.join(&file.relative_path);
        let installed = fs::read(&path)
            .with_context(|| format!("verify installed managed file {}", path.display()))?;
        if !content_matches(file.content_kind, &installed, &file.content) {
            return Err(anyhow!(
                "setup-project verification found unexpected content at {}; rerun setup-project after checking the filesystem",
                path.display()
            ));
        }
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
        ([(start, _)], [(_, end)]) if start < end => {
            let suffix = &existing[*end..];
            if !suffix
                .lines()
                .any(|line| line.trim_start().starts_with('!'))
            {
                // Positive rules cannot undo the owned protection. Preserve the
                // repository's ordering unless a later negation may override it.
                let mut content = existing[..*start].to_owned();
                content.push_str(&managed_block);
                content.push_str(suffix);
                return Ok(content);
            }
            let mut content = existing[..*start].to_owned();
            content.push_str(suffix);
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
            authority_path: None,
            skill_target: Some(SkillTargetRequest::Both),
        }
    }

    fn cleanup(project: &Path) {
        fs::remove_dir_all(project.parent().unwrap()).unwrap();
    }

    #[test]
    fn dry_run_reports_without_writing() {
        let (project, binary) = fixture("dry-run");
        let mut request = request(&project, &binary);
        request.dry_run = true;
        let result = setup_project(request).unwrap();
        assert!(result.dry_run);
        assert_eq!(result.schema, "papertiger.project_install_result.v7");
        assert_eq!(result.runtime_install.binary.bytes, 17);
        assert_eq!(
            result.runtime_install.binary.sha256,
            papertiger::sha256(b"papertiger-binary")
        );
        assert_eq!(
            result.runtime_receipt_path,
            format!(
                "tools/papertiger/bin/papertiger{}.runtime-install.json",
                std::env::consts::EXE_SUFFIX
            )
        );
        assert!(
            result
                .actions
                .iter()
                .all(|action| action.action == SetupActionKind::Create)
        );
        assert!(!project.join("tools").exists());
        assert!(!project.join(".gitignore").exists());
        cleanup(&project);
    }

    #[test]
    fn ready_dry_run_replays_reviewed_authority_path() {
        let (project, binary) = fixture("dry-run-replay");
        fs::create_dir_all(project.join("plans")).unwrap();
        fs::write(
            project.join("plans/papertiger.sqlite"),
            b"existing-authority",
        )
        .unwrap();

        let mut preview_request = request(&project, &binary);
        preview_request.dry_run = true;
        preview_request.authority_path = Some(Path::new("plans/papertiger.sqlite"));
        let preview = setup_project(preview_request).unwrap();
        assert_eq!(preview.operation, SetupOperation::Install);
        assert!(preview.next_actions.iter().any(|action| {
            action
                == &format!(
                    "Preview is ready; apply with: papertiger setup-project \"{}\" --authority-path plans/papertiger.sqlite --skill-target both",
                    normalized_path(&project)
                )
        }));
        assert!(!project.join("tools").exists());

        let mut apply_request = request(&project, &binary);
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
    fn auto_skill_targets_follow_existing_harness_markers_without_guessing() {
        let cases: &[(&str, &[&str], Vec<SkillTarget>)] = &[
            ("unmarked", &[], Vec::new()),
            ("agents-guidance", &["AGENTS.md"], vec![SkillTarget::Agents]),
            ("agent-skills", &[".agents/"], vec![SkillTarget::Agents]),
            ("codex", &[".codex/"], vec![SkillTarget::Agents]),
            ("cursor", &[".cursor/"], vec![SkillTarget::Agents]),
            ("pi", &[".pi/"], vec![SkillTarget::Agents]),
            ("omp", &[".omp/"], vec![SkillTarget::Agents]),
            (
                "opencode-directory",
                &[".opencode/"],
                vec![SkillTarget::Agents],
            ),
            (
                "opencode-json",
                &["opencode.json"],
                vec![SkillTarget::Agents],
            ),
            (
                "opencode-jsonc",
                &["opencode.jsonc"],
                vec![SkillTarget::Agents],
            ),
            (
                "claude",
                &["CLAUDE.md"],
                vec![SkillTarget::Agents, SkillTarget::Claude],
            ),
            (
                "both",
                &["AGENTS.md", "CLAUDE.md"],
                vec![SkillTarget::Agents, SkillTarget::Claude],
            ),
        ];
        for (name, markers, expected) in cases {
            let (project, binary) = fixture(&format!("auto-{name}"));
            for marker in *markers {
                if let Some(directory) = marker.strip_suffix('/') {
                    fs::create_dir_all(project.join(directory)).unwrap();
                } else {
                    fs::write(project.join(marker), "repository harness marker\n").unwrap();
                }
            }
            let mut auto = request(&project, &binary);
            auto.skill_target = None;
            let result = setup_project(auto).unwrap();
            assert_eq!(&result.skill_targets, expected, "case {name}");
            assert_eq!(
                project.join(".agents/skills/papertiger/SKILL.md").exists(),
                expected.contains(&SkillTarget::Agents),
                "case {name}"
            );
            assert_eq!(
                project.join(".claude/skills/papertiger/SKILL.md").exists(),
                expected.contains(&SkillTarget::Claude),
                "case {name}"
            );
            for duplicate in [
                ".codex/skills/papertiger/SKILL.md",
                ".pi/skills/papertiger/SKILL.md",
                ".omp/skills/papertiger/SKILL.md",
                ".opencode/skills/papertiger/SKILL.md",
            ] {
                assert!(
                    !project.join(duplicate).exists(),
                    "auto detection must not create a duplicate harness-native skill at {duplicate}"
                );
            }
            cleanup(&project);
        }
    }

    #[test]
    fn auto_skill_targets_require_marker_file_types() {
        let (project, binary) = fixture("auto-marker-types");
        fs::write(project.join(".pi"), "not a harness directory\n").unwrap();
        fs::create_dir(project.join("opencode.json")).unwrap();
        fs::write(project.join(".claude"), "not a harness directory\n").unwrap();
        fs::create_dir(project.join("CLAUDE.md")).unwrap();

        let mut auto = request(&project, &binary);
        auto.skill_target = None;
        let result = setup_project(auto).unwrap();
        assert!(result.skill_targets.is_empty());
        assert!(!project.join(".agents/skills/papertiger/SKILL.md").exists());
        assert!(!project.join(".claude/skills/papertiger/SKILL.md").exists());
        cleanup(&project);
    }

    #[test]
    fn omitted_target_preserves_receipt_choice_and_reselection_removes_deselected_skill() {
        let (project, binary) = fixture("target-migration");
        setup_project(request(&project, &binary)).unwrap();
        fs::remove_dir_all(project.join(".claude")).unwrap();

        let mut preserve = request(&project, &binary);
        preserve.skill_target = None;
        let preserved = setup_project(preserve).unwrap();
        assert_eq!(
            preserved.skill_targets,
            vec![SkillTarget::Agents, SkillTarget::Claude]
        );
        let claude_skill = project.join(".claude/skills/papertiger/SKILL.md");
        assert!(claude_skill.is_file());
        fs::write(&claude_skill, "local edit\n").unwrap();

        let mut agents_only = request(&project, &binary);
        agents_only.skill_target = Some(SkillTargetRequest::Agents);
        let reselected = setup_project(agents_only).unwrap();
        assert_eq!(reselected.skill_targets, vec![SkillTarget::Agents]);
        assert_eq!(reselected.operation, SetupOperation::Upgrade);
        assert!(reselected.actions.iter().any(|action| {
            action.path == ".claude/skills/papertiger/SKILL.md"
                && action.action == SetupActionKind::Remove
        }));
        assert!(!claude_skill.exists());
        cleanup(&project);
    }

    #[test]
    fn explicit_auto_redetects_while_none_overrides_present_harness_markers() {
        let (project, binary) = fixture("target-explicit-auto");
        fs::write(project.join("AGENTS.md"), "repository contract\n").unwrap();
        fs::write(project.join("CLAUDE.md"), "repository contract\n").unwrap();
        setup_project(request(&project, &binary)).unwrap();
        fs::remove_file(project.join("CLAUDE.md")).unwrap();
        fs::remove_dir_all(project.join(".claude")).unwrap();

        let mut redetect = request(&project, &binary);
        redetect.skill_target = Some(SkillTargetRequest::Auto);
        let redetected = setup_project(redetect).unwrap();
        assert_eq!(redetected.skill_targets, vec![SkillTarget::Agents]);

        let mut none = request(&project, &binary);
        none.skill_target = Some(SkillTargetRequest::None);
        let none = setup_project(none).unwrap();
        assert!(none.skill_targets.is_empty());
        assert!(!project.join(".agents/skills/papertiger/SKILL.md").exists());
        assert_eq!(
            fs::read_to_string(project.join("AGENTS.md")).unwrap(),
            "repository contract\n"
        );
        cleanup(&project);
    }

    #[test]
    fn previous_receipt_schema_is_rewritten_with_its_selection() {
        let (project, binary) = fixture("previous-receipt");
        fs::create_dir(project.join("plans")).unwrap();
        let mut install = request(&project, &binary);
        install.authority_path = Some(Path::new("plans/papertiger.sqlite"));
        setup_project(install).unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let previous = serde_json::json!({
            "schema": "papertiger.project_install.v2",
            "papertiger_version": env!("CARGO_PKG_VERSION"),
            "authority_path": "plans/papertiger.sqlite",
            "skill_targets": ["agents", "claude"],
            "managed_files": [{
                "path": AGENT_INTEGRATION_PATH,
                "sha256": "0".repeat(64),
            }],
        });
        fs::write(&receipt_path, serde_json::to_vec_pretty(&previous).unwrap()).unwrap();

        let mut upgrade = request(&project, &binary);
        upgrade.skill_target = None;
        let result = setup_project(upgrade).unwrap();
        assert_eq!(result.operation, SetupOperation::Upgrade);
        assert_eq!(result.authority_path, "plans/papertiger.sqlite");
        assert_eq!(
            result.skill_targets,
            vec![SkillTarget::Agents, SkillTarget::Claude]
        );
        let rewritten: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        assert_eq!(rewritten["schema"], receipt::INSTALL_RECEIPT_SCHEMA);
        assert!(rewritten.get("managed_files").is_none());
        cleanup(&project);
    }

    #[test]
    fn unsupported_receipt_schema_refuses_with_reinstall_command() {
        let (project, binary) = fixture("unsupported-receipt");
        setup_project(request(&project, &binary)).unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        receipt["schema"] = serde_json::Value::String("papertiger.project_install.v1".to_owned());
        let unsupported = serde_json::to_vec_pretty(&receipt).unwrap();
        fs::write(&receipt_path, &unsupported).unwrap();
        let contract_before = fs::read(project.join(AGENT_INTEGRATION_PATH)).unwrap();

        let message = format!(
            "{:#}",
            setup_project(request(&project, &binary)).unwrap_err()
        );
        assert!(
            message.contains("unsupported project-install receipt schema")
                && message.contains("papertiger setup-project <project-root>"),
            "{message}"
        );
        assert_eq!(fs::read(&receipt_path).unwrap(), unsupported);
        assert_eq!(
            fs::read(project.join(AGENT_INTEGRATION_PATH)).unwrap(),
            contract_before
        );
        cleanup(&project);
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
        assert_eq!(first.schema, "papertiger.project_install_result.v7");
        assert_eq!(first.operation, SetupOperation::Install);
        assert_eq!(first.authority_path, DEFAULT_AUTHORITY_PATH);
        assert!(first.next_actions.iter().any(|action| {
            action.contains("tools/papertiger/bin/papertiger")
                && action.contains("no shell launcher is required")
        }));
        assert!(first.next_actions.iter().any(|action| {
            action.contains("discovers this project receipt")
                && action.contains("project root or any nested directory")
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
            canonical_text(AGENT_SKILL).as_ref()
        );
        assert_eq!(
            fs::read(project.join(".claude/skills/papertiger/SKILL.md")).unwrap(),
            canonical_text(AGENT_SKILL).into_owned()
        );
        let receipt = load_install_receipt(&project.join(INSTALL_RECEIPT_PATH))
            .unwrap()
            .unwrap();
        assert_eq!(receipt.authority_path, DEFAULT_AUTHORITY_PATH);
        assert_eq!(
            receipt.skill_targets,
            vec![SkillTarget::Agents, SkillTarget::Claude]
        );
        assert!(project.join(&first.runtime_receipt_path).is_file());

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
    fn modified_release_text_is_previewed_then_replaced() {
        let (project, binary) = fixture("replace");
        setup_project(request(&project, &binary)).unwrap();
        let contract = project.join(AGENT_INTEGRATION_PATH);
        fs::write(&contract, b"repository edit").unwrap();
        fs::remove_file(project.join(".agents/skills/papertiger/SKILL.md")).unwrap();

        let mut dry_run = request(&project, &binary);
        dry_run.dry_run = true;
        let result = setup_project(dry_run).unwrap();
        assert_eq!(result.operation, SetupOperation::Repair);
        assert!(result.actions.iter().any(|action| {
            action.path == AGENT_INTEGRATION_PATH && action.action == SetupActionKind::Replace
        }));
        assert!(result.actions.iter().any(|action| {
            action.path == ".agents/skills/papertiger/SKILL.md"
                && action.action == SetupActionKind::Create
        }));
        assert_eq!(fs::read(&contract).unwrap(), b"repository edit");

        setup_project(request(&project, &binary)).unwrap();
        assert_eq!(
            fs::read(&contract).unwrap(),
            canonical_text(AGENT_INTEGRATION).as_ref()
        );
        assert!(project.join(".agents/skills/papertiger/SKILL.md").is_file());
        cleanup(&project);
    }

    #[test]
    fn older_release_upgrade_replaces_release_owned_files() {
        let (project, binary) = fixture("receipt-upgrade");
        let initial = setup_project(request(&project, &binary)).unwrap();
        fs::write(
            project.join(AGENT_INTEGRATION_PATH),
            b"old release contract\n",
        )
        .unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut receipt = load_install_receipt(&receipt_path).unwrap().unwrap();
        receipt.papertiger_version = "0.4.0".to_owned();
        fs::write(&receipt_path, receipt_bytes(&receipt).unwrap()).unwrap();

        let runtime_receipt_path = project.join(&initial.runtime_receipt_path);
        let mut runtime_receipt: RuntimeInstallReceipt =
            serde_json::from_slice(&fs::read(&runtime_receipt_path).unwrap()).unwrap();
        runtime_receipt.papertiger_version = "0.4.0".to_owned();
        fs::write(
            &runtime_receipt_path,
            runtime_receipt_bytes(&runtime_receipt).unwrap(),
        )
        .unwrap();
        fs::write(&binary, b"new-papertiger-binary").unwrap();

        let upgraded = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(upgraded.operation, SetupOperation::Upgrade);
        for path in [
            AGENT_INTEGRATION_PATH,
            upgraded.runtime_receipt_path.as_str(),
            INSTALL_RECEIPT_PATH,
        ] {
            assert!(
                upgraded.actions.iter().any(|action| {
                    action.path == path && action.action == SetupActionKind::Replace
                }),
                "{path}"
            );
        }
        assert_eq!(
            fs::read(project.join(AGENT_INTEGRATION_PATH)).unwrap(),
            canonical_text(AGENT_INTEGRATION).as_ref()
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
    fn receipt_with_unknown_fields_refuses_before_any_write() {
        let (project, binary) = fixture("receipt-unknown-field");
        setup_project(request(&project, &binary)).unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        receipt["managed_files"] = serde_json::json!([]);
        let invalid = serde_json::to_vec_pretty(&receipt).unwrap();
        fs::write(&receipt_path, &invalid).unwrap();
        fs::write(project.join(AGENT_INTEGRATION_PATH), b"repository edit").unwrap();

        let message = format!(
            "{:#}",
            setup_project(request(&project, &binary)).unwrap_err()
        );
        assert!(
            message.contains("restore a valid papertiger.project_install.v3 receipt"),
            "{message}"
        );
        assert_eq!(fs::read(&receipt_path).unwrap(), invalid);
        assert_eq!(
            fs::read(project.join(AGENT_INTEGRATION_PATH)).unwrap(),
            b"repository edit"
        );
        cleanup(&project);
    }

    #[test]
    fn newer_receipt_version_refuses_downgrade() {
        let (project, binary) = fixture("receipt-downgrade");
        setup_project(request(&project, &binary)).unwrap();
        let contract_path = project.join("tools/papertiger/agent_integration.md");
        let contract_before = fs::read(&contract_path).unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut receipt = load_install_receipt(&receipt_path).unwrap().unwrap();
        receipt.papertiger_version = "999.0.0".to_owned();
        let future_receipt = receipt_bytes(&receipt).unwrap();
        fs::write(&receipt_path, &future_receipt).unwrap();

        let error = setup_project(request(&project, &binary)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("refuses to downgrade"));
        assert!(message.contains("verified Papertiger 999.0.0 or newer"));
        assert_eq!(fs::read(&receipt_path).unwrap(), future_receipt);
        assert_eq!(fs::read(&contract_path).unwrap(), contract_before);
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
        assert_eq!(
            discover_project_authority(&project).unwrap(),
            Some(
                fs::canonicalize(&project)
                    .unwrap()
                    .join("plans/papertiger.sqlite")
            )
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
    fn native_binary_discovers_receipt_authority_from_nested_directory() {
        let (project, binary) = fixture("discover-nested-authority");
        fs::create_dir(project.join("plans")).unwrap();
        let mut install = request(&project, &binary);
        install.authority_path = Some(Path::new("plans/papertiger.sqlite"));
        setup_project(install).unwrap();
        let nested = project.join("nested/work");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            discover_project_authority(&nested).unwrap(),
            Some(
                fs::canonicalize(&project)
                    .unwrap()
                    .join("plans/papertiger.sqlite")
            )
        );
        cleanup(&project);
    }

    #[test]
    fn native_binary_refuses_receipt_version_drift_with_upgrade_command() {
        let (project, binary) = fixture("discover-version-drift");
        setup_project(request(&project, &binary)).unwrap();
        let receipt_path = project.join(INSTALL_RECEIPT_PATH);
        let mut receipt = load_install_receipt(&receipt_path).unwrap().unwrap();
        receipt.papertiger_version = "0.7.1".to_owned();
        fs::write(&receipt_path, receipt_bytes(&receipt).unwrap()).unwrap();

        let error = discover_project_authority(&project).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("requires Papertiger 0.7.1"));
        assert!(message.contains("running binary is"));
        assert!(message.contains("papertiger setup-project"));
        assert!(message.contains("demo-project"));

        let error = project_authority(&project).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("requires Papertiger 0.7.1"));
        assert!(message.contains("running binary is"));
        assert!(message.contains("papertiger setup-project"));
        assert!(message.contains("demo-project"));
        cleanup(&project);
    }

    #[test]
    fn discovery_refuses_missing_or_drifted_host_identity_and_setup_repairs_it() {
        let (project, binary) = fixture("runtime-identity-repair");
        let installed = setup_project(request(&project, &binary)).unwrap();
        let runtime_receipt_path = project.join(&installed.runtime_receipt_path);
        fs::remove_file(&runtime_receipt_path).unwrap();

        let error = discover_project_authority(&project).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("runtime-install receipt"), "{message}");
        assert!(message.contains("papertiger setup-project"), "{message}");

        let repaired = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(repaired.operation, SetupOperation::Repair);
        assert!(repaired.actions.iter().any(|action| {
            action.path == repaired.runtime_receipt_path && action.action == SetupActionKind::Create
        }));
        assert!(discover_project_authority(&project).unwrap().is_some());

        let installed_binary = project.join(&repaired.runtime_install.binary.path);
        fs::write(&installed_binary, b"tampered-binary").unwrap();
        let error = discover_project_authority(&project).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("identity does not match"), "{message}");
        assert!(message.contains("SHA-256"), "{message}");
        assert!(message.contains("trusted external"), "{message}");

        let repaired = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(
            fs::read(project.join(&repaired.runtime_install.binary.path)).unwrap(),
            b"papertiger-binary"
        );
        assert!(discover_project_authority(&project).unwrap().is_some());

        fs::write(&runtime_receipt_path, b"not-json\n").unwrap();
        let error = discover_project_authority(&project).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("parse runtime-install receipt"),
            "{message}"
        );
        let repaired_receipt = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(repaired_receipt.operation, SetupOperation::Repair);
        assert!(repaired_receipt.actions.iter().any(|action| {
            action.path == repaired_receipt.runtime_receipt_path
                && action.action == SetupActionKind::Replace
        }));
        assert!(discover_project_authority(&project).unwrap().is_some());
        cleanup(&project);
    }

    #[test]
    fn authority_path_refuses_nonportable_or_escaping_values() {
        let (project, binary) = fixture("authority-path-refusal");
        for invalid in [
            "../outside.sqlite",
            "plans/space name.sqlite",
            "/root.sqlite",
            "TOOLS/papertiger.sqlite",
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
        assert!(!project.join("tools").exists());
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
        assert!(!project.join("tools").exists());
        cleanup(&project);
    }

    #[test]
    fn crlf_checkout_of_release_text_is_unchanged() {
        fn as_crlf(text: &str) -> String {
            text.replace("\r\n", "\n").replace('\n', "\r\n")
        }

        let (project, binary) = fixture("crlf-checkout");
        setup_project(request(&project, &binary)).unwrap();
        let mut original_bytes = Vec::new();
        for path in [
            AGENT_INTEGRATION_PATH,
            SkillTarget::Agents.managed_path(),
            SkillTarget::Claude.managed_path(),
            INSTALL_RECEIPT_PATH,
        ] {
            let path = project.join(path);
            let text = fs::read_to_string(&path).unwrap();
            fs::write(&path, as_crlf(&text)).unwrap();
            original_bytes.push((path.clone(), fs::read(&path).unwrap()));
        }

        let current = setup_project(request(&project, &binary)).unwrap();
        assert_eq!(current.operation, SetupOperation::Unchanged);
        for (path, bytes) in original_bytes {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        cleanup(&project);
    }

    #[test]
    fn text_line_ending_equivalence_never_weakens_binary_identity() {
        let (project, _) = fixture("binary-line-ending-identity");
        let destination = project.join("tools/papertiger/bin/papertiger.exe");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"binary\r\npayload").unwrap();

        let text_action = preflight_managed_file(
            &destination,
            &ManagedFile {
                relative_path: PathBuf::from(AGENT_INTEGRATION_PATH),
                content: b"binary\npayload".to_vec(),
                executable: false,
                content_kind: ManagedContentKind::Text,
            },
        )
        .unwrap();
        assert_eq!(text_action, SetupActionKind::Unchanged);

        let binary_action = preflight_managed_file(
            &destination,
            &ManagedFile {
                relative_path: PathBuf::from("tools/papertiger/bin/papertiger.exe"),
                content: b"binary\npayload".to_vec(),
                executable: false,
                content_kind: ManagedContentKind::RuntimeBinary,
            },
        )
        .unwrap();
        assert_eq!(binary_action, SetupActionKind::Replace);
        cleanup(&project);
    }

    #[test]
    fn managed_text_rendering_is_checkout_line_ending_independent() {
        for guidance in [AGENT_INTEGRATION, AGENT_SKILL] {
            let lf = String::from_utf8_lossy(guidance).replace("\r\n", "\n");
            let crlf = lf.replace('\n', "\r\n");
            assert_eq!(
                canonical_text(lf.as_bytes()),
                canonical_text(crlf.as_bytes())
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
    fn gitignore_positive_suffix_keeps_repeat_setup_byte_identical() {
        for newline in ["\n", "\r\n"] {
            let (project, binary) = fixture("gitignore-positive-suffix");
            setup_project(request(&project, &binary)).unwrap();
            let path = project.join(".gitignore");
            let existing = fs::read_to_string(&path).unwrap().replace('\n', newline);
            let suffix = format!(
                "{newline}# Another tool's binary policy{newline}/tools/another-tool/bin/{newline}\\!literal{newline}# !comment{newline}"
            );
            let expected = format!("{existing}{suffix}");
            fs::write(&path, &expected).unwrap();

            assert_eq!(
                setup_project(request(&project, &binary)).unwrap().operation,
                SetupOperation::Unchanged
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), expected);
            cleanup(&project);
        }
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
            assert!(!project.join("tools").exists());
            cleanup(&project);
        }
    }

    #[test]
    fn receipt_bound_authority_cannot_be_silently_rebound() {
        let (project, binary) = fixture("authority-rebind-refusal");
        setup_project(request(&project, &binary)).unwrap();
        let receipt_before = fs::read(project.join(INSTALL_RECEIPT_PATH)).unwrap();
        let contract_before =
            fs::read(project.join("tools/papertiger/agent_integration.md")).unwrap();

        let mut rebind = request(&project, &binary);
        rebind.authority_path = Some(Path::new("plans/papertiger.sqlite"));
        let error = setup_project(rebind).unwrap_err();
        assert!(error.to_string().contains("will not rebind"));
        assert_eq!(
            fs::read(project.join(INSTALL_RECEIPT_PATH)).unwrap(),
            receipt_before
        );
        assert_eq!(
            fs::read(project.join("tools/papertiger/agent_integration.md")).unwrap(),
            contract_before
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
        assert!(!project.join("tools").exists());
        cleanup(&project);
    }

    #[test]
    fn non_file_gitignore_refuses_before_managed_writes() {
        let (project, binary) = fixture("gitignore-directory");
        fs::create_dir(project.join(".gitignore")).unwrap();
        let error = setup_project(request(&project, &binary)).unwrap_err();
        assert!(error.to_string().contains("cannot preserve non-file"));
        assert!(!project.join("tools").exists());
        cleanup(&project);
    }

    #[test]
    fn non_directory_managed_parent_refuses_before_any_write() {
        let (project, binary) = fixture("managed-parent-file");
        fs::write(project.join(".agents"), b"not a directory").unwrap();

        let error = setup_project(request(&project, &binary)).unwrap_err();
        assert!(error.to_string().contains("parent path is not a directory"));
        assert!(!project.join("tools").exists());
        assert_eq!(
            fs::read(project.join(".agents")).unwrap(),
            b"not a directory"
        );
        cleanup(&project);
    }

    #[test]
    fn managed_agent_text_has_unambiguous_routing_boundaries() {
        for (name, bytes) in [
            ("agent integration contract", AGENT_INTEGRATION),
            ("agent skill", AGENT_SKILL),
        ] {
            let text = std::str::from_utf8(bytes).expect("managed text is UTF-8");
            assert!(
                !text.to_lowercase().contains("disposable"),
                "{name} must not use repository-lifecycle words as skip criteria"
            );
            assert!(
                text.contains("intermediate steps inside one independently")
                    || text.contains("intermediate steps within one independently")
                    || text.contains("investigation and verification are steps inside"),
                "{name} must name the skipped unit of work"
            );
        }
        for (name, bytes) in [
            ("agent integration contract", AGENT_INTEGRATION),
            ("agent skill", AGENT_SKILL),
        ] {
            let text = std::str::from_utf8(bytes).expect("managed text is UTF-8");
            assert!(
                text.contains("--intent-source user")
                    && (text.contains("before task completion")
                        || text.contains("Before completion, record any commit")),
                "{name} must preserve known user provenance and inward commit association ordering"
            );
        }
    }
}
