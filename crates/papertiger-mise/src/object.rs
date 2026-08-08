use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::digest::{sha256, validate_sha256};
use crate::store::now;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreservedObject {
    pub sha256: String,
    pub bytes: u64,
    pub locator: String,
}

pub fn object_locator(sha256: &str) -> Result<String> {
    validate_sha256(sha256, "object digest")?;
    Ok(format!("sha256/{}/{sha256}", &sha256[..2]))
}

pub fn preserve_object(root: &Path, bytes: &[u8]) -> Result<PreservedObject> {
    let sha256 = sha256(bytes);
    let locator = object_locator(&sha256)?;
    let destination = root.join(Path::new(&locator));
    reject_linked_existing_ancestors(root, &destination)?;
    fs::create_dir_all(
        destination
            .parent()
            .context("content-addressed object has no parent")?,
    )?;
    if destination.exists() {
        verify_exact_object(&destination, bytes, &sha256)?;
        return Ok(PreservedObject {
            sha256,
            bytes: u64::try_from(bytes.len()).context("object is too large")?,
            locator,
        });
    }

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time precedes Unix epoch")?
        .as_nanos();
    let temporary = destination.with_file_name(format!(
        ".{}-{}-{nonce}.mise.tmp",
        sha256,
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("create temporary object {}", temporary.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = fs::rename(&temporary, &destination) {
        if destination.exists() {
            verify_exact_object(&destination, bytes, &sha256)?;
            fs::remove_file(&temporary)?;
        } else {
            let _ = fs::remove_file(&temporary);
            return Err(error).context("publish content-addressed Mise object");
        }
    }
    verify_exact_object(&destination, bytes, &sha256)?;
    Ok(PreservedObject {
        sha256,
        bytes: u64::try_from(bytes.len()).context("object is too large")?,
        locator,
    })
}

pub fn verify_object(root: &Path, object: &PreservedObject) -> Result<()> {
    read_object(root, object).map(|_| ())
}

pub fn read_object(root: &Path, object: &PreservedObject) -> Result<Vec<u8>> {
    let expected = object_locator(&object.sha256)?;
    if object.locator != expected {
        bail!("object locator does not match its SHA-256 identity");
    }
    let path = root.join(&object.locator);
    reject_linked_existing_ancestors(root, &path)?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("retained object is not a plain file: {}", path.display());
    }
    let bytes =
        fs::read(&path).with_context(|| format!("read retained object {}", path.display()))?;
    if u64::try_from(bytes.len())? != object.bytes || sha256(&bytes) != object.sha256 {
        bail!("retained object '{}' failed exact identity", object.locator);
    }
    Ok(bytes)
}

pub(crate) fn read_verified_json<T>(
    root: &Path,
    object: &PreservedObject,
    role: &str,
) -> Result<(T, Vec<u8>)>
where
    T: DeserializeOwned,
{
    let bytes = read_object(root, object)?;
    let value: T = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse retained {role} as typed JSON"))?;
    Ok((value, bytes))
}

pub(crate) fn indexed_object(connection: &Connection, sha256: &str) -> Result<PreservedObject> {
    connection
        .query_row(
            "SELECT bytes, locator FROM artifacts WHERE sha256=?1",
            params![sha256],
            |row| {
                Ok(PreservedObject {
                    sha256: sha256.to_owned(),
                    bytes: u64::try_from(row.get::<_, i64>(0)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                    locator: row.get(1)?,
                })
            },
        )
        .optional()?
        .with_context(|| format!("indexed artifact {sha256} disappeared"))
}

pub(crate) fn record_indexed_object(
    connection: &Connection,
    object: &PreservedObject,
    media_type: &str,
) -> Result<()> {
    if let Some((bytes, locator, durable_media_type)) = connection
        .query_row(
            "SELECT bytes, locator, media_type FROM artifacts WHERE sha256=?1",
            params![object.sha256],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
    {
        if u64::try_from(bytes)? != object.bytes
            || locator != object.locator
            || durable_media_type != media_type
        {
            bail!(
                "artifact '{}' conflicts with durable identity",
                object.sha256
            );
        }
        return Ok(());
    }
    connection.execute(
        "INSERT INTO artifacts (sha256, bytes, locator, media_type, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            object.sha256,
            i64::try_from(object.bytes)?,
            object.locator,
            media_type,
            now(),
        ],
    )?;
    Ok(())
}

fn verify_exact_object(path: &Path, expected_bytes: &[u8], expected_sha256: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!(
            "content-addressed object is not a plain file: {}",
            path.display()
        );
    }
    let actual = fs::read(path)?;
    if actual != expected_bytes || sha256(&actual) != expected_sha256 {
        bail!("content-addressed object collision at {}", path.display());
    }
    Ok(())
}

fn reject_linked_existing_ancestors(root: &Path, destination: &Path) -> Result<()> {
    let root = absolute_lexical(root)?;
    let destination = absolute_lexical(destination)?;
    if !destination.starts_with(&root) || destination == root {
        bail!("object destination escapes the configured object root");
    }
    let relative = destination
        .strip_prefix(&root)
        .context("derive object path beneath configured root")?;
    let mut current = root;
    if current.exists() {
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() {
            bail!(
                "object path traverses a symbolic link: {}",
                current.display()
            );
        }
    }
    for component in relative.components() {
        current.push(component.as_os_str());
        if current.exists() {
            let metadata = fs::symlink_metadata(&current)?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "object path traverses a symbolic link: {}",
                    current.display()
                );
            }
        }
    }
    Ok(())
}

