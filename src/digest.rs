use anyhow::{Result, bail};
use sha2::{Digest as _, Sha256};

/// Refuse any value that is not exactly 64 lowercase hexadecimal characters.
pub fn validate_sha256(value: &str, name: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{name} must be exactly 64 lowercase hexadecimal characters");
    }
    Ok(())
}

/// Hex-render the SHA-256 digest of raw bytes.
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Return the raw SHA-256 digest for callers that need binary combination.
pub fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
