//! Canonical validation vocabulary shared across Mise trust boundaries.

use anyhow::{Result, bail};

pub(crate) fn validate_nonblank(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must be nonblank");
    }
    Ok(())
}

pub(crate) fn validate_bounded_token(field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        bail!("{field} must be a bounded canonical token of at most 256 bytes");
    }
    Ok(())
}

pub(crate) fn validate_ascii_token(field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || value.trim() != value
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':')
        })
    {
        bail!("{field} must be a canonical nonempty token of at most 128 ASCII bytes");
    }
    Ok(())
}

pub(crate) fn validate_actor(field: &str, value: &str) -> Result<()> {
    validate_bounded_token(field, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_classes_are_deliberately_distinct() {
        assert!(validate_bounded_token("label", "human readable").is_ok());
        assert!(validate_ascii_token("identifier", "cohort:a/b-1.0").is_ok());
        assert!(validate_ascii_token("identifier", "human readable").is_err());
        assert!(validate_bounded_token("label", &"x".repeat(257)).is_err());
        assert!(validate_nonblank("detail", "  context  ").is_ok());
        assert!(validate_actor("actor", "operator\nname").is_err());
    }
}
