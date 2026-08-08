use std::io::{Read, Write};
use std::time::Duration;

const CONTRACT: &[u8] = include_bytes!("deterministic_evaluator.rs");

fn main() {
    if let Err(error) = run() {
        eprintln!("deterministic fixture evaluator: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let arguments = std::env::args().collect::<Vec<_>>();
    if arguments.get(1).map(String::as_str)
        != Some("fixtures/mise/deterministic_evaluator.rs")
        || arguments.len() != 2
    {
        return Err("expected the exact protected evaluator contract locator".to_owned());
    }
    let contract = std::fs::read(&arguments[1]).map_err(|error| error.to_string())?;
    if contract != CONTRACT {
        return Err("protected evaluator contract bytes drifted".to_owned());
    }
    let mut request = Vec::new();
    std::io::stdin()
        .read_to_end(&mut request)
        .map_err(|error| error.to_string())?;
    if !request.starts_with(b"{\"schema\":\"papertiger-mise.deterministic-evaluator-request.v1\"") {
        return Err("stdin is not a deterministic evaluator request".to_owned());
    }
    let score = std::fs::read_to_string("src/score.txt")
        .map_err(|error| error.to_string())?
        .trim()
        .to_owned();
    let (latency, tests, reason) = match score.as_str() {
        "known-bad" => ("100.0", "0.0", "\"known-regression\""),
        "incorrect" => ("9.0", "0.0", "null"),
        "crash" => std::process::exit(7),
        "sleep" => {
            for _ in 0..600 {
                if std::path::Path::new(".papertiger-mise-test-exit").exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            ("7.0", "1.0", "null")
        }
        "8" => ("8.0", "1.0", "null"),
        _ => ("10.0", "1.0", "null"),
    };
    let build_root = std::env::var("PAPERTIGER_MISE_JUDGE_BUILD_ROOT")
        .map_err(|_| "missing runtime-owned judge-build root".to_owned())?;
    let build_tool = std::env::var("MISE_JUDGE_BUILD_TOOL")
        .map_err(|_| "missing frozen fixture build tool".to_owned())?;
    std::fs::write(
        std::path::Path::new(&build_root).join("judge.bin"),
        b"fixture-parent-built-judge-v1",
    )
    .map_err(|error| error.to_string())?;
    let output = format!(
        "{{\"schema\":\"papertiger-mise.deterministic-evaluator-output.v1\",\"observations\":[{{\"objective\":\"latency-ms\",\"baseline\":10.0,\"candidate\":{latency}}},{{\"objective\":\"tests-pass\",\"baseline\":1.0,\"candidate\":{tests}}}],\"reason_code\":{reason},\"judge_build\":{{\"argv\":[\"{}\",\"fixture-judge-build-v1\"],\"executable_locator\":\"judge.bin\"}}}}",
        json_string_content(&build_tool)
    );
    std::io::stdout()
        .write_all(output.as_bytes())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn json_string_content(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    escaped
}
