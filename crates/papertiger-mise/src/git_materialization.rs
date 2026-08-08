use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

use crate::candidate::{
    CandidateMaterial, GitChange, GitChangeOperation, GitFileContent, GitFileIdentity, GitFileMode,
};
use crate::digest::sha256;
use crate::lifecycle::{MaterializationReceipt, MaterializationRecord, artifact_object, candidate};
use crate::manifest::CampaignManifest;
use crate::object::read_object;
use crate::path_identity::{canonical_or_pending_absolute, portable_absolute};

pub(crate) fn git_text(repository: &Path, arguments: &[&str]) -> Result<String> {
    let repository = portable_absolute(repository)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&repository)
        .args(arguments)
        .output()
        .context("launch Git")?;
    if !output.status.success() {
        bail!(
            "Git command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("Git output is not UTF-8")
}

pub(crate) fn git_run(
    repository: &Path,
    arguments: &[&str],
    path_argument: Option<&Path>,
    trailing_argument: Option<&str>,
    stdin_bytes: Option<&[u8]>,
) -> Result<()> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(portable_absolute(repository)?)
        .args(arguments);
    if let Some(path) = path_argument {
        command.arg(portable_absolute(path)?);
    }
    if let Some(argument) = trailing_argument {
        command.arg(argument);
    }
    if stdin_bytes.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().context("launch Git mutation")?;
    if let Some(bytes) = stdin_bytes {
        child
            .stdin
            .take()
            .context("Git mutation has no stdin")?
            .write_all(bytes)?;
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("Git mutation failed with status {status}");
    }
    Ok(())
}

