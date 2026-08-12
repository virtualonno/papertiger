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

const OBJECTIVES: [&str; 9] = [
    "misplaced-trial-transition-sites",
    "raw-stored-state-sites",
    "correctness",
    "compatibility",
    "public-contract-gates",
    "test-sites",
    "assertion-sites",
    "refusal-sites",
    "allow-attributes",
];

const STORED_STATES: [&str; 14] = [
    "owned",
    "prepared",
    "running",
    "qualified",
    "inconclusive",
    "calibrated",
    "launched",
    "succeeded",
    "rejected",
    "infrastructure_failed",
    "integrity_failed",
    "reserved",
    "settled",
    "charged",
];

const TRIAL_TRANSITION_MARKERS: [&str; 2] = ["INSERTINTOTRIALS", "UPDATETRIALSSETSTATUS="];

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
struct SourceStats {
    misplaced_trial_transition_sites: u64,
    raw_stored_state_sites: u64,
    test_sites: u64,
    assertion_sites: u64,
    refusal_sites: u64,
    allow_attributes: u64,
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
    let reason_code = regression_reason(&baseline, &candidate).map(str::to_owned);

    let output = DeterministicEvaluatorOutput {
        schema: OUTPUT_SCHEMA.to_owned(),
        observations: objective_observations(&request.objectives, &baseline, &candidate)?,
        reason_code,
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
    objectives: &[Objective],
    baseline: &SourceStats,
    candidate: &SourceStats,
) -> Result<Vec<DeterministicObservation>> {
    objectives
        .iter()
        .map(|objective| {
            let (baseline_value, candidate_value) = match objective.key.as_str() {
                "misplaced-trial-transition-sites" => (
                    baseline.misplaced_trial_transition_sites,
                    candidate.misplaced_trial_transition_sites,
                ),
                "raw-stored-state-sites" => (
                    baseline.raw_stored_state_sites,
                    candidate.raw_stored_state_sites,
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

fn require_objectives(objectives: &[Objective]) -> Result<()> {
    for key in OBJECTIVES {
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
    let mut production_files = Vec::new();
    collect_rust_files(&source_root, &mut production_files)?;
    if production_files.is_empty() {
        bail!(
            "no Rust source files found below '{}'",
            source_root.display()
        );
    }
    production_files.sort();
    let mut stats = SourceStats {
        misplaced_trial_transition_sites: 0,
        raw_stored_state_sites: 0,
        test_sites: 0,
        assertion_sites: 0,
        refusal_sites: 0,
        allow_attributes: 0,
    };
    for file in &production_files {
        let source = fs::read_to_string(file)
            .with_context(|| format!("read Rust source '{}'", file.display()))?;
        let relative = file
            .strip_prefix(&source_root)
            .with_context(|| format!("source '{}' escaped its scan root", file.display()))?;
        if !is_transition_owner(relative) {
            let normalized = normalize_ascii_code(&source);
            stats.misplaced_trial_transition_sites += TRIAL_TRANSITION_MARKERS
                .iter()
                .map(|marker| count_token(&normalized, marker))
                .sum::<u64>();
        }
        if !is_canonical_state_owner(relative) && !is_test_source(relative) {
            stats.raw_stored_state_sites += STORED_STATES
                .iter()
                .map(|state| count_token(&source, &format!("'{state}'")))
                .sum::<u64>();
        }
    }

    let mut repository_files = Vec::new();
    collect_rust_files(root, &mut repository_files)?;
    repository_files.sort();
    for file in repository_files {
        let source = fs::read_to_string(&file)
            .with_context(|| format!("read Rust source '{}'", file.display()))?;
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

fn is_transition_owner(relative: &Path) -> bool {
    relative == Path::new("store.rs")
        || relative == Path::new("lifecycle").join("trial_runtime.rs")
        || relative == Path::new("lifecycle").join("trial_recovery.rs")
}

fn is_canonical_state_owner(relative: &Path) -> bool {
    matches!(relative.to_str(), Some("state.rs" | "store.rs"))
}

fn is_test_source(relative: &Path) -> bool {
    relative
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
}

fn normalize_ascii_code(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .flat_map(char::to_uppercase)
        .collect()
}

fn count_token(source: &str, token: &str) -> u64 {
    source
        .match_indices(token)
        .count()
        .try_into()
        .expect("token count fits u64")
}

fn regression_reason(baseline: &SourceStats, candidate: &SourceStats) -> Option<&'static str> {
    if candidate.misplaced_trial_transition_sites > baseline.misplaced_trial_transition_sites {
        return Some("trial-transition-boundary-regressed");
    }
    if candidate.raw_stored_state_sites > baseline.raw_stored_state_sites {
        return Some("stored-state-vocabulary-regressed");
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

fn collect_rust_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("read source directory '{}'", directory.display()))?
    {
        let entry = entry.context("read source directory entry")?;
        let file_type = entry.file_type().context("read source entry type")?;
        if file_type.is_dir() {
            let name = entry.file_name();
            if name != ".git" && name != "target" && name != "vendor" {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detector_counts_state_vocabulary_and_transition_boundary_hazards() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root
            .path()
            .join("crates")
            .join("papertiger-mise")
            .join("src");
        fs::create_dir_all(source.join("lifecycle"))?;
        fs::write(
            source.join("lifecycle.rs"),
            "// retained rationale\n#[test]\nfn check() { assert!(true); bail!(\"no\"); }\n#[allow(dead_code)]\nfn root() { execute(\"INSERT INTO trials (status) VALUES ('owned')\"); execute(\"update trials set status = 'launched'\"); }\n",
        )?;
        fs::write(
            source.join("lifecycle").join("trial_runtime.rs"),
            "fn owned() { execute(\"UPDATE trials SET status='succeeded'\"); }\n",
        )?;
        fs::write(
            source.join("store.rs"),
            "const SCHEMA: &str = \"UPDATE trials SET status='integrity_failed'\";\n",
        )?;
        fs::write(
            source.join("state.rs"),
            "const OWNED: &str = \"'owned'\";\n",
        )?;
        fs::write(
            source.join("lifecycle_tests.rs"),
            "const FIXTURE: &str = \"status='rejected'\";\n",
        )?;
        fs::write(source.join("ignored.txt"), "not Rust\n")?;

        let stats = source_stats(root.path())?;
        assert_eq!(stats.misplaced_trial_transition_sites, 2);
        assert_eq!(stats.raw_stored_state_sites, 3);
        assert_eq!(stats.test_sites, 1);
        assert_eq!(stats.assertion_sites, 1);
        assert_eq!(stats.refusal_sites, 1);
        assert_eq!(stats.allow_attributes, 1);
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
    fn objective_observations_follow_manifest_order() {
        let baseline = SourceStats {
            misplaced_trial_transition_sites: 4,
            raw_stored_state_sites: 12,
            test_sites: 10,
            assertion_sites: 20,
            refusal_sites: 40,
            allow_attributes: 1,
        };
        let objectives = [
            Objective {
                key: "refusal-sites".to_owned(),
            },
            Objective {
                key: "misplaced-trial-transition-sites".to_owned(),
            },
            Objective {
                key: "correctness".to_owned(),
            },
        ];
        let keys = objective_observations(&objectives, &baseline, &baseline)
            .expect("supported objectives")
            .into_iter()
            .map(|observation| observation.objective)
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "refusal-sites",
                "misplaced-trial-transition-sites",
                "correctness",
            ]
        );
    }

    #[test]
    fn structural_and_countermetric_regressions_are_named() {
        let baseline = SourceStats {
            misplaced_trial_transition_sites: 4,
            raw_stored_state_sites: 12,
            test_sites: 10,
            assertion_sites: 20,
            refusal_sites: 40,
            allow_attributes: 1,
        };
        let mut candidate = baseline.clone();
        candidate.misplaced_trial_transition_sites += 1;
        assert_eq!(
            regression_reason(&baseline, &candidate),
            Some("trial-transition-boundary-regressed")
        );
        candidate = baseline.clone();
        candidate.raw_stored_state_sites += 1;
        assert_eq!(
            regression_reason(&baseline, &candidate),
            Some("stored-state-vocabulary-regressed")
        );
        candidate = baseline.clone();
        candidate.refusal_sites -= 1;
        assert_eq!(
            regression_reason(&baseline, &candidate),
            Some("anti-golf-countermetric-regressed")
        );
    }
}
