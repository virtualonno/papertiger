//! Canonical recovery-export materialization and its verification receipt.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::{Dump, atomic_create_file, atomic_replace_file, portable_absolute, sha256};

#[derive(Debug, Clone, Serialize)]
pub struct ExportFileReceipt {
    pub schema: String,
    pub output: String,
    pub dump_schema: String,
    pub sha256: String,
    pub bytes: usize,
    pub plans: usize,
    pub tasks: usize,
    pub events: usize,
    pub mise_projections: usize,
}

pub fn write_export_file(path: &Path, dump: &Dump, replace: bool) -> Result<ExportFileReceipt> {
    if path.as_os_str().is_empty() {
        bail!("export --output requires a nonblank file path");
    }
    let destination_exists = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => bail!(
            "export refuses symlink destination {}; choose a regular file path",
            path.display()
        ),
        Ok(metadata) if !metadata.is_file() => bail!(
            "export destination {} exists but is not a regular file; choose a regular file path",
            path.display()
        ),
        Ok(_) if !replace => bail!(
            "export destination {} already exists; inspect it, then rerun with --replace to atomically replace it",
            path.display()
        ),
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error).with_context(|| {
                format!("inspect Papertiger export destination {}", path.display())
            });
        }
    };
    let mut bytes = serde_json::to_vec_pretty(dump)?;
    bytes.push(b'\n');
    if destination_exists {
        atomic_replace_file(
            path,
            &bytes,
            "Papertiger export",
            "`papertiger export --output <path> --replace`",
        )?;
    } else {
        atomic_create_file(
            path,
            &bytes,
            "Papertiger export",
            "`papertiger export --output <path>`",
        )?;
    }
    let resolved = std::fs::canonicalize(path)
        .with_context(|| format!("resolve written Papertiger export {}", path.display()))?;
    Ok(ExportFileReceipt {
        schema: "papertiger.export_file.v1".into(),
        output: portable_absolute(&resolved)?,
        dump_schema: dump.schema.clone(),
        sha256: sha256(&bytes),
        bytes: bytes.len(),
        plans: dump.plans.len(),
        tasks: dump.tasks.len(),
        events: dump.events.len(),
        mise_projections: dump.mise_projections.len(),
    })
}
