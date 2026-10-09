//! Selecting a project's planning authority: `--project-root` names it
//! exactly, otherwise the nearest `state/papertiger.sqlite` above the working
//! directory, the way Git finds its repository. A Git repository whose
//! planning belongs to another project carries a route: a one-line file in
//! its shared Git directory naming that project's root, which every worktree
//! sees and Git never commits. Nothing is installed in a project; `init` adds
//! the ignore rules when it creates an authority.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

/// Project-relative location of every project's planning authority.
pub(crate) const AUTHORITY_PATH: &str = "state/papertiger.sqlite";

/// The route file's name inside a repository's shared Git directory.
pub(crate) const ROUTE_FILE: &str = "papertiger-authority";

const IGNORE_START: &str = "# Papertiger project-local runtime and authorities";
const IGNORE_END: &str = "# End Papertiger managed ignore block";
const IGNORE_ENTRIES: &[&str] = &[
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

/// A project root and the authority inside it.
pub(crate) struct ProjectAuthority {
    pub(crate) root: PathBuf,
    pub(crate) authority: PathBuf,
    /// The route file that led here, when a repository routed to this project.
    pub(crate) route: Option<PathBuf>,
}

impl ProjectAuthority {
    fn at(root: PathBuf, authority: PathBuf) -> Self {
        Self {
            root,
            authority,
            route: None,
        }
    }
}

/// The authority of the project rooted exactly at `root`, whether or not it
/// exists yet: `init` creates it and every other command refuses a missing one.
/// A linked Git worktree without its own authority selects its main working
/// tree's, as discovery does.
pub(crate) fn at_root(root: &Path) -> Result<ProjectAuthority> {
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolve project root {}", root.display()))?;
    if !root.is_dir() {
        return Err(anyhow!(
            "project root {} is not a directory; pass the directory that contains {AUTHORITY_PATH}",
            root.display()
        ));
    }
    let authority = root.join(AUTHORITY_PATH);
    let present = require_regular_or_absent(&authority)?;
    if let Some(routed) = follow_route(&root, present)? {
        return Ok(routed);
    }
    if !present && let Some(main) = linked_worktree_main(&root)? {
        let main_authority = main.join(AUTHORITY_PATH);
        if require_regular_or_absent(&main_authority)? {
            return Ok(ProjectAuthority::at(main, main_authority));
        }
    }
    Ok(ProjectAuthority::at(root, authority))
}

/// The nearest project above `start` that has an authority, if any. Inside a
/// linked Git worktree with none above it, that is the main working tree's
/// authority: every worktree of a repository plans in one authority.
pub(crate) fn discover(start: &Path) -> Result<Option<ProjectAuthority>> {
    let start = fs::canonicalize(start)
        .with_context(|| format!("resolve current directory {}", start.display()))?;
    for root in start.ancestors() {
        let authority = root.join(AUTHORITY_PATH);
        let present = require_regular_or_absent(&authority)?;
        if let Some(routed) = follow_route(root, present)? {
            return Ok(Some(routed));
        }
        if present {
            return Ok(Some(ProjectAuthority::at(root.to_path_buf(), authority)));
        }
    }
    if let Some(main) = linked_worktree_main(&start)? {
        let authority = main.join(AUTHORITY_PATH);
        if require_regular_or_absent(&authority)? {
            return Ok(Some(ProjectAuthority::at(main, authority)));
        }
    }
    Ok(None)
}

/// The authority a Git repository rooted at `top` routes to, if it carries a
/// route. A route never chains: it must name a project root that holds its
/// own authority. A repository with both an authority and a route is
/// ambiguous and refused, as is a route naming a missing authority.
fn follow_route(top: &Path, own_authority: bool) -> Result<Option<ProjectAuthority>> {
    let Some(file) = route_file(top)? else {
        return Ok(None);
    };
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", file.display())),
    };
    let shown_top = portable(top);
    let shown_file = portable(&file);
    if own_authority {
        return Err(anyhow!(
            "{shown_top} has its own {AUTHORITY_PATH} and also a route in {shown_file}; keep one: run `papertiger route clear` in {shown_top} to plan in its own authority"
        ));
    }
    let named = text.trim();
    let target = Path::new(named);
    if named.is_empty() || named.lines().count() != 1 || !target.is_absolute() {
        return Err(anyhow!(
            "{shown_file} must hold one absolute project root; rewrite it with `papertiger route set <project-root>` in {shown_top}, or remove it with `papertiger route clear`"
        ));
    }
    let Ok(root) = fs::canonicalize(target) else {
        return Err(anyhow!(
            "the route in {shown_file} names {named}, which does not exist; point it at the owning project with `papertiger route set <project-root>` in {shown_top}, or remove it with `papertiger route clear`"
        ));
    };
    let authority = root.join(AUTHORITY_PATH);
    if !require_regular_or_absent(&authority)? {
        return Err(anyhow!(
            "the route in {shown_file} names {}, which has no {AUTHORITY_PATH}; point it at the project that owns this repository's planning with `papertiger route set <project-root>` in {shown_top}, or remove it with `papertiger route clear`",
            portable(&root)
        ));
    }
    Ok(Some(ProjectAuthority {
        root,
        authority,
        route: Some(file),
    }))
}

