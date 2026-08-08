use std::path::Path;

use anyhow::{Context, Result};

use crate::digest::sha256;

const TRIAL_PATH_IDENTITY_HEX_LENGTH: usize = 32;

/// Derive one bounded filesystem component from the exact campaign and trial
/// identities. Receipts retain the full identifiers; runtime-owned build paths
/// use this 128-bit domain-separated identity so caller-chosen names cannot
/// exhaust host path limits.
pub(crate) fn trial_path_identity(campaign_id: &str, trial_id: &str) -> String {
    let mut bytes = b"papertiger-mise.trial-path.v1\0".to_vec();
    bytes.extend_from_slice(campaign_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(trial_id.as_bytes());
    sha256(&bytes)[..TRIAL_PATH_IDENTITY_HEX_LENGTH].to_owned()
}

/// Render a path in Mise's platform-neutral form: Windows verbatim prefixes
/// removed, separators forward-slash. This one rendering serves both locator
/// identity and arguments to cross-platform tools (Rust's Windows
/// canonicalization returns an extended-length path, which native APIs accept
/// but Git for Windows does not consistently parse).
///
/// This does not resolve the filesystem. Callers that bind filesystem identity
/// must canonicalize first or use [`canonical_or_pending_absolute`].
pub fn portable_absolute(path: &Path) -> Result<String> {
    let value = path.to_str().context("portable path is not UTF-8")?;
    let value = if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = value.strip_prefix(r"\\?\") {
        local.to_owned()
    } else {
        value.to_owned()
    };
    Ok(value.replace('\\', "/"))
}

/// Bind an existing path, or a not-yet-created direct child, to its canonical
/// filesystem identity before rendering its portable locator.
pub(crate) fn canonical_or_pending_absolute(path: &Path) -> Result<String> {
    let canonical = if path.exists() {
        std::fs::canonicalize(path)?
    } else {
        let parent = path.parent().context("pending path has no parent")?;
        let parent = std::fs::canonicalize(parent)
            .with_context(|| format!("canonicalize pending path parent {}", parent.display()))?;
        parent.join(
            path.file_name()
                .context("pending path has no final component")?,
        )
    };
    portable_absolute(&canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_paths_remove_windows_verbatim_prefixes() {
        assert_eq!(
            portable_absolute(Path::new(r"\\?\C:\projects\papertiger")).expect("local path"),
            "C:/projects/papertiger"
        );
        assert_eq!(
            portable_absolute(Path::new(r"\\?\UNC\server\share\repo")).expect("UNC path"),
            "//server/share/repo"
        );
        assert_eq!(
            portable_absolute(Path::new("/tmp/fixture/run")).expect("POSIX path"),
            "/tmp/fixture/run"
        );
    }

    #[test]
    fn trial_paths_are_bounded_deterministic_and_exactly_scoped() {
        let identity = trial_path_identity(&"campaign-".repeat(40), &"trial-".repeat(40));
        assert_eq!(identity.len(), TRIAL_PATH_IDENTITY_HEX_LENGTH);
        assert!(identity.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            identity,
            trial_path_identity(&"campaign-".repeat(40), &"trial-".repeat(40))
        );
        assert_ne!(
            identity,
            trial_path_identity(&"campaign-".repeat(40), "different-trial")
        );
    }
}
