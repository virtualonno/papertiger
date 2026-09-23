//! Operator rationale input for Mise mutations. Mirrors the planner's
//! `--why`/`--why-file` contract: exactly one source, UTF-8, trimmed, nonblank.

use std::io::Read as _;

use anyhow::{Context, Result, bail};
use clap::Args;

#[derive(Debug, Args)]
pub struct WhyArgs {
    /// Durable operator rationale recorded with this mutation.
    #[arg(long, value_name = "TEXT", conflicts_with = "why_file")]
    why: Option<String>,
    /// Read the rationale as UTF-8 from PATH, or stdin with '-'.
    #[arg(long, value_name = "PATH|-")]
    why_file: Option<String>,
}

impl WhyArgs {
    pub fn required(self) -> Result<String> {
        let (value, external) = match (self.why, self.why_file) {
            (Some(_), Some(_)) => bail!("pass --why or --why-file, not both"),
            (Some(value), None) => (value, false),
            (None, Some(path)) if path == "-" => {
                let mut value = String::new();
                std::io::stdin()
                    .read_to_string(&mut value)
                    .context("read --why-file - from stdin")?;
                (value, true)
            }
            (None, Some(path)) => (
                std::fs::read_to_string(&path)
                    .with_context(|| format!("read --why-file {path}"))?,
                true,
            ),
            (None, None) => bail!("pass --why <TEXT> or --why-file <PATH|-> with nonblank text"),
        };
        let value = if external {
            value.strip_prefix('\u{feff}').unwrap_or(&value)
        } else {
            &value
        };
        let value = value.trim();
        if value.is_empty() {
            bail!(
                "--why requires nonblank text; pass --why <TEXT> or --why-file <PATH|-> with content"
            );
        }
        Ok(value.to_owned())
    }
}