fn git_output_with_stdin(
    repository: &Path,
    arguments: &[&str],
    stdin_bytes: &[u8],
) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(portable_absolute(repository)?)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("launch Git content operation")?;
    child
        .stdin
        .take()
        .context("Git content operation has no stdin")?
        .write_all(stdin_bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "Git content operation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn git_output_bytes(repository: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(portable_absolute(repository)?)
        .args(arguments)
        .output()
        .context("launch Git read")?;
    if !output.status.success() {
        bail!(
            "Git read failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

pub(crate) fn git_worktree_add_without_hooks(
    repository: &Path,
    worktree: &Path,
    base_commit: &str,
) -> Result<()> {
    let controls = tempfile::tempdir().context("create Git checkout controls")?;
    let hooks = controls.path().join("empty-hooks");
    let attributes = controls.path().join("empty-attributes");
    std::fs::create_dir(&hooks)?;
    std::fs::write(&attributes, [])?;
    let repository = portable_absolute(repository)?;
    let worktree = portable_absolute(worktree)?;
    let hooks = portable_absolute(&hooks)?;
    let attributes = portable_absolute(&attributes)?;
    let status = Command::new("git")
        .arg("-C")
        .arg(&repository)
        .arg("-c")
        .arg(format!("core.hooksPath={hooks}"))
        .arg("-c")
        .arg(format!("core.attributesFile={attributes}"))
        .env("GIT_ATTR_NOSYSTEM", "1")
        .args(["worktree", "add", "--detach"])
        .arg(&worktree)
        .arg(base_commit)
        .status()
        .context("launch controlled Git worktree creation")?;
    if !status.success() {
        bail!("controlled Git worktree creation failed with status {status}");
    }
    Ok(())
}

#[derive(Clone)]
struct TreeFile {
    mode: GitFileMode,
    bytes: Vec<u8>,
    sha256: String,
}

/// Construct the one writable candidate-material format from two exact trees
/// in the same repository. The result is canonical compact JSON.
pub fn build_git_change_set_material(
    repository: &Path,
    base_tree: &str,
    result_tree: &str,
) -> Result<Vec<u8>> {
    let repository = std::fs::canonicalize(repository).context("canonicalize Git repository")?;
    let base = list_tree_files(&repository, base_tree)?;
    let result = if result_tree == base_tree {
        base.clone()
    } else {
        list_tree_files(&repository, result_tree)?
    };
    let paths = base
        .keys()
        .chain(result.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    for path in paths {
        let old = base.get(&path);
        let new = result.get(&path);
        let change = match (old, new) {
            (None, Some(new)) => Some(GitChange {
                operation: GitChangeOperation::Add,
                path,
                old: None,
                new: Some(GitFileContent::from_bytes(new.mode, &new.bytes)),
            }),
            (Some(old), None) => Some(GitChange {
                operation: GitChangeOperation::Delete,
                path,
                old: Some(GitFileIdentity {
                    mode: old.mode,
                    sha256: old.sha256.clone(),
                }),
                new: None,
            }),
            (Some(old), Some(new)) if old.sha256 != new.sha256 || old.mode != new.mode => {
                if old.mode != new.mode {
                    bail!(
                        "Git path '{path}' changes executable mode; candidate material admits content changes only"
                    );
                }
                Some(GitChange {
                    operation: GitChangeOperation::Modify,
                    path,
                    old: Some(GitFileIdentity {
                        mode: old.mode,
                        sha256: old.sha256.clone(),
                    }),
                    new: Some(GitFileContent::from_bytes(new.mode, &new.bytes)),
                })
            }
            _ => None,
        };
        if let Some(change) = change {
            changes.push(change);
        }
    }
    reject_copy_or_rename_content(&base, &changes)?;
    CandidateMaterial::from_changes(changes)?.canonical_bytes()
}

fn list_tree_files(repository: &Path, tree: &str) -> Result<BTreeMap<String, TreeFile>> {
    let resolved = git_text(
        repository,
        &["rev-parse", "--verify", &format!("{tree}^{{tree}}")],
    )?;
    if resolved.trim() != tree {
        bail!("Git tree identity '{tree}' is not an exact full tree identity");
    }
    let listing = git_output_bytes(repository, &["ls-tree", "-rz", "--full-tree", tree])?;
    let mut entries = Vec::new();
    for record in listing
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("Git tree record has no path separator")?;
        let metadata = &record[..separator];
        let raw_path = &record[separator + 1..];
        let metadata = std::str::from_utf8(metadata).context("Git tree metadata is not UTF-8")?;
        let fields = metadata.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[1] != "blob" {
            bail!("Git tree contains a non-blob candidate material entry");
        }
        let mode = match fields[0] {
            "100644" => GitFileMode::Regular,
            "100755" => GitFileMode::Executable,
            other => bail!("Git tree contains forbidden candidate material mode {other}"),
        };
        let path = std::str::from_utf8(raw_path)
            .context("Git tree path is not UTF-8")?
            .to_owned();
        crate::candidate::validate_portable_relative_path(&path)?;
        entries.push((path, mode, fields[2].to_owned()));
    }
    let object_ids = entries
        .iter()
        .map(|(_, _, object_id)| object_id.clone())
        .collect::<Vec<_>>();
    let blobs = read_blob_batch(repository, &object_ids)?;
    let mut files = BTreeMap::new();
    for ((path, mode, _), bytes) in entries.into_iter().zip(blobs) {
        let sha256 = sha256(&bytes);
        if files
            .insert(
                path,
                TreeFile {
                    mode,
                    bytes,
                    sha256,
                },
            )
            .is_some()
        {
            bail!("Git tree contains a duplicate path");
        }
    }
    Ok(files)
}

fn read_blob_batch(repository: &Path, object_ids: &[String]) -> Result<Vec<Vec<u8>>> {
    if object_ids.is_empty() {
        return Ok(Vec::new());
    }
    #[cfg(test)]
    CAT_FILE_BATCH_INVOCATIONS.with(|count| count.set(count.get() + 1));
    let mut child = Command::new("git")
        .arg("-C")
        .arg(portable_absolute(repository)?)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("launch batched Git object read")?;
    let stderr = child
        .stderr
        .take()
        .context("batched Git object read has no stderr")?;
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        BufReader::new(stderr).read_to_end(&mut bytes)?;
        std::io::Result::Ok(bytes)
    });
    let mut stdin = BufWriter::new(
        child
            .stdin
            .take()
            .context("batched Git object read has no stdin")?,
    );
    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .context("batched Git object read has no stdout")?,
    );
    let read_result = (|| -> Result<Vec<Vec<u8>>> {
        let mut blobs = Vec::with_capacity(object_ids.len());
        for expected_id in object_ids {
            writeln!(stdin, "{expected_id}")?;
            stdin.flush()?;
            let mut header = String::new();
            if stdout.read_line(&mut header)? == 0 {
                bail!("batched Git object read ended before object '{expected_id}'");
            }
            let fields = header
                .trim_end_matches(['\r', '\n'])
                .split_whitespace()
                .collect::<Vec<_>>();
            if fields.len() != 3 || fields[0] != expected_id || fields[1] != "blob" {
                bail!("batched Git object read returned an unexpected header for '{expected_id}'");
            }
            let size = fields[2]
                .parse::<u64>()
                .context("batched Git object size is not an integer")?;
            let mut bytes = vec![0; usize::try_from(size).context("Git blob is too large")?];
            stdout.read_exact(&mut bytes)?;
            let mut delimiter = [0_u8; 1];
            stdout.read_exact(&mut delimiter)?;
            if delimiter != *b"\n" {
                bail!("batched Git object '{expected_id}' has no exact record delimiter");
            }
            blobs.push(bytes);
        }
        drop(stdin);
        let mut trailing = Vec::new();
        stdout.read_to_end(&mut trailing)?;
        if !trailing.is_empty() {
            bail!("batched Git object read emitted unexpected trailing bytes");
        }
        Ok(blobs)
    })();
    if read_result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("batched Git stderr reader panicked"))??;
    let blobs = read_result?;
    if !status.success() {
        bail!(
            "batched Git object read failed: {}",
            String::from_utf8_lossy(&stderr).trim()
        );
    }
    Ok(blobs)
}

