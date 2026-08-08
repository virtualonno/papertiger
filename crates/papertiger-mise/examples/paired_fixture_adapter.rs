//! Cross-platform external adapter used by the Contextmink paired dogfood.
//!
//! The adapter deliberately measures a tracked synthetic score fixture. It
//! proves Mise's process, schedule, CAS, and classification lifecycle without
//! presenting that score as a Contextmink performance measurement.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use papertiger_mise::manifest::Sha256Digest;
use papertiger_mise::{DomainTrialMeasurement, DomainTrialResult, PairedTrialRequest};
use serde_json::json;

const RESULT_SCHEMA: &str = "contextmink.synthetic-paired-trial-result.v1";

fn main() -> Result<()> {
    let mut request_bytes = Vec::new();
    std::io::stdin().read_to_end(&mut request_bytes)?;
    let request_value: serde_json::Value =
        serde_json::from_slice(&request_bytes).context("parse paired request JSON")?;
    if serde_json::to_vec(&request_value)? != request_bytes {
        bail!("paired request is not canonical compact JSON");
    }
    let request: PairedTrialRequest =
        serde_json::from_value(request_value).context("type paired request")?;
    let root = participant_root(&request.participant.identity_sha256.0)?;
    let cargo = std::fs::read(root.join("Cargo.toml"))
        .with_context(|| format!("read Contextmink Cargo.toml in {}", root.display()))?;
    let score_path = root.join(".mise-paired-score");
    let (score, score_source_sha256) = if score_path.exists() {
        let bytes = std::fs::read(&score_path)
            .with_context(|| format!("read synthetic score fixture {}", score_path.display()))?;
        let score = std::str::from_utf8(&bytes)
            .context("synthetic score is not UTF-8")?
            .trim()
            .parse::<i64>()
            .context("synthetic score is not a signed integer")?;
        (score, sha256(&bytes))
    } else {
        (10_000, sha256(b"contextmink.synthetic-baseline.v1"))
    };
    let executable = std::fs::canonicalize(std::env::current_exe()?)?;
    let result = DomainTrialResult {
        schema: RESULT_SCHEMA.to_owned(),
        execution_id: request.execution_id.clone(),
        request_sha256: Sha256Digest(sha256(&request_bytes)),
        adapter_executable_sha256: Sha256Digest(sha256(&std::fs::read(&executable)?)),
        participant_identity_sha256: request.participant.identity_sha256.clone(),
        domain_trial_receipt: json!({
            "cargo_toml_sha256": sha256(&cargo),
            "execution_id": request.execution_id,
            "participant_revision": request.participant.revision,
            "score_source_sha256": score_source_sha256,
        }),
        domain_authority: json!({
            "kind": "contextmink-tracked-synthetic-score",
            "performance_claim": false,
        }),
        measurements: request
            .objectives
            .iter()
            .map(|objective| {
                let units = match objective.objective.as_str() {
                    "correct" => 1,
                    "frame-ms" => score,
                    "memory-mib" => 100_000,
                    unknown => bail!("unsupported synthetic objective '{unknown}'"),
                };
                Ok(DomainTrialMeasurement {
                    objective: objective.objective.clone(),
                    units,
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    let output = serde_json::to_vec(&serde_json::to_value(result)?)?;
    std::io::stdout().write_all(&output)?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}

fn participant_root(identity: &str) -> Result<PathBuf> {
    for role in ["BASELINE", "KNOWN_BAD", "RESEARCH"] {
        let bound_identity = std::env::var(format!("MISE_{role}_ID"))
            .with_context(|| format!("missing MISE_{role}_ID"))?;
        if identity == bound_identity {
            let root = std::env::var(format!("MISE_{role}_ROOT"))
                .with_context(|| format!("missing MISE_{role}_ROOT"))?;
            let canonical = std::fs::canonicalize(&root)
                .with_context(|| format!("canonicalize bound {role} root {root}"))?;
            if !canonical.is_dir() {
                bail!("bound {role} root is not a directory");
            }
            return Ok(canonical);
        }
    }
    bail!("paired participant has no exact dogfood worktree binding")
}

fn sha256(bytes: &[u8]) -> String {
    papertiger_mise::sha256(bytes)
}
