use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

const CONTRACT: &[u8] = include_bytes!("lifecycle_evaluator.rs");
const MODE: &str = "PAPERTIGER_MISE_LIFECYCLE_FIXTURE_MODE";
const DESCENDANT: &str = "PAPERTIGER_MISE_LIFECYCLE_FIXTURE_DESCENDANT";
/// Canonical `papertiger-mise.deterministic_evaluator_output.v3` rendered by
/// the test harness from the admitted measurement contracts. It carries
/// placeholders only for values the runtime selects per trial.
const OUTPUT_TEMPLATE: &str = "PAPERTIGER_MISE_LIFECYCLE_FIXTURE_OUTPUT_TEMPLATE";
const BASELINE_TREE: &str = "@@baseline_result_tree@@";
const CANDIDATE_TREE: &str = "@@candidate_result_tree@@";
const FIXTURE: &str = "@@fixture_sha256@@";
const ENVIRONMENT: &str = "@@environment_sha256@@";
const EXECUTABLE: &str = "@@executable_locator@@";
const PID: &str = "4242424242";

fn main() {
    if std::env::var_os(DESCENDANT).is_some() {
        // Long enough that a successful parent cannot wait for natural exit:
        // the supervisor must close the inherited streams through its native
        // cleanup attempt or fail its short quiescence check.
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    if let Err(error) = run() {
        eprintln!("lifecycle fixture evaluator: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let arguments = std::env::args().collect::<Vec<_>>();
    if arguments.get(1).map(String::as_str) != Some("fixtures/mise/evaluator.rs")
        || arguments.len() != 2
    {
        return Err("expected the exact protected evaluator contract locator".to_owned());
    }
    let contract = std::fs::read(&arguments[1]).map_err(|error| error.to_string())?;
    if contract != CONTRACT {
        return Err("protected evaluator contract bytes drifted".to_owned());
    }
    let mode = std::env::var(MODE).unwrap_or_else(|_| "success".to_owned());
    if mode == "blocked-stdin" {
        std::thread::sleep(Duration::from_secs(5));
        return Ok(());
    }
    let mut request = Vec::new();
    std::io::stdin()
        .read_to_end(&mut request)
        .map_err(|error| error.to_string())?;
    if !request.starts_with(b"{\"schema\":\"papertiger-mise.deterministic_evaluator_request.v3\"") {
        return Err("stdin is not a deterministic evaluator request".to_owned());
    }
    let request = String::from_utf8(request).map_err(|error| error.to_string())?;
    match mode.as_str() {
        "cancellable" => std::thread::sleep(Duration::from_secs(30)),
        "success" => {}
        "stderr" => std::io::stderr()
            .write_all(b"warning that must not disappear")
            .map_err(|error| error.to_string())?,
        "inherited-handle" => {
            Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
                .env(DESCENDANT, "1")
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|error| error.to_string())?;
        }
        other => return Err(format!("unknown lifecycle fixture mode '{other}'")),
    }
    let executable = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .to_str()
        .ok_or("evaluator path is not UTF-8")?
        .replace('\\', "/");
    let output = std::env::var(OUTPUT_TEMPLATE)
        .map_err(|_| format!("missing {OUTPUT_TEMPLATE}"))?
        .replace(BASELINE_TREE, request_field(&request, "baseline_result_tree")?)
        .replace(CANDIDATE_TREE, request_field(&request, "candidate_result_tree")?)
        .replace(FIXTURE, request_field(&request, "fixture_sha256")?)
        .replace(ENVIRONMENT, request_field(&request, "environment_sha256")?)
        .replace(EXECUTABLE, &executable)
        .replace(PID, &std::process::id().to_string());
    std::io::stdout()
        .write_all(output.as_bytes())
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// First string value of a top-level request field. The requested fields are
/// lowercase hex identities that precede the objectives, so no JSON escape
/// can occur inside them.
fn request_field<'a>(request: &'a str, key: &str) -> Result<&'a str, String> {
    let marker = format!("\"{key}\":\"");
    let start = request
        .find(&marker)
        .ok_or_else(|| format!("request omitted {key}"))?
        + marker.len();
    let length = request[start..]
        .find('"')
        .ok_or_else(|| format!("request {key} is unterminated"))?;
    let value = &request[start..start + length];
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("request {key} is not a hex identity"));
    }
    Ok(value)
}
