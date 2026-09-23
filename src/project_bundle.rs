//! Relocatable release identity; local receipts retain authority selection.
//!
//! Binary bytes are verified once, at download, against the release checksum.
//! At run time the manifest only has to name this release.
use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const MANIFEST_SCHEMA: &str = "papertiger.release_manifest.v3";

#[derive(Deserialize)]
struct Manifest {
    schema: String,
    name: String,
    version: String,
}

pub(crate) fn verify(root: &Path) -> Result<bool> {
    let path = root.join("tools/papertiger/manifest.json");
    if !path.exists() {
        return Ok(false);
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
    let running = env!("CARGO_PKG_VERSION");
    if manifest.version != running {
        bail!(
            "project bundle {} is Papertiger {}, but the running binary is {running}; invoke {} from that release, or unpack the verified Papertiger {running} release over the project root",
            path.display(),
            manifest.version,
            root.join(format!(
                "tools/papertiger/bin/papertiger{}",
                std::env::consts::EXE_SUFFIX
            ))
            .display()
        );
    }
    Ok(true)
}
