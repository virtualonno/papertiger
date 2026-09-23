//! Fail-closed destination preflight and staged filesystem application.

use std::borrow::Cow;
use std::fs;
use std::path::{Component, Path};

use anyhow::{Context, Result, anyhow};

use super::{ManagedContentKind, ManagedFile, SetupActionKind};

pub(super) fn validate_destination(root: &Path, relative: &Path) -> Result<()> {
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

/// Read an existing destination that setup-project may create or replace.
fn read_existing_file(destination: &Path) -> Result<Option<Vec<u8>>> {
    if !destination.exists() {
        return Ok(None);
    }
    if !destination.is_file() {
        return Err(anyhow!(
            "setup-project destination is not a file: {}; move it aside, then rerun setup-project",
            destination.display()
        ));
    }
    fs::read(destination)
        .map(Some)
        .with_context(|| format!("read managed file {}", destination.display()))
}

/// Release-owned text is written unconditionally; the comparison only decides
/// whether the write is a create, replace, or no-op.
pub(super) fn preflight_managed_file(
    destination: &Path,
    file: &ManagedFile,
) -> Result<SetupActionKind> {
    let Some(existing) = read_existing_file(destination)? else {
        return Ok(SetupActionKind::Create);
    };
    if !content_matches(file.content_kind, &existing, &file.content) {
        return Ok(SetupActionKind::Replace);
    }
    if file.executable && executable_bit_missing(destination)? {
        Ok(SetupActionKind::MakeExecutable)
    } else {
        Ok(SetupActionKind::Unchanged)
    }
}

pub(super) fn preflight_text_file(destination: &Path, expected: &[u8]) -> Result<SetupActionKind> {
    Ok(match read_existing_file(destination)? {
        None => SetupActionKind::Create,
        Some(existing) if text_matches(&existing, expected) => SetupActionKind::Unchanged,
        Some(_) => SetupActionKind::Replace,
    })
}

pub(super) fn content_matches(kind: ManagedContentKind, existing: &[u8], expected: &[u8]) -> bool {
    match kind {
        ManagedContentKind::Text => text_matches(existing, expected),
        ManagedContentKind::RuntimeBinary => existing == expected,
    }
}

/// A CRLF checkout of release text is the same content as its LF source.
pub(super) fn text_matches(existing: &[u8], expected: &[u8]) -> bool {
    canonical_text(existing) == canonical_text(expected)
}

pub(super) fn canonical_text(content: &[u8]) -> Cow<'_, [u8]> {
    if !content.windows(2).any(|pair| pair == b"\r\n") {
        return Cow::Borrowed(content);
    }
    let mut canonical = Vec::with_capacity(content.len());
    let mut index = 0;
    while index < content.len() {
        if content.get(index..index + 2) == Some(b"\r\n") {
            canonical.push(b'\n');
            index += 2;
        } else {
            canonical.push(content[index]);
            index += 1;
        }
    }
    Cow::Owned(canonical)
}

pub(super) fn write_new_file(path: &Path, content: &[u8]) -> Result<()> {
    papertiger::atomic_create_file(
        path,
        content,
        "setup-managed file",
        "`papertiger setup-project <project-root>`",
    )
}

pub(super) fn write_file(path: &Path, content: &[u8], create_new: bool) -> Result<()> {
    if create_new {
        write_new_file(path, content)
    } else {
        papertiger::atomic_replace_file(
            path,
            content,
            "setup-managed file",
            "`papertiger setup-project <project-root>`",
        )
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
pub(super) fn ensure_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata =
        fs::metadata(path).with_context(|| format!("read permissions for {}", path.display()))?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("set executable permissions on {}", path.display()))
}

#[cfg(not(unix))]
pub(super) fn ensure_executable(_path: &Path) -> Result<()> {
    Ok(())
}
