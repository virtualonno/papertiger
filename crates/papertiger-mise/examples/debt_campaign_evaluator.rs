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
const MODULE_LINE_BUDGET: u64 = 2_000;

const OBJECTIVES: [&str; 9] = [
    "files-over-2000-lines",
    "correctness",
    "compatibility",
    "public-contract-gates",
    "test-sites",
    "assertion-sites",
    "comment-lines",
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
    files_over_line_budget: u64,
    test_sites: u64,
    assertion_sites: u64,
    comment_lines: u64,
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
    let source_root = root.join("crates").join("papertiger-mise").join("src");
    let mut production_files = Vec::new();
    collect_rust_files(&source_root, &mut production_files)?;
    if production_files.is_empty() {
        bail!(
            "no Rust source files found below '{}'",
            source_root.display()
        );
    }
    let files_over_line_budget = production_files
        .iter()
        .map(|path| physical_line_count(path))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|lines| *lines > MODULE_LINE_BUDGET)
        .count();

    let mut repository_files = Vec::new();
    collect_rust_files(root, &mut repository_files)?;
    repository_files.retain(|path| {
        path.strip_prefix(root).ok().is_some_and(|relative| {
            !relative.starts_with("vendor") && !relative.starts_with("target")
        })
    });
    let mut stats = DebtStats {
        files_over_line_budget: u64::try_from(files_over_line_budget)
            .context("files-over-budget count overflow")?,
        test_sites: 0,
        assertion_sites: 0,
        comment_lines: 0,
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
        let comment_lines = source
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                line.starts_with("//")
                    || line.starts_with("/*")
                    || line.starts_with('*')
                    || line.starts_with("*/")
            })
            .count();
        let comment_lines = u64::try_from(comment_lines).context("comment-line count overflow")?;
        stats.comment_lines += comment_lines;
        stats.refusal_sites += count_token(&source, "bail!(") + count_token(&source, "ensure!(");
        stats.allow_attributes += count_token(&source, "#[allow(");
    }
    Ok(stats)
}

fn physical_line_count(path: &Path) -> Result<u64> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("read Rust source '{}'", path.display()))?;
    if source.is_empty() {
        return Ok(0);
    }
    let breaks = source.bytes().filter(|byte| *byte == b'\n').count();
    let lines = if source.ends_with('\n') {
        breaks
    } else {
        breaks + 1
    };
    u64::try_from(lines).context("physical line count overflow")
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
    if candidate.allow_attributes > baseline.allow_attributes {
        return Some("suppression-regressed");
    }
    if candidate.test_sites < baseline.test_sites
        || candidate.assertion_sites < baseline.assertion_sites
        || candidate.comment_lines < baseline.comment_lines
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
                "files-over-2000-lines" => (
                    baseline.files_over_line_budget,
                    candidate.files_over_line_budget,
                ),
                "correctness" | "compatibility" | "public-contract-gates" => (1, 1),
                "test-sites" => (baseline.test_sites, candidate.test_sites),
                "assertion-sites" => (baseline.assertion_sites, candidate.assertion_sites),
                "comment-lines" => (baseline.comment_lines, candidate.comment_lines),
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
    fn detector_counts_debt_and_anti_golf_metrics() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root
            .path()
            .join("crates")
            .join("papertiger-mise")
            .join("src");
        fs::create_dir_all(&source)?;
        let oversized = format!(
            "// retained rationale\n#[test]\nfn check() {{ assert!(true); bail!(\"no\"); }}\n{}#[allow(dead_code)]\n",
            "line\n".repeat(2_000)
        );
        fs::write(source.join("oversized.rs"), oversized)?;

        let stats = debt_stats(root.path())?;
        assert_eq!(stats.files_over_line_budget, 1);
        assert_eq!(stats.test_sites, 1);
        assert_eq!(stats.assertion_sites, 1);
        assert_eq!(stats.comment_lines, 1);
        assert_eq!(stats.refusal_sites, 1);
        assert_eq!(stats.allow_attributes, 1);
        Ok(())
    }

    #[test]
    fn suppression_and_countermetric_regressions_are_named() {
        let baseline = DebtStats {
            files_over_line_budget: 2,
            test_sites: 10,
            assertion_sites: 20,
            comment_lines: 30,
            refusal_sites: 40,
            allow_attributes: 1,
        };
        let mut candidate = baseline.clone();
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
            files_over_line_budget: 2,
            test_sites: 10,
            assertion_sites: 20,
            comment_lines: 30,
            refusal_sites: 40,
            allow_attributes: 1,
        };
        let objectives = [
            Objective {
                key: "test-sites".to_owned(),
            },
            Objective {
                key: "files-over-2000-lines".to_owned(),
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
            ["test-sites", "files-over-2000-lines", "correctness"]
        );
    }

    #[test]
    fn diagnostic_capture_is_utf8_safe_and_bounded() {
        assert_eq!(capture_tail("alpha", 8), "alpha");
        assert_eq!(capture_tail("aébc", 2), "bc");
    }
}
