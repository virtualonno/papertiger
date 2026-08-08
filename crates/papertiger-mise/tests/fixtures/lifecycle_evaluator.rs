use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

const CONTRACT: &[u8] = include_bytes!("lifecycle_evaluator.rs");
const MODE: &str = "PAPERTIGER_MISE_LIFECYCLE_FIXTURE_MODE";
const DESCENDANT: &str = "PAPERTIGER_MISE_LIFECYCLE_FIXTURE_DESCENDANT";
const OUTPUT: &str = "{\"schema\":\"papertiger-mise.deterministic-evaluator-output.v1\",\"observations\":[{\"objective\":\"latency-ms\",\"baseline\":10.0,\"candidate\":8.0},{\"objective\":\"tests-pass\",\"baseline\":1.0,\"candidate\":1.0}],\"reason_code\":null}";

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
    if !request.starts_with(b"{\"schema\":\"papertiger-mise.deterministic-evaluator-request.v1\"") {
        return Err("stdin is not a deterministic evaluator request".to_owned());
    }
    match mode.as_str() {
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
    std::io::stdout()
        .write_all(OUTPUT.as_bytes())
        .map_err(|error| error.to_string())?;
    Ok(())
}