#[cfg(test)]
thread_local! {
    static CAT_FILE_BATCH_INVOCATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn cat_file_batch_invocations() -> u64 {
    CAT_FILE_BATCH_INVOCATIONS.with(std::cell::Cell::get)
}

fn reject_copy_or_rename_content(
    base: &BTreeMap<String, TreeFile>,
    changes: &[GitChange],
) -> Result<()> {
    let mut new_content = BTreeMap::<String, String>::new();
    for change in changes {
        let Some(new) = &change.new else {
            continue;
        };
        if let Some((source, _)) = base
            .iter()
            .find(|(path, file)| path.as_str() != change.path && file.sha256 == new.sha256)
        {
            bail!(
                "Git change '{}' copies or renames exact content from '{source}'; record independently authored content instead",
                change.path
            );
        }
        if let Some(source) = new_content.insert(new.sha256.clone(), change.path.clone()) {
            bail!(
                "Git changes '{source}' and '{}' introduce duplicate content; copies are outside the v1 material contract",
                change.path
            );
        }
    }
    Ok(())
}

fn verify_git_change_set_against_base(
    repository: &Path,
    base_tree: &str,
    material: &CandidateMaterial,
) -> Result<()> {
    let base = list_tree_files(repository, base_tree)?;
    for change in &material.change_set.changes {
        let observed = base.get(&change.path);
        match change.operation {
            GitChangeOperation::Add if observed.is_some() => {
                bail!(
                    "Git add path '{}' already exists in the frozen base tree",
                    change.path
                )
            }
            GitChangeOperation::Modify | GitChangeOperation::Delete => {
                let observed = observed.with_context(|| {
                    format!(
                        "Git {} path '{}' is absent from the frozen base tree",
                        match change.operation {
                            GitChangeOperation::Modify => "modify",
                            _ => "delete",
                        },
                        change.path
                    )
                })?;
                let expected = change
                    .old
                    .as_ref()
                    .context("Git change omitted old state")?;
                if observed.mode != expected.mode || observed.sha256 != expected.sha256 {
                    bail!(
                        "Git change '{}' old state differs from the frozen base tree",
                        change.path
                    );
                }
            }
            GitChangeOperation::Add => {}
        }
    }
    reject_copy_or_rename_content(&base, &material.change_set.changes)
}

pub(crate) fn verify_materialized_worktree(
    manifest: &CampaignManifest,
    proposal: &crate::candidate::CandidateProposal,
    record: &MaterializationRecord,
    worktree: &Path,
) -> Result<()> {
    require_confined_worktree(manifest, worktree)?;
    if canonical_or_pending_absolute(worktree)? != record.worktree_locator {
        bail!("materialized worktree locator drifted");
    }
    if git_text(worktree, &["rev-parse", "HEAD^{commit}"])?.trim() != manifest.source.base_commit {
        bail!("materialized worktree base commit drifted");
    }
    if git_text(worktree, &["write-tree"])?.trim() != record.result_tree {
        bail!("materialized result tree differs from its immutable receipt");
    }
    verify_tree_has_only_regular_files(manifest, &record.result_tree)?;
    verify_worktree_has_no_link_escape(worktree)?;
    let process_worktree = portable_absolute(worktree)?;
    let unstaged = Command::new("git")
        .arg("-C")
        .arg(&process_worktree)
        .args(["diff", "--quiet", "--no-ext-diff"])
        .status()?;
    if !unstaged.success() {
        bail!("materialized worktree has unstaged drift");
    }
    if !git_text(worktree, &["ls-files", "--others"])?
        .trim()
        .is_empty()
    {
        bail!("materialized worktree has untracked drift");
    }
    let staged = git_text(worktree, &["diff", "--cached", "--name-only"])?
        .lines()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if staged != proposal.changed_paths {
        bail!("materialized staged paths differ from the candidate material scope");
    }
    let source_common = git_text(
        Path::new(&manifest.source.repository_locator),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let worktree_common = git_text(
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if std::fs::canonicalize(source_common.trim())?
        != std::fs::canonicalize(worktree_common.trim())?
    {
        bail!("materialized worktree is not owned by the frozen source repository");
    }
    Ok(())
}

fn verify_worktree_has_no_link_escape(worktree: &Path) -> Result<()> {
    let root = std::fs::canonicalize(worktree)?;
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                bail!("materialized worktree contains a symbolic-link escape surface");
            }
            let resolved = std::fs::canonicalize(&path)?;
            if !resolved.starts_with(&root) {
                bail!("materialized worktree entry resolves outside its frozen root");
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if !metadata.is_file() {
                bail!("materialized worktree contains a non-regular filesystem entry");
            }
        }
    }
    Ok(())
}

pub(crate) fn verify_materialization_receipt(
    connection: &Connection,
    object_root: &Path,
    manifest: &CampaignManifest,
    proposal: &crate::candidate::CandidateProposal,
    record: &MaterializationRecord,
) -> Result<()> {
    let object = artifact_object(connection, &record.receipt_sha256)?;
    let bytes = read_object(object_root, &object)?;
    let receipt: MaterializationReceipt = serde_json::from_slice(&bytes)?;
    let expected_schema = if manifest.candidate_material.is_some() {
        "papertiger-mise.materialization.v2"
    } else {
        "papertiger-mise.materialization.v1"
    };
    if serde_json::to_vec(&receipt)? != bytes
        || receipt.schema != expected_schema
        || receipt.campaign_id != manifest.campaign_id
        || receipt.candidate_id != record.candidate_id
        || receipt.base_commit != manifest.source.base_commit
        || receipt.base_tree != manifest.source.base_tree
        || receipt.result_tree != record.result_tree
        || receipt.worktree_locator != record.worktree_locator
        || receipt.adapter_sha256 != proposal.adapter_sha256
    {
        bail!("materialization receipt does not recompute its exact frozen identity");
    }
    verify_tree_has_only_regular_files(manifest, &receipt.result_tree)?;
    let candidate = candidate(connection, &record.candidate_id)?
        .context("materialization candidate disappeared")?;
    let material_matches = if manifest.candidate_material.is_some() {
        receipt.patch_sha256.is_none()
            && receipt.material_sha256.as_deref() == Some(candidate.material_sha256.as_str())
    } else {
        receipt.material_sha256.is_none()
            && receipt.patch_sha256.as_deref() == Some(candidate.material_sha256.as_str())
    };
    if !material_matches {
        bail!("materialization receipt differs from its candidate material identity");
    }
    let material_object = artifact_object(connection, &candidate.material_sha256)?;
    let material_bytes = read_object(object_root, &material_object)?;
    if expected_result_tree(manifest, &material_bytes)? != receipt.result_tree {
        bail!("materialization result tree is not the frozen base plus retained material");
    }
    Ok(())
}

pub(crate) fn require_confined_worktree(
    manifest: &CampaignManifest,
    worktree: &Path,
) -> Result<()> {
    let root = std::fs::canonicalize(&manifest.execution_limits.workspace_root_locator)
        .context("canonicalize frozen Mise workspace root")?;
    let candidate = if worktree.exists() {
        std::fs::canonicalize(worktree)?
    } else {
        let parent = std::fs::canonicalize(
            worktree
                .parent()
                .context("candidate worktree has no parent")?,
        )?;
        parent.join(
            worktree
                .file_name()
                .context("candidate worktree has no final component")?,
        )
    };
    if candidate == root || !candidate.starts_with(&root) {
        bail!("candidate worktree must be a child of the frozen Mise workspace root");
    }
    Ok(())
}

pub(crate) fn expected_result_tree(
    manifest: &CampaignManifest,
    material_bytes: &[u8],
) -> Result<String> {
    let scratch = tempfile::tempdir().context("create temporary Git index directory")?;
    let index = scratch.path().join("candidate.index");
    let object_directory = scratch.path().join("objects");
    std::fs::create_dir(&object_directory)?;
    let repository = Path::new(&manifest.source.repository_locator);
    let alternate = git_text(repository, &["rev-parse", "--git-path", "objects"])?;
    let alternate = PathBuf::from(alternate.trim());
    let alternate = if alternate.is_absolute() {
        alternate
    } else {
        repository.join(alternate)
    };
    let alternate = std::fs::canonicalize(alternate)?;
    git_with_index(
        repository,
        &index,
        &object_directory,
        &alternate,
        &["read-tree", &manifest.source.base_tree],
        None,
    )?;
    if manifest.candidate_material.is_some() {
        let material = CandidateMaterial::parse_canonical(material_bytes)?;
        verify_git_change_set_against_base(repository, &manifest.source.base_tree, &material)?;
        for change in &material.change_set.changes {
            match change.operation {
                GitChangeOperation::Delete => git_with_index(
                    repository,
                    &index,
                    &object_directory,
                    &alternate,
                    &["update-index", "--force-remove", "--", &change.path],
                    None,
                )?,
                GitChangeOperation::Add | GitChangeOperation::Modify => {
                    let new = change
                        .new
                        .as_ref()
                        .context("Git change omitted new state")?;
                    let bytes = new.bytes()?;
                    let object = String::from_utf8(git_with_index_output(
                        repository,
                        &index,
                        &object_directory,
                        &alternate,
                        &["hash-object", "-w", "--stdin"],
                        Some(&bytes),
                    )?)
                    .context("Git object identity is not UTF-8")?;
                    git_with_index(
                        repository,
                        &index,
                        &object_directory,
                        &alternate,
                        &[
                            "update-index",
                            "--add",
                            "--cacheinfo",
                            new.mode.as_str(),
                            object.trim(),
                            &change.path,
                        ],
                        None,
                    )?;
                }
            }
        }
    } else if !material_bytes.is_empty() {
        git_with_index(
            repository,
            &index,
            &object_directory,
            &alternate,
            &["apply", "--cached", "--whitespace=nowarn", "-"],
            Some(material_bytes),
        )?;
    }
    let tree = git_with_index_text(
        repository,
        &index,
        &object_directory,
        &alternate,
        &["write-tree"],
    )?
    .trim()
    .to_owned();
    verify_quarantined_result_tree(repository, &object_directory, &alternate, &tree)?;
    Ok(tree)
}

pub(crate) fn apply_candidate_material(
    manifest: &CampaignManifest,
    material_bytes: &[u8],
    worktree: &Path,
) -> Result<()> {
    if manifest.candidate_material.is_none() {
        if !material_bytes.is_empty() {
            git_run(
                worktree,
                &["apply", "--index", "--whitespace=nowarn", "-"],
                None,
                None,
                Some(material_bytes),
            )?;
        }
        return Ok(());
    }
    verify_worktree_has_no_link_escape(worktree)?;
    if !git_text(worktree, &["ls-files", "--others"])?
        .trim()
        .is_empty()
    {
        bail!("candidate worktree has untracked entries before material application");
    }
    let material = CandidateMaterial::parse_canonical(material_bytes)?;
    verify_git_change_set_against_base(
        Path::new(&manifest.source.repository_locator),
        &manifest.source.base_tree,
        &material,
    )?;
    for change in &material.change_set.changes {
        match change.operation {
            GitChangeOperation::Delete => {
                git_run(
                    worktree,
                    &["update-index", "--force-remove", "--"],
                    None,
                    Some(&change.path),
                    None,
                )?;
                let path = worktree.join(&change.path);
                let metadata = std::fs::symlink_metadata(&path).with_context(|| {
                    format!("inspect deleted candidate path {}", path.display())
                })?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    bail!("deleted candidate path is not a plain regular file");
                }
                std::fs::remove_file(&path)
                    .with_context(|| format!("delete candidate path {}", path.display()))?;
            }
            GitChangeOperation::Add | GitChangeOperation::Modify => {
                let new = change
                    .new
                    .as_ref()
                    .context("Git change omitted new state")?;
                let bytes = new.bytes()?;
                let object = String::from_utf8(git_output_with_stdin(
                    worktree,
                    &["hash-object", "-w", "--stdin"],
                    &bytes,
                )?)
                .context("Git object identity is not UTF-8")?;
                git_run(
                    worktree,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        new.mode.as_str(),
                        object.trim(),
                    ],
                    None,
                    Some(&change.path),
                    None,
                )?;
                git_run(
                    worktree,
                    &["checkout-index", "--force", "--"],
                    None,
                    Some(&change.path),
                    None,
                )?;
            }
        }
    }
    Ok(())
}

