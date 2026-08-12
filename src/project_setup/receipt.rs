//! Persisted ownership receipt and canonical identity for setup-managed text.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use semver::Version;
use serde::{Deserialize, Serialize};

use super::{
    ManagedFile, PRE_RECEIPT_MANIFEST_PATH, SetupActionKind, normalize_authority_path,
    normalized_path,
};

pub(super) const INSTALL_RECEIPT_SCHEMA: &str = "papertiger.project_install.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallReceipt {
    pub(super) schema: String,
    pub(super) papertiger_version: String,
    pub(super) authority_path: String,
    pub(super) managed_files: Vec<ManagedFileReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManagedFileReceipt {
    pub(super) path: String,
    pub(super) sha256: String,
}

pub(super) fn build_install_receipt(
    authority_path: &str,
    managed: &[ManagedFile],
) -> InstallReceipt {
    InstallReceipt {
        schema: INSTALL_RECEIPT_SCHEMA.to_owned(),
        papertiger_version: env!("CARGO_PKG_VERSION").to_owned(),
        authority_path: authority_path.to_owned(),
        managed_files: managed
            .iter()
            .filter(|file| file.content_kind.is_receipt_text())
            .map(|file| ManagedFileReceipt {
                path: normalized_path(&file.relative_path),
                sha256: managed_text_sha256(&file.content),
            })
            .collect(),
    }
}

pub(super) fn receipt_bytes(receipt: &InstallReceipt) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(receipt).context("serialize project-install receipt")?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(super) fn load_install_receipt(path: &Path) -> Result<Option<InstallReceipt>> {
    if !path.exists() {
        return Ok(None);
    }
    if !path.is_file() {
        return Err(anyhow!(
            "setup-project install receipt is not a file: {}",
            path.display()
        ));
    }
    let bytes =
        fs::read(path).with_context(|| format!("read install receipt {}", path.display()))?;
    let receipt: InstallReceipt = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "parse {}; restore a valid {} receipt or move it aside before setup-project",
            path.display(),
            INSTALL_RECEIPT_SCHEMA
        )
    })?;
    validate_install_receipt(&receipt)?;
    Ok(Some(receipt))
}

fn validate_install_receipt(receipt: &InstallReceipt) -> Result<()> {
    if receipt.schema != INSTALL_RECEIPT_SCHEMA {
        return Err(anyhow!(
            "unsupported project-install receipt schema {:?}; use a Papertiger release that owns it or migrate the receipt deliberately",
            receipt.schema
        ));
    }
    let version = Version::parse(&receipt.papertiger_version).map_err(|_| {
        anyhow!(
            "project-install receipt papertiger_version must be a canonical semantic version, found {:?}",
            receipt.papertiger_version
        )
    })?;
    if version.to_string() != receipt.papertiger_version {
        return Err(anyhow!(
            "project-install receipt papertiger_version must be canonical: expected {version}"
        ));
    }
    let canonical_authority = normalize_authority_path(Path::new(&receipt.authority_path))?;
    if canonical_authority != receipt.authority_path {
        return Err(anyhow!(
            "project-install receipt authority_path must be canonical: expected {canonical_authority}"
        ));
    }
    let mut paths = HashSet::new();
    for file in &receipt.managed_files {
        let relative = Path::new(&file.path);
        validate_receipt_managed_path(relative)?;
        if normalized_path(relative) != file.path {
            return Err(anyhow!(
                "project-install receipt managed path must be canonical: {}",
                file.path
            ));
        }
        if !paths.insert(file.path.as_str()) {
            return Err(anyhow!(
                "project-install receipt repeats managed path {}",
                file.path
            ));
        }
        papertiger::validate_sha256(&file.sha256, "project-install managed file sha256")?;
    }
    Ok(())
}

pub(super) fn refuse_release_downgrade(receipt: &InstallReceipt) -> Result<()> {
    let installed = Version::parse(&receipt.papertiger_version)
        .context("parse validated project-install receipt version")?;
    let running = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("parse running Papertiger package version")?;
    if installed > running {
        return Err(anyhow!(
            "setup-project refuses to downgrade project-managed Papertiger from receipt version {installed} to running version {running}; use a verified Papertiger {installed} or newer binary and rerun setup-project"
        ));
    }
    Ok(())
}

pub(super) fn validate_receipt_managed_path(path: &Path) -> Result<()> {
    let normalized = normalized_path(path);
    let allowed = matches!(
        normalized.as_str(),
        "scripts/papertiger"
            | "scripts/papertiger.cmd"
            | "tools/papertiger/agent_integration.md"
            | ".agents/skills/papertiger/SKILL.md"
            | ".claude/skills/papertiger/SKILL.md"
            | PRE_RECEIPT_MANIFEST_PATH
    );
    if !allowed {
        return Err(anyhow!(
            "project-install receipt names unsupported managed path {normalized}; this release will not modify or remove it"
        ));
    }
    Ok(())
}

pub(super) fn receipt_hashes(receipt: &InstallReceipt) -> Result<BTreeMap<String, String>> {
    validate_install_receipt(receipt)?;
    Ok(receipt
        .managed_files
        .iter()
        .map(|file| (file.path.clone(), file.sha256.clone()))
        .collect())
}

pub(super) fn preflight_receipt(
    destination: &Path,
    expected: &[u8],
    prior_receipt_loaded: bool,
    dry_run: bool,
    replace_managed: bool,
) -> Result<(SetupActionKind, bool)> {
    if !destination.exists() {
        return Ok((SetupActionKind::Create, false));
    }
    let existing = fs::read(destination)
        .with_context(|| format!("read project-install receipt {}", destination.display()))?;
    if managed_text_sha256(&existing) == managed_text_sha256(expected) {
        return Ok((SetupActionKind::Unchanged, false));
    }
    if prior_receipt_loaded || replace_managed {
        return Ok((SetupActionKind::Replace, false));
    }
    if dry_run {
        return Ok((SetupActionKind::ModifiedRefusal, true));
    }
    Err(anyhow!(
        "setup-project found an unowned file at {}; move it aside or review and rerun with --replace-managed",
        destination.display()
    ))
}

pub(super) fn canonical_managed_text(content: &[u8]) -> Cow<'_, [u8]> {
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

pub(super) fn managed_text_sha256(content: &[u8]) -> String {
    papertiger::sha256(canonical_managed_text(content).as_ref())
}
