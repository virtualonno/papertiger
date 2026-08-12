//! Cross-platform rendering for already resolved local filesystem identities.

use std::path::Path;

use anyhow::{Context, Result};

/// Render an absolute path without Windows verbatim prefixes and with forward
/// slashes so diagnostics and cross-platform tool arguments stay readable.
/// This function does not resolve the filesystem; canonicalize first when
/// physical identity matters.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_paths_remove_windows_verbatim_prefixes() {
        assert_eq!(
            portable_absolute(Path::new(r"\\?\C:\projects\papertiger")).unwrap(),
            "C:/projects/papertiger"
        );
        assert_eq!(
            portable_absolute(Path::new(r"\\?\UNC\server\share\repo")).unwrap(),
            "//server/share/repo"
        );
        assert_eq!(
            portable_absolute(Path::new("/tmp/fixture/run")).unwrap(),
            "/tmp/fixture/run"
        );
    }
}
