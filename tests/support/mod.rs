//! Authorship and process setup shared by planner tests.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::process::Command;

/// A command for a Papertiger executable whose authorship does not depend on
/// the harness running the test suite: harness session variables and
/// `PAPERTIGER_*` authorship inputs are removed, and the author is the
/// operator's manual model unless a test overrides it.
pub fn papertiger_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for harness in papertiger::authorship::HARNESSES {
        command.env_remove(harness.session_variable);
        if let Some(effort) = harness.effort_variable {
            command.env_remove(effort);
        }
    }
    for name in [
        "PAPERTIGER_ACTOR",
        "PAPERTIGER_SESSION",
        "PAPERTIGER_REASONING_EFFORT",
    ] {
        command.env_remove(name);
    }
    command.env("PAPERTIGER_MODEL", papertiger::HUMAN_MODEL);
    command
}

/// Attribute a test connection's writes to the operator's manual model.
pub fn author(conn: &rusqlite::Connection) {
    author_as(conn, papertiger::HUMAN_MODEL, None);
}

/// Attribute a test connection's writes to `model`, optionally with effort.
pub fn author_as(conn: &rusqlite::Connection, model: &str, reasoning_effort: Option<&str>) {
    let authorship = papertiger::resolve_authorship(
        papertiger::AuthorshipFlags {
            model: Some(model),
            reasoning_effort,
            ..Default::default()
        },
        &|_| None,
    )
    .unwrap();
    papertiger::set_authorship(conn, &authorship).unwrap();
}