/// Where the route of the Git repository rooted at `top` lives, if `top` is a
/// repository root: its shared Git directory, so linked worktrees share it.
fn route_file(top: &Path) -> Result<Option<PathBuf>> {
    Ok(git_directories(top)?.map(|dirs| dirs.common.join(ROUTE_FILE)))
}

/// The root of the Git repository containing `start`, if any.
fn repository_top(start: &Path) -> Option<&Path> {
    start
        .ancestors()
        .find(|directory| directory.join(".git").exists())
}

/// The repository containing `cwd` and its route file.
fn repository_route(cwd: &Path, action: &str) -> Result<(PathBuf, PathBuf)> {
    let cwd = fs::canonicalize(cwd)
        .with_context(|| format!("resolve current directory {}", cwd.display()))?;
    let Some(top) = repository_top(&cwd) else {
        return Err(anyhow!(
            "{} is not inside a Git repository; a route lives in a repository's Git directory, so run `papertiger route {action}` from inside the repository whose planning belongs to another project",
            portable(&cwd)
        ));
    };
    let Some(file) = route_file(top)? else {
        return Err(anyhow!("{} has no Git directory", portable(top)));
    };
    Ok((top.to_path_buf(), file))
}

/// Route the Git repository containing `cwd` to the authority at `target`.
/// Returns the repository root and the authority it now selects.
pub(crate) fn set_route(cwd: &Path, target: &Path) -> Result<(PathBuf, ProjectAuthority)> {
    let (top, file) = repository_route(cwd, "set <project-root>")?;
    let own = top.join(AUTHORITY_PATH);
    if require_regular_or_absent(&own)? {
        return Err(anyhow!(
            "{} has its own authority {}; a repository keeps the planning history it has, so it cannot also route elsewhere",
            portable(&top),
            portable(&own)
        ));
    }
    let root = fs::canonicalize(target).with_context(|| {
        format!(
            "resolve project root {}; name the existing root of the project that owns this repository's planning",
            target.display()
        )
    })?;
    let authority = root.join(AUTHORITY_PATH);
    if !require_regular_or_absent(&authority)? {
        return Err(anyhow!(
            "{} has no {AUTHORITY_PATH}; route only to an existing project authority, or start that project's authority with `papertiger --project-root {} init`",
            portable(&root),
            portable(&root)
        ));
    }
    if root == top {
        return Err(anyhow!(
            "{} cannot route to itself; name the other project's root",
            portable(&top)
        ));
    }
    let content = format!("{}\n", portable(&root));
    let corrective = format!("`papertiger route set {}` again", portable(&root));
    if file.exists() {
        papertiger::atomic_replace_file(&file, content.as_bytes(), "route", &corrective)?;
    } else {
        papertiger::atomic_create_file(&file, content.as_bytes(), "route", &corrective)?;
    }
    Ok((
        top,
        ProjectAuthority {
            root,
            authority,
            route: Some(file),
        },
    ))
}