fn verify_quarantined_result_tree(
    repository: &Path,
    object_directory: &Path,
    alternate_object_directory: &Path,
    tree: &str,
) -> Result<()> {
    let run = |arguments: &[&str]| {
        git_with_object_output(
            repository,
            object_directory,
            alternate_object_directory,
            arguments,
        )
    };
    let listing = String::from_utf8(run(&["ls-tree", "-r", "--full-tree", tree])?)
        .context("quarantined-object Git output is not UTF-8")?;
    verify_listing_has_only_regular_modes(&listing)?;
    verify_tree_attribute_rules(&run, tree)
}

fn verify_listing_has_only_regular_modes(listing: &str) -> Result<()> {
    for line in listing.lines() {
        let mode = line
            .split_whitespace()
            .next()
            .context("Git tree entry has no mode")?;
        if mode != "100644" && mode != "100755" {
            bail!("materialized tree contains forbidden non-regular mode {mode}");
        }
    }
    Ok(())
}

fn verify_tree_attribute_rules(run: &dyn Fn(&[&str]) -> Result<Vec<u8>>, tree: &str) -> Result<()> {
    let paths = run(&["ls-tree", "-rz", "--name-only", "--full-tree", tree])?;
    for raw_path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(raw_path).context("Git attribute path is not UTF-8")?;
        if path.rsplit('/').next() != Some(".gitattributes") {
            continue;
        }
        let object = format!("{tree}:{path}");
        let body = run(&["cat-file", "blob", &object])
            .with_context(|| format!("read frozen attributes '{path}'"))?;
        reject_checkout_transform_rules(
            std::str::from_utf8(&body)
                .with_context(|| format!("frozen attributes '{path}' are not UTF-8"))?,
            path,
        )?;
    }
    Ok(())
}

