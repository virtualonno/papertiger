use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
const TOOL: &str = "papertiger";
const BINARY: &str = env!("CARGO_BIN_EXE_papertiger");
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "personal-{TOOL}-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(BINARY)
            .args(args)
            .arg("--home")
            .arg(&self.0)
            .env_remove("PAPERTIGER_DB")
            .output()
            .unwrap()
    }
    fn install(&self) -> Value {
        let result = self.run(&["setup-user", "--json"]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&result.stdout).unwrap()
    }
    fn skill(&self) -> PathBuf {
        self.0.join(format!(".agents/skills/{TOOL}/SKILL.md"))
    }
    fn runtime(&self) -> PathBuf {
        self.0.join(format!(
            ".local/share/{TOOL}/bin/{TOOL}{}",
            std::env::consts::EXE_SUFFIX
        ))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            result.extend(snapshot(&p));
        } else {
            result.insert(p.clone(), fs::read(p).unwrap());
        }
    }
    result
}
#[test]
fn personal_install_is_guidance_free_idempotent_and_reversible() {
    let f = Fixture::new();
    fs::write(f.0.join("AGENTS.md"), "Existing user guidance\n").unwrap();
    let original = snapshot(&f.0);
    let preview = f.run(&["setup-user", "--dry-run", "--json"]);
    assert!(preview.status.success());
    let preview_json: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview_json["authority"]["action"], "initialize");
    assert_eq!(snapshot(&f.0), original);
    f.install();
    let skill = fs::read_to_string(f.skill()).unwrap();
    assert!(skill.starts_with("---\nname:"));
    assert!(!skill.contains("<!-- installed-command -->"));
    assert!(skill.contains(".local/share/"));
    assert!(!skill.contains("//?/"));
    assert_eq!(
        fs::read(f.skill()).unwrap(),
        fs::read(f.0.join(format!(".claude/skills/{TOOL}/SKILL.md"))).unwrap()
    );
    let installed = snapshot(&f.0);
    f.install();
    assert_eq!(snapshot(&f.0), installed);
    let version = Command::new(f.runtime()).arg("--version").output().unwrap();
    assert!(
        version.status.success(),
        "{}",
        String::from_utf8_lossy(&version.stderr)
    );
    assert!(f.run(&["uninstall-user", "--dry-run"]).status.success());
    assert_eq!(snapshot(&f.0), installed);
    assert!(f.run(&["uninstall-user"]).status.success());
    assert!(!f.runtime().exists());
    assert!(!f.skill().exists());
    assert_eq!(
        fs::read(f.0.join("AGENTS.md")).unwrap(),
        original[&f.0.join("AGENTS.md")]
    );
    f.install();
}
#[test]
fn personal_skills_are_release_owned_and_runtime_identity_is_gated() {
    let f = Fixture::new();
    fs::create_dir_all(f.skill().parent().unwrap()).unwrap();
    fs::write(f.skill(), "Locally owned skill").unwrap();
    let before = snapshot(&f.0);
    let preview = f.run(&["setup-user", "--dry-run", "--json"]);
    assert!(preview.status.success());
    assert_eq!(snapshot(&f.0), before);
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert!(preview["actions"].as_array().unwrap().iter().any(|action| {
        action["action"] == "replace" && action["path"].as_str().unwrap().ends_with("SKILL.md")
    }));
    f.install();
    fs::write(f.skill(), "Modified installed skill").unwrap();
    let runtime = Command::new(f.runtime()).arg("--version").output().unwrap();
    assert!(
        runtime.status.success(),
        "{}",
        String::from_utf8_lossy(&runtime.stderr)
    );
    f.install();
    assert!(!fs::read_to_string(f.skill()).unwrap().contains("Modified"));

    let mut tampered = fs::read(f.runtime()).unwrap();
    tampered.extend_from_slice(b"tampered");
    fs::write(f.runtime(), &tampered).unwrap();
    let runtime = Command::new(f.runtime()).arg("--version").output().unwrap();
    assert!(
        !runtime.status.success(),
        "a divergent runtime must fail closed"
    );
    assert!(String::from_utf8_lossy(&runtime.stderr).contains("differs from its receipt"));
    let removal = f.run(&["uninstall-user"]);
    assert!(
        removal.status.success(),
        "uninstall-user removes owned paths without comparing content: {}",
        String::from_utf8_lossy(&removal.stderr)
    );
    assert!(
        !f.runtime().exists(),
        "a tampered runtime is removed by path"
    );
    assert!(!f.skill().exists());
    f.install();
    assert!(
        Command::new(f.runtime())
            .arg("--version")
            .output()
            .unwrap()
            .status
            .success()
    );
}
#[test]
fn non_file_at_owned_personal_path_refuses_uninstall() {
    let f = Fixture::new();
    f.install();
    fs::remove_file(f.skill()).unwrap();
    fs::create_dir(f.skill()).unwrap();
    let before = snapshot(&f.0);
    let removal = f.run(&["uninstall-user"]);
    assert!(!removal.status.success());
    assert!(String::from_utf8_lossy(&removal.stderr).contains("wrong file type"));
    assert_eq!(snapshot(&f.0), before);
    assert!(f.runtime().is_file());
}
#[test]
fn personal_receipt_cannot_claim_foreign_paths_or_downgrade() {
    let f = Fixture::new();
    f.install();
    let path = f.0.join(format!(".local/share/{TOOL}/user-install.json"));
    let mut receipt: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    receipt["files"]
        .as_array_mut()
        .unwrap()
        .push(Value::String("AGENTS.md".into()));
    fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let before = snapshot(&f.0);
    assert!(!f.run(&["uninstall-user"]).status.success());
    assert_eq!(snapshot(&f.0), before);
    receipt["files"]
        .as_array_mut()
        .unwrap()
        .retain(|path| path != "AGENTS.md");
    receipt["version"] = Value::String("999.0.0".into());
    fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let before = snapshot(&f.0);
    assert!(!f.run(&["setup-user"]).status.success());
    assert_eq!(snapshot(&f.0), before);
}
#[test]
fn missing_runtime_repairs_but_installed_runtime_cannot_overwrite_itself() {
    let f = Fixture::new();
    f.install();
    let output = Command::new(f.runtime())
        .arg("setup-user")
        .arg("--home")
        .arg(&f.0)
        .output()
        .unwrap();
    assert!(!output.status.success());
    fs::remove_file(f.runtime()).unwrap();
    f.install();
    assert!(f.runtime().exists());
}
#[cfg(unix)]
#[test]
fn symlinked_skill_parent_refuses_without_touching_target() {
    let f = Fixture::new();
    let other = Fixture::new();
    std::os::unix::fs::symlink(&other.0, f.0.join(".agents")).unwrap();
    assert!(!f.run(&["setup-user"]).status.success());
    assert!(snapshot(&other.0).is_empty());
    fs::remove_file(f.0.join(".agents")).unwrap();
}

