//! Installing a release as exact, hash-verified copies: the skill directory
//! into each agent skills directory, and the executables into one directory
//! on PATH for harnesses that load no skills. The release's
//! `papertiger-release.json` lists the SHA-256 of every file; nothing is ever
//! linked. Each copy is staged and verified beside its target, then the old
//! copy is renamed aside and the new one renamed into place: Windows refuses
//! to delete a running executable but lets it be renamed, so a busy target is
//! never left half replaced. A retired copy still held by a running process
//! stays in the target's `.papertiger-install` directory until a later
//! install removes it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

/// The release manifest beside SKILL.md.
pub(crate) const MANIFEST: &str = "papertiger-release.json";
const MANIFEST_SCHEMA: &str = "papertiger.release.v1";
/// The skill directory's name inside an agent skills directory.
const SKILL: &str = "papertiger";
/// Staged and retired copies, inside each target's parent directory.
const WORK: &str = ".papertiger-install";
const BINARIES: [&str; 2] = ["papertiger", "papertiger-mise"];
/// Differences listed per target before the rest are counted.
const SHOWN_DIFFERENCES: usize = 6;

#[derive(clap::Args)]
pub(crate) struct InstallArgs {
    /// Only report each target's version and divergence from this release; fails when any target differs
    #[arg(long)]
    check: bool,
    /// Agent skills directory to hold a papertiger/ copy; repeat for each. With --skills-dir or --bin-dir only the named targets are used (default: ~/.claude/skills, ~/.agents/skills and ~/.local/bin)
    #[arg(long = "skills-dir", value_name = "DIR")]
    skills_dirs: Vec<PathBuf>,
    /// Directory on PATH to hold the executables for harnesses that load no skills (default: ~/.local/bin)
    #[arg(long = "bin-dir", value_name = "DIR")]
    bin_dir: Option<PathBuf>,
    /// Replace a target that is a symbolic link or junction with a copy instead of refusing; the link's destination is left untouched
    #[arg(long)]
    replace_links: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    version: String,
    files: BTreeMap<String, String>,
}

/// The verified release this executable belongs to.
struct Release {
    dir: PathBuf,
    version: String,
    manifest: Vec<u8>,
    files: BTreeMap<String, String>,
}

enum Kind {
    Skill,
    Bin,
}

struct Target {
    kind: Kind,
    /// The skill directory itself, or the PATH directory.
    path: PathBuf,
}

enum State {
    Missing,
    Link,
    Current,
    Differs(Vec<String>),
}

pub(crate) fn run(args: &InstallArgs) -> Result<()> {
    let release = Release::of_running_executable()?;
    let targets = targets(args)?;
    papertiger::outln!(
        "papertiger {} release at {}",
        release.version,
        shown(&release.dir)
    );
    if args.check {
        return check(&release, &targets);
    }
    let mut states = Vec::new();
    let mut links = Vec::new();
    for target in &targets {
        let state = release.inspect(target)?;
        if matches!(state, State::Link) && !args.replace_links {
            links.push(shown(&target.path));
        }
        states.push(state);
    }
    if !links.is_empty() {
        bail!(
            "refusing to install over {}: a link would make two targets one copy; rerun with --replace-links to replace each link with an exact copy (its destination is left untouched); nothing was changed",
            links.join(", ")
        );
    }
    for (target, state) in targets.iter().zip(states) {
        let outcome = if same_path(&target.path, &release.dir) {
            "is this release's own directory; left as it is".to_owned()
        } else {
            match target.kind {
                Kind::Skill => release.install_skill(&target.path, &state)?,
                Kind::Bin => release.install_bin(&target.path, &state)?,
            }
        };
        papertiger::outln!("{} {}", label(target), outcome);
        if matches!(target.kind, Kind::Bin) && !on_path(&target.path) {
            papertiger::outln!(
                "note: {} is not on PATH, so harnesses that load no skills will not find papertiger there",
                shown(&target.path)
            );
        }
    }
    Ok(())
}

