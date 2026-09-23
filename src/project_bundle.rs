//! Relocatable release identity; local receipts retain authority selection.
use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Deserialize)]
struct Manifest {
    schema: String,
    name: String,
    version: String,
    binary_sha256: BTreeMap<String, String>,
}

pub(crate) fn verify(root: &Path) -> Result<bool> {
    let path = root.join("tools/papertiger/manifest.json");
    if !path.exists() {
        return Ok(false);
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let header: serde_json::Value = serde_json::from_slice(&bytes)
        .context("invalid bundle manifest; restore tools/papertiger from a verified release")?;
    let manifest: Manifest = serde_json::from_value(header)
        .context("incomplete bundle manifest; restore tools/papertiger from a verified release")?;
    let binary = format!("bin/papertiger{}", std::env::consts::EXE_SUFFIX);
    let expected = manifest.binary_sha256.get(&binary);
    let installed = root.join("tools/papertiger").join(&binary);
    if manifest.schema != "papertiger.release_manifest.v3"
        || manifest.name != "papertiger"
        || manifest.version != env!("CARGO_PKG_VERSION")
        || expected
            != Some(&papertiger::sha256(&fs::read(&installed).with_context(
                || {
                    format!(
                        "missing {}; restore the complete project release",
                        installed.display()
                    )
                },
            )?))
        || expected != Some(&papertiger::sha256(&fs::read(std::env::current_exe()?)?))
    {
        bail!(
            "project bundle identity differs from the running executable; use {} from a complete verified release",
            installed.display()
        );
    }
    Ok(true)
}