#[test]
fn personal_history_survives_removal_and_missing_history_is_not_reinitialized() {
    let f = Fixture::new();
    f.install();
    let db = f.0.join(".local/share/papertiger/state/papertiger.sqlite");
    let before = fs::read(&db).unwrap();
    assert!(f.run(&["uninstall-user"]).status.success());
    assert_eq!(fs::read(&db).unwrap(), before);
    fs::rename(&db, db.with_extension("saved")).unwrap();
    let state = snapshot(&f.0);
    assert!(!f.run(&["setup-user"]).status.success());
    assert_eq!(snapshot(&f.0), state);
}

#[test]
fn incomplete_receipt_refuses_without_adopting_files() {
    let f = Fixture::new();
    f.install();
    let path = f.0.join(format!(".local/share/{TOOL}/user-install.json"));
    let mut receipt: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let skill = format!(".agents/skills/{TOOL}/SKILL.md");
    receipt["files"]
        .as_array_mut()
        .unwrap()
        .retain(|path| *path != Value::String(skill.clone()));
    fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let before = snapshot(&f.0);
    assert!(!f.run(&["setup-user"]).status.success());
    assert!(!f.run(&["uninstall-user"]).status.success());
    assert_eq!(snapshot(&f.0), before);
}

#[test]
fn setup_finishes_interrupted_initialization_without_replacing_authority() {
    let f = Fixture::new();
    let db = f.0.join(".local/share/papertiger/state/papertiger.sqlite");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let initialized = Command::new(BINARY)
        .arg("--db")
        .arg(&db)
        .arg("init")
        .output()
        .unwrap();
    assert!(initialized.status.success());
    f.install();
    let output = Command::new(f.runtime())
        .arg("--db")
        .arg(&db)
        .args(["plan", "list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let plans: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plans["plans"][0]["slug"], "personal");
    assert_eq!(plans["plans"].as_array().unwrap().len(), 1);
}

#[test]
fn personal_default_uses_project_receipts_before_private_store_from_nested_cwd() {
    let home = Fixture::new();
    home.install();
    let project = Fixture::new();
    let nested = project.0.join("nested/work");
    fs::create_dir_all(&nested).unwrap();
    let status = || {
        let output = Command::new(home.runtime())
            .args(["status", "--json"])
            .current_dir(&nested)
            .env_remove("PAPERTIGER_DB")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    assert_eq!(status()["active_plans"][0]["plan"]["slug"], "personal");
    let install = Command::new(BINARY)
        .arg("setup-project")
        .arg(&project.0)
        .args(["--skill-target", "none"])
        .output()
        .unwrap();
    assert!(install.status.success());
    for args in [
        vec!["init"],
        vec![
            "plan",
            "add",
            "project",
            "Project work",
            "--intent",
            "Existing project authority",
        ],
    ] {
        let output = Command::new(BINARY)
            .arg("--project-root")
            .arg(&project.0)
            .args(args)
            .env("PAPERTIGER_ACTOR", "fixture")
            .env_remove("PAPERTIGER_DB")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(status()["active_plans"][0]["plan"]["slug"], "project");
    assert!(!nested.join("state").exists());
}

#[test]
fn previous_personal_receipt_is_rewritten_and_older_schemas_refuse() {
    use sha2::Digest;
    let f = Fixture::new();
    f.install();
    let path = f.0.join(format!(".local/share/{TOOL}/user-install.json"));
    let current: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut files = serde_json::Map::new();
    for relative in current["files"].as_array().unwrap() {
        let relative = relative.as_str().unwrap();
        let digest = sha2::Sha256::digest(fs::read(f.0.join(relative)).unwrap());
        files.insert(relative.to_owned(), Value::String(format!("{digest:x}")));
    }
    let previous = serde_json::json!({
        "schema": format!("{TOOL}.user_install.v1"),
        "version": current["version"],
        "home": current["home"],
        "installed": true,
        "files": files,
    });
    fs::write(&path, serde_json::to_vec(&previous).unwrap()).unwrap();
    f.install();
    let rewritten: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(rewritten, current);

    let mut older = current.clone();
    older["schema"] = Value::String(format!("{TOOL}.user_install.v0"));
    fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
    let before = snapshot(&f.0);
    let refused = f.run(&["setup-user"]);
    assert!(!refused.status.success());
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        error.contains("move it aside and rerun setup-user"),
        "{error}"
    );
    assert_eq!(snapshot(&f.0), before);
}
