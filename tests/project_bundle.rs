use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
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
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        fs::remove_dir_all(&self.0).unwrap();
    }
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
fn overlay_retains_custom_authority_from_an_older_receipt_and_refuses_changed_identity() {
    let f = Fixture::new();
    let setup = Command::new(BINARY)
        .args([
            "setup-project",
            f.0.to_str().unwrap(),
            "--authority-path",
            "private/work.sqlite",
        ])
        .env_remove("PAPERTIGER_DB")
        .output()
        .unwrap();
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    f.ok(&["init"]);
    f.ok(&[
        "plan",
        "add",
        "work",
        "Work",
        "--intent",
        "Preserve decisions",
    ]);
    f.ok(&[
        "add",
        "Retained outcome",
        "--plan",
        "work",
        "--intent",
        "Preserve this outcome",
        "--intent-source",
        "user",
    ]);
    let before: Value = serde_json::from_str(&f.ok(&["show", "1", "--json"])).unwrap();
    let receipt_path = f.0.join("tools/papertiger/project-install.json");
    let mut receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    receipt["papertiger_version"] = json!("0.1.0");
    let receipt_bytes = serde_json::to_vec(&receipt).unwrap();
    fs::write(&receipt_path, &receipt_bytes).unwrap();
    f.overlay();
    let after: Value = serde_json::from_str(&f.ok(&["show", "1", "--json"])).unwrap();
    assert_eq!(before["task"], after["task"]);
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt_bytes);
    assert!(!f.0.join("state/papertiger.sqlite").exists());
    let manifest = f.0.join("tools/papertiger/manifest.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["version"] = json!("0.1.0");
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(!f.run(&["status"]).status.success());
}
