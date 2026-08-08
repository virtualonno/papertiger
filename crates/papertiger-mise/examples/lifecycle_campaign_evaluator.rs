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
const EVALUATOR_LOCATOR: &str = "crates/papertiger-mise/examples/lifecycle_campaign_evaluator.rs";
const SOURCE_VOLUME_TOLERANCE: u64 = 250;

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

#[derive(Debug)]
struct SourceStats {
    lifecycle_module_lines: u64,
    largest_module_lines: u64,
    total_lines: u64,
}

fn main() {
    if let Err(error) = evaluate() {
        eprintln!("lifecycle campaign evaluator: {error:#}");
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
    run_cargo(
        &cargo,
        &["test", "--workspace", "--exclude", "papertiger-mise"],
        "non-Mise workspace tests",
    )?;
    run_cargo(
        &cargo,
        &["test", "-p", "papertiger-mise", "--lib"],
        "Mise library tests",
    )?;
    run_cargo(
        &cargo,
        &["test", "-p", "papertiger-mise", "--test", "lineage_cli"],
        "Mise lineage CLI tests",
    )?;
    run_cargo(
        &cargo,
        &[
            "test",
            "-p",
            "papertiger-mise",
            "--test",
            "deterministic_dogfood",
            "--",
            "--test-threads=1",
        ],
        "serialized deterministic dogfood",
    )?;
    run_cargo(
        &cargo,
        &[
            "test",
            "-p",
            "papertiger-mise",
            "--example",
            "lifecycle_campaign_evaluator",
        ],
        "lifecycle evaluator tests",
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
    let baseline = source_stats(&baseline_root)?;
    let candidate = source_stats(&candidate_root)?;
    let source_volume_ok =
        baseline.total_lines.abs_diff(candidate.total_lines) <= SOURCE_VOLUME_TOLERANCE;
    let reason_code = if candidate.largest_module_lines > baseline.largest_module_lines {
        Some("largest-module-regressed")
    } else if !source_volume_ok {
        Some("production-source-drift")
    } else {
        None
    };

    let output = DeterministicEvaluatorOutput {
        schema: OUTPUT_SCHEMA.to_owned(),
        observations: objective_observations(&baseline, &candidate, source_volume_ok),
        reason_code: reason_code.map(str::to_owned),
        judge_build: Some(EvaluatorJudgeBuild {
            argv: [
                cargo.as_str(),
                "build",
                "-p",
                "papertiger-mise",
                "--bin",
                "papertiger-mise",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            executable_locator: executable_name.to_owned(),
        }),
    };
    serde_json::to_writer(io::stdout(), &output).context("write evaluator output")?;
    Ok(())
}

fn objective_observations(
    baseline: &SourceStats,
    candidate: &SourceStats,
    source_volume_ok: bool,
) -> Vec<DeterministicObservation> {
    vec![
        DeterministicObservation {
            objective: "largest-module-lines".to_owned(),
            baseline: baseline.largest_module_lines as f64,
            candidate: candidate.largest_module_lines as f64,
        },
        DeterministicObservation {
            objective: "lifecycle-module-lines".to_owned(),
            baseline: baseline.lifecycle_module_lines as f64,
            candidate: candidate.lifecycle_module_lines as f64,
        },
        DeterministicObservation {
            objective: "production-source-band".to_owned(),
            baseline: 1.0,
            candidate: if source_volume_ok { 1.0 } else { 0.0 },
        },
        DeterministicObservation {
            objective: "workspace-gates".to_owned(),
            baseline: 1.0,
            candidate: 1.0,
        },
    ]
}

fn require_objectives(objectives: &[Objective]) -> Result<()> {
    let expected = [
        "lifecycle-module-lines",
        "largest-module-lines",
        "production-source-band",
        "workspace-gates",
    ];
    for key in expected {
        if !objectives.iter().any(|objective| objective.key == key) {
            bail!("frozen campaign objective '{key}' is missing from the evaluator request");
        }
    }
    Ok(())
}

fn run_cargo(cargo: &str, arguments: &[&str], gate: &str) -> Result<()> {
    let output = Command::new(cargo)
        .args(arguments)
        .output()
        .with_context(|| format!("launch {gate}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    bail!(
        "{gate} failed with {}; stdout tail: {}; stderr tail: {}",
        output.status,
        capture_tail(&stdout, 8192),
        capture_tail(&stderr, 8192)
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

fn source_stats(root: &Path) -> Result<SourceStats> {
    let source_root = root.join("crates").join("papertiger-mise").join("src");
    let lifecycle_module = source_root.join("lifecycle.rs");
    let lifecycle_source = fs::read_to_string(&lifecycle_module).with_context(|| {
        format!(
            "read lifecycle module source '{}'",
            lifecycle_module.display()
        )
    })?;
    let lifecycle_module_lines = u64::try_from(lifecycle_source.lines().count())
        .context("lifecycle module line count overflow")?;
    let mut files = Vec::new();
    collect_rust_files(&source_root, &mut files)?;
    if files.is_empty() {
        bail!(
            "no Rust source files found below '{}'",
            source_root.display()
        );
    }
    let mut largest_module_lines = 0;
    let mut total_lines = 0;
    for file in files {
        let source = fs::read_to_string(&file)
            .with_context(|| format!("read Rust source '{}'", file.display()))?;
        let lines = u64::try_from(source.lines().count()).context("source line count overflow")?;
        largest_module_lines = largest_module_lines.max(lines);
        total_lines += lines;
    }
    Ok(SourceStats {
        lifecycle_module_lines,
        largest_module_lines,
        total_lines,
    })
}

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read source directory '{}'", directory.display()))?
    {
        let entry = entry.context("read source directory entry")?;
        let file_type = entry.file_type().context("read source entry type")?;
        if file_type.is_dir() {
            collect_rust_files(&entry.path(), files)?;
        } else if file_type.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("rs")
        {
            files.push(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_measurement_is_recursive_and_baseline_relative() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root
            .path()
            .join("crates")
            .join("papertiger-mise")
            .join("src");
        fs::create_dir_all(source.join("nested"))?;
        fs::write(source.join("lib.rs"), "one\ntwo\nthree\n")?;
        fs::write(source.join("lifecycle.rs"), "one\ntwo\n")?;
        fs::write(source.join("nested").join("module.rs"), "one\ntwo\n")?;
        fs::write(source.join("ignored.txt"), "not Rust\n")?;

        let stats = source_stats(root.path())?;
        assert_eq!(stats.lifecycle_module_lines, 2);
        assert_eq!(stats.largest_module_lines, 3);
        assert_eq!(stats.total_lines, 7);
        Ok(())
    }

    #[test]
    fn diagnostic_capture_is_utf8_safe_and_bounded() {
        assert_eq!(capture_tail("alpha", 8), "alpha");
        assert_eq!(capture_tail("aébc", 2), "bc");
    }

    #[test]
    fn evaluator_output_uses_the_runtime_struct_byte_order() -> Result<()> {
        let output = DeterministicEvaluatorOutput {
            schema: OUTPUT_SCHEMA.to_owned(),
            observations: Vec::new(),
            reason_code: None,
            judge_build: Some(EvaluatorJudgeBuild {
                argv: vec!["cargo".to_owned(), "build".to_owned()],
                executable_locator: "papertiger-mise".to_owned(),
            }),
        };
        assert_eq!(
            String::from_utf8(serde_json::to_vec(&output)?)?,
            "{\"schema\":\"papertiger-mise.deterministic-evaluator-output.v1\",\"observations\":[],\"reason_code\":null,\"judge_build\":{\"argv\":[\"cargo\",\"build\"],\"executable_locator\":\"papertiger-mise\"}}"
        );
        Ok(())
    }

    #[test]
    fn objective_observations_follow_admission_canonical_order() {
        let baseline = SourceStats {
            lifecycle_module_lines: 100,
            largest_module_lines: 110,
            total_lines: 1_000,
        };
        let candidate = SourceStats {
            lifecycle_module_lines: 80,
            largest_module_lines: 105,
            total_lines: 990,
        };
        let keys = objective_observations(&baseline, &candidate, true)
            .into_iter()
            .map(|observation| observation.objective)
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "largest-module-lines",
                "lifecycle-module-lines",
                "production-source-band",
                "workspace-gates",
            ]
        );
    }
}