fn check(release: &Release, targets: &[Target]) -> Result<()> {
    let mut differing = 0;
    for target in targets {
        let state = release.inspect(target)?;
        let report = match &state {
            State::Current => format!("current ({})", release.version),
            State::Missing => "missing".to_owned(),
            State::Link => format!(
                "a link, not a copy (version {}); install --replace-links replaces it",
                installed_version(target)
            ),
            State::Differs(differences) => format!(
                "differs (version {}): {}",
                installed_version(target),
                summarize(differences)
            ),
        };
        if !matches!(state, State::Current) {
            differing += 1;
        }
        papertiger::outln!("{} {report}", label(target));
    }
    if differing > 0 {
        bail!(
            "{differing} of {} targets differ from this release; run `{} install` with the same target options to replace them with verified copies",
            targets.len(),
            shown(&release.dir.join("bin").join(executable("papertiger")))
        );
    }
    Ok(())
}

impl Release {
    /// The release skill directory containing the running executable, after
    /// verifying every file against its manifest.
    fn of_running_executable() -> Result<Self> {
        let exe = std::env::current_exe().context("locate the running papertiger executable")?;
        let exe = fs::canonicalize(&exe).with_context(|| format!("resolve {}", exe.display()))?;
        let dir = exe
            .parent()
            .filter(|bin| bin.file_name().is_some_and(|name| name == "bin"))
            .and_then(Path::parent)
            .filter(|dir| dir.join(MANIFEST).is_file())
            .ok_or_else(|| {
                anyhow!(
                    "install copies the release skill directory it runs from, but {} is not inside one (no bin/ beside a {MANIFEST}); run bin/{} from an extracted release or a packaged candidate",
                    shown(&exe),
                    executable("papertiger")
                )
            })?
            .to_path_buf();
        let manifest_path = dir.join(MANIFEST);
        let manifest = fs::read(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let parsed: Manifest = serde_json::from_slice(&manifest).with_context(|| {
            format!(
                "{} is not a {MANIFEST_SCHEMA} manifest; repackage the release",
                shown(&manifest_path)
            )
        })?;
        if parsed.schema != MANIFEST_SCHEMA {
            bail!(
                "{} has schema {}, not {MANIFEST_SCHEMA}; install with the executable of the release that wrote it",
                shown(&manifest_path),
                parsed.schema
            );
        }
        let running = env!("CARGO_PKG_VERSION");
        if parsed.version != running {
            bail!(
                "{} describes version {} but this executable is {running}; repackage the release",
                shown(&manifest_path),
                parsed.version
            );
        }
        for (path, digest) in &parsed.files {
            papertiger::validate_sha256(digest, &format!("the digest of {path}"))?;
            let portable = Path::new(path)
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)));
            if !portable || path.contains('\\') || path == MANIFEST {
                bail!(
                    "{} lists {path:?}, which is not a plain relative path; repackage the release",
                    shown(&manifest_path)
                );
            }
        }
        for name in BINARIES {
            let path = format!("bin/{}", executable(name));
            if !parsed.files.contains_key(&path) {
                bail!(
                    "{} does not list {path}; repackage the release for this platform",
                    shown(&manifest_path)
                );
            }
        }
        let release = Self {
            dir,
            version: parsed.version,
            manifest,
            files: parsed.files,
        };
        if let State::Differs(differences) = release.inspect_skill(&release.dir)? {
            bail!(
                "the release at {} does not match its {MANIFEST}: {}; extract the release again and verify its checksum",
                shown(&release.dir),
                summarize(&differences)
            );
        }
        Ok(release)
    }

    fn inspect(&self, target: &Target) -> Result<State> {
        match target.kind {
            Kind::Skill => self.inspect_skill(&target.path),
            Kind::Bin => self.inspect_bin(&target.path),
        }
    }

    /// Compare a skill directory with the release, file by file.
    fn inspect_skill(&self, dir: &Path) -> Result<State> {
        match fs::symlink_metadata(dir) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(State::Missing);
            }
            Err(error) => return Err(error).with_context(|| format!("inspect {}", dir.display())),
            Ok(metadata) if metadata.file_type().is_symlink() || is_reparse_point(&metadata) => {
                return Ok(State::Link);
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Ok(State::Differs(vec!["not a directory".to_owned()]));
            }
            Ok(_) => {}
        }
        let mut present = BTreeMap::new();
        let mut differences = Vec::new();
        walk(dir, "", &mut present, &mut differences)?;
        for (path, digest) in &self.files {
            match present.remove(path) {
                None => differences.push(format!("missing {path}")),
                Some(file) if &sha256_file(&file)? != digest => {
                    differences.push(format!("changed {path}"));
                }
                Some(_) => {}
            }
        }
        match present.remove(MANIFEST) {
            None => differences.push(format!("missing {MANIFEST}")),
            Some(file) => {
                if fs::read(&file).with_context(|| format!("read {}", file.display()))?
                    != self.manifest
                {
                    differences.push(format!("changed {MANIFEST}"));
                }
            }
        }
        differences.extend(present.into_keys().map(|path| format!("extra {path}")));
        Ok(if differences.is_empty() {
            State::Current
        } else {
            State::Differs(differences)
        })
    }

    /// Compare the executables in a PATH directory with the release's.
    fn inspect_bin(&self, dir: &Path) -> Result<State> {
        let mut missing = 0;
        let mut differences = Vec::new();
        for name in BINARIES {
            let file = executable(name);
            let path = dir.join(&file);
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing += 1;
                    differences.push(format!("missing {file}"));
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("inspect {}", path.display()));
                }
                Ok(metadata)
                    if metadata.file_type().is_symlink() || is_reparse_point(&metadata) =>
                {
                    return Ok(State::Link);
                }
                Ok(metadata) if !metadata.is_file() => {
                    differences.push(format!("{file} is not a regular file"));
                }
                Ok(_) => {
                    if sha256_file(&path)? != self.files[&format!("bin/{file}")] {
                        differences.push(format!("changed {file}"));
                    }
                }
            }
        }
        Ok(if missing == BINARIES.len() {
            State::Missing
        } else if differences.is_empty() {
            State::Current
        } else {
            State::Differs(differences)
        })
    }

    fn install_skill(&self, target: &Path, state: &State) -> Result<String> {
        let parent = target
            .parent()
            .context("a skill directory target has a parent")?;
        let work = parent.join(WORK);
        let kept = sweep(&work)?;
        if matches!(state, State::Current) {
            return Ok(format!("already current{}", kept_note(&work, kept)));
        }
        fs::create_dir_all(&work).with_context(|| format!("create {}", work.display()))?;
        let stamp = stamp();
        let staging = work.join(format!("staging-{stamp}"));
        for path in self.files.keys() {
            let destination = staging.join(path);
            if let Some(directory) = destination.parent() {
                fs::create_dir_all(directory)
                    .with_context(|| format!("create {}", directory.display()))?;
            }
            fs::copy(self.dir.join(path), &destination)
                .with_context(|| format!("copy {path} to {}", destination.display()))?;
        }
        fs::write(staging.join(MANIFEST), &self.manifest)
            .with_context(|| format!("write {}", staging.join(MANIFEST).display()))?;
        if let State::Differs(differences) = self.inspect_skill(&staging)? {
            bail!(
                "the staged copy at {} does not match the release: {}; nothing was replaced; inspect the disk, then rerun install",
                shown(&staging),
                summarize(&differences)
            );
        }
        let retired = work.join(format!("retired-{stamp}"));
        let replaced = self.swap(target, &staging, &retired)?;
        if let State::Differs(differences) = self.inspect_skill(target)? {
            bail!(
                "{} does not match the release after installation: {}; the previous copy is at {}",
                shown(target),
                summarize(&differences),
                shown(&retired)
            );
        }
        let kept = kept + usize::from(replaced && !remove(&retired));
        let verb = if replaced { "replaced" } else { "installed" };
        Ok(format!(
            "{verb} with a verified copy{}",
            kept_note(&work, kept)
        ))
    }

    fn install_bin(&self, dir: &Path, state: &State) -> Result<String> {
        let work = dir.join(WORK);
        let mut kept = sweep(&work)?;
        if matches!(state, State::Current) {
            return Ok(format!("already current{}", kept_note(&work, kept)));
        }
        fs::create_dir_all(&work).with_context(|| format!("create {}", work.display()))?;
        let stamp = stamp();
        let mut replaced = false;
        for name in BINARIES {
            let file = executable(name);
            let target = dir.join(&file);
            let digest = &self.files[&format!("bin/{file}")];
            if target.is_file() && !target.is_symlink() && &sha256_file(&target)? == digest {
                continue;
            }
            let staged = work.join(format!("staging-{stamp}-{file}"));
            fs::copy(self.dir.join("bin").join(&file), &staged)
                .with_context(|| format!("copy {file} to {}", staged.display()))?;
            if &sha256_file(&staged)? != digest {
                bail!(
                    "the staged copy {} does not match the release; nothing was replaced; inspect the disk, then rerun install",
                    shown(&staged)
                );
            }
            let retired = work.join(format!("retired-{stamp}-{file}"));
            if self.swap(&target, &staged, &retired)? {
                replaced = true;
                kept += usize::from(!remove(&retired));
            }
            if &sha256_file(&target)? != digest {
                bail!(
                    "{} does not match the release after installation; the previous copy is in {}",
                    shown(&target),
                    shown(&work)
                );
            }
        }
        let verb = if replaced { "replaced" } else { "installed" };
        Ok(format!(
            "{verb} with verified copies{}",
            kept_note(&work, kept)
        ))
    }

    /// Rename any existing `target` to `retired`, then `staged` to `target`,
    /// restoring the previous copy when the second rename fails. Returns
    /// whether a previous copy was retired.
    fn swap(&self, target: &Path, staged: &Path, retired: &Path) -> Result<bool> {
        let existed = fs::symlink_metadata(target).is_ok();
        if existed {
            fs::rename(target, retired).with_context(|| {
                format!(
                    "rename {} aside to {}; nothing was replaced",
                    target.display(),
                    retired.display()
                )
            })?;
        }
        if let Err(error) = fs::rename(staged, target) {
            let restored = existed && fs::rename(retired, target).is_ok();
            return Err(error).with_context(|| {
                format!(
                    "move the verified copy {} into place at {}; {}",
                    staged.display(),
                    target.display(),
                    if !existed || restored {
                        "the previous state was kept".to_owned()
                    } else {
                        format!("the previous copy is at {}", retired.display())
                    }
                )
            });
        }
        Ok(existed)
    }
}

