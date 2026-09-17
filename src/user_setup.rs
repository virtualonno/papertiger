//! Personal installation owns tool files, never consuming projects or harness settings.
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use papertiger::sha256;
const TOOL: &str = "papertiger";
const SKILL: &str = include_str!("../templates/papertiger/SKILL.md");
const REFERENCE: &[u8] = include_bytes!("../agent_integration.md");

/// Separate derive boundary keeps Clap's debug stack bounded on Windows.
#[derive(clap::Subcommand)]
pub(crate) enum Command {
    /// Install personal skills, native runtime and private fallback store; leaves projects untouched
    #[command(name = "setup-user")]
    Setup {
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        replace_managed: bool,
    },
    /// Remove receipt-owned personal skills/runtime; retain planning history
    #[command(name = "uninstall-user")]
    Uninstall {
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: String,
    version: String,
    home: PathBuf,
    installed: bool,
    files: BTreeMap<String, String>,
}

fn personal_root() -> String {
    format!(".local/share/{TOOL}")
}

fn binary_path() -> String {
    format!(
        "{}/bin/{TOOL}{}",
        personal_root(),
        std::env::consts::EXE_SUFFIX
    )
}

fn paths() -> Vec<String> {
    vec![
        binary_path(),
        format!("{}/agent_integration.md", personal_root()),
        format!(".agents/skills/{TOOL}/SKILL.md"),
        format!(".claude/skills/{TOOL}/SKILL.md"),
    ]
}

/// Refuse links and wrong file types at every boundary before reading or writing.
fn validate_path(home: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = home.to_path_buf();
    let components: Vec<_> = Path::new(relative).components().collect();
    for (i, component) in components.iter().enumerate() {
        let std::path::Component::Normal(part) = component else {
            bail!("invalid personal install path {relative}; use a verified release's setup-user");
        };
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if meta.file_type().is_symlink()
                    || (i + 1 < components.len() && !meta.is_dir())
                    || (i + 1 == components.len() && !meta.is_file())
                {
                    bail!(
                        "personal install path {} is a link or wrong file type; move it aside before setup-user",
                        path.display()
                    );
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        }
    }
    Ok(path)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn publish(path: &Path, content: &[u8]) -> Result<()> {
    fs::create_dir_all(
        path.parent()
            .context("personal destination needs a parent")?,
    )?;
    if path.exists() {
        papertiger::atomic_replace_file(
            path,
            content,
            "personal installation",
            "setup-user from an external release",
        )
    } else {
        papertiger::atomic_create_file(
            path,
            content,
            "personal installation",
            "setup-user from an external release",
        )
    }
}

pub(crate) fn run(
    home: Option<&Path>,
    dry_run: bool,
    replace: bool,
    remove: bool,
) -> Result<serde_json::Value> {
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .context("home is unavailable; pass setup-user --home <existing-directory>")?,
    };
    let home = PathBuf::from(papertiger::portable_absolute(
        &fs::canonicalize(&home).context("home must exist; pass --home <existing-directory>")?,
    )?);
    if !home.is_dir() {
        bail!("home is not a directory; pass --home <existing-directory>");
    }
    let receipt_path = validate_path(&home, &format!("{}/user-install.json", personal_root()))?;
    let previous: Option<Receipt> = read_optional(&receipt_path)?
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()
        .context("invalid user-install.json; restore its verified backup before setup-user")?;
    let schema = format!("{TOOL}.user_install.v1");
    if let Some(receipt) = &previous {
        if receipt.schema != schema
            || receipt.home != home
            || (receipt.installed && receipt.files.len() != paths().len())
            || (!receipt.installed && !receipt.files.is_empty())
            || receipt.files.keys().any(|p| !paths().contains(p))
            || receipt
                .files
                .values()
                .any(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            bail!(
                "personal receipt identity or paths are invalid; restore user-install.json for this home before setup-user"
            );
        }
        if semver::Version::parse(&receipt.version)?
            > semver::Version::parse(env!("CARGO_PKG_VERSION"))?
        {
            bail!(
                "personal installation is newer; run setup-user with version {} or newer",
                receipt.version
            );
        }
    } else if remove {
        bail!(
            "no personal receipt at {}; nothing is owned for uninstall-user",
            receipt_path.display()
        );
    }
    let source = std::env::current_exe()?;
    let destination = home.join(binary_path());
    if destination.exists() && fs::canonicalize(&destination)? == fs::canonicalize(&source)? {
        bail!(
            "run setup-user/uninstall-user from an external release binary, outside {}",
            destination.display()
        );
    }
    if !SKILL.contains("<!-- installed-command -->") {
        bail!("release skill has no command binding; use a complete verified release");
    }
    let binding = installed_binding(&home);
    let reference = home.join(format!("{}/agent_integration.md", personal_root()));
    let skill = SKILL
        .replace("\r\n", "\n")
        .replace("<!-- installed-command -->", &binding)
        .replace(
            "../../../tools/papertiger/agent_integration.md",
            &reference.to_string_lossy().replace('\\', "/"),
        );
    let wanted = if remove {
        BTreeMap::new()
    } else {
        BTreeMap::from([
            (binary_path(), fs::read(&source)?),
            (
                format!("{}/agent_integration.md", personal_root()),
                REFERENCE.to_vec(),
            ),
            (
                format!(".agents/skills/{TOOL}/SKILL.md"),
                skill.as_bytes().to_vec(),
            ),
            (
                format!(".claude/skills/{TOOL}/SKILL.md"),
                skill.as_bytes().to_vec(),
            ),
        ])
    };
    let mut actions = Vec::new();
    for relative in paths() {
        let path = validate_path(&home, &relative)?;
        let existing = read_optional(&path)?;
        let owned = previous.as_ref().and_then(|r| r.files.get(&relative));
        let action = match (existing.as_ref(), wanted.get(&relative)) {
            (Some(old), Some(new)) if old == new => "unchanged",
            (Some(old), _) if owned.is_some_and(|hash| *hash == sha256(old)) => {
                if remove {
                    "remove"
                } else {
                    "replace"
                }
            }
            (Some(_), Some(_)) if replace => "replace",
            (Some(_), _) => bail!(
                "{} is unowned or modified; review it and use setup-user --home {:?} --replace-managed to replace selected files, or move it aside before uninstall-user",
                path.display(),
                home
            ),
            (None, Some(_)) => "create",
            (None, None) => "absent",
        };
        actions.push(serde_json::json!({"path":path,"action":action}));
    }
    check_authority(&home, previous.is_some(), remove)?;
    let authority_path = authority(&home)?;
    let authority_action = if remove {
        "preserve"
    } else if authority_path.exists() {
        "validate_existing"
    } else {
        "initialize"
    };
    if !dry_run {
        for action in &actions {
            let path = PathBuf::from(
                action["path"]
                    .as_str()
                    .context("personal path is not UTF-8")?,
            );
            match action["action"].as_str() {
                Some("create" | "replace") => {
                    let relative = path
                        .strip_prefix(&home)?
                        .to_string_lossy()
                        .replace('\\', "/");
                    publish(&path, &wanted[&relative])?;
                }
                Some("remove") => fs::remove_file(&path)?,
                _ => {}
            }
        }
        if !remove {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))?;
            }
            provision_authority(&home)?;
        }
        let receipt = Receipt {
            schema,
            version: env!("CARGO_PKG_VERSION").into(),
            home: home.clone(),
            installed: !remove,
            files: wanted.iter().map(|(p, b)| (p.clone(), sha256(b))).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        if read_optional(&receipt_path)?.as_deref() != Some(bytes.as_slice()) {
            publish(&receipt_path, &bytes)?;
        }
        if !remove {
            let probe = std::process::Command::new(&destination).arg("--version").output()
                .with_context(|| format!("installed executable {} could not run; check execute permissions and run setup-user again", destination.display()))?;
            let expected = format!("{TOOL} {}", env!("CARGO_PKG_VERSION"));
            if !probe.status.success() || String::from_utf8_lossy(&probe.stdout).trim() != expected
            {
                bail!(
                    "installed runtime verification failed: {}; repair with setup-user from a verified external release",
                    String::from_utf8_lossy(&probe.stderr)
                );
            }
        }
    }
    Ok(
        serde_json::json!({"schema":format!("{TOOL}.user_setup.v1"),"home":home,"dry_run":dry_run,"operation":if remove {"remove"} else {"install"},"actions":actions,"authority":{"path":authority_path,"action":authority_action,"plan":"personal","preserved_on_uninstall":true},
        "next_actions": ["After installation, start a fresh agent session to refresh skill discovery. No agent-side copying or AGENTS.md edits are needed. Selection remains model-dependent.", "Personal data and the lifecycle receipt survive uninstall-user. Project installations and project guidance are untouched."]}),
    )
}

