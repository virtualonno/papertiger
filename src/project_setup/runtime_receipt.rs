//! Host-local identity receipt for the installed native planner binary.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use semver::Version;
use serde::{Deserialize, Serialize};

use super::filesystem::validate_destination;
use super::{SetupActionKind, normalized_path};

const RUNTIME_INSTALL_RECEIPT_SCHEMA: &str = "papertiger.runtime_install.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeInstallReceipt {
    pub(crate) schema: String,
    pub(crate) papertiger_version: String,
    pub(crate) binary: RuntimeBinaryIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeBinaryIdentity {
    pub(crate) path: String,
    pub(crate) bytes: u64,
    pub(crate) sha256: String,
}

pub(super) fn runtime_receipt_relative_path(binary_path: &Path) -> Result<PathBuf> {
    let binary_path = normalized_path(binary_path);
    let supported = [
        "tools/papertiger/bin/papertiger",
        "tools/papertiger/bin/papertiger.exe",
    ];
    if !supported.contains(&binary_path.as_str()) {
        return Err(anyhow!(
            "runtime-install receipt requires one canonical host binary path under tools/papertiger/bin, found {binary_path:?}"
        ));
    }
    Ok(PathBuf::from(format!("{binary_path}.runtime-install.json")))
}

pub(super) fn current_host_binary_path() -> PathBuf {
    PathBuf::from(format!(
        "tools/papertiger/bin/papertiger{}",
        std::env::consts::EXE_SUFFIX
    ))
}

pub(super) fn build_runtime_install_receipt(
    binary_path: &Path,
    binary: &[u8],
) -> RuntimeInstallReceipt {
    RuntimeInstallReceipt {
        schema: RUNTIME_INSTALL_RECEIPT_SCHEMA.to_owned(),
        papertiger_version: env!("CARGO_PKG_VERSION").to_owned(),
        binary: RuntimeBinaryIdentity {
            path: normalized_path(binary_path),
            bytes: binary.len() as u64,
            sha256: papertiger::sha256(binary),
        },
    }
}

pub(super) fn runtime_receipt_bytes(receipt: &RuntimeInstallReceipt) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(receipt).context("serialize runtime-install receipt")?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// The runtime receipt is release-owned host state: setup-project always
/// rewrites it to describe the binary it installs.
pub(super) fn preflight_runtime_receipt(path: &Path, desired: &[u8]) -> Result<SetupActionKind> {
    if !path.exists() {
        return Ok(SetupActionKind::Create);
    }
    if !path.is_file() {
        return Err(anyhow!(
            "runtime-install receipt is not a file: {}; move it aside, then rerun `papertiger setup-project <project-root>`",
            path.display()
        ));
    }
    let existing = fs::read(path)
        .with_context(|| format!("read runtime-install receipt {}", path.display()))?;
    Ok(if existing == desired {
        SetupActionKind::Unchanged
    } else {
        SetupActionKind::Replace
    })
}

pub(super) fn write_runtime_receipt(
    path: &Path,
    content: &[u8],
    action: SetupActionKind,
) -> Result<()> {
    match action {
        SetupActionKind::Create => papertiger::atomic_create_file(
            path,
            content,
            "runtime-install receipt",
            "`papertiger setup-project <project-root>`",
        )?,
        SetupActionKind::Replace => papertiger::atomic_replace_file(
            path,
            content,
            "runtime-install receipt",
            "`papertiger setup-project <project-root>`",
        )?,
        SetupActionKind::Unchanged => {}
        _ => unreachable!("runtime receipt action must create, replace, or remain unchanged"),
    }
    let installed = fs::read(path)
        .with_context(|| format!("verify runtime-install receipt {}", path.display()))?;
    if installed != content {
        return Err(anyhow!(
            "runtime-install receipt verification failed at {}; rerun setup-project after checking the filesystem",
            path.display()
        ));
    }
    Ok(())
}

pub(super) fn load_runtime_install_receipt(path: &Path) -> Result<RuntimeInstallReceipt> {
    let bytes = fs::read(path).with_context(|| {
        format!(
            "read runtime-install receipt {}; repair the host-local installation with `papertiger setup-project <project-root>`",
            path.display()
        )
    })?;
    let receipt: RuntimeInstallReceipt = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "parse runtime-install receipt {}; repair the host-local installation with `papertiger setup-project <project-root>`",
            path.display()
        )
    })?;
    validate_runtime_install_receipt(&receipt, env!("CARGO_PKG_VERSION"))?;
    Ok(receipt)
}