pub(crate) fn projected_materialization_disk_bytes(
    manifest: &CampaignManifest,
    material_bytes: &[u8],
) -> Result<u64> {
    let listing = git_text(
        Path::new(&manifest.source.repository_locator),
        &[
            "ls-tree",
            "-r",
            "-l",
            "--full-tree",
            &manifest.source.base_tree,
        ],
    )?;
    let mut file_bytes = 0_u64;
    let mut entries = 0_u64;
    for line in listing.lines() {
        let metadata = line
            .split_once('\t')
            .map(|(metadata, _)| metadata)
            .context("Git tree listing has no path separator")?;
        let fields = metadata.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4 {
            bail!("Git tree listing has an unexpected shape");
        }
        if fields[0] != "100644" && fields[0] != "100755" {
            bail!(
                "campaign source tree contains a non-regular entry mode {}",
                fields[0]
            );
        }
        file_bytes = file_bytes
            .checked_add(fields[3].parse::<u64>().context("invalid Git blob size")?)
            .context("materialization source size overflow")?;
        entries = entries
            .checked_add(1)
            .context("tree entry count overflow")?;
    }
    // Checkout writes the base payload, applying candidate material can rewrite
    // affected payload once, and Git/worktree metadata is conservatively
    // charged at 4 KiB per entry plus a fixed index/receipt allowance.
    file_bytes
        .checked_mul(2)
        .and_then(|value| {
            value.checked_add(u64::try_from(material_bytes.len()).ok()?.checked_mul(2)?)
        })
        .and_then(|value| value.checked_add(entries.checked_mul(4_096)?))
        .and_then(|value| value.checked_add(65_536))
        .context("materialization logical-write bound overflow")
}