fn absolute_lexical(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn exact_bytes_are_published_once_and_reverified() {
        let directory = tempdir().expect("object root");
        let first =
            preserve_object(directory.path(), b"rejected patch bytes").expect("first preservation");
        let second = preserve_object(directory.path(), b"rejected patch bytes")
            .expect("idempotent preservation");
        assert_eq!(first, second);
        verify_object(directory.path(), &second).expect("verified retained object");
    }

    #[test]
    fn locator_is_derived_only_from_an_exact_digest() {
        assert_eq!(
            object_locator(&"a".repeat(64)).expect("locator"),
            format!("sha256/aa/{}", "a".repeat(64))
        );
        assert!(object_locator("short").is_err());
        assert!(object_locator(&"z".repeat(64)).is_err());
    }

    #[test]
    fn indexed_object_identity_includes_media_type() {
        let connection = Connection::open_in_memory().expect("database");
        crate::store::init(&connection).expect("schema");
        let directory = tempdir().expect("object root");
        let object = preserve_object(directory.path(), b"typed bytes").expect("object");
        record_indexed_object(&connection, &object, "application/json").expect("first index");
        record_indexed_object(&connection, &object, "application/json")
            .expect("idempotent exact index");
        let error = record_indexed_object(&connection, &object, "text/plain")
            .expect_err("media-type drift must fail closed");
        assert!(
            error
                .to_string()
                .contains("conflicts with durable identity")
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_path_alias_is_accepted_but_links_inside_the_object_root_are_refused() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("fixture root");
        let actual_parent = directory.path().join("actual");
        fs::create_dir(&actual_parent).expect("actual parent");
        let alias_parent = directory.path().join("alias");
        symlink(&actual_parent, &alias_parent).expect("external path alias");

        let aliased_root = alias_parent.join("objects");
        fs::create_dir(&aliased_root).expect("aliased object root");
        preserve_object(&aliased_root, b"accepted through host alias")
            .expect("a symlink outside the configured root is host path identity, not CAS content");

        let guarded_root = actual_parent.join("guarded-objects");
        fs::create_dir(&guarded_root).expect("guarded object root");
        let outside = actual_parent.join("outside");
        fs::create_dir(&outside).expect("outside directory");
        symlink(&outside, guarded_root.join("sha256")).expect("internal path link");
        let error = preserve_object(&guarded_root, b"must remain inside the CAS")
            .expect_err("a symlink below the configured root must fail closed");
        assert!(error.to_string().contains("traverses a symbolic link"));
    }
}