pub(super) fn verify_runtime_installation(
    root: &Path,
    receipt: &RuntimeInstallReceipt,
) -> Result<()> {
    validate_runtime_install_receipt(receipt, env!("CARGO_PKG_VERSION"))?;
    let expected_binary_path = normalized_path(&current_host_binary_path());
    if receipt.binary.path != expected_binary_path {
        return Err(anyhow!(
            "runtime-install receipt binary path must match this host installation: expected {expected_binary_path:?}, found {:?}; repair it with `papertiger setup-project <project-root>`",
            receipt.binary.path
        ));
    }
    let relative = Path::new(&receipt.binary.path);
    validate_destination(root, relative)?;
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).with_context(|| {
        format!(
            "inspect installed Papertiger binary {}; repair it with `papertiger setup-project <project-root>`",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(anyhow!(
            "installed Papertiger binary is not a regular non-symlink file at {}; repair it with `papertiger setup-project <project-root>`",
            path.display()
        ));
    }
    let bytes = stable_read(&path)?;
    let actual_sha256 = papertiger::sha256(&bytes);
    if bytes.len() as u64 != receipt.binary.bytes || actual_sha256 != receipt.binary.sha256 {
        return Err(anyhow!(
            "installed Papertiger binary identity does not match {}: expected {} bytes with SHA-256 {}, found {} bytes with SHA-256 {}; repair it with `papertiger setup-project <project-root>` from a trusted external Papertiger {} binary",
            path.display(),
            receipt.binary.bytes,
            receipt.binary.sha256,
            bytes.len(),
            actual_sha256,
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(())
}

fn validate_runtime_install_receipt(
    receipt: &RuntimeInstallReceipt,
    expected_version: &str,
) -> Result<()> {
    if receipt.schema != RUNTIME_INSTALL_RECEIPT_SCHEMA {
        return Err(anyhow!(
            "unsupported runtime-install receipt schema {:?}; repair the host-local installation with `papertiger setup-project <project-root>`",
            receipt.schema
        ));
    }
    let version = Version::parse(&receipt.papertiger_version).map_err(|_| {
        anyhow!(
            "runtime-install receipt papertiger_version must be a canonical semantic version, found {:?}; repair it with `papertiger setup-project <project-root>`",
            receipt.papertiger_version
        )
    })?;
    if version.to_string() != receipt.papertiger_version {
        return Err(anyhow!(
            "runtime-install receipt papertiger_version must be canonical: expected {version}; repair it with `papertiger setup-project <project-root>`"
        ));
    }
    if receipt.papertiger_version != expected_version {
        return Err(anyhow!(
            "runtime-install receipt requires Papertiger {}, but the expected installation version is {}; upgrade the project deliberately with `papertiger setup-project <project-root>`",
            receipt.papertiger_version,
            expected_version
        ));
    }
    let binary_path = PathBuf::from(&receipt.binary.path);
    let canonical_path = normalized_path(&binary_path);
    let supported = [
        "tools/papertiger/bin/papertiger",
        "tools/papertiger/bin/papertiger.exe",
    ];
    if canonical_path != receipt.binary.path || !supported.contains(&receipt.binary.path.as_str()) {
        return Err(anyhow!(
            "runtime-install receipt binary path must be one canonical host path under tools/papertiger/bin, found {:?}; repair it with `papertiger setup-project <project-root>`",
            receipt.binary.path
        ));
    }
    if receipt.binary.bytes == 0 {
        return Err(anyhow!(
            "runtime-install receipt binary byte count must be positive; repair it with `papertiger setup-project <project-root>`"
        ));
    }
    papertiger::validate_sha256(&receipt.binary.sha256, "runtime-install binary sha256")?;
    Ok(())
}

fn stable_read(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;

    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let before = file
        .metadata()
        .with_context(|| format!("inspect open file {}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    let after = file
        .metadata()
        .with_context(|| format!("reinspect open file {}", path.display()))?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err(anyhow!(
            "installed Papertiger binary changed while its bytes were read at {}; retry, then repair with `papertiger setup-project <project-root>` if the change persists",
            path.display()
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_carries_only_observable_host_binary_identity() {
        let receipt = build_runtime_install_receipt(
            Path::new("tools/papertiger/bin/papertiger.exe"),
            b"host binary",
        );
        assert_eq!(receipt.schema, RUNTIME_INSTALL_RECEIPT_SCHEMA);
        assert_eq!(receipt.binary.bytes, 11);
        assert_eq!(receipt.binary.sha256, papertiger::sha256(b"host binary"));
        validate_runtime_install_receipt(&receipt, env!("CARGO_PKG_VERSION")).unwrap();
        assert_eq!(
            runtime_receipt_relative_path(Path::new("tools/papertiger/bin/papertiger.exe"))
                .unwrap(),
            PathBuf::from("tools/papertiger/bin/papertiger.exe.runtime-install.json")
        );
    }
}