/// Collect every entry below `dir`: regular files by relative path, anything
/// else as a difference.
fn walk(
    dir: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, PathBuf>,
    differences: &mut Vec<String>,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry.with_context(|| format!("read {}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = format!("{prefix}{name}");
        let metadata = fs::symlink_metadata(entry.path())
            .with_context(|| format!("inspect {}", entry.path().display()))?;
        if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            differences.push(format!("link {relative}"));
        } else if metadata.is_dir() {
            walk(&entry.path(), &format!("{relative}/"), files, differences)?;
        } else if metadata.is_file() {
            files.insert(relative, entry.path());
        } else {
            differences.push(format!("not a regular file {relative}"));
        }
    }
    Ok(())
}

/// Remove what earlier installs left in `work`; returns how many entries a
/// running process still holds. An empty work directory is removed.
fn sweep(work: &Path) -> Result<usize> {
    let entries = match fs::read_dir(work) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("read {}", work.display())),
    };
    let mut kept = 0;
    for entry in entries {
        let entry = entry.with_context(|| format!("read {}", work.display()))?;
        kept += usize::from(!remove(&entry.path()));
    }
    if kept == 0 {
        fs::remove_dir(work).with_context(|| format!("remove empty {}", work.display()))?;
    }
    Ok(kept)
}

