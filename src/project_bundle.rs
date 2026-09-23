//! Relocatable release identity; local receipts retain authority selection.
//!
//! Binary bytes are verified once, at download, against the release checksum.
//! At run time the manifest only has to name this release.
use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const MANIFEST_SCHEMA: &str = "papertiger.release_manifest.v3";
pub(crate) const MANIFEST_PATH: &str = "tools/papertiger/manifest.json";

#[derive(Deserialize)]
struct Manifest {
    schema: String,
    name: String,
    version: String,
}

/// The release a project's bundle manifest names, relative to the running
/// binary. An unreadable or invalid manifest is an error, never `Absent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BundleRelease {
    Absent,
    Running,
    Other(String),
}

pub(crate) fn release(root: &Path) -> Result<BundleRelease> {
    let path = root.join(MANIFEST_PATH);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BundleRelease::Absent);
        }
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        Ok(_) => {}
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let manifest: Manifest = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "invalid bundle manifest {}; restore tools/papertiger from a verified release",
            path.display()
        )
    })?;
    if manifest.schema != MANIFEST_SCHEMA || manifest.name != "papertiger" {
        bail!(
            "bundle manifest {} is not a {MANIFEST_SCHEMA} Papertiger manifest; restore tools/papertiger from a verified release",
            path.display()
        );
    }
    if manifest.version == env!("CARGO_PKG_VERSION") {
        Ok(BundleRelease::Running)
    } else {
        Ok(BundleRelease::Other(manifest.version))
    }
}

pub(crate) fn verify(root: &Path) -> Result<bool> {
    match release(root)? {
        BundleRelease::Absent => Ok(false),
        BundleRelease::Running => Ok(true),
        BundleRelease::Other(version) => {
            let running = env!("CARGO_PKG_VERSION");
            bail!(
                "project bundle {} is Papertiger {version}, but the running binary is {running}; invoke {} from that release, or unpack the verified Papertiger {running} release over the project root",
                root.join(MANIFEST_PATH).display(),
                root.join(format!(
                    "tools/papertiger/bin/papertiger{}",
                    std::env::consts::EXE_SUFFIX
                ))
                .display()
            );
        }
    }
}