pub(crate) fn verify_tree_has_only_regular_files(
    manifest: &CampaignManifest,
    result_tree: &str,
) -> Result<()> {
    let listing = git_text(
        Path::new(&manifest.source.repository_locator),
        &["ls-tree", "-r", "--full-tree", result_tree],
    )?;
    verify_listing_has_only_regular_modes(&listing)
}

pub(crate) fn verify_no_checkout_transforms(manifest: &CampaignManifest, tree: &str) -> Result<()> {
    let repository = Path::new(&manifest.source.repository_locator);
    let run = |arguments: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(portable_absolute(repository)?)
            .args(arguments)
            .output()
            .context("launch Git attribute inspection")?;
        if !output.status.success() {
            bail!(
                "Git attribute inspection failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
    };
    verify_tree_attribute_rules(&run, tree)
}

pub(crate) fn reject_checkout_transform_rules(body: &str, locator: &str) -> Result<()> {
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        for token in line.split_ascii_whitespace().skip(1) {
            let name = token
                .trim_start_matches(['-', '!'])
                .split_once('=')
                .map_or(token.trim_start_matches(['-', '!']), |(name, _)| name);
            if matches!(name, "filter" | "working-tree-encoding" | "ident") {
                bail!("Git checkout transform attribute '{name}' is forbidden in {locator}");
            }
        }
    }
    Ok(())
}

pub(crate) fn verify_no_repository_info_attributes(manifest: &CampaignManifest) -> Result<()> {
    let repository = Path::new(&manifest.source.repository_locator);
    let locator = git_text(repository, &["rev-parse", "--git-path", "info/attributes"])?;
    let locator = PathBuf::from(locator.trim());
    let locator = if locator.is_absolute() {
        locator
    } else {
        repository.join(locator)
    };
    if !locator.exists() {
        return Ok(());
    }
    let body = std::fs::read_to_string(&locator)
        .with_context(|| format!("read repository attributes {}", locator.display()))?;
    if body
        .lines()
        .map(str::trim)
        .any(|line| !line.is_empty() && !line.starts_with('#'))
    {
        bail!("repository-local info/attributes must be empty for bounded materialization");
    }
    Ok(())
}

fn git_with_index(
    repository: &Path,
    index: &Path,
    object_directory: &Path,
    alternate_object_directory: &Path,
    arguments: &[&str],
    stdin_bytes: Option<&[u8]>,
) -> Result<()> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(portable_absolute(repository)?)
        .env("GIT_INDEX_FILE", portable_absolute(index)?)
        .env("GIT_OBJECT_DIRECTORY", portable_absolute(object_directory)?)
        .env(
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            portable_absolute(alternate_object_directory)?,
        )
        .args(arguments);
    if stdin_bytes.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().context("launch Git with isolated index")?;
    if let Some(bytes) = stdin_bytes {
        child
            .stdin
            .take()
            .context("isolated-index Git has no stdin")?
            .write_all(bytes)?;
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("isolated-index Git failed with status {status}");
    }
    Ok(())
}

fn git_with_index_text(
    repository: &Path,
    index: &Path,
    object_directory: &Path,
    alternate_object_directory: &Path,
    arguments: &[&str],
) -> Result<String> {
    String::from_utf8(git_with_index_output(
        repository,
        index,
        object_directory,
        alternate_object_directory,
        arguments,
        None,
    )?)
    .context("isolated-index Git output is not UTF-8")
}