fn installed_binding(home: &Path) -> String {
    format!(
        "Personal executable: `{}`. Its default authority is the consuming project's receipt-bound store when present, otherwise its installed private store with plan `personal`; no --db argument is needed. Use --project-root only to select an existing project installation, not to label a personal task. Include the absolute consuming-project root in personal task intent and search that root before creating work. This store was initialized by setup-user; never initialize it to replace missing history. An explicitly selected canonical project authority or shared tracker still takes precedence.\n",
        home.join(binary_path())
            .to_string_lossy()
            .replace('\\', "/")
    )
}

fn authority(home: &Path) -> Result<PathBuf> {
    validate_path(
        home,
        &format!("{}/state/papertiger.sqlite", personal_root()),
    )
}

fn authority_command(home: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = std::process::Command::new(std::env::current_exe()?)
        .arg("--db")
        .arg(authority(home)?)
        .args(args)
        .env("PAPERTIGER_ACTOR", "setup-user")
        .env_remove("PAPERTIGER_DB")
        .env_remove("PAPERTIGER_SESSION")
        .env_remove("PAPERTIGER_MODEL")
        .env_remove("PAPERTIGER_REASONING_EFFORT")
        .output()?;
    if !output.status.success() {
        bail!(
            "personal authority check failed: {}; restore the authority or follow its explicit migration command",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

fn check_authority(home: &Path, previously_installed: bool, remove: bool) -> Result<()> {
    let path = authority(home)?;
    if previously_installed && !path.is_file() && !remove {
        bail!(
            "personal authority {} is missing; restore its backup, never rerun init to replace history",
            path.display()
        );
    }
    if path.is_file() && !remove {
        authority_command(home, &["status", "--json"])?;
        let plans: serde_json::Value =
            serde_json::from_slice(&authority_command(home, &["plan", "list", "--json"])?)?;
        if let Some(personal) = plans["plans"]
            .as_array()
            .context("plan list has no plans array")?
            .iter()
            .find(|plan| plan["slug"] == "personal")
        {
            if personal["status"] != "active" {
                bail!(
                    "personal plan is not active; review it with papertiger --db {:?} plan list --json before restoring it with plan edit personal --status active",
                    path
                );
            }
        } else if previously_installed {
            bail!(
                "expected personal plan is missing; restore the authority backup at {:?} before setup-user",
                path
            );
        }
    }
    Ok(())
}

fn provision_authority(home: &Path) -> Result<()> {
    if !authority(home)?.exists() {
        authority_command(home, &["init"])?;
    }
    let plans: serde_json::Value =
        serde_json::from_slice(&authority_command(home, &["plan", "list", "--json"])?)?;
    if !plans["plans"]
        .as_array()
        .context("plan list has no plans array")?
        .iter()
        .any(|plan| plan["slug"] == "personal")
    {
        authority_command(
            home,
            &[
                "plan",
                "add",
                "personal",
                "Personal work",
                "--intent",
                "Private durable work across consuming projects; task intent records the owning project root.",
            ],
        )?;
    }
    Ok(())
}

/// Resolve the location that distinguishes a managed personal executable.
fn personal_home() -> Result<Option<PathBuf>> {
    let exe = std::env::current_exe()?;
    let Some(root) = exe.parent().and_then(Path::parent) else {
        return Ok(None);
    };
    if root.file_name().and_then(|s| s.to_str()) != Some(TOOL)
        || root
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            != Some("share")
        || root
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            != Some(".local")
    {
        return Ok(None);
    }
    let home = root
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("personal runtime has no home; run setup-user from an external release")?;
    Ok(Some(home.to_path_buf()))
}

/// Only the installed personal executable has a private fallback.
pub(crate) fn fallback_authority() -> Result<Option<PathBuf>> {
    personal_home()?.map(|home| authority(&home)).transpose()
}

/// Refuse a torn or divergent installation before doing work.
pub(crate) fn verify_runtime() -> Result<()> {
    let Some(home) = personal_home()? else {
        return Ok(());
    };
    let root = home.join(personal_root());
    let path = validate_path(&home, &format!("{}/user-install.json", personal_root()))?;
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "personal receipt is missing; run {TOOL} setup-user from a verified external release"
        )
    })?;
    let receipt: Receipt = serde_json::from_slice(&bytes).context(
        "invalid personal receipt; restore it or repair with setup-user from a verified release",
    )?;
    let expected_root = receipt.home.join(personal_root());
    if !receipt.installed
        || receipt.schema != format!("{TOOL}.user_install.v1")
        || receipt.version != env!("CARGO_PKG_VERSION")
        || fs::canonicalize(expected_root)? != fs::canonicalize(&root)?
        || receipt.files.len() != paths().len()
        || receipt.files.keys().any(|p| !paths().contains(p))
    {
        bail!(
            "personal installation identity differs; run {TOOL} setup-user from a verified external release"
        );
    }
    for (relative, hash) in &receipt.files {
        let file = validate_path(&receipt.home, relative)?;
        if sha256(
            &fs::read(&file)
                .with_context(|| format!("missing {}; repair with setup-user", file.display()))?,
        ) != *hash
        {
            bail!(
                "personal file {} differs from its receipt; review it, then repair with setup-user --replace-managed",
                file.display()
            );
        }
    }
    Ok(())
}
