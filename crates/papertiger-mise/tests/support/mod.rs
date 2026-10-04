//! Shared launcher for CLI integration tests.

use std::process::Command;

/// The model these tests attribute their authority writes to.
pub const TEST_MODEL: &str = "mise-test-model";

/// A `papertiger-mise` command isolated from the invoking harness: no
/// harness session, effort, actor or session leaks in, and every write is
/// attributed to [`TEST_MODEL`] unless a test overrides it.
pub fn mise_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_papertiger-mise"));
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
    command.env("PAPERTIGER_MODEL", TEST_MODEL);
    command
}