/// Remove a retired copy; returns false when a running process holds part of
/// it. Everything removable goes, so a held copy keeps no SKILL.md for a
/// harness to load; what remains is reported and swept by a later install.
fn remove(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return true;
    };
    if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return fs::remove_file(path).is_ok() || fs::remove_dir(path).is_ok();
    }
    if !metadata.is_dir() {
        return fs::remove_file(path).is_ok();
    }
    if fs::remove_dir_all(path).is_ok() {
        return true;
    }
    let mut emptied = true;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            emptied &= remove(&entry.path());
        }
    }
    emptied && fs::remove_dir(path).is_ok()
}

fn kept_note(work: &Path, kept: usize) -> String {
    if kept == 0 {
        String::new()
    } else {
        format!(
            "; {kept} retired cop{} in {} still held by a running process; a later install removes {}",
            if kept == 1 { "y" } else { "ies" },
            shown(work),
            if kept == 1 { "it" } else { "them" }
        )
    }
}

fn targets(args: &InstallArgs) -> Result<Vec<Target>> {
    let (skills_dirs, bin_dir) = if args.skills_dirs.is_empty() && args.bin_dir.is_none() {
        let home = home()?;
        (
            vec![home.join(".claude/skills"), home.join(".agents/skills")],
            Some(home.join(".local/bin")),
        )
    } else {
        (args.skills_dirs.clone(), args.bin_dir.clone())
    };
    let mut targets: Vec<Target> = skills_dirs
        .into_iter()
        .map(|dir| Target {
            kind: Kind::Skill,
            path: absolute(&dir.join(SKILL)),
        })
        .collect();
    targets.extend(bin_dir.map(|dir| Target {
        kind: Kind::Bin,
        path: absolute(&dir),
    }));
    for (index, target) in targets.iter().enumerate() {
        if targets[..index]
            .iter()
            .any(|earlier| same_path(&earlier.path, &target.path))
        {
            bail!(
                "{} is named twice; name each target directory once",
                shown(&target.path)
            );
        }
    }
    Ok(targets)
}