fn git_with_index_output(
    repository: &Path,
    index: &Path,
    object_directory: &Path,
    alternate_object_directory: &Path,
    arguments: &[&str],
    stdin_bytes: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(portable_absolute(repository)?)
        .env("GIT_INDEX_FILE", portable_absolute(index)?)
        .env("GIT_OBJECT_DIRECTORY", portable_absolute(object_directory)?)
        .env(
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            portable_absolute(alternate_object_directory)?,
        )
        .args(arguments);
    if stdin_bytes.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(bytes) = stdin_bytes {
        child
            .stdin
            .take()
            .context("isolated-index Git has no stdin")?
            .write_all(bytes)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "isolated-index Git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn git_with_object_output(
    repository: &Path,
    object_directory: &Path,
    alternate_object_directory: &Path,
    arguments: &[&str],
) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(portable_absolute(repository)?)
        .env("GIT_OBJECT_DIRECTORY", portable_absolute(object_directory)?)
        .env(
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            portable_absolute(alternate_object_directory)?,
        )
        .args(arguments)
        .output()?;
    if !output.status.success() {
        bail!(
            "quarantined-object Git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{CandidateMaterial, GitChangeOperation};
    use crate::manifest::CandidateMaterialContract;

    #[test]
    fn typed_material_builds_and_applies_add_modify_delete_exactly() {
        let fixture = GitMaterialFixture::new();
        let result = fixture.worktree("result");
        std::fs::write(result.join("modify.txt"), b"changed\n").expect("modify file");
        std::fs::remove_file(result.join("delete.txt")).expect("delete file");
        std::fs::create_dir(result.join("new")).expect("new directory");
        std::fs::write(result.join("new/add.txt"), b"independently authored\n").expect("add file");
        git_run(&result, &["add", "--all"], None, None, None).expect("stage result");
        let result_tree = git_text(&result, &["write-tree"])
            .expect("result tree")
            .trim()
            .to_owned();
        let batches_before = cat_file_batch_invocations();
        let bytes =
            build_git_change_set_material(&fixture.repository, &fixture.base_tree, &result_tree)
                .expect("build material");
        assert_eq!(cat_file_batch_invocations() - batches_before, 2);
        let material = CandidateMaterial::parse_canonical(&bytes).expect("canonical material");
        assert_eq!(
            material.scope.operations,
            BTreeSet::from([
                GitChangeOperation::Add,
                GitChangeOperation::Modify,
                GitChangeOperation::Delete,
            ])
        );
        assert_eq!(material.scope.changed_paths.len(), 3);

        let manifest = fixture.manifest();
        assert_eq!(
            expected_result_tree(&manifest, &bytes).expect("expected result tree"),
            result_tree
        );
        let applied = fixture.worktree("applied");
        apply_candidate_material(&manifest, &bytes, &applied).expect("apply material");
        assert_eq!(
            git_text(&applied, &["write-tree"])
                .expect("applied tree")
                .trim(),
            result_tree
        );
        assert_eq!(
            std::fs::read(applied.join("modify.txt")).unwrap(),
            b"changed\n"
        );
        assert_eq!(
            std::fs::read(applied.join("new/add.txt")).unwrap(),
            b"independently authored\n"
        );
        assert!(!applied.join("delete.txt").exists());
    }

    #[test]
    fn typed_material_batches_thousands_of_exact_git_objects() {
        let fixture = GitMaterialFixture::new();
        let bulk = fixture.repository.join("bulk");
        std::fs::create_dir(&bulk).expect("bulk directory");
        for index in 0..2_048 {
            std::fs::write(
                bulk.join(format!("file-{index:04}.txt")),
                format!("independent fixture {index}\n"),
            )
            .expect("bulk fixture");
        }
        git_run(&fixture.repository, &["add", "--all"], None, None, None)
            .expect("stage bulk fixture");
        git_run(
            &fixture.repository,
            &["commit", "--quiet", "-m", "large exact tree"],
            None,
            None,
            None,
        )
        .expect("commit bulk fixture");
        let base_commit = git_text(&fixture.repository, &["rev-parse", "HEAD^{commit}"])
            .expect("large base commit")
            .trim()
            .to_owned();
        let base_tree = git_text(&fixture.repository, &["rev-parse", "HEAD^{tree}"])
            .expect("large base tree")
            .trim()
            .to_owned();
        let result = fixture.workspace.join("large-result");
        git_worktree_add_without_hooks(&fixture.repository, &result, &base_commit)
            .expect("large result worktree");
        std::fs::write(result.join("bulk/file-1024.txt"), b"changed exactly once\n")
            .expect("modify one bulk file");
        git_run(&result, &["add", "--all"], None, None, None).expect("stage bulk result");
        let result_tree = git_text(&result, &["write-tree"])
            .expect("large result tree")
            .trim()
            .to_owned();

        let batches_before = cat_file_batch_invocations();
        let material = build_git_change_set_material(&fixture.repository, &base_tree, &result_tree)
            .expect("build large material");
        assert_eq!(cat_file_batch_invocations() - batches_before, 2);
        let material = CandidateMaterial::parse_canonical(&material).expect("large material");
        assert_eq!(material.change_set.changes.len(), 1);
        assert_eq!(material.change_set.changes[0].path, "bulk/file-1024.txt");
    }

    #[test]
    fn typed_material_refuses_copies_nonregular_entries_and_stale_old_state() {
        let fixture = GitMaterialFixture::new();

        let copied = fixture.worktree("copied");
        std::fs::write(copied.join("copy.txt"), b"old\n").expect("copied content");
        git_run(&copied, &["add", "--all"], None, None, None).expect("stage copy");
        let copied_tree = git_text(&copied, &["write-tree"]).expect("copy tree");
        let error = build_git_change_set_material(
            &fixture.repository,
            &fixture.base_tree,
            copied_tree.trim(),
        )
        .expect_err("copy content must refuse");
        assert!(error.to_string().contains("copies or renames"), "{error:#}");

        let linked = fixture.worktree("linked");
        let object = String::from_utf8(
            git_output_with_stdin(&linked, &["hash-object", "-w", "--stdin"], b"target")
                .expect("link blob"),
        )
        .expect("object UTF-8");
        git_run(
            &linked,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "120000",
                object.trim(),
            ],
            None,
            Some("link"),
            None,
        )
        .expect("stage link mode");
        let linked_tree = git_text(&linked, &["write-tree"]).expect("link tree");
        let error = build_git_change_set_material(
            &fixture.repository,
            &fixture.base_tree,
            linked_tree.trim(),
        )
        .expect_err("nonregular entry must refuse");
        assert!(
            error
                .to_string()
                .contains("forbidden candidate material mode")
        );

        let changed = fixture.worktree("stale");
        std::fs::write(changed.join("modify.txt"), b"changed\n").expect("modify file");
        git_run(&changed, &["add", "--all"], None, None, None).expect("stage change");
        let changed_tree = git_text(&changed, &["write-tree"]).expect("change tree");
        let bytes = build_git_change_set_material(
            &fixture.repository,
            &fixture.base_tree,
            changed_tree.trim(),
        )
        .expect("material");
        let mut material = CandidateMaterial::parse_canonical(&bytes).expect("material");
        material.change_set.changes[0]
            .old
            .as_mut()
            .expect("old state")
            .sha256 = "0".repeat(64);
        material.payload_sha256 = sha256(&serde_json::to_vec(&material.change_set).unwrap());
        let stale = material
            .canonical_bytes()
            .expect("canonical stale material");
        let error = expected_result_tree(&fixture.manifest(), &stale)
            .expect_err("stale old state must refuse");
        assert!(error.to_string().contains("old state differs"), "{error:#}");
    }

    struct GitMaterialFixture {
        _temporary: tempfile::TempDir,
        repository: PathBuf,
        workspace: PathBuf,
        base_commit: String,
        base_tree: String,
    }

    impl GitMaterialFixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().expect("temporary root");
            let repository = temporary.path().join("source");
            let workspace = temporary.path().join("workspace");
            std::fs::create_dir(&repository).expect("source");
            std::fs::create_dir(&workspace).expect("workspace");
            git_run(&repository, &["init"], None, None, None).expect("initialize Git");
            git_run(
                &repository,
                &["config", "user.name", "Mise Test"],
                None,
                None,
                None,
            )
            .expect("Git name");
            git_run(
                &repository,
                &["config", "user.email", "mise@example.invalid"],
                None,
                None,
                None,
            )
            .expect("Git email");
            git_run(
                &repository,
                &["config", "core.autocrlf", "false"],
                None,
                None,
                None,
            )
            .expect("Git line endings");
            std::fs::write(repository.join("modify.txt"), b"old\n").expect("modify base");
            std::fs::write(repository.join("delete.txt"), b"delete me\n").expect("delete base");
            std::fs::write(repository.join("keep.txt"), b"keep\n").expect("keep base");
            git_run(&repository, &["add", "--all"], None, None, None).expect("stage base");
            git_run(&repository, &["commit", "-m", "base"], None, None, None).expect("commit base");
            let base_commit = git_text(&repository, &["rev-parse", "HEAD^{commit}"])
                .expect("base commit")
                .trim()
                .to_owned();
            let base_tree = git_text(&repository, &["rev-parse", "HEAD^{tree}"])
                .expect("base tree")
                .trim()
                .to_owned();
            Self {
                _temporary: temporary,
                repository,
                workspace,
                base_commit,
                base_tree,
            }
        }

        fn worktree(&self, name: &str) -> PathBuf {
            let path = self.workspace.join(name);
            git_worktree_add_without_hooks(&self.repository, &path, &self.base_commit)
                .expect("worktree");
            path
        }

        fn manifest(&self) -> CampaignManifest {
            let mut manifest = crate::manifest::tests::valid_manifest();
            manifest.source.repository_locator = portable_absolute(&self.repository).unwrap();
            manifest.source.base_commit = self.base_commit.clone();
            manifest.source.base_tree = self.base_tree.clone();
            manifest.execution_limits.workspace_root_locator =
                portable_absolute(&self.workspace).unwrap();
            manifest.candidate_material = Some(CandidateMaterialContract {
                kind: "git_change_set".to_owned(),
                protocol: crate::candidate::GIT_CHANGE_SET_PROTOCOL_V1.to_owned(),
                media_type: crate::candidate::GIT_CHANGE_SET_MEDIA_TYPE.to_owned(),
            });
            manifest
        }
    }
}