/// Remove the route of the Git repository containing `cwd`. Returns the route
/// file and whether one was removed.
pub(crate) fn clear_route(cwd: &Path) -> Result<(PathBuf, bool)> {
    let (_, file) = repository_route(cwd, "clear")?;
    match fs::remove_file(&file) {
        Ok(()) => Ok((file, true)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((file, false)),
        Err(error) => Err(error).with_context(|| format!("remove {}", file.display())),
    }
}

/// The refusal for a command that needs an authority where none was found.
pub(crate) fn not_found(start: &Path) -> anyhow::Error {
    let shown = portable(start);
    match linked_worktree_main(start) {
        Ok(Some(main)) => anyhow!(
            "no Papertiger authority ({AUTHORITY_PATH}) was found in {shown} or any parent directory, nor in {}, the main working tree of this linked Git worktree; the project's authority belongs to the main working tree: for a genuinely new project start it with `papertiger --project-root {} init`",
            portable(&main),
            portable(&main)
        ),
        Ok(None) => anyhow!(
            "no Papertiger authority ({AUTHORITY_PATH}) was found in {shown} or any parent directory; run from inside the project, pass --project-root <project-root>, route a repository whose planning belongs to another project once with `papertiger route set <project-root>` from inside it, or start a genuinely new project's authority with `papertiger init` at its Git root or `papertiger --project-root <project-root> init`"
        ),
        Err(error) => error,
    }
}

/// Refuse `init` in a linked Git worktree whose main working tree has no
/// authority: one created there would fork the project's plan. When the main
/// tree has one, selection already resolves to it.
pub(crate) fn refuse_init_in_linked_worktree(directory: &Path) -> Result<()> {
    let cwd = &fs::canonicalize(directory)
        .with_context(|| format!("resolve project root {}", directory.display()))?;
    // A routed repository's init reaches the project it routes to.
    if let Some(top) = repository_top(cwd)
        && let Some(file) = route_file(top)?
        && file.is_file()
    {
        return Ok(());
    }
    let Some(main) = linked_worktree_main(cwd)? else {
        return Ok(());
    };
    if require_regular_or_absent(&main.join(AUTHORITY_PATH))? {
        return Ok(());
    }
    Err(anyhow!(
        "refusing to create an authority in {}, a linked Git worktree of {}: the project's authority belongs to the main working tree; for a genuinely new project start it with `papertiger --project-root {} init`",
        portable(cwd),
        portable(&main),
        portable(&main)
    ))
}

/// The main working tree of the linked Git worktree containing `start`, if
/// any. A submodule is its own project, as is a worktree of a bare
/// repository, which has no main tree.
fn linked_worktree_main(start: &Path) -> Result<Option<PathBuf>> {
    let Some(top) = repository_top(start) else {
        return Ok(None);
    };
    let Some(dirs) = git_directories(top)? else {
        return Ok(None);
    };
    if !dirs.linked {
        return Ok(None);
    }
    match dirs.common.file_name() {
        Some(name) if name == ".git" => Ok(dirs.common.parent().map(Path::to_path_buf)),
        _ => Ok(None),
    }
}

/// A repository root's shared Git directory, and whether the root is a linked
/// worktree.
struct GitDirectories {
    common: PathBuf,
    linked: bool,
}

/// The Git directories of the repository rooted at `top`, if `top/.git`
/// exists. A main working tree's `.git` is that directory. A linked
/// worktree's `.git` is a file naming `<main>/.git/worktrees/<name>`, whose
/// `commondir` leads back to the shared `<main>/.git`. A submodule's `.git`
/// file has no `commondir`: its own Git directory is shared with nothing.
fn git_directories(top: &Path) -> Result<Option<GitDirectories>> {
    let dot_git = top.join(".git");
    let metadata = match fs::symlink_metadata(&dot_git) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", dot_git.display())),
    };
    if !metadata.is_file() {
        return Ok(Some(GitDirectories {
            common: dot_git,
            linked: false,
        }));
    }
    let text =
        fs::read_to_string(&dot_git).with_context(|| format!("read {}", dot_git.display()))?;
    let Some(named) = text.lines().find_map(|line| line.strip_prefix("gitdir:")) else {
        return Err(anyhow!(
            "{} is not a Git worktree link (no 'gitdir:' line); repair it with `git worktree repair`, or run papertiger with --project-root <project-root>",
            portable(&dot_git)
        ));
    };
    let gitdir = top.join(named.trim());
    if !gitdir.is_dir() {
        return Err(anyhow!(
            "{} names Git directory {}, which does not exist; repair the worktree with `git worktree repair`, or run papertiger with --project-root <project-root>",
            portable(&dot_git),
            gitdir.display()
        ));
    }
    let commondir = match fs::read_to_string(gitdir.join("commondir")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(GitDirectories {
                common: gitdir,
                linked: false,
            }));
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read {}", gitdir.join("commondir").display()));
        }
    };
    let common = fs::canonicalize(gitdir.join(commondir.trim())).with_context(|| {
        format!(
            "resolve the shared Git directory named by {}",
            gitdir.display()
        )
    })?;
    Ok(Some(GitDirectories {
        common,
        linked: true,
    }))
}

