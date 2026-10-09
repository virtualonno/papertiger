//! A repository whose planning belongs to another project routes there
//! through a file in its Git directory; a runner selects a project per
//! process with PAPERTIGER_PROJECT_ROOT. Both fail fast on a wrong target.
use super::{TestDirectory, assert_success, support};
use std::path::Path;
use std::process::Output;

fn run(directory: &Path, args: &[&str], env: &[(&str, &Path)]) -> Output {
    let mut command = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"));
    command
        .args(args)
        .current_dir(directory)
        .env_remove("PAPERTIGER_DB");
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn stderr(output: &Output) -> String {
    assert!(!output.status.success(), "unexpected success");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    assert_success(output);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_routed_repository_and_its_worktrees_plan_in_the_owning_authority() {
    let sandbox = TestDirectory::new("routed-repository");
    let owner = sandbox.0.join("owner");
    let member = sandbox.0.join("member");
    let nested = member.join("src/deep");
    let worktree = sandbox.0.join("member-feature");
    std::fs::create_dir_all(&owner).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    // Git's on-disk shape for a main tree and one linked worktree.
    let private = member.join(".git/worktrees/feature");
    std::fs::create_dir_all(&private).unwrap();
    std::fs::write(private.join("commondir"), "../..\n").unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", private.display()),
    )
    .unwrap();
    let owner_arg = owner.to_str().unwrap();

    // A route needs an existing authority to name and refuses itself.
    assert!(stderr(&run(&nested, &["route", "set", owner_arg], &[])).contains("init"));
    assert_success(&run(
        &sandbox.0,
        &["--project-root", owner_arg, "init"],
        &[],
    ));
    assert!(stderr(&run(&member, &["route", "set", "."], &[])).contains("has no state"));
    assert!(stderr(&run(&sandbox.0, &["route", "set", owner_arg], &[])).contains("Git repository"));
    assert!(stderr(&run(&nested, &["status"], &[])).contains("papertiger route set"));

    let routed = stdout(&run(&nested, &["route", "set", owner_arg], &[]));
    let route = member.join(".git/papertiger-authority");
    assert!(
        route.is_file() && routed.contains("never commits"),
        "{routed}"
    );
    assert!(!member.join("state").exists());
    for directory in [&nested, &worktree] {
        let shown = stdout(&run(directory, &["route", "show"], &[]));
        assert!(
            shown.contains("owner/state/papertiger.sqlite") && shown.contains("routed by"),
            "{shown}"
        );
        assert!(
            stdout(&run(directory, &["status"], &[])).contains("owner/state/papertiger.sqlite")
        );
    }
    // init reaches the owning authority rather than creating a second one.
    assert!(stdout(&run(&worktree, &["init"], &[])).contains("nothing changed"));
    assert!(!worktree.join("state").exists() && !member.join("state").exists());

    // A route naming a missing project, or beside an own authority, refuses.
    std::fs::write(&route, format!("{}\n", sandbox.0.join("gone").display())).unwrap();
    let missing = stderr(&run(&nested, &["status"], &[]));
    assert!(
        missing.contains("does not exist") && missing.contains("route clear"),
        "{missing}"
    );
    assert!(stdout(&run(&nested, &["route", "clear"], &[])).contains("removed"));
    assert!(stdout(&run(&nested, &["route", "clear"], &[])).contains("nothing changed"));
    std::fs::write(&route, format!("{owner_arg}\n")).unwrap();
    assert_success(&run(
        &sandbox.0,
        &["--project-root", member.to_str().unwrap(), "status"],
        &[],
    ));
    std::fs::create_dir_all(member.join("state")).unwrap();
    std::fs::copy(
        owner.join("state/papertiger.sqlite"),
        member.join("state/papertiger.sqlite"),
    )
    .unwrap();
    assert!(stderr(&run(&nested, &["status"], &[])).contains("also a route"));
    assert!(stderr(&run(&nested, &["route", "set", owner_arg], &[])).contains("its own authority"));
}

#[test]
fn a_runner_selects_the_project_per_process_and_a_wrong_value_refuses() {
    let sandbox = TestDirectory::new("runner-project-root");
    let owner = sandbox.0.join("owner");
    let elsewhere = sandbox.0.join("elsewhere");
    std::fs::create_dir_all(&owner).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert_success(&run(
        &sandbox.0,
        &["--project-root", owner.to_str().unwrap(), "init"],
        &[],
    ));
    let variable = "PAPERTIGER_PROJECT_ROOT";

    let shown = stdout(&run(&elsewhere, &["route", "show"], &[(variable, &owner)]));
    assert!(
        shown.contains("owner/state/papertiger.sqlite") && shown.contains(variable),
        "{shown}"
    );
    assert_success(&run(&elsewhere, &["status"], &[(variable, &owner)]));
    // A runner's variable never creates an authority and never combines with
    // a database override.
    for args in [&["status"][..], &["init"]] {
        let refused = stderr(&run(&owner, args, &[(variable, &elsewhere)]));
        assert!(
            refused.contains("has no state/papertiger.sqlite"),
            "{refused}"
        );
    }
    assert!(!elsewhere.join("state").exists());
    let combined = stderr(&run(
        &elsewhere,
        &["status"],
        &[(variable, &owner), ("PAPERTIGER_DB", &owner)],
    ));
    assert!(combined.contains(variable), "{combined}");
    let empty = support::papertiger_command(env!("CARGO_BIN_EXE_papertiger"))
        .arg("status")
        .current_dir(&owner)
        .env(variable, "")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&empty.stderr).contains("set but empty"));
}
