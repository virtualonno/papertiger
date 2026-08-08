use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;

const BASH_LAUNCHER: &[u8] = include_bytes!("../assets/project-launcher.sh");
const AGENT_INTEGRATION: &[u8] = include_bytes!("../agent_integration.md");
const GITIGNORE_COMMENT: &str = "# Papertiger project-local runtime and authorities";
const GITIGNORE_ENTRIES: &[&str] = &[
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupActionKind {
    Create,
    Replace,
    Unchanged,
    MakeExecutable,
    UpdateGitignore,
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
    pub(crate) dry_run: bool,
    pub(crate) actions: Vec<SetupAction>,
    pub(crate) agent_guidance_files_found: Vec<PathBuf>,
    pub(crate) next_actions: Vec<String>,
}

struct ManagedFile {
    relative_path: PathBuf,
    content: Vec<u8>,
    executable: bool,
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

    let managed = vec![
        ManagedFile {
            relative_path: PathBuf::from(format!("tools/papertiger/bin/papertiger{suffix}")),
            content: binary,
            executable: true,
        },
        ManagedFile {
            relative_path: PathBuf::from("scripts/papertiger"),
            content: BASH_LAUNCHER.to_vec(),
            executable: true,
        },
        ManagedFile {
            relative_path: PathBuf::from("tools/papertiger/agent_integration.md"),
            content: AGENT_INTEGRATION.to_vec(),
            executable: false,
        },
    ];

    let mut actions = Vec::with_capacity(managed.len() + 1);
    for file in &managed {
        validate_destination(&root, &file.relative_path)?;
        let destination = root.join(&file.relative_path);
        let action = preflight_managed_file(
            &destination,
            &file.content,
            file.executable,
            request.replace_managed || request.dry_run,
        )?;
        actions.push(SetupAction {
            path: normalized_path(&file.relative_path),
            action,
        });
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
    let updated_gitignore = gitignore_content(existing_gitignore.as_deref());
    let gitignore_action = if existing_gitignore.as_deref() == Some(updated_gitignore.as_str()) {
        SetupActionKind::Unchanged
    } else if existing_gitignore.is_some() {
        SetupActionKind::UpdateGitignore
    } else {
        SetupActionKind::Create
    };
    actions.push(SetupAction {
        path: normalized_path(gitignore_relative),
        action: gitignore_action,
    });

    if !request.dry_run {
        for (file, action) in managed.iter().zip(&actions) {
            let destination = root.join(&file.relative_path);
            match action.action {
                SetupActionKind::Create => write_new_file(&destination, &file.content)?,
                SetupActionKind::Replace => fs::write(&destination, &file.content)
                    .with_context(|| format!("replace managed file {}", destination.display()))?,
                SetupActionKind::MakeExecutable | SetupActionKind::Unchanged => {}
                SetupActionKind::UpdateGitignore => {
                    unreachable!("managed files never update .gitignore")
                }
            }
            if file.executable {
                ensure_executable(&destination)?;
            }
        }
        if !matches!(gitignore_action, SetupActionKind::Unchanged) {
            write_file(
                &gitignore_path,
                updated_gitignore.as_bytes(),
                existing_gitignore.is_none(),
            )?;
        }
    }

    let agent_guidance_files_found = ["AGENTS.md", "CLAUDE.md"]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| root.join(path).is_file())
        .collect();
    Ok(SetupProjectResult {
        schema: "papertiger.project_setup.v1",
        version: env!("CARGO_PKG_VERSION"),
        project_root: normalized_path(&root),
        dry_run: request.dry_run,
        actions,
        agent_guidance_files_found,
        next_actions: vec![
            "Review the additive .gitignore entries before creating either SQLite authority."
                .to_owned(),
            "Read tools/papertiger/agent_integration.md and incorporate its short contract into the repository's existing AGENTS.md or CLAUDE.md; setup-project never edits those files."
                .to_owned(),
            "Set PAPERTIGER_ACTOR to the acting agent name, then run scripts/papertiger init once from the canonical planning worktree."
                .to_owned(),
            "Run scripts/papertiger status, focus, and audit to prove the project-root authority is selected."
                .to_owned(),
            "Commit scripts/papertiger with executable mode (`git update-index --chmod=+x scripts/papertiger` when the host filesystem does not preserve it), plus the integration contract and .gitignore policy; keep tools/papertiger/bin and state host-local."
                .to_owned(),
            "When an RSI campaign is warranted, run the release's peer papertiger-mise binary with --project-root <this-project>; do not vendor the Mise runtime into the project."
                .to_owned(),
        ],
    })
}

fn validate_destination(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        match component {
            Component::Normal(part) => current.push(part),
            _ => {
                return Err(anyhow!(
                    "setup-project managed path must be relative and normalized: {}",
                    relative.display()
                ));
            }
        }
        let is_destination = components.peek().is_none();
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(anyhow!(
                    "setup-project refuses symlinked managed path component {}; replace it with a directory or regular file inside the project root",
                    current.display()
                ));
            }
            Ok(metadata) if !is_destination && !metadata.is_dir() => {
                return Err(anyhow!(
                    "setup-project managed parent path is not a directory: {}",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn preflight_managed_file(
    destination: &Path,
    expected: &[u8],
    executable: bool,
    replace_managed: bool,
) -> Result<SetupActionKind> {
    if !destination.exists() {
        return Ok(SetupActionKind::Create);
    }
    if !destination.is_file() {
        return Err(anyhow!(
            "setup-project destination is not a file: {}",
            destination.display()
        ));
    }
    let existing = fs::read(destination)
        .with_context(|| format!("read managed file {}", destination.display()))?;
    if existing != expected {
        if replace_managed {
            return Ok(SetupActionKind::Replace);
        }
        return Err(anyhow!(
            "setup-project found divergent release-managed file {}; review it, then rerun with --replace-managed",
            destination.display()
        ));
    }
    if executable && executable_bit_missing(destination)? {
        Ok(SetupActionKind::MakeExecutable)
    } else {
        Ok(SetupActionKind::Unchanged)
    }
}

fn gitignore_content(existing: Option<&str>) -> String {
    let existing = existing.unwrap_or("");
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let missing = GITIGNORE_ENTRIES
        .iter()
        .copied()
        .filter(|entry| !existing.lines().any(|line| line.trim() == *entry))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return existing.to_owned();
    }

    let mut output = existing.to_owned();
    if !output.is_empty() && !output.ends_with('\n') {
        output.push_str(newline);
    }
    if !output.is_empty() && !output.ends_with(&format!("{newline}{newline}")) {
        output.push_str(newline);
    }
    if !existing
        .lines()
        .any(|line| line.trim() == GITIGNORE_COMMENT)
    {
        output.push_str(GITIGNORE_COMMENT);
        output.push_str(newline);
    }
    for entry in missing {
        output.push_str(entry);
        output.push_str(newline);
    }
    output
}

fn write_new_file(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("managed destination has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create directory {}", parent.display()))?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create managed file {}", path.display()))?;
    output
        .write_all(content)
        .with_context(|| format!("write managed file {}", path.display()))
}

fn write_file(path: &Path, content: &[u8], create_new: bool) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.truncate(true);
    }
    let mut output = options
        .open(path)
        .with_context(|| format!("write {}", path.display()))?;
    output
        .write_all(content)
        .with_context(|| format!("write {}", path.display()))
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

#[cfg(unix)]
fn executable_bit_missing(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)
        .with_context(|| format!("read permissions for {}", path.display()))?
        .permissions()
        .mode();
    Ok(mode & 0o111 == 0)
}

