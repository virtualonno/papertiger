//! Crash-resistant single-file installation shared by setup and recovery export.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};

static NEXT_STAGED_WRITE: AtomicU64 = AtomicU64::new(0);

pub fn atomic_create_file(
    path: &Path,
    content: &[u8],
    subject: &str,
    corrective_command: &str,
) -> Result<()> {
    atomic_write_file(path, content, subject, corrective_command, true)
}

pub fn atomic_replace_file(
    path: &Path,
    content: &[u8],
    subject: &str,
    corrective_command: &str,
) -> Result<()> {
    atomic_write_file(path, content, subject, corrective_command, false)
}

fn atomic_write_file(
    path: &Path,
    content: &[u8],
    subject: &str,
    corrective_command: &str,
    create_new: bool,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{subject} destination has no parent: {}", path.display()))?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    fs::create_dir_all(parent)
        .with_context(|| format!("create {subject} directory {}", parent.display()))?;
    if create_new && path.exists() {
        return Err(anyhow!(
            "refusing to replace concurrently created {subject} {}; inspect it, then run {corrective_command}",
            path.display(),
        ));
    }
    if !create_new && !path.is_file() {
        return Err(anyhow!(
            "refusing to replace missing or non-file {subject} {}; inspect it, then run {corrective_command}",
            path.display(),
        ));
    }

    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("{subject} destination has no file name: {}", path.display()))?;
    let serial = NEXT_STAGED_WRITE.fetch_add(1, Ordering::Relaxed);
    let staged = parent.join(format!(
        ".{}.papertiger-stage-{}-{serial}",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| -> Result<()> {
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .with_context(|| format!("create staged {subject} {}", staged.display()))?;
        output
            .write_all(content)
            .with_context(|| format!("write staged {subject} {}", staged.display()))?;
        output
            .flush()
            .with_context(|| format!("flush staged {subject} {}", staged.display()))?;
        output
            .sync_all()
            .with_context(|| format!("sync staged {subject} {}", staged.display()))?;
        drop(output);

        if create_new {
            fs::hard_link(&staged, path).with_context(|| {
                format!(
                    "atomically install new {subject} {}; check filesystem hard-link support, then run {corrective_command}",
                    path.display()
                )
            })?;
            fs::remove_file(&staged)
                .with_context(|| format!("remove staged {subject} {}", staged.display()))?;
        } else {
            replace_existing_file(&staged, path, subject)?;
        }
        let installed = fs::read(path)
            .with_context(|| format!("verify atomically installed {subject} {}", path.display()))?;
        if installed != content {
            return Err(anyhow!(
                "atomic {subject} verification failed at {}; inspect the filesystem, then run {corrective_command}",
                path.display()
            ));
        }
        Ok(())
    })();
    if let Err(error) = result {
        match fs::remove_file(&staged) {
            Ok(()) => return Err(error),
            Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => return Err(error),
            Err(cleanup) => {
                return Err(error).with_context(|| {
                    format!(
                        "cleanup also failed for staged {subject} {}: {cleanup}; remove that exact staged file after inspecting the original error",
                        staged.display()
                    )
                });
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_existing_file(staged: &Path, destination: &Path, subject: &str) -> Result<()> {
    fs::rename(staged, destination).with_context(|| {
        format!(
            "atomically replace {subject} {} from {}",
            destination.display(),
            staged.display()
        )
    })
}

#[cfg(windows)]
fn replace_existing_file(staged: &Path, destination: &Path, subject: &str) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }

    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let staged_wide = staged
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replaced = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            staged_wide.as_ptr(),
            std::ptr::null(),
            0x0000_0001,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if replaced == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "atomically replace {subject} {} from {}",
                destination.display(),
                staged.display()
            )
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_replace_refusals_bind_exact_corrective_messages() {
        let temporary = std::env::temp_dir().join(format!(
            "papertiger-atomic-file-test-{}-{}",
            std::process::id(),
            NEXT_STAGED_WRITE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&temporary).unwrap();
        let existing = temporary.join("existing.json");
        fs::write(&existing, b"original").unwrap();
        let error = atomic_create_file(&existing, b"replacement", "receipt", "retry-create")
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!(
                "refusing to replace concurrently created receipt {}; inspect it, then run retry-create",
                existing.display()
            )
        );
        assert_eq!(fs::read(&existing).unwrap(), b"original");

        let missing = temporary.join("missing.json");
        let error = atomic_replace_file(&missing, b"replacement", "receipt", "retry-replace")
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!(
                "refusing to replace missing or non-file receipt {}; inspect it, then run retry-replace",
                missing.display()
            )
        );
        assert!(!missing.exists());
        fs::remove_dir_all(temporary).unwrap();
    }
}
