use std::io::{Read, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use papertiger_mise::measurement::{
    MEASUREMENT_SAMPLE_SCHEMA_V1, MeasuredProcess, MeasurementSample, ObservationProvenance,
};
use papertiger_mise::{
    DeterministicEvaluatorOutput, DeterministicEvaluatorRequest, DeterministicObservation,
    EvaluatorJudgeBuild,
};

#[path = "../../examples/support/synthetic_measurement.rs"]
mod synthetic_measurement;

const CONTRACT: &[u8] = include_bytes!("deterministic_evaluator.rs");

fn main() {
    if let Err(error) = run() {
        eprintln!("deterministic fixture evaluator: {error:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let arguments = std::env::args().collect::<Vec<_>>();
    if arguments.get(1).map(String::as_str) != Some("fixtures/mise/deterministic_evaluator.rs")
        || arguments.len() != 2
    {
        bail!("expected the exact protected evaluator contract locator");
    }
    if std::fs::read(&arguments[1])? != CONTRACT {
        bail!("protected evaluator contract bytes drifted");
    }
    let mut request_bytes = Vec::new();
    std::io::stdin().read_to_end(&mut request_bytes)?;
    let request: DeterministicEvaluatorRequest = serde_json::from_slice(&request_bytes)?;
    if request.schema != "papertiger-mise.deterministic-evaluator-request.v2" {
        bail!("fixture requires deterministic-evaluator-request.v2");
    }
    let score_bytes = std::fs::read("src/score.txt")?;
    let score = std::str::from_utf8(&score_bytes)?.trim();
    let (latency, tests, reason) = match score {
        "known-bad" => (100.0, 0.0, Some("known-regression".to_owned())),
        "incorrect" => (9.0, 0.0, None),
        "crash" => std::process::exit(7),
        "sleep" => {
            for _ in 0..600 {
                if std::path::Path::new(".papertiger-mise-test-exit").exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            (7.0, 1.0, None)
        }
        "8" => (8.0, 1.0, None),
        _ => (10.0, 1.0, None),
    };
    let baseline_bytes = std::fs::read(
        std::path::Path::new(&request.baseline_working_directory).join("src/score.txt"),
    )?;
    if baseline_bytes != b"10\n" {
        bail!("fixture requires baseline score 10");
    }
    let process = MeasuredProcess::current()?;
    let executable_name = std::path::Path::new(&process.executable_locator)
        .file_name()
        .and_then(|name| name.to_str())
        .context("fixture executable filename")?;
    let environment = request
        .environment_sha256
        .as_ref()
        .context("missing frozen environment_sha256")?;
    let mut observations = request.objectives.iter().map(|objective| {
        let (baseline, candidate) = match objective.key.as_str() {
            "latency-ms" => (10.0, latency),
            "tests-pass" => (1.0, tests),
            _ => bail!("unsupported fixture objective '{}'", objective.key),
        };
        let observed = synthetic_measurement::contract(&objective.unit, executable_name);
        if objective.measurement.as_ref() != Some(&observed) { bail!("fixture measurement scope differs from admitted contract"); }
        let sample = |revision: &str, value: f64, bytes: &[u8]| MeasurementSample {
            schema: MEASUREMENT_SAMPLE_SCHEMA_V1.to_owned(),
            observed: observed.clone(), process: process.clone(),
            participant_revision: revision.to_owned(), fixture_sha256: request.fixture_sha256.clone(),
            environment_sha256: environment.clone(), value: serde_json::Number::from_f64(value).expect("finite fixture value"), scale10: 0,
            evidence: serde_json::json!({"score_source_sha256": papertiger_mise::sha256(bytes), "score_source": String::from_utf8_lossy(bytes), "objective": objective.key, "synthetic": true}),
        };
        Ok(DeterministicObservation { objective: objective.key.clone(), baseline, candidate,
            provenance: Some(ObservationProvenance { baseline: sample(&request.baseline_result_tree, baseline, &baseline_bytes), candidate: sample(&request.candidate_result_tree, candidate, &score_bytes) }) })
    }).collect::<Result<Vec<_>>>()?;
    match score {
        "provenance-missing" => observations[0].provenance = None,
        "provenance-compiler" => {
            observations[0]
                .provenance
                .as_mut()
                .unwrap()
                .candidate
                .process
                .executable_locator = "/toolchain/rustc".to_owned()
        }
        "provenance-environment" => {
            observations[0]
                .provenance
                .as_mut()
                .unwrap()
                .candidate
                .environment_sha256 = "0".repeat(64)
        }
        _ => {}
    }
    let build_root = std::env::var("PAPERTIGER_MISE_JUDGE_BUILD_ROOT")?;
    let build_tool = std::env::var("MISE_JUDGE_BUILD_TOOL")?;
    std::fs::write(
        std::path::Path::new(&build_root).join("judge.bin"),
        b"fixture-parent-built-judge-v1",
    )?;
    let output = DeterministicEvaluatorOutput {
        schema: "papertiger-mise.deterministic-evaluator-output.v2".to_owned(),
        observations,
        reason_code: reason,
        judge_build: Some(EvaluatorJudgeBuild {
            argv: vec![build_tool, "fixture-judge-build-v1".to_owned()],
            executable_locator: "judge.bin".to_owned(),
        }),
    };
    std::io::stdout().write_all(&serde_json::to_vec(&output)?)?;
    Ok(())
}