#[cfg(not(unix))]
fn executable_bit_missing(_path: &Path) -> Result<bool> {
    Ok(false)
}

#[cfg(unix)]
fn ensure_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata =
        fs::metadata(path).with_context(|| format!("read permissions for {}", path.display()))?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("set executable permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn ensure_executable(_path: &Path) -> Result<()> {
    Ok(())
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
        assert_eq!(first.schema, "papertiger.project_setup.v1");
        assert_eq!(first.agent_guidance_files_found.len(), 2);
        assert!(
            first
                .next_actions
                .iter()
                .any(|action| action.contains("git update-index --chmod=+x scripts/papertiger"))
        );
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
        for entry in GITIGNORE_ENTRIES {
            assert_eq!(
                ignore.lines().filter(|line| line.trim() == *entry).count(),
                1
            );
        }
        assert!(!project.join("AGENTS.md.papertiger").exists());
        assert!(
            !project
                .join("tools/papertiger/bin/papertiger-mise")
                .exists()
        );

        let second = setup_project(request(&project, &binary)).unwrap();
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
        assert!(error.to_string().contains("divergent release-managed file"));
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
        assert!(result.actions.iter().any(|action| {
            action.path == "scripts/papertiger" && action.action == SetupActionKind::Replace
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
            BASH_LAUNCHER
        );
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
