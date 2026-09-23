//! Persisted project-install receipt: release version, authority path, and
//! selected skill targets.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use semver::Version;
use serde::{Deserialize, Serialize};

use super::{DEFAULT_AUTHORITY_PATH, normalize_authority_path};

pub(super) const INSTALL_RECEIPT_SCHEMA: &str = "papertiger.project_install.v3";
const PREVIOUS_INSTALL_RECEIPT_SCHEMA: &str = "papertiger.project_install.v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SkillTarget {
    Agents,
    Claude,
}

impl SkillTarget {
    pub(super) fn managed_path(self) -> &'static str {
        match self {
            Self::Agents => ".agents/skills/papertiger/SKILL.md",
            Self::Claude => ".claude/skills/papertiger/SKILL.md",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallReceipt {
    pub(super) schema: String,
    pub(super) papertiger_version: String,
    pub(super) authority_path: String,
    pub(super) skill_targets: Vec<SkillTarget>,
}

/// A v2 receipt also carries a `managed_files` list; setup-project ignores it
/// and rewrites the receipt as v3.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviousInstallReceipt {
    schema: String,
    papertiger_version: String,
    authority_path: String,
    skill_targets: Vec<SkillTarget>,
    #[serde(rename = "managed_files")]
    _managed_files: serde::de::IgnoredAny,
}

#[derive(Debug, Deserialize)]
struct ReceiptHeader {
    schema: String,
}

pub(super) fn build_install_receipt(
    authority_path: &str,
    skill_targets: &[SkillTarget],
) -> InstallReceipt {
    InstallReceipt {
        schema: INSTALL_RECEIPT_SCHEMA.to_owned(),
        papertiger_version: env!("CARGO_PKG_VERSION").to_owned(),
        authority_path: authority_path.to_owned(),
        skill_targets: skill_targets.to_vec(),
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
            "project-install receipt is not a file: {}; move it aside, then rerun `papertiger setup-project <project-root>`",
            path.display()
        ));
    }
    let bytes =
        fs::read(path).with_context(|| format!("read install receipt {}", path.display()))?;
    let invalid = || {
        format!(
            "parse {}; restore a valid {} receipt before changing project integration",
            path.display(),
            INSTALL_RECEIPT_SCHEMA
        )
    };
    let header: ReceiptHeader = serde_json::from_slice(&bytes).with_context(invalid)?;
    let receipt = match header.schema.as_str() {
        INSTALL_RECEIPT_SCHEMA => serde_json::from_slice(&bytes).with_context(invalid)?,
        PREVIOUS_INSTALL_RECEIPT_SCHEMA => {
            let previous: PreviousInstallReceipt =
                serde_json::from_slice(&bytes).with_context(invalid)?;
            InstallReceipt {
                schema: previous.schema,
                papertiger_version: previous.papertiger_version,
                authority_path: previous.authority_path,
                skill_targets: previous.skill_targets,
            }
        }
        other => return Err(unsupported_schema(path, other)),
    };
    validate_install_receipt(path, &receipt)?;
    Ok(Some(receipt))
}

fn unsupported_schema(path: &Path, schema: &str) -> anyhow::Error {
    anyhow!(
        "unsupported project-install receipt schema {schema:?} at {}; move it aside and reinstall with `papertiger setup-project <project-root>`, adding `--authority-path <path>` when the authority is not {DEFAULT_AUTHORITY_PATH}",
        path.display()
    )
}

fn validate_install_receipt(path: &Path, receipt: &InstallReceipt) -> Result<()> {
    if receipt.schema != INSTALL_RECEIPT_SCHEMA && receipt.schema != PREVIOUS_INSTALL_RECEIPT_SCHEMA
    {
        return Err(unsupported_schema(path, &receipt.schema));
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
    if !receipt
        .skill_targets
        .windows(2)
        .all(|pair| pair[0] < pair[1])
    {
        return Err(anyhow!(
            "project-install receipt skill_targets must be unique and ordered as agents, claude"
        ));
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
