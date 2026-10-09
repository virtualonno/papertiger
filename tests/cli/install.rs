//! `install` writes exact, hash-verified copies of the release it runs from
//! and never links; `install --check` reports every target's divergence.
use super::{TestDirectory, assert_success, support};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

const EXE: &str = std::env::consts::EXE_SUFFIX;

/// A release skill directory around the test build of the planner, with a
/// manifest as packaging writes it.
fn release(root: &Path, skill_text: &str) -> PathBuf {
    let dir = root.join("papertiger");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::create_dir_all(dir.join("references")).unwrap();
    let planner = dir.join(format!("bin/papertiger{EXE}"));
    if !planner.exists() {
        std::fs::copy(env!("CARGO_BIN_EXE_papertiger"), &planner).unwrap();
    }
    std::fs::write(dir.join(format!("bin/papertiger-mise{EXE}")), b"mise").unwrap();
    std::fs::write(dir.join("SKILL.md"), skill_text).unwrap();
    std::fs::write(dir.join("references/agent_integration.md"), "reference\n").unwrap();
    let mut files = BTreeMap::new();
    for name in [
        format!("bin/papertiger{EXE}"),
        format!("bin/papertiger-mise{EXE}"),
        "SKILL.md".to_owned(),
        "references/agent_integration.md".to_owned(),
    ] {
        files.insert(
            name.clone(),
            papertiger::sha256(&std::fs::read(dir.join(&name)).unwrap()),
        );
    }
    let manifest = serde_json::json!({
        "schema": "papertiger.release.v1",
        "version": env!("CARGO_PKG_VERSION"),
        "files": files,
    });
    std::fs::write(
        dir.join("papertiger-release.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    dir
}

fn install(release: &Path, targets: &[String], extra: &[&str]) -> Output {
    support::papertiger_command(release.join(format!("bin/papertiger{EXE}")))
        .arg("install")
        .args(extra)
        .args(targets)
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn install_writes_verified_copies_replaces_busy_ones_and_check_reports_divergence() {
    let sandbox = TestDirectory::new("install-copies");
    let source = release(&sandbox.0.join("release"), "skill v1\n");
    let claude = sandbox.0.join("claude/skills");
    let agents = sandbox.0.join("agents/skills");
    let bin = sandbox.0.join("bin");
    let targets: Vec<String> = [
        ("--skills-dir", &claude),
        ("--skills-dir", &agents),
        ("--bin-dir", &bin),
    ]
    .iter()
    .flat_map(|(flag, path)| [flag.to_string(), path.display().to_string()])
    .collect();

    let check = install(&source, &targets, &["--check"]);
    assert!(!check.status.success() && text(&check).contains("3 of 3 targets differ"));
    assert_success(&install(&source, &targets, &[]));
    let again = install(&source, &targets, &[]);
    assert_success(&again);
    assert_eq!(
        text(&again).matches("already current").count(),
        3,
        "{}",
        text(&again)
    );
    assert_success(&install(&source, &targets, &["--check"]));
    for skills in [&claude, &agents] {
        let copy = skills.join("papertiger");
        assert!(!copy.is_symlink());
        assert_eq!(std::fs::read(copy.join("SKILL.md")).unwrap(), b"skill v1\n");
        assert!(!skills.join(".papertiger-install").exists());
    }
    let planner = bin.join(format!("papertiger{EXE}"));
    assert_eq!(
        std::fs::read(&planner).unwrap(),
        std::fs::read(env!("CARGO_BIN_EXE_papertiger")).unwrap()
    );

    // Divergence names the file; an extra file counts too.
    std::fs::write(agents.join("papertiger/SKILL.md"), "edited\n").unwrap();
    std::fs::write(claude.join("papertiger/notes.txt"), "extra\n").unwrap();
    let diverged = install(&source, &targets, &["--check"]);
    let report = text(&diverged);
    assert!(!diverged.status.success());
    assert!(
        report.contains("changed SKILL.md")
            && report.contains("extra notes.txt")
            && report.contains("2 of 3 targets differ"),
        "{report}"
    );

    // An upgrade replaces a copy whose executable is running: the old copy is
    // renamed aside, the verified new one takes its place, and a later
    // install removes what the process held.
    let database = sandbox.0.join("held.sqlite");
    let held_exe = agents.join(format!("papertiger/bin/papertiger{EXE}"));
    let held_db = database.to_str().unwrap();
    assert_success(
        &support::papertiger_command(&held_exe)
            .args(["--db", held_db, "init"])
            .output()
            .unwrap(),
    );
    let mut held = support::papertiger_command(&held_exe)
        .args(["--db", held_db, "note", "--text-file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        held.try_wait().unwrap().is_none(),
        "the held process exited early"
    );
    let upgraded = release(&sandbox.0.join("release"), "skill v2\n");
    let upgrade = install(&upgraded, &targets, &[]);
    assert_success(&upgrade);
    for skills in [&claude, &agents] {
        assert_eq!(
            std::fs::read(skills.join("papertiger/SKILL.md")).unwrap(),
            b"skill v2\n"
        );
    }
    if cfg!(windows) {
        assert!(
            text(&upgrade).contains("still held by a running process"),
            "{}",
            text(&upgrade)
        );
        let retired = agents.join(".papertiger-install");
        let held_files: Vec<_> = walk(&retired);
        assert!(
            held_files.iter().all(|path| !path.ends_with("SKILL.md")),
            "a held copy keeps no SKILL.md: {held_files:?}"
        );
    }
    let mut stdin = held.stdin.take().unwrap();
    stdin.write_all(b"released\n").unwrap();
    drop(stdin);
    held.wait().unwrap();
    assert_success(&install(&upgraded, &targets, &[]));
    assert!(!agents.join(".papertiger-install").exists());
    assert_success(&install(&upgraded, &targets, &["--check"]));

    // A tampered release refuses before touching any target.
    std::fs::write(upgraded.join("SKILL.md"), "tampered\n").unwrap();
    let tampered = install(&upgraded, &targets, &[]);
    assert!(!tampered.status.success() && text(&tampered).contains("does not match its"));
    assert_eq!(
        std::fs::read(claude.join("papertiger/SKILL.md")).unwrap(),
        b"skill v2\n"
    );
}

#[test]
fn install_refuses_a_linked_target_unless_asked_to_replace_it() {
    let sandbox = TestDirectory::new("install-link");
    let source = release(&sandbox.0.join("release"), "skill\n");
    let shared = sandbox.0.join("shared/papertiger");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("SKILL.md"), "shared\n").unwrap();
    let skills = sandbox.0.join("skills");
    std::fs::create_dir_all(&skills).unwrap();
    // Without the symlink privilege, Windows still allows a junction, which
    // is how such a link is usually made there.
    #[cfg(windows)]
    let linked =
        std::os::windows::fs::symlink_dir(&shared, skills.join("papertiger")).or_else(|_| {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(native(&skills.join("papertiger")))
                .arg(native(&shared))
                .output()
                .and_then(|output| {
                    if output.status.success() {
                        Ok(())
                    } else {
                        Err(std::io::Error::other(
                            String::from_utf8_lossy(&output.stderr).into_owned(),
                        ))
                    }
                })
        });
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&shared, skills.join("papertiger"));
    if let Err(error) = linked {
        eprintln!("skipping: this host cannot create a directory symlink: {error}");
        return;
    }
    let targets = ["--skills-dir".to_owned(), skills.display().to_string()];
    let check = install(&source, &targets, &["--check"]);
    assert!(
        text(&check).contains("a link, not a copy"),
        "{}",
        text(&check)
    );
    let refused = install(&source, &targets, &[]);
    assert!(!refused.status.success() && text(&refused).contains("--replace-links"));
    assert!(skills.join("papertiger").is_symlink());
    assert_success(&install(&source, &targets, &["--replace-links"]));
    assert!(!skills.join("papertiger").is_symlink());
    assert_eq!(
        std::fs::read(skills.join("papertiger/SKILL.md")).unwrap(),
        b"skill\n"
    );
    assert_eq!(std::fs::read(shared.join("SKILL.md")).unwrap(), b"shared\n");
}

/// A path as cmd.exe parses it: forward slashes would read as switches.
#[cfg(windows)]
fn native(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}
