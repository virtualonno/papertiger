//! Consistent authority recovery without reinterpreting historical records.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use serde::Serialize;

use crate::{
    SCHEMA_VERSION, atomic_file::atomic_create_with, configure_connection, digest::sha256_reader,
    has_table, portable_absolute, require_planner_identity, schema_version,
};

#[derive(Debug, Serialize)]
pub struct BackupReceipt {
    pub schema: &'static str,
    pub source: String,
    pub output: String,
    pub source_schema_version: i64,
    pub sha256: String,
    pub bytes: u64,
    pub tasks: i64,
    pub events: i64,
    pub semantic_validation: &'static str,
}

/// Make a standalone SQLite recovery file. Older supported schemas remain older;
/// this neither migrates nor validates task/evidence semantics.
pub fn backup_authority(source: &Path, output: &Path) -> Result<BackupReceipt> {
    backup_inner(source, output).with_context(|| {
        format!("back up Papertiger authority {}; retain the source and retry `papertiger --db <source> backup --output <new-path>` after correcting the reported input", source.display())
    })
}

fn backup_inner(source: &Path, output: &Path) -> Result<BackupReceipt> {
    if output.as_os_str().is_empty() || output.file_name().is_none() {
        bail!("backup --output requires a nonblank new file path");
    }
    require_absent(output)?;
    require_no_sidecars(output)?;
    let source = fs::canonicalize(source).context(
        "source must be an existing Papertiger SQLite file; do not initialize a missing authority",
    )?;
    if !source.is_file() {
        bail!("backup source must be a regular Papertiger SQLite file; select it with --db");
    }
    let source_name = portable_absolute(&source)?;
    let conn = configure_connection(Connection::open_with_flags(
        &source,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)?;
    // Pin schema admission, counts and every copied page to one SQLite read snapshot.
    conn.execute_batch("BEGIN DEFERRED TRANSACTION")?;
    if !has_table(&conn, "meta")? {
        bail!(
            "backup source has no Papertiger authority metadata; select the planning database with --db"
        );
    }
    require_planner_identity(&conn, &source_name, true)?;
    let version = schema_version(&conn, &source_name)?;
    if !(1..=SCHEMA_VERSION).contains(&version) {
        bail!(
            "backup does not support schema v{version}; use a Papertiger release that supports that schema"
        );
    }
    let tasks = conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
    let events = conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
    let mut snapshot_sha256 = String::new();
    atomic_create_with(
        output,
        "Papertiger backup",
        "`papertiger backup --output <new-path>`",
        |staged| {
            // Resolve the now-existing parent so aliases such as new/../source-wal
            // cannot publish a recovery file into the live database's sidecars.
            require_distinct_from_source(&source, output, staged.parent().unwrap())?;
            require_no_sidecars(staged)?;
            let mut destination = configure_connection(Connection::open_with_flags(
                staged,
                OpenFlags::SQLITE_OPEN_READ_WRITE,
            )?)?;
            {
                let backup = Backup::new(&conn, &mut destination)?;
                if backup.step(-1)? != StepResult::Done {
                    bail!(
                        "backup could not acquire the database locks; retry `papertiger backup --output <new-path>` after the current database operation finishes"
                    );
                }
            }
            // A WAL source must still produce one self-contained recovery file.
            let journal: String =
                destination.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
            if journal != "delete" {
                bail!(
                    "backup did not produce a standalone database; retain the source and select a new --output path"
                );
            }
            let checks = destination
                .prepare("PRAGMA quick_check")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if checks != ["ok"] {
                bail!(
                    "backup SQLite integrity check failed: {}; retain the source and recover a known intact authority",
                    checks.join("; ")
                );
            }
            destination
                .close()
                .map_err(|(_, error)| error)
                .context("close standalone recovery database before publication")?;
            require_no_sidecars(staged)?;
            // Recheck just before exclusive publication; existing sidecars are never removed.
            require_no_sidecars(output)?;
            snapshot_sha256 = sha256_reader(&mut fs::File::open(staged)?)?;
            Ok(())
        },
    )?;
    let actual = sha256_reader(&mut fs::File::open(output)?)?;
    if actual != snapshot_sha256 {
        bail!(
            "published backup bytes changed; retain the source and retry backup --output <new-path>"
        );
    }
    require_no_sidecars(output)?;
    Ok(BackupReceipt {
        schema: "papertiger.backup.v1",
        source: source_name,
        output: portable_absolute(&fs::canonicalize(output)?)?,
        source_schema_version: version,
        sha256: actual,
        bytes: fs::metadata(output)?.len(),
        tasks,
        events,
        semantic_validation: "not_performed",
    })
}

fn require_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!(
            "backup destination {} already exists; choose a new --output path",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "inspect backup destination {}; choose an accessible --output path",
                path.display()
            )
        }),
    }
}

fn require_no_sidecars(path: &Path) -> Result<()> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        require_absent(Path::new(&sidecar))?;
    }
    Ok(())
}

fn require_distinct_from_source(source: &Path, output: &Path, parent: &Path) -> Result<()> {
    let name = output
        .file_name()
        .unwrap()
        .to_str()
        .context("backup --output file name must be UTF-8")?;
    #[cfg(windows)]
    if name.contains(':') {
        bail!("backup --output must name a standalone file, not a Windows alternate data stream");
    }
    let parent = fs::canonicalize(parent)?;
    let identity = |path: &Path| -> Result<String> {
        let text = portable_absolute(path)?;
        #[cfg(windows)]
        {
            Ok(text.trim_end_matches([' ', '.']).to_uppercase())
        }
        #[cfg(not(windows))]
        {
            Ok(text)
        }
    };
    let output_identity = identity(&parent.join(name))?;
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut reserved = source.as_os_str().to_owned();
        reserved.push(suffix);
        if output_identity == identity(Path::new(&reserved))? {
            bail!(
                "backup destination names the source database or its SQLite sidecar; choose a new --output path"
            );
        }
    }
    Ok(())
}
