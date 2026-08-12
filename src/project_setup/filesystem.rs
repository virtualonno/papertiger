//! Fail-closed destination preflight and staged filesystem application.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path};

use anyhow::{Context, Result, anyhow};

use super::receipt::managed_text_sha256;
use super::{ManagedFile, SetupActionKind};
use super::{PRE_RECEIPT_BINARY_PATH, PRE_RECEIPT_MANIFEST_PATH, PRE_RECEIPT_MISE_PATH};

#[derive(Debug)]
pub(super) enum PreReceiptInstall {
    Unrecognized,
    Verified {
        owned_hashes: BTreeMap<String, String>,
        retired_paths: Vec<String>,
    },
}

pub(super) fn preflight_retired_file(
    destination: &Path,
    prior_sha256: &str,
    dry_run: bool,
) -> Result<Option<SetupActionKind>> {
    if !destination.exists() {
        return Ok(None);
    }
    if !destination.is_file() {
        return Err(anyhow!(
            "retired setup-project destination is not a file: {}",
            destination.display()
        ));
    }
    let existing = fs::read(destination)
        .with_context(|| format!("read retired managed file {}", destination.display()))?;
    if managed_text_sha256(&existing) == prior_sha256 {
        return Ok(Some(SetupActionKind::RemoveRetired));
    }
    if dry_run {
        return Ok(Some(SetupActionKind::ModifiedRefusal));
    }
    Err(anyhow!(
        "setup-project refuses modified retired managed file {}; move or delete it deliberately, then rerun setup-project",
        destination.display()
    ))
}

pub(super) fn inspect_pre_receipt_install(root: &Path) -> Result<Option<PreReceiptInstall>> {
    let manifest = root.join(PRE_RECEIPT_MANIFEST_PATH);
    if !manifest.exists() {
        return Ok(None);
    }
    if !manifest.is_file() {
        return Err(anyhow!(
            "pre-receipt Papertiger install manifest is not a file: {}",
            manifest.display()
        ));
    }
    let bytes = fs::read(&manifest)
        .with_context(|| format!("read pre-receipt install manifest {}", manifest.display()))?;
    let text = match std::str::from_utf8(&bytes) {
        Ok(text)
            if text.starts_with("# Vendored Papertiger")
                && text.contains(
                    "This directory vendors the project-generic Papertiger planning client",
                )
                && text.contains("Canonical planning database:")
                && text.contains("tools/papertiger/papertiger.exe") =>
        {
            text
        }
        _ => return Ok(Some(PreReceiptInstall::Unrecognized)),
    };

    let entries = [
        (
            "Binary SHA-256",
            PRE_RECEIPT_BINARY_PATH,
            "pre-receipt Papertiger binary",
            false,
        ),
        (
            "Agent contract SHA-256",
            "tools/papertiger/agent_integration.md",
            "pre-receipt agent contract",
            true,
        ),
        (
            "Mise contract SHA-256",
            PRE_RECEIPT_MISE_PATH,
            "pre-receipt Mise contract",
            true,
        ),
    ];
    let mut owned_hashes = BTreeMap::new();
    for (label, relative, description, canonical_text) in entries {
        let recorded = manifest_digest(text, label)
            .with_context(|| format!("validate {} in {}", label, manifest.display()))?;
        let path = root.join(relative);
        if !path.is_file() {
            return Err(anyhow!(
                "{} records {} but {} is missing; restore the recorded pre-receipt install or move {} and its old install files aside deliberately, then rerun setup-project",
                manifest.display(),
                description,
                path.display(),
                manifest.display()
            ));
        }
        let bytes =
            fs::read(&path).with_context(|| format!("read {} {}", description, path.display()))?;
        let actual = if canonical_text {
            managed_text_sha256(&bytes)
        } else {
            papertiger::sha256(&bytes)
        };
        if actual != recorded {
            return Err(anyhow!(
                "{} records {} SHA-256 {} but {} has {}; restore the recorded file or move the pre-receipt install aside deliberately, then rerun setup-project",
                manifest.display(),
                description,
                recorded,
                path.display(),
                actual
            ));
        }
        owned_hashes.insert(relative.to_owned(), recorded);
    }

    Ok(Some(PreReceiptInstall::Verified {
        owned_hashes,
        retired_paths: vec![
            PRE_RECEIPT_MANIFEST_PATH.to_owned(),
            PRE_RECEIPT_BINARY_PATH.to_owned(),
            PRE_RECEIPT_MISE_PATH.to_owned(),
        ],
    }))
}

fn manifest_digest(text: &str, label: &str) -> Result<String> {
    let prefix = format!("- {label}: `");
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix(&prefix)?.strip_suffix('`'))
        .ok_or_else(|| anyhow!("missing exact {label} entry"))?;
    papertiger::validate_sha256(value, label)?;
    Ok(value.to_owned())
}

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

pub(super) fn preflight_managed_file(
    destination: &Path,
    file: &ManagedFile,
    prior_sha256: Option<&str>,
    install_owned_runtime: bool,
    dry_run: bool,
    replace_managed: bool,
) -> Result<(SetupActionKind, bool)> {
    if !destination.exists() {
        return Ok((SetupActionKind::Create, false));
    }
    if !destination.is_file() {
        return Err(anyhow!(
            "setup-project destination is not a file: {}",
            destination.display()
        ));
    }
    let existing = fs::read(destination)
        .with_context(|| format!("read managed file {}", destination.display()))?;
    let content_matches = if file.content_kind.is_receipt_text() {
        managed_text_sha256(&existing) == managed_text_sha256(&file.content)
    } else {
        existing == file.content
    };
    if !content_matches {
        let existing_sha256 = if file.content_kind.is_receipt_text() {
            managed_text_sha256(&existing)
        } else {
            papertiger::sha256(&existing)
        };
        let prior_content_owned = prior_sha256 == Some(existing_sha256.as_str());
        if prior_content_owned || install_owned_runtime || replace_managed {
            return Ok((SetupActionKind::Replace, false));
        }
        if dry_run {
            return Ok((SetupActionKind::ModifiedRefusal, true));
        }
        return Err(anyhow!(
            "setup-project found modified or unowned managed file {}; preserve or move repository-owned content, or review and rerun with --replace-managed",
            destination.display()
        ));
    }
    if file.executable && executable_bit_missing(destination)? {
        Ok((SetupActionKind::MakeExecutable, false))
    } else {
        Ok((SetupActionKind::Unchanged, false))
    }
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
