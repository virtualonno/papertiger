//! Selecting a project's planning authority: `--project-root` names it
//! exactly, otherwise the nearest `state/papertiger.sqlite` above the working
//! directory, the way Git finds its repository. Nothing is installed in a
//! project; `init` adds the ignore rules when it creates an authority.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

/// Project-relative location of every project's planning authority.
pub(crate) const AUTHORITY_PATH: &str = "state/papertiger.sqlite";

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
}

/// The authority of the project rooted exactly at `root`, whether or not it
/// exists yet: `init` creates it and every other command refuses a missing one.
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
    require_regular_or_absent(&authority)?;
    Ok(ProjectAuthority { root, authority })
}

/// The nearest project above `start` that has an authority, if any.
pub(crate) fn discover(start: &Path) -> Result<Option<ProjectAuthority>> {
    let start = fs::canonicalize(start)
        .with_context(|| format!("resolve current directory {}", start.display()))?;
    for root in start.ancestors() {
        let authority = root.join(AUTHORITY_PATH);
        if require_regular_or_absent(&authority)? {
            return Ok(Some(ProjectAuthority {
                root: root.to_path_buf(),
                authority,
            }));
        }
    }
    Ok(None)
}

/// The refusal for a command that needs an authority where none was found.
pub(crate) fn not_found(start: &Path) -> anyhow::Error {
    anyhow!(
        "no Papertiger authority ({AUTHORITY_PATH}) was found in {} or any parent directory; run from inside the project, pass --project-root <project-root>, or start a genuinely new project's authority with `papertiger --project-root <project-root> init`",
        papertiger::portable_absolute(start).unwrap_or_else(|_| start.display().to_string())
    )
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
