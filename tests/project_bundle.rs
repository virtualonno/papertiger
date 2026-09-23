use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

const BINARY: &str = env!("CARGO_BIN_EXE_papertiger");
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static ID: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "bundle-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn native(&self) -> PathBuf {
        self.0.join(format!(
            "tools/papertiger/bin/papertiger{}",
            std::env::consts::EXE_SUFFIX
        ))
    }
    fn overlay(&self) {
        let binary = self.native();
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::copy(BINARY, &binary).unwrap();
        let key = format!("bin/papertiger{}", std::env::consts::EXE_SUFFIX);
        let manifest = json!({"schema":"papertiger.release_manifest.v3", "name":"papertiger", "version":env!("CARGO_PKG_VERSION"), "binary_sha256":{key:papertiger::sha256(&fs::read(BINARY).unwrap())}});
        fs::write(
            self.0.join("tools/papertiger/manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        let cwd = self.0.join("nested/work");
        fs::create_dir_all(&cwd).unwrap();
        Command::new(self.native())
            .args(args)
            .current_dir(cwd)
            .env_remove("PAPERTIGER_DB")
            .env("PAPERTIGER_ACTOR", "test")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let result = self.run(args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    }
    /// Run setup-project from the external release binary, as an upgrade does.
    fn setup(&self, args: &[&str]) -> std::process::Output {
        Command::new(BINARY)
            .arg("setup-project")
            .arg(&self.0)
            .args(args)
            .env_remove("PAPERTIGER_DB")
            .output()
            .unwrap()
    }
    /// A managed project whose older receipt selects a custom authority that
    /// holds one task; returns that task as `show --json` reports it.
    fn custom_authority_history(&self) -> Value {
        let setup = self.setup(&["--authority-path", "private/work.sqlite"]);
        assert!(
            setup.status.success(),
            "{}",
            String::from_utf8_lossy(&setup.stderr)
        );
        self.ok(&["init"]);
        self.ok(&[
            "plan",
            "add",
            "work",
            "Work",
            "--intent",
            "Preserve decisions",
        ]);
        self.ok(&[
            "add",
            "Retained outcome",
            "--plan",
            "work",
            "--intent",
            "Preserve this outcome",
            "--intent-source",
            "user",
        ]);
        let task: Value = serde_json::from_str(&self.ok(&["show", "1", "--json"])).unwrap();
        let receipt_path = self.0.join("tools/papertiger/project-install.json");
        let mut receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        receipt["papertiger_version"] = json!("0.1.0");
        fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        task
    }
    fn set_manifest_version(&self, version: &str) {
        let manifest = self.0.join("tools/papertiger/manifest.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        value["version"] = json!(version);
        fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    /// Every file beneath the project root with its exact bytes.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, files);
                } else {
                    files.insert(path.clone(), fs::read(&path).unwrap());
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(&self.0, &mut files);
        files
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn refusal(output: &std::process::Output) -> String {
    assert!(
        !output.status.success(),
        "setup-project unexpectedly succeeded"
    );
    assert!(output.stdout.is_empty());
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn overlay_discovers_root_without_setup_and_never_recreates_missing_history() {
    let f = Fixture::new();
    f.overlay();
    let first_use = f.run(&["status"]);
    assert!(!first_use.status.success());
    let hint = String::from_utf8(first_use.stderr).unwrap();
    assert!(hint.contains("with `init`"), "{hint}");
    assert!(hint.contains("same authority selectors"), "{hint}");
    assert!(!hint.contains("--db"), "{hint}");
    assert!(!f.0.join("state").exists());
    f.ok(&["init"]);
    f.ok(&["--project-root", f.0.to_str().unwrap(), "audit"]);
    assert!(!f.0.join("nested/work/state").exists());
    let database = f.0.join("state/papertiger.sqlite");
    fs::remove_file(&database).unwrap();
    let missing_history = f.run(&["status"]);
    assert!(!missing_history.status.success());
    assert!(
        String::from_utf8(missing_history.stderr)
            .unwrap()
            .contains("restore an export instead of initializing")
    );
    assert!(!database.exists());
}

#[test]
fn overlay_retains_custom_authority_from_an_older_receipt_and_refuses_another_release() {
    let f = Fixture::new();
    let before = f.custom_authority_history();
    let receipt_path = f.0.join("tools/papertiger/project-install.json");
    let receipt_bytes = fs::read(&receipt_path).unwrap();
    f.overlay();
    let after: Value = serde_json::from_str(&f.ok(&["show", "1", "--json"])).unwrap();
    assert_eq!(before["task"], after["task"]);
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt_bytes);
    assert!(!f.0.join("state/papertiger.sqlite").exists());
    f.set_manifest_version("0.1.0");
    let refused = f.run(&["status"]);
    assert!(!refused.status.success());
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        error.contains("is Papertiger 0.1.0") && error.contains("unpack the verified Papertiger"),
        "{error}"
    );
}

#[test]
fn overlay_runs_without_hashing_installed_binaries() {
    let f = Fixture::new();
    f.overlay();
    let manifest = f.0.join("tools/papertiger/manifest.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let key = format!("bin/papertiger{}", std::env::consts::EXE_SUFFIX);
    value["binary_sha256"][&key] = json!("0".repeat(64));
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    f.ok(&["init"]);
    f.ok(&["status"]);
}

#[test]
fn setup_refuses_an_overlay_naming_another_release_before_any_write() {
    let f = Fixture::new();
    f.custom_authority_history();
    f.overlay();
    f.set_manifest_version("0.1.0");
    let before = f.snapshot();

    let preview = refusal(&f.setup(&["--dry-run", "--json"]));
    for expected in [
        "tools/papertiger/manifest.json names Papertiger 0.1.0",
        concat!("running binary is ", env!("CARGO_PKG_VERSION")),
        "nothing was written",
        concat!(
            "unpack the verified Papertiger ",
            env!("CARGO_PKG_VERSION"),
            " release archive over the project root"
        ),
        "move tools/papertiger/manifest.json aside and rerun setup-project",
    ] {
        assert!(
            preview.contains(expected),
            "{expected:?} missing: {preview}"
        );
    }
    assert_eq!(refusal(&f.setup(&["--dry-run"])), preview);
    assert_eq!(refusal(&f.setup(&[])), preview);
    assert_eq!(refusal(&f.setup(&["--json"])), preview);
    assert_eq!(refusal(&f.setup(&["--dry-run", "--json"])), preview);
    assert!(
        f.snapshot() == before,
        "a refused setup changed project files"
    );
}

#[test]
fn moving_the_other_release_manifest_aside_lets_setup_manage_existing_history() {
    let f = Fixture::new();
    let task = f.custom_authority_history();
    f.overlay();
    f.set_manifest_version("0.1.0");
    refusal(&f.setup(&[]));

    let manifest = f.0.join("tools/papertiger/manifest.json");
    fs::rename(&manifest, f.0.join("manifest.json.aside")).unwrap();
    let applied = f.setup(&["--json"]);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let result: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(result["operation"], "upgrade");
    assert_eq!(result["authority_path"], "private/work.sqlite");
    assert!(!manifest.exists());
    f.ok(&["status"]);
    f.ok(&["audit"]);
    let after: Value = serde_json::from_str(&f.ok(&["show", "1", "--json"])).unwrap();
    assert_eq!(after["task"], task["task"]);
    assert!(!f.0.join("state/papertiger.sqlite").exists());
}

#[test]
fn unpacking_the_running_release_over_the_overlay_keeps_history_and_setup_proceeds() {
    let f = Fixture::new();
    let task = f.custom_authority_history();
    f.overlay();
    f.set_manifest_version("0.1.0");
    refusal(&f.setup(&["--dry-run"]));

    // Unpacking the running release's archive rewrites its manifest.
    f.overlay();
    let after: Value = serde_json::from_str(&f.ok(&["show", "1", "--json"])).unwrap();
    assert_eq!(after["task"], task["task"]);
    f.ok(&["audit"]);
    let applied = f.setup(&["--json"]);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let result: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(result["authority_path"], "private/work.sqlite");
    f.ok(&["status"]);
    let after: Value = serde_json::from_str(&f.ok(&["show", "1", "--json"])).unwrap();
    assert_eq!(after["task"], task["task"]);
    assert!(!f.0.join("state/papertiger.sqlite").exists());
}

#[test]
fn setup_refuses_an_unreadable_release_manifest_before_any_write() {
    let f = Fixture::new();
    f.overlay();
    fs::write(f.0.join("tools/papertiger/manifest.json"), b"{not json").unwrap();
    let before = f.snapshot();
    for args in [&["--dry-run"][..], &[][..]] {
        let error = refusal(&f.setup(args));
        assert!(
            error.contains("is not a readable Papertiger release manifest")
                && error.contains("nothing was written")
                && error.contains("move tools/papertiger/manifest.json aside"),
            "{error}"
        );
    }
    assert!(
        f.snapshot() == before,
        "a refused setup changed project files"
    );
}