fn portable(path: &Path) -> String {
    papertiger::portable_absolute(path).unwrap_or_else(|_| path.display().to_string())
}

/// Whether `path` is an existing regular file; refuses anything else that
/// occupies the authority path, so a link or directory never stands in for it.
fn require_regular_or_absent(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(anyhow!(
            "{} exists but is not a regular file; restore the project authority there, or select a database elsewhere with --db <path>",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

/// Add Papertiger's managed ignore block to a Git working tree at `root`, so a
/// new authority is never committed. Returns whether `.gitignore` changed;
/// a root without `.git` is left alone.
pub(crate) fn write_ignore_block(root: &Path) -> Result<bool> {
    if !root.join(".git").exists() {
        return Ok(false);
    }
    let path = root.join(".gitignore");
    let existing = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let updated = ignore_content(existing.as_deref())?;
    if existing.as_deref() == Some(updated.as_str()) {
        return Ok(false);
    }
    fs::write(&path, updated).with_context(|| format!("write {}", path.display()))?;
    Ok(true)
}

/// `existing` with exactly one managed block. A block already present is kept
/// in place, entries and all, so older blocks stay as their projects wrote them.
fn ignore_content(existing: Option<&str>) -> Result<String> {
    let existing = existing.unwrap_or("");
    let starts = marker_lines(existing, IGNORE_START);
    let ends = marker_lines(existing, IGNORE_END);
    match (starts.len(), ends.len()) {
        (0, 0) => {}
        (1, 1) if starts[0] < ends[0] => return Ok(existing.to_owned()),
        _ => {
            return Err(anyhow!(
                "refusing to edit .gitignore: it has malformed or duplicate Papertiger markers; keep no marker lines or exactly one ordered '{IGNORE_START}' / '{IGNORE_END}' pair, then rerun init"
            ));
        }
    }
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut output = existing.to_owned();
    if !output.is_empty() && !output.ends_with('\n') {
        output.push_str(newline);
    }
    if !output.is_empty() && !output.ends_with(&format!("{newline}{newline}")) {
        output.push_str(newline);
    }
    for line in std::iter::once(IGNORE_START)
        .chain(IGNORE_ENTRIES.iter().copied())
        .chain(std::iter::once(IGNORE_END))
    {
        output.push_str(line);
        output.push_str(newline);
    }
    Ok(output)
}

fn marker_lines(content: &str, marker: &str) -> Vec<usize> {
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim_end_matches('\r') == marker)
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ignore_block_is_added_once_and_an_existing_block_is_kept() {
        let added = ignore_content(Some("target/")).unwrap();
        assert!(added.starts_with("target/\n\n# Papertiger"));
        assert!(added.contains("/state/papertiger.sqlite-wal\n"));
        assert_eq!(ignore_content(Some(&added)).unwrap(), added);
        let crlf = ignore_content(Some("target/\r\n")).unwrap();
        assert!(crlf.ends_with("block\r\n"));
        let older = format!("{IGNORE_START}\n/tools/papertiger/bin/\n{IGNORE_END}\n");
        assert_eq!(ignore_content(Some(&older)).unwrap(), older);
        let duplicated = format!("{older}{older}");
        assert!(ignore_content(Some(&duplicated)).is_err());
    }
}