fn home() -> Result<PathBuf> {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    match std::env::var_os(variable) {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home)),
        _ => bail!(
            "{variable} is not set, so the default install targets are unknown; name them with --skills-dir and --bin-dir"
        ),
    }
}

/// The version a target's executable reports, or `unknown`.
fn installed_version(target: &Target) -> String {
    let exe = match target.kind {
        Kind::Skill => target.path.join("bin").join(executable("papertiger")),
        Kind::Bin => target.path.join(executable("papertiger")),
    };
    std::process::Command::new(&exe)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            String::from_utf8(output.stdout)
                .ok()?
                .trim()
                .strip_prefix("papertiger ")
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn summarize(differences: &[String]) -> String {
    let mut text = differences
        .iter()
        .take(SHOWN_DIFFERENCES)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if differences.len() > SHOWN_DIFFERENCES {
        text.push_str(&format!(
            " and {} more",
            differences.len() - SHOWN_DIFFERENCES
        ));
    }
    text
}

fn label(target: &Target) -> String {
    let kind = match target.kind {
        Kind::Skill => "skill",
        Kind::Bin => "PATH executables",
    };
    format!("{kind} {}:", shown(&target.path))
}

fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    papertiger::sha256_reader(&mut file).with_context(|| format!("hash {}", path.display()))
}

fn stamp() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("{nanos}-{}", std::process::id())
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => absolute(left) == absolute(right),
    }
}

fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .any(|entry| !entry.as_os_str().is_empty() && same_path(&entry, dir))
    })
}

fn shown(path: &Path) -> String {
    papertiger::portable_absolute(path).unwrap_or_else(|_| path.display().to_string())
}

/// A Windows junction is a reparse point that Rust's file type does not call
/// a symlink; it is a link all the same.
#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &fs::Metadata) -> bool {
    false
}
