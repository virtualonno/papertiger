use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use papertiger_mise::{
    DeterministicEvaluatorOutput, DeterministicObservation, EvaluatorJudgeBuild,
};
use serde::Deserialize;

const REQUEST_SCHEMA: &str = "papertiger-mise.deterministic-evaluator-request.v1";
const OUTPUT_SCHEMA: &str = "papertiger-mise.deterministic-evaluator-output.v1";
const EVALUATOR_LOCATOR: &str = "crates/papertiger-mise/examples/debt_campaign_evaluator.rs";

// These markers are concrete copies of boundary decisions currently repeated
// by independent adapter surfaces. ASCII whitespace is removed before matching
// so formatting cannot improve the primary. Counting every candidate-owned
// Rust source below the repository root keeps moving a copy into another crate,
// module, or test helper from looking like an improvement. The evaluator itself
// is the sole exclusion because admission freezes it outside candidate scope.
const DUPLICATED_BOUNDARY_DECISIONS: [&str; 4] = [
    r#".strip_suffix(b"\r\n")"#,
    "Component::Prefix(_)|Component::RootDir|Component::Normal(_)",
    "Ok(sha256(&std::fs::read(path)?))",
    "PROHIBITED.contains(&key.as_str())",
];

const OBJECTIVES: [&str; 8] = [
    "duplicated-boundary-decision-sites",
    "correctness",
    "compatibility",
    "public-contract-gates",
    "test-sites",
    "assertion-sites",
    "refusal-sites",
    "allow-attributes",
];

#[derive(Deserialize)]
struct Request {
    schema: String,
    baseline_working_directory: String,
    objectives: Vec<Objective>,
}

#[derive(Deserialize)]
struct Objective {
    key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DebtStats {
    duplicated_boundary_decision_sites: u64,
    test_sites: u64,
    assertion_sites: u64,
    refusal_sites: u64,
    allow_attributes: u64,
}

fn main() {
    if let Err(error) = evaluate() {
        eprintln!("debt campaign evaluator: {error:#}");
        std::process::exit(1);
    }
}

fn evaluate() -> Result<()> {
    if env::args().nth(1).as_deref() != Some(EVALUATOR_LOCATOR) {
        bail!("expected exact evaluator locator argument '{EVALUATOR_LOCATOR}'");
    }
    let mut request_json = Vec::new();
    io::stdin()
        .read_to_end(&mut request_json)
        .context("read evaluator request")?;
    let request: Request =
        serde_json::from_slice(&request_json).context("parse evaluator request")?;
    if request.schema != REQUEST_SCHEMA {
        bail!(
            "unexpected evaluator request schema '{}'; expected '{REQUEST_SCHEMA}'",
            request.schema
        );
    }
    require_objectives(&request.objectives)?;

    let cargo = env::var("PAPERTIGER_MISE_CARGO_EXECUTABLE")
        .context("PAPERTIGER_MISE_CARGO_EXECUTABLE is not bound by the trial environment")?;
    run_cargo(&cargo, &["fmt", "--all", "--", "--check"], "cargo fmt")?;
    run_cargo(
        &cargo,
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        "cargo clippy",
    )?;
    run_cargo(&cargo, &["test", "--workspace"], "workspace tests")?;
    run_cargo(
        &cargo,
        &["test", "-p", "papertiger-mise", "--test", "lineage_cli"],
        "public CLI contract tests",
    )?;
    run_cargo(
        &cargo,
        &[
            "test",
            "-p",
            "papertiger-mise",
            "--example",
            "debt_campaign_evaluator",
        ],
        "debt detector tests",
    )?;

    let build_arguments = ["build", "-p", "papertiger-mise", "--bin", "papertiger-mise"];
    run_cargo(&cargo, &build_arguments, "judge build")?;
    let executable_name = if cfg!(windows) {
        "papertiger-mise.exe"
    } else {
        "papertiger-mise"
    };
    preserve_judge_build(executable_name)?;

    let candidate_root = env::current_dir().context("resolve candidate working directory")?;
    let baseline_root = PathBuf::from(&request.baseline_working_directory);
    let baseline = debt_stats(&baseline_root)?;
    let candidate = debt_stats(&candidate_root)?;
    let reason_code = regression_reason(&baseline, &candidate).map(str::to_owned);
    let observations = objective_observations(&request.objectives, &baseline, &candidate)?;

    let output = DeterministicEvaluatorOutput {
        schema: OUTPUT_SCHEMA.to_owned(),
        observations,
        reason_code,
        judge_build: Some(judge_build_attestation(
            &cargo,
            &build_arguments,
            executable_name,
        )),
    };
    serde_json::to_writer(io::stdout(), &output).context("write evaluator output")?;
    Ok(())
}

fn judge_build_attestation(
    cargo: &str,
    build_arguments: &[&str],
    executable_name: &str,
) -> EvaluatorJudgeBuild {
    EvaluatorJudgeBuild {
        argv: std::iter::once(cargo.to_owned())
            .chain(
                build_arguments
                    .iter()
                    .map(|argument| (*argument).to_owned()),
            )
            .collect(),
        executable_locator: executable_name.to_owned(),
    }
}

fn require_objectives(objectives: &[Objective]) -> Result<()> {
    for key in OBJECTIVES {
        if !objectives.iter().any(|objective| objective.key == key) {
            bail!("frozen campaign objective '{key}' is missing from the evaluator request");
        }
    }
    Ok(())
}

fn debt_stats(root: &Path) -> Result<DebtStats> {
    let mut repository_files = Vec::new();
    collect_rust_files(root, &mut repository_files)?;
    repository_files.retain(|path| {
        path.strip_prefix(root).ok().is_some_and(|relative| {
            relative != Path::new(EVALUATOR_LOCATOR)
                && !relative.starts_with("vendor")
                && !relative.starts_with("target")
        })
    });
    if repository_files.is_empty() {
        bail!(
            "no candidate-owned Rust source files found below '{}'",
            root.display()
        );
    }
    repository_files.sort();
    let duplicated_boundary_decision_sites = duplicated_boundary_decision_sites(&repository_files)?;
    let mut stats = DebtStats {
        duplicated_boundary_decision_sites,
        test_sites: 0,
        assertion_sites: 0,
        refusal_sites: 0,
        allow_attributes: 0,
    };
    for path in repository_files {
        let source = fs::read_to_string(&path)
            .with_context(|| format!("read Rust source '{}'", path.display()))?;
        stats.test_sites += count_token(&source, "#[test]");
        stats.assertion_sites += ["assert!(", "assert_eq!(", "assert_ne!(", ".expect_err("]
            .into_iter()
            .map(|token| count_token(&source, token))
            .sum::<u64>();
        stats.refusal_sites += count_token(&source, "bail!(") + count_token(&source, "ensure!(");
        stats.allow_attributes += count_token(&source, "#[allow(");
    }
    Ok(stats)
}

fn duplicated_boundary_decision_sites(files: &[PathBuf]) -> Result<u64> {
    let normalized_sources = files
        .iter()
        .map(|path| {
            fs::read_to_string(path)
                .with_context(|| format!("read Rust source '{}'", path.display()))
                .map(|source| strip_ascii_whitespace(&source))
        })
        .collect::<Result<Vec<_>>>()?;
    DUPLICATED_BOUNDARY_DECISIONS
        .iter()
        .try_fold(0_u64, |duplicates, marker| {
            let sites = normalized_sources
                .iter()
                .map(|source| count_token(source, marker))
                .sum::<u64>();
            duplicates
                .checked_add(sites.saturating_sub(1))
                .context("duplicated boundary-decision count overflow")
        })
}

fn strip_ascii_whitespace(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect()
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read source directory '{}'", directory.display()))?
    {
        let entry = entry.context("read source directory entry")?;
        let file_type = entry.file_type().context("read source entry type")?;
        if file_type.is_dir() {
            let name = entry.file_name();
            if name != ".git" && name != "target" {
                collect_rust_files(&entry.path(), files)?;
            }
        } else if file_type.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("rs")
        {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn count_token(source: &str, token: &str) -> u64 {
    source
        .match_indices(token)
        .count()
        .try_into()
        .expect("token count fits u64")
}

fn regression_reason(baseline: &DebtStats, candidate: &DebtStats) -> Option<&'static str> {
    if candidate.duplicated_boundary_decision_sites > baseline.duplicated_boundary_decision_sites {
        return Some("boundary-decision-duplication-regressed");
    }
    if candidate.allow_attributes > baseline.allow_attributes {
        return Some("suppression-regressed");
    }
    if candidate.test_sites < baseline.test_sites
        || candidate.assertion_sites < baseline.assertion_sites
        || candidate.refusal_sites < baseline.refusal_sites
    {
        return Some("anti-golf-countermetric-regressed");
    }
    None
}

fn objective_observations(
    objectives: &[Objective],
    baseline: &DebtStats,
    candidate: &DebtStats,
) -> Result<Vec<DeterministicObservation>> {
    objectives
        .iter()
        .map(|objective| {
            let (baseline_value, candidate_value) = match objective.key.as_str() {
                "duplicated-boundary-decision-sites" => (
                    baseline.duplicated_boundary_decision_sites,
                    candidate.duplicated_boundary_decision_sites,
                ),
                "correctness" | "compatibility" | "public-contract-gates" => (1, 1),
                "test-sites" => (baseline.test_sites, candidate.test_sites),
                "assertion-sites" => (baseline.assertion_sites, candidate.assertion_sites),
                "refusal-sites" => (baseline.refusal_sites, candidate.refusal_sites),
                "allow-attributes" => (baseline.allow_attributes, candidate.allow_attributes),
                unknown => bail!("unsupported frozen campaign objective '{unknown}'"),
            };
            Ok(DeterministicObservation {
                objective: objective.key.clone(),
                baseline: baseline_value as f64,
                candidate: candidate_value as f64,
            })
        })
        .collect()
}

fn run_cargo(cargo: &str, arguments: &[&str], gate: &str) -> Result<()> {
    let output = Command::new(cargo)
        .args(arguments)
        .output()
        .with_context(|| format!("launch {gate}"))?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{gate} failed with {}; stdout tail: {}; stderr tail: {}",
        output.status,
        capture_tail(&String::from_utf8_lossy(&output.stdout), 8_192),
        capture_tail(&String::from_utf8_lossy(&output.stderr), 8_192)
    )
}

fn capture_tail(value: &str, maximum_chars: usize) -> &str {
    let character_count = value.chars().count();
    let start = value
        .char_indices()
        .nth(character_count.saturating_sub(maximum_chars))
        .map_or(0, |(index, _)| index);
    &value[start..]
}

fn preserve_judge_build(executable_name: &str) -> Result<()> {
    let target =
        PathBuf::from(env::var("CARGO_TARGET_DIR").context("CARGO_TARGET_DIR is not trial-owned")?);
    let source = target.join("debug").join(executable_name);
    if !source.is_file() {
        bail!("judge build did not produce '{}'", source.display());
    }
    let judge_root = PathBuf::from(
        env::var("PAPERTIGER_MISE_JUDGE_BUILD_ROOT")
            .context("PAPERTIGER_MISE_JUDGE_BUILD_ROOT is not trial-owned")?,
    );
    fs::create_dir_all(&judge_root).context("create judge build root")?;
    fs::copy(&source, judge_root.join(executable_name))
        .with_context(|| format!("preserve judge executable from '{}'", source.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judge_build_attestation_includes_the_frozen_toolchain_executable() {
        let attestation = judge_build_attestation(
            "C:/toolchain/cargo.exe",
            &["build", "-p", "papertiger-mise"],
            "papertiger-mise.exe",
        );
        assert_eq!(
            attestation.argv,
            ["C:/toolchain/cargo.exe", "build", "-p", "papertiger-mise"]
        );
        assert_eq!(attestation.executable_locator, "papertiger-mise.exe");
    }

    #[test]
    fn detector_counts_structural_debt_and_anti_golf_metrics() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root
            .path()
            .join("crates")
            .join("papertiger-mise")
            .join("src");
        fs::create_dir_all(&source)?;
        fs::create_dir_all(root.path().join("tests"))?;
        fs::create_dir_all(
            root.path()
                .join("crates")
                .join("papertiger-mise")
                .join("examples"),
        )?;
        fs::write(
            source.join("adapter.rs"),
            "// retained rationale\n#[test]\nfn check() { assert!(true); bail!(\"no\"); }\n#[allow(dead_code)]\nfn first(value: &[u8]) { let _ = value.strip_suffix(b\"\\r\\n\"); }\n",
        )?;
        fs::write(
            root.path().join("tests").join("shadow.rs"),
            "fn second(value: &[u8]) { let _ = value\n    .strip_suffix( b\"\\r\\n\" ); }\n",
        )?;
        fs::write(
            root.path().join(EVALUATOR_LOCATOR),
            "fn frozen(value: &[u8]) { let _ = value.strip_suffix(b\"\\r\\n\"); }\n",
        )?;

        let stats = debt_stats(root.path())?;
        assert_eq!(stats.duplicated_boundary_decision_sites, 1);
        assert_eq!(stats.test_sites, 1);
        assert_eq!(stats.assertion_sites, 1);
        assert_eq!(stats.refusal_sites, 1);
        assert_eq!(stats.allow_attributes, 1);
        Ok(())
    }

    #[test]
    fn suppression_and_countermetric_regressions_are_named() {
        let baseline = DebtStats {
            duplicated_boundary_decision_sites: 2,
            test_sites: 10,
            assertion_sites: 20,
            refusal_sites: 40,
            allow_attributes: 1,
        };
        let mut candidate = baseline.clone();
        candidate.duplicated_boundary_decision_sites += 1;
        assert_eq!(
            regression_reason(&baseline, &candidate),
            Some("boundary-decision-duplication-regressed")
        );
        candidate = baseline.clone();
        candidate.allow_attributes += 1;
        assert_eq!(
            regression_reason(&baseline, &candidate),
            Some("suppression-regressed")
        );
        candidate = baseline.clone();
        candidate.refusal_sites -= 1;
        assert_eq!(
            regression_reason(&baseline, &candidate),
            Some("anti-golf-countermetric-regressed")
        );
    }

    #[test]
    fn observations_follow_manifest_order() {
        let stats = DebtStats {
            duplicated_boundary_decision_sites: 2,
            test_sites: 10,
            assertion_sites: 20,
            refusal_sites: 40,
            allow_attributes: 1,
        };
        let objectives = [
            Objective {
                key: "test-sites".to_owned(),
            },
            Objective {
                key: "duplicated-boundary-decision-sites".to_owned(),
            },
            Objective {
                key: "correctness".to_owned(),
            },
        ];
        assert_eq!(
            objective_observations(&objectives, &stats, &stats)
                .expect("supported objectives")
                .into_iter()
                .map(|observation| observation.objective)
                .collect::<Vec<_>>(),
            [
                "test-sites",
                "duplicated-boundary-decision-sites",
                "correctness"
            ]
        );
    }

    #[test]
    fn diagnostic_capture_is_utf8_safe_and_bounded() {
        assert_eq!(capture_tail("alpha", 8), "alpha");
        assert_eq!(capture_tail("aébc", 2), "bc");
    }
}
