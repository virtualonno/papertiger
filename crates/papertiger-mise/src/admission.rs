use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::digest::{sha256, validate_sha256};
use crate::manifest::{CampaignManifest, GitObjectFormat, Sha256Digest, SourceBinding};
use crate::path_identity::portable_absolute;
use crate::store::{AdmissionOutcome, CampaignAdmission, admit_campaign};

pub const FIXTURE_BUNDLE_SCHEMA_V1: &str = "papertiger-mise.fixture-bundle.v1";
pub const CAMPAIGN_PREFLIGHT_SCHEMA_V1: &str = "papertiger-mise.campaign-preflight.v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CampaignPreflightDefect {
    pub check: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CampaignPreflightReport {
    pub schema: String,
    pub manifest_locator: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_sha256: Option<String>,
    pub ready: bool,
    pub defects: Vec<CampaignPreflightDefect>,
    pub corrective_command: String,
}

struct CampaignPreflightOutcome {
    report: CampaignPreflightReport,
    verified: Option<VerifiedCampaignAdmission>,
}

/// An admission capability that can only be constructed by checking the live
/// repositories and every locally accessible campaign-control artifact.
///
/// Its fields deliberately remain private: accepting a plain manifest in a
/// mutation API would make the CLI's verification advisory rather than an
/// authority boundary.
#[derive(Debug)]
pub struct VerifiedCampaignAdmission {
    admission: CampaignAdmission,
    manifest: CampaignManifest,
    manifest_path: PathBuf,
}

impl VerifiedCampaignAdmission {
    pub fn campaign_id(&self) -> &str {
        &self.manifest.campaign_id
    }

    pub fn manifest_sha256(&self) -> &str {
        self.admission.manifest_sha256()
    }

    pub(crate) fn admission(&self) -> &CampaignAdmission {
        &self.admission
    }

    pub fn manifest(&self) -> &CampaignManifest {
        &self.manifest
    }

    pub(crate) fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }
}

/// Canonical index of every calibration and holdout fixture admitted into a
/// campaign. Opaque locators (for example `sealed://`) are identity-bound but
/// are not disclosed or fetched during admission.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureBundleDescriptor {
    pub schema: String,
    pub fixtures: Vec<FixtureBundleEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureBundleEntry {
    pub key: String,
    pub locator: String,
    pub sha256: Sha256Digest,
}

impl FixtureBundleDescriptor {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        if self.schema != FIXTURE_BUNDLE_SCHEMA_V1 {
            bail!(
                "unsupported fixture bundle schema '{}' (expected '{FIXTURE_BUNDLE_SCHEMA_V1}')",
                self.schema
            );
        }
        let mut canonical = self.clone();
        canonical
            .fixtures
            .sort_by(|left, right| left.key.cmp(&right.key));
        let mut keys = BTreeSet::new();
        for fixture in &canonical.fixtures {
            if fixture.key.trim() != fixture.key || fixture.key.is_empty() {
                bail!("fixture bundle keys must be nonempty canonical strings");
            }
            if !keys.insert(fixture.key.as_str()) {
                bail!("duplicate fixture bundle key '{}'", fixture.key);
            }
            if fixture.locator.trim() != fixture.locator || fixture.locator.is_empty() {
                bail!("fixture '{}' locator is not canonical", fixture.key);
            }
            validate_sha256(&fixture.sha256.0, &fixture.key)?;
        }
        serde_json::to_vec(&canonical).context("serialize canonical fixture bundle descriptor")
    }
}

/// Inspect the exact live Git identity used in a campaign manifest.
pub fn inspect_source_binding(repository: impl AsRef<Path>) -> Result<SourceBinding> {
    let repository = std::fs::canonicalize(repository.as_ref()).with_context(|| {
        format!(
            "canonicalize Git repository {}",
            repository.as_ref().display()
        )
    })?;
    let base_commit = git_output(&repository, &["rev-parse", "HEAD^{commit}"])?;
    let base_tree = git_output(&repository, &["rev-parse", "HEAD^{tree}"])?;
    let object_format = git_output(&repository, &["rev-parse", "--show-object-format"])?;
    let git_object_format = match object_format.trim() {
        "sha1" => GitObjectFormat::Sha1,
        "sha256" => GitObjectFormat::Sha256,
        other => bail!("unsupported Git object format '{other}'"),
    };
    let common_dir = git_output(
        &repository,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common_dir = std::fs::canonicalize(common_dir.trim())
        .with_context(|| format!("canonicalize Git common directory '{}'", common_dir.trim()))?;
    let repository_locator = portable_absolute(&repository)?;
    let common_locator = portable_absolute(&common_dir)?;
    let repository_identity_sha256 = sha256(
        format!("papertiger-mise.git-repository.v1\n{repository_locator}\n{common_locator}\n")
            .as_bytes(),
    );
    Ok(SourceBinding {
        repository_id: repository
            .file_name()
            .and_then(|name| name.to_str())
            .context("repository has no portable final name")?
            .to_owned(),
        repository_locator,
        repository_identity_sha256: Sha256Digest(repository_identity_sha256),
        git_object_format,
        base_commit: base_commit.trim().to_owned(),
        base_tree: base_tree.trim().to_owned(),
    })
}

/// Validate and freeze a campaign admission against live Git and artifact
/// state. This is intentionally the sole public constructor of an admission
/// accepted by the SQLite mutation path.
pub fn verify_campaign_admission(
    manifest_path: impl AsRef<Path>,
) -> Result<VerifiedCampaignAdmission> {
    let outcome = inspect_campaign_preflight(manifest_path.as_ref());
    outcome.verified.with_context(|| {
        let details = outcome
            .report
            .defects
            .iter()
            .map(|defect| format!("{}: {}", defect.check, defect.message))
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "campaign admission preflight found {} defect(s): {details}; {}",
            outcome.report.defects.len(),
            outcome.report.corrective_command
        )
    })
}

/// Inspect every independently checkable admission binding without opening or
/// creating a Mise database or object store.
pub fn preflight_campaign_admission(manifest_path: impl AsRef<Path>) -> CampaignPreflightReport {
    inspect_campaign_preflight(manifest_path.as_ref()).report
}

fn inspect_campaign_preflight(requested_manifest_path: &Path) -> CampaignPreflightOutcome {
    let requested_locator = requested_manifest_path.display().to_string();
    let corrective_command = format!(
        "fix every reported defect, commit the source and control repositories, then rerun `papertiger-mise campaign preflight \"{requested_locator}\"`"
    );
    let mut report = CampaignPreflightReport {
        schema: CAMPAIGN_PREFLIGHT_SCHEMA_V1.to_owned(),
        manifest_locator: requested_locator,
        campaign_id: None,
        manifest_sha256: None,
        ready: false,
        defects: Vec::new(),
        corrective_command,
    };

    let Some(manifest_path) = collect_check(
        &mut report.defects,
        "manifest.path",
        std::fs::canonicalize(requested_manifest_path).with_context(|| {
            format!(
                "canonicalize campaign manifest {}",
                requested_manifest_path.display()
            )
        }),
    ) else {
        return CampaignPreflightOutcome {
            report,
            verified: None,
        };
    };
    report.manifest_locator = manifest_path.display().to_string();
    report.corrective_command = format!(
        "fix every reported defect, commit the source and control repositories, then rerun `papertiger-mise campaign preflight \"{}\"`",
        manifest_path.display()
    );
    let Some(manifest_bytes) = collect_check(
        &mut report.defects,
        "manifest.read",
        std::fs::read(&manifest_path)
            .with_context(|| format!("read campaign manifest {}", manifest_path.display())),
    ) else {
        return CampaignPreflightOutcome {
            report,
            verified: None,
        };
    };
    let Some(manifest) = collect_check(
        &mut report.defects,
        "manifest.parse",
        serde_json::from_slice::<CampaignManifest>(&manifest_bytes)
            .with_context(|| format!("parse campaign manifest {}", manifest_path.display())),
    ) else {
        return CampaignPreflightOutcome {
            report,
            verified: None,
        };
    };
    report.campaign_id = Some(manifest.campaign_id.clone());
    collect_check(
        &mut report.defects,
        "manifest.contract",
        manifest.validate(),
    );

    let source_repository = collect_check(
        &mut report.defects,
        "source.repository",
        std::fs::canonicalize(&manifest.source.repository_locator).with_context(|| {
            format!(
                "canonicalize source repository {}",
                manifest.source.repository_locator
            )
        }),
    );
    if let Some(source_repository) = source_repository.as_deref() {
        if let Some(observed) = collect_check(
            &mut report.defects,
            "source.binding",
            inspect_source_binding(source_repository),
        ) && !same_source_binding(&observed, &manifest.source)
        {
            push_defect(
                &mut report.defects,
                "source.binding",
                "campaign source binding differs from the exact live Git repository state",
            );
        }
        collect_check(
            &mut report.defects,
            "source.clean",
            require_clean_repository(source_repository, "source"),
        );
        if manifest.candidate_material.is_none() {
            collect_check(
                &mut report.defects,
                "mutation_scope.v1",
                verify_v1_mutation_scope(source_repository, &manifest),
            );
        }
    }

    let control_repository = manifest_path.parent().and_then(|parent| {
        collect_check(
            &mut report.defects,
            "control.repository",
            git_repository_root(parent),
        )
    });
    if manifest_path.parent().is_none() {
        push_defect(
            &mut report.defects,
            "control.repository",
            "campaign manifest has no parent directory",
        );
    }
    if let Some(control_repository) = control_repository.as_deref() {
        collect_check(
            &mut report.defects,
            "control.clean",
            require_clean_repository(control_repository, "manifest control"),
        );
        collect_check(
            &mut report.defects,
            "control.manifest",
            require_tracked_plain_file(control_repository, &manifest_path, "campaign manifest"),
        );
        collect_check(
            &mut report.defects,
            "artifact.proposal_policy",
            verify_bound_artifact(
                control_repository,
                &manifest.generation.proposal_policy_locator,
                &manifest.generation.proposal_policy_sha256.0,
                "proposal policy",
            ),
        );
    }

    let mut fixture_bundle_path = None;
    if let Some(source_repository) = source_repository.as_deref() {
        collect_check(
            &mut report.defects,
            "artifact.adapter",
            verify_bound_artifact(
                source_repository,
                &manifest.adapter.implementation_locator,
                &manifest.adapter.implementation_sha256.0,
                "adapter",
            ),
        );
        collect_check(
            &mut report.defects,
            "artifact.evaluator",
            verify_bound_artifact(
                source_repository,
                &manifest.evaluator.evaluator_locator,
                &manifest.evaluator.evaluator_sha256.0,
                "evaluator",
            ),
        );
        fixture_bundle_path = collect_check(
            &mut report.defects,
            "artifact.fixture_bundle",
            verify_bound_artifact(
                source_repository,
                &manifest.evaluator.fixture_bundle_locator,
                &manifest.evaluator.fixture_bundle_sha256.0,
                "fixture bundle",
            ),
        );
        if let Some(requirement) = &manifest.containment_requirement {
            collect_check(
                &mut report.defects,
                "artifact.containment_executor",
                verify_bound_artifact(
                    source_repository,
                    &requirement.executor_locator,
                    &requirement.executor_sha256.0,
                    "containment executor",
                ),
            );
        }
        collect_environment_checks(&mut report.defects, source_repository, &manifest);
        collect_paired_checks(&mut report.defects, source_repository, &manifest);
    }
    collect_check(
        &mut report.defects,
        "artifact.evaluator_launcher",
        verify_external_bound_artifact(
            Path::new(&manifest.evaluator.launcher_locator),
            &manifest.evaluator.launcher_sha256.0,
            "evaluator launcher",
        ),
    );
    collect_check(
        &mut report.defects,
        "artifact.outer_judge",
        verify_external_bound_artifact(
            Path::new(&manifest.generation.outer_judge_executable_locator),
            &manifest.generation.outer_judge_executable_sha256.0,
            "outer judge executable",
        ),
    );
    if let Some(build) = &manifest.evaluator.judge_build {
        collect_check(
            &mut report.defects,
            "environment.judge_build_toolchain",
            verify_external_bound_artifact(
                Path::new(&build.toolchain_executable_locator),
                &build.toolchain_executable_sha256.0,
                "judge-build toolchain executable",
            ),
        );
    }
    if let (Some(source_repository), Some(fixture_bundle_path)) =
        (source_repository.as_deref(), fixture_bundle_path.as_deref())
    {
        collect_check(
            &mut report.defects,
            "fixtures.bundle",
            verify_fixture_bundle(source_repository, fixture_bundle_path, &manifest),
        );
    }

    let verified = if report.defects.is_empty() {
        match CampaignAdmission::from_manifest(&manifest) {
            Ok(admission) => {
                report.manifest_sha256 = Some(admission.manifest_sha256().to_owned());
                report.ready = true;
                Some(VerifiedCampaignAdmission {
                    admission,
                    manifest,
                    manifest_path,
                })
            }
            Err(error) => {
                push_defect(&mut report.defects, "manifest.canonical", error.to_string());
                None
            }
        }
    } else {
        None
    };
    CampaignPreflightOutcome { report, verified }
}

fn collect_environment_checks(
    defects: &mut Vec<CampaignPreflightDefect>,
    source_repository: &Path,
    manifest: &CampaignManifest,
) {
    let Some(environment) = &manifest.evaluator.rust_build_environment else {
        return;
    };
    collect_check(
        defects,
        "environment.cargo",
        verify_external_bound_artifact(
            Path::new(&environment.cargo_executable_locator),
            &environment.cargo_executable_sha256.0,
            "Cargo executable",
        ),
    );
    collect_check(
        defects,
        "environment.rustc",
        verify_external_bound_artifact(
            Path::new(&environment.rustc_executable_locator),
            &environment.rustc_executable_sha256.0,
            "rustc executable",
        ),
    );
    match &environment.linker {
        Some(linker) => {
            collect_check(
                defects,
                "environment.linker",
                verify_external_bound_artifact(
                    Path::new(&linker.executable_locator),
                    &linker.executable_sha256.0,
                    "Rust target linker executable",
                ),
            );
        }
        None => push_defect(
            defects,
            "environment.linker",
            "new Rust build environments require an exact target linker binding",
        ),
    }
    collect_check(
        defects,
        "environment.lockfile",
        verify_bound_artifact(
            source_repository,
            &environment.lockfile_locator,
            &environment.lockfile_sha256.0,
            "Rust lockfile",
        ),
    );
    collect_check(
        defects,
        "environment.cargo_config",
        verify_bound_artifact(
            source_repository,
            &environment.cargo_config_locator,
            &environment.cargo_config_sha256.0,
            "Cargo config",
        ),
    );
    collect_check(
        defects,
        "environment.vendored_sources",
        verify_bound_tree(
            source_repository,
            &manifest.source.base_tree,
            &environment.vendored_sources_locator,
            &environment.vendored_sources_tree,
        ),
    );
}

fn collect_paired_checks(
    defects: &mut Vec<CampaignPreflightDefect>,
    source_repository: &Path,
    manifest: &CampaignManifest,
) {
    let Some(plan) = &manifest.paired_analysis else {
        return;
    };
    if let Some(adapter) = &plan.trial_adapter {
        collect_check(defects, "paired.adapter", adapter.validate());
    }
    collect_check(
        defects,
        "paired.order_seed_protocol",
        verify_bound_artifact(
            source_repository,
            &plan.order_seed_protocol_locator,
            &plan.order_seed_protocol_sha256.0,
            "paired order seed protocol",
        ),
    );
    if let crate::statistics::PairedInferenceScope::IndependentBlockPopulation {
        sampling_protocol_locator,
        sampling_protocol_sha256,
        ..
    } = &plan.inference_scope
    {
        collect_check(
            defects,
            "paired.sampling_protocol",
            verify_bound_artifact(
                source_repository,
                sampling_protocol_locator,
                &sampling_protocol_sha256.0,
                "paired sampling protocol",
            ),
        );
    }
}

fn collect_check<T>(
    defects: &mut Vec<CampaignPreflightDefect>,
    check: &str,
    result: Result<T>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            push_defect(defects, check, format!("{error:#}"));
            None
        }
    }
}

fn push_defect(
    defects: &mut Vec<CampaignPreflightDefect>,
    check: &str,
    message: impl Into<String>,
) {
    defects.push(CampaignPreflightDefect {
        check: check.to_owned(),
        message: message.into(),
    });
}

pub fn admit_verified_campaign(
    connection: &Connection,
    actor: &str,
    verified: &VerifiedCampaignAdmission,
) -> Result<AdmissionOutcome> {
    let current = verify_campaign_admission(&verified.manifest_path)?;
    if current.campaign_id() != verified.campaign_id()
        || current.manifest_sha256() != verified.manifest_sha256()
    {
        bail!("verified campaign admission became stale before the database mutation");
    }
    admit_campaign(connection, actor, &current.admission)
}

fn verify_fixture_bundle(
    source_repository: &Path,
    bundle_path: &Path,
    manifest: &CampaignManifest,
) -> Result<()> {
    let bytes = std::fs::read(bundle_path)
        .with_context(|| format!("read fixture bundle {}", bundle_path.display()))?;
    let descriptor: FixtureBundleDescriptor = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse fixture bundle {}", bundle_path.display()))?;
    let canonical = descriptor.canonical_bytes()?;
    if canonical != bytes {
        bail!("fixture bundle must use exact canonical compact JSON serialization");
    }

    let expected = expected_fixtures(manifest);
    let observed = descriptor
        .fixtures
        .iter()
        .map(|fixture| (fixture.key.as_str(), fixture))
        .collect::<BTreeMap<_, _>>();
    if observed.len() != expected.len() {
        bail!(
            "fixture bundle contains {} entries but the campaign requires {}",
            observed.len(),
            expected.len()
        );
    }
    for (key, expected_locator, expected_sha256) in expected {
        let fixture = observed
            .get(key.as_str())
            .with_context(|| format!("fixture bundle is missing required key '{key}'"))?;
        if fixture.locator != expected_locator || fixture.sha256.0 != expected_sha256 {
            bail!("fixture bundle key '{key}' differs from the campaign manifest binding");
        }
        if !is_opaque_locator(&fixture.locator) {
            validate_local_fixture_locator(&fixture.locator)?;
            verify_bound_artifact(
                source_repository,
                &fixture.locator,
                &fixture.sha256.0,
                &format!("fixture '{key}'"),
            )?;
        }
    }
    Ok(())
}

fn expected_fixtures(manifest: &CampaignManifest) -> Vec<(String, String, String)> {
    let mut fixtures = vec![
        (
            "calibration.known_bad".to_owned(),
            manifest.calibration.known_bad.fixture_locator.clone(),
            manifest.calibration.known_bad.fixture_sha256.0.clone(),
        ),
        (
            "calibration.no_op".to_owned(),
            manifest.calibration.no_op.fixture_locator.clone(),
            manifest.calibration.no_op.fixture_sha256.0.clone(),
        ),
    ];
    fixtures.extend(manifest.holdouts.tiers.iter().map(|tier| {
        (
            format!("holdout.{}", tier.key),
            tier.fixture_locator.clone(),
            tier.fixture_sha256.0.clone(),
        )
    }));
    if let Some(plan) = &manifest.paired_analysis {
        fixtures.extend(plan.blocks.iter().map(|block| {
            (
                format!("paired.block.{}", block.block_index),
                block.fixture_locator.clone(),
                block.fixture_sha256.0.clone(),
            )
        }));
    }
    fixtures.sort_by(|left, right| left.0.cmp(&right.0));
    fixtures
}

fn same_source_binding(observed: &SourceBinding, declared: &SourceBinding) -> bool {
    observed.repository_id == declared.repository_id
        && observed.repository_locator == declared.repository_locator
        && observed.repository_identity_sha256.0 == declared.repository_identity_sha256.0
        && observed.git_object_format == declared.git_object_format
        && observed.base_commit == declared.base_commit
        && observed.base_tree == declared.base_tree
}

fn require_clean_repository(repository: &Path, role: &str) -> Result<()> {
    let status = git_output(
        repository,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )?;
    if !status.trim().is_empty() {
        bail!("campaign admission requires a clean {role} repository");
    }
    Ok(())
}

fn verify_v1_mutation_scope(repository: &Path, manifest: &CampaignManifest) -> Result<()> {
    for path in &manifest.mutation_scope.allowlist {
        let listing = git_output(
            repository,
            &["ls-tree", &manifest.source.base_tree, "--", path],
        )?;
        let line = listing.trim();
        if line.is_empty() {
            bail!(
                "mutation_scope.allowlist path '{path}' is absent from source.base_tree; Mise v1 cannot create it because candidate patches reject new-file and mode records. Preseed the tracked path before campaign admission or select an existing file/directory"
            );
        }
        let mode = line
            .split_whitespace()
            .next()
            .context("Git tree listing omitted the allowlist path mode")?;
        if !matches!(mode, "040000" | "100644" | "100755") {
            bail!(
                "mutation_scope.allowlist path '{path}' has unsupported Git mode {mode}; Mise v1 requires a pre-existing regular file or directory"
            );
        }
    }
    Ok(())
}

fn git_repository_root(path: &Path) -> Result<PathBuf> {
    let root = git_output(path, &["rev-parse", "--show-toplevel"])?;
    std::fs::canonicalize(root.trim())
        .with_context(|| format!("canonicalize Git repository root '{}'", root.trim()))
}

fn verify_bound_artifact(
    repository: &Path,
    locator: &str,
    expected_sha256: &str,
    role: &str,
) -> Result<PathBuf> {
    validate_sha256(expected_sha256, role)?;
    let path = repository.join(locator);
    let path = require_tracked_plain_file(repository, &path, role)?;
    let bytes = std::fs::read(&path).with_context(|| format!("read {role} {}", path.display()))?;
    let actual = sha256(&bytes);
    if actual != expected_sha256 {
        bail!("{role} '{locator}' differs from its bound SHA-256");
    }
    Ok(path)
}

fn verify_external_bound_artifact(
    path: &Path,
    expected_sha256: &str,
    role: &str,
) -> Result<PathBuf> {
    validate_sha256(expected_sha256, role)?;
    let lexical_metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect {role} {}", path.display()))?;
    if !lexical_metadata.is_file() || lexical_metadata.file_type().is_symlink() {
        bail!("{role} is not a plain local file: {}", path.display());
    }
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("canonicalize {role} {}", path.display()))?;
    if portable_absolute(&canonical)? != portable_absolute(path)? {
        bail!("{role} locator must be its exact canonical absolute path");
    }
    let bytes = std::fs::read(&canonical)
        .with_context(|| format!("read {role} {}", canonical.display()))?;
    if sha256(&bytes) != expected_sha256 {
        bail!("{role} differs from its bound SHA-256");
    }
    Ok(canonical)
}

fn verify_bound_tree(
    repository: &Path,
    base_tree: &str,
    locator: &str,
    expected_tree: &str,
) -> Result<()> {
    let tree_spec = format!("{base_tree}:{locator}");
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(["rev-parse", "--verify"])
        .arg(&tree_spec)
        .output()
        .context("inspect vendored Rust source tree")?;
    if !output.status.success() {
        bail!("vendored Rust sources '{locator}' are absent from the frozen source tree");
    }
    let observed = String::from_utf8(output.stdout)
        .context("vendored Rust source tree identity is not UTF-8")?
        .trim()
        .to_owned();
    if observed != expected_tree {
        bail!("vendored Rust sources differ from their exact frozen Git tree");
    }
    let kind = git_output(repository, &["cat-file", "-t", expected_tree])?;
    if kind.trim() != "tree" {
        bail!("vendored Rust source identity is not a Git tree");
    }
    Ok(())
}

fn require_tracked_plain_file(repository: &Path, path: &Path, role: &str) -> Result<PathBuf> {
    let lexical_metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect {role} {}", path.display()))?;
    if !lexical_metadata.is_file() || lexical_metadata.file_type().is_symlink() {
        bail!("{role} is not a plain tracked file: {}", path.display());
    }
    let path = std::fs::canonicalize(path)
        .with_context(|| format!("canonicalize {role} {}", path.display()))?;
    let relative = path
        .strip_prefix(repository)
        .with_context(|| format!("{role} is outside the owning repository"))?;
    let relative = relative
        .to_str()
        .context("tracked artifact path is not UTF-8")?;
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(relative)
        .output()
        .with_context(|| format!("query Git tracking for {role}"))?;
    if !output.status.success() {
        bail!("{role} '{relative}' is not tracked by the owning repository");
    }
    Ok(path)
}

fn validate_local_fixture_locator(locator: &str) -> Result<()> {
    let path = Path::new(locator);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("local fixture locator '{locator}' must be a safe repository-relative path");
    }
    Ok(())
}

fn is_opaque_locator(locator: &str) -> bool {
    locator
        .split_once("://")
        .is_some_and(|(scheme, rest)| !scheme.is_empty() && !rest.is_empty())
}

fn git_output(repository: &Path, arguments: &[&str]) -> Result<String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::TempDir;

    use super::{
        CAMPAIGN_PREFLIGHT_SCHEMA_V1, FIXTURE_BUNDLE_SCHEMA_V1, FixtureBundleDescriptor,
        FixtureBundleEntry, admit_verified_campaign, inspect_source_binding, is_opaque_locator,
        portable_absolute, preflight_campaign_admission, validate_local_fixture_locator,
        verify_campaign_admission,
    };
    use crate::digest::sha256;
    use crate::manifest::{
        CampaignManifest, RustBuildEnvironment, RustLinkerBinding, Sha256Digest,
    };
    use crate::store::{AdmissionOutcome, init};

    #[test]
    fn portable_paths_remove_windows_verbatim_prefixes() {
        assert_eq!(
            portable_absolute(Path::new(r"\\?\C:\projects\papertiger")).expect("local path"),
            "C:/projects/papertiger"
        );
        assert_eq!(
            portable_absolute(Path::new(r"\\?\UNC\server\share\repo")).expect("UNC path"),
            "//server/share/repo"
        );
    }

    #[test]
    fn fixture_locators_separate_local_files_from_opaque_capabilities() {
        validate_local_fixture_locator("fixtures/exploration.json").expect("local");
        assert!(validate_local_fixture_locator("../outside.json").is_err());
        assert!(is_opaque_locator("sealed://fixture/confirmation"));
        assert!(!is_opaque_locator("fixtures/confirmation.json"));
    }

    #[test]
    fn public_admission_requires_exact_tracked_artifacts_and_fixture_members() {
        let fixture = AdmissionFixture::new();
        let verified = verify_campaign_admission(&fixture.manifest_path).expect("verify campaign");
        assert_eq!(verified.campaign_id(), "fixture-rsi-1");

        let database = rusqlite::Connection::open_in_memory().expect("database");
        init(&database).expect("schema");
        assert_eq!(
            admit_verified_campaign(&database, "test", &verified).expect("admit"),
            AdmissionOutcome::Admitted
        );

        fixture.replace_judge_with_missing_locator();
        let error = verify_campaign_admission(&fixture.manifest_path).expect_err("must refuse");
        assert!(
            error.to_string().contains("outer judge executable"),
            "{error:#}"
        );
        let error = admit_verified_campaign(&database, "test", &verified)
            .expect_err("stale token must be reverified");
        assert!(
            error.to_string().contains("outer judge executable"),
            "{error:#}"
        );
    }

    #[test]
    fn preflight_reports_independent_defects_without_creating_authority_state() {
        let fixture = AdmissionFixture::new();
        let ready = preflight_campaign_admission(&fixture.manifest_path);
        assert_eq!(ready.schema, CAMPAIGN_PREFLIGHT_SCHEMA_V1);
        assert!(ready.ready, "{:#?}", ready.defects);
        assert!(ready.manifest_sha256.is_some());
        assert!(!fixture._temporary.path().join("state").exists());

        fixture.replace_judge_with_missing_locator();
        std::fs::write(
            fixture.source.join("fixtures/mise/adapter.json"),
            b"{\"adapter\":\"drifted\"}",
        )
        .expect("drift adapter");
        git(&fixture.source, &["add", "."]);
        git(
            &fixture.source,
            &["commit", "-m", "drift source and adapter"],
        );

        let report = preflight_campaign_admission(&fixture.manifest_path);
        assert!(!report.ready);
        assert!(report.manifest_sha256.is_none());
        for expected in ["source.binding", "artifact.adapter", "artifact.outer_judge"] {
            assert!(
                report.defects.iter().any(|defect| defect.check == expected),
                "preflight omitted {expected}: {:#?}",
                report.defects
            );
        }
        assert!(!fixture._temporary.path().join("state").exists());

        let refusal = verify_campaign_admission(&fixture.manifest_path)
            .expect_err("admission must consume the same failing preflight")
            .to_string();
        assert!(refusal.contains("source.binding"), "{refusal}");
        assert!(refusal.contains("artifact.adapter"), "{refusal}");
        assert!(refusal.contains("artifact.outer_judge"), "{refusal}");
    }

    #[test]
    fn paired_admission_verifies_seed_sampling_and_block_artifacts() {
        let fixture = AdmissionFixture::new_paired();
        verify_campaign_admission(&fixture.manifest_path).expect("verify paired campaign");

        fixture.replace_paired_seed_protocol();
        let error = verify_campaign_admission(&fixture.manifest_path)
            .expect_err("paired seed protocol drift must refuse admission");
        assert!(
            error.to_string().contains("paired order seed protocol"),
            "{error:#}"
        );
    }

    #[test]
    fn v1_admission_refuses_an_allowlist_path_absent_from_the_base_tree() {
        let fixture = AdmissionFixture::new();
        fixture.replace_allowlist_with_missing_path();

        let error = verify_campaign_admission(&fixture.manifest_path)
            .expect_err("missing v1 mutation path must refuse admission");
        assert!(
            error
                .to_string()
                .contains("Mise v1 cannot create it because candidate patches reject new-file"),
            "{error:#}"
        );
        assert!(error.to_string().contains("Preseed the tracked path"));
    }

    #[test]
    fn rust_build_admission_binds_tools_configuration_and_vendor_tree() {
        let fixture = AdmissionFixture::new();
        fixture.enable_rust_build_environment();
        verify_campaign_admission(&fixture.manifest_path).expect("verify Rust build profile");

        fixture.drift_vendor_and_rebind_source();
        let error = verify_campaign_admission(&fixture.manifest_path)
            .expect_err("changed vendor tree must refuse admission");
        assert!(
            error
                .to_string()
                .contains("vendored Rust sources differ from their exact frozen Git tree"),
            "{error:#}"
        );
    }

    #[test]
    fn rust_build_admission_refuses_linker_identity_drift() {
        let fixture = AdmissionFixture::new();
        fixture.enable_rust_build_environment();
        let mut manifest: CampaignManifest =
            serde_json::from_slice(&std::fs::read(&fixture.manifest_path).expect("manifest"))
                .expect("parse manifest");
        manifest
            .evaluator
            .rust_build_environment
            .as_mut()
            .and_then(|rust| rust.linker.as_mut())
            .expect("linker binding")
            .executable_sha256 = digest(b"not the linker");
        fixture.write_and_commit_manifest(&manifest, "forge linker identity");

        let error = verify_campaign_admission(&fixture.manifest_path)
            .expect_err("wrong linker SHA-256 must fail closed");
        assert!(
            error.to_string().contains("Rust target linker executable"),
            "{error:#}"
        );
    }

    struct AdmissionFixture {
        _temporary: TempDir,
        source: PathBuf,
        control: PathBuf,
        manifest_path: PathBuf,
    }

    impl AdmissionFixture {
        fn new() -> Self {
            Self::new_with_paired(false)
        }

        fn new_paired() -> Self {
            Self::new_with_paired(true)
        }

        fn new_with_paired(paired: bool) -> Self {
            let temporary = tempfile::tempdir().expect("temporary root");
            let source = temporary.path().join("source");
            let control = temporary.path().join("control");
            let run = temporary.path().join("run");
            std::fs::create_dir_all(source.join("fixtures/mise")).expect("source fixtures");
            std::fs::create_dir_all(&control).expect("control");
            std::fs::create_dir_all(&run).expect("run");

            let mut manifest = crate::manifest::tests::valid_manifest();
            if paired {
                configure_paired_manifest(&mut manifest);
            }
            manifest.execution_limits.workspace_root_locator =
                portable_absolute(&run).expect("run locator");
            manifest.execution_limits.runtime_root_locator =
                Some(portable_absolute(temporary.path()).expect("runtime root locator"));
            write_bound_source_artifacts(&source, &mut manifest);
            init_repository(&source);
            manifest.source = inspect_source_binding(&source).expect("source binding");

            let judge = std::fs::canonicalize(std::env::current_exe().expect("test executable"))
                .expect("canonical test executable");
            manifest.generation.outer_judge_executable_locator =
                portable_absolute(&judge).expect("judge locator");
            manifest.generation.proposal_policy_locator = "policy.json".to_owned();
            let policy = b"{\"policy\":\"bounded\"}";
            std::fs::write(control.join("policy.json"), policy).expect("policy");
            manifest.generation.outer_judge_executable_sha256 =
                digest(&std::fs::read(&judge).expect("judge bytes"));
            manifest.generation.proposal_policy_sha256 = digest(policy);

            let manifest_path = control.join("campaign.json");
            std::fs::write(
                &manifest_path,
                serde_json::to_vec_pretty(&manifest).expect("manifest JSON"),
            )
            .expect("manifest");
            init_repository(&control);

            Self {
                _temporary: temporary,
                source,
                control,
                manifest_path,
            }
        }

        fn replace_judge_with_missing_locator(&self) {
            let mut manifest: CampaignManifest =
                serde_json::from_slice(&std::fs::read(&self.manifest_path).expect("read manifest"))
                    .expect("parse manifest");
            manifest.generation.outer_judge_executable_locator =
                portable_absolute(&self.control.join("missing-judge.bin"))
                    .expect("missing judge locator");
            std::fs::write(
                &self.manifest_path,
                serde_json::to_vec_pretty(&manifest).expect("manifest JSON"),
            )
            .expect("manifest");
            git(&self.control, &["add", "campaign.json"]);
            git(&self.control, &["commit", "-m", "point at missing judge"]);
        }

        fn replace_paired_seed_protocol(&self) {
            std::fs::write(
                self.source.join("fixtures/mise/order-seed-protocol.json"),
                b"{\"scheduler\":\"drifted\"}",
            )
            .expect("replace seed protocol");
            git(&self.source, &["add", "."]);
            git(&self.source, &["commit", "-m", "drift seed protocol"]);

            let mut manifest: CampaignManifest =
                serde_json::from_slice(&std::fs::read(&self.manifest_path).expect("read manifest"))
                    .expect("parse manifest");
            manifest.source = inspect_source_binding(&self.source).expect("new source binding");
            std::fs::write(
                &self.manifest_path,
                serde_json::to_vec_pretty(&manifest).expect("manifest JSON"),
            )
            .expect("manifest");
            git(&self.control, &["add", "campaign.json"]);
            git(&self.control, &["commit", "-m", "bind drifted source"]);
        }

        fn replace_allowlist_with_missing_path(&self) {
            let mut manifest: CampaignManifest =
                serde_json::from_slice(&std::fs::read(&self.manifest_path).expect("read manifest"))
                    .expect("parse manifest");
            manifest.mutation_scope.allowlist = vec!["src/not-preseeded.rs".to_owned()];
            std::fs::write(
                &self.manifest_path,
                serde_json::to_vec_pretty(&manifest).expect("manifest JSON"),
            )
            .expect("manifest");
            git(&self.control, &["add", "campaign.json"]);
            git(
                &self.control,
                &["commit", "-m", "bind missing mutation path"],
            );
        }

        fn enable_rust_build_environment(&self) {
            std::fs::create_dir_all(self.source.join(".cargo")).expect("Cargo config directory");
            std::fs::create_dir_all(self.source.join("vendor/example/src"))
                .expect("vendor fixture directory");
            std::fs::write(self.source.join("Cargo.lock"), b"# frozen lockfile\n")
                .expect("lockfile");
            std::fs::write(
                self.source.join(".cargo/config.toml"),
                b"[source.crates-io]\nreplace-with = \"vendored-sources\"\n[source.vendored-sources]\ndirectory = \"vendor\"\n",
            )
            .expect("Cargo config");
            std::fs::write(
                self.source.join("vendor/example/Cargo.toml"),
                b"[package]\nname = \"example\"\nversion = \"0.1.0\"\n",
            )
            .expect("vendored manifest");
            std::fs::write(
                self.source.join("vendor/example/src/lib.rs"),
                b"pub fn value() {}\n",
            )
            .expect("vendored source");
            git(&self.source, &["add", "."]);
            git(
                &self.source,
                &["commit", "-m", "add frozen Rust build inputs"],
            );

            let mut manifest: CampaignManifest =
                serde_json::from_slice(&std::fs::read(&self.manifest_path).expect("read manifest"))
                    .expect("parse manifest");
            let executable =
                std::fs::canonicalize(std::env::current_exe().expect("test executable"))
                    .expect("canonical test executable");
            let executable_bytes = std::fs::read(&executable).expect("test executable bytes");
            manifest.source = inspect_source_binding(&self.source).expect("source binding");
            manifest.mutation_scope.protected_paths.extend([
                ".cargo".to_owned(),
                "Cargo.lock".to_owned(),
                "vendor".to_owned(),
            ]);
            manifest.evaluator.rust_build_environment = Some(RustBuildEnvironment {
                cargo_executable_locator: portable_absolute(&executable).expect("Cargo locator"),
                cargo_executable_sha256: digest(&executable_bytes),
                rustc_executable_locator: portable_absolute(&executable).expect("rustc locator"),
                rustc_executable_sha256: digest(&executable_bytes),
                toolchain: "1.95.0".to_owned(),
                lockfile_locator: "Cargo.lock".to_owned(),
                lockfile_sha256: digest(
                    &std::fs::read(self.source.join("Cargo.lock")).expect("lockfile"),
                ),
                cargo_config_locator: ".cargo/config.toml".to_owned(),
                cargo_config_sha256: digest(
                    &std::fs::read(self.source.join(".cargo/config.toml")).expect("Cargo config"),
                ),
                vendored_sources_locator: "vendor".to_owned(),
                vendored_sources_tree: git_stdout(&self.source, &["rev-parse", "HEAD:vendor"]),
                linker: Some(RustLinkerBinding {
                    target_triple: "x86_64-unknown-linux-gnu".to_owned(),
                    executable_locator: portable_absolute(&executable).expect("linker locator"),
                    executable_sha256: digest(&executable_bytes),
                }),
            });
            self.write_and_commit_manifest(&manifest, "bind frozen Rust build inputs");
        }

        fn drift_vendor_and_rebind_source(&self) {
            std::fs::write(
                self.source.join("vendor/example/src/lib.rs"),
                b"pub fn changed() {}\n",
            )
            .expect("drift vendor source");
            git(&self.source, &["add", "."]);
            git(&self.source, &["commit", "-m", "drift vendored source"]);
            let mut manifest: CampaignManifest =
                serde_json::from_slice(&std::fs::read(&self.manifest_path).expect("read manifest"))
                    .expect("parse manifest");
            manifest.source = inspect_source_binding(&self.source).expect("source binding");
            self.write_and_commit_manifest(&manifest, "rebind source without vendor receipt");
        }

        fn write_and_commit_manifest(&self, manifest: &CampaignManifest, message: &str) {
            std::fs::write(
                &self.manifest_path,
                serde_json::to_vec_pretty(manifest).expect("manifest JSON"),
            )
            .expect("manifest");
            git(&self.control, &["add", "campaign.json"]);
            git(&self.control, &["commit", "-m", message]);
        }
    }

    fn configure_paired_manifest(manifest: &mut CampaignManifest) {
        manifest.objectives = crate::statistics::tests::objectives();
        manifest.evaluator.protocol = crate::statistics::PAIRED_MEASUREMENT_PROTOCOL_V1.to_owned();
        let mut paired = crate::statistics::tests::plan();
        paired.calibration_fixtures.no_op.locator =
            manifest.calibration.no_op.fixture_locator.clone();
        paired.calibration_fixtures.no_op.sha256 =
            manifest.calibration.no_op.fixture_sha256.clone();
        paired.calibration_fixtures.known_bad.locator =
            manifest.calibration.known_bad.fixture_locator.clone();
        paired.calibration_fixtures.known_bad.sha256 =
            manifest.calibration.known_bad.fixture_sha256.clone();
        manifest.calibration.no_op.minimum_repetitions = 16;
        manifest.calibration.known_bad.minimum_repetitions = 16;
        let confirmation = manifest
            .holdouts
            .tiers
            .iter_mut()
            .find(|tier| tier.kind == crate::manifest::HoldoutTierKind::Confirmation)
            .expect("confirmation tier");
        for block in &mut paired.blocks {
            block.fixture_locator = confirmation.fixture_locator.clone();
            block.fixture_sha256 = confirmation.fixture_sha256.clone();
        }
        confirmation.minimum_repetitions = 16;
        confirmation.maximum_disclosures = 16;
        manifest.holdouts.disclosure_cap = 21;
        manifest
            .budgets
            .caps
            .iter_mut()
            .find(|cap| cap.resource == crate::budget::BudgetResource::HoldoutDisclosures)
            .expect("disclosure budget")
            .hard_limit = 21;
        manifest
            .budgets
            .caps
            .iter_mut()
            .find(|cap| cap.resource == crate::budget::BudgetResource::Trials)
            .expect("trial budget")
            .hard_limit = 64;
        manifest.stop_rules.max_trials_without_qualified_improvement = 24;
        manifest.paired_analysis = Some(paired);
    }

    fn write_bound_source_artifacts(source: &Path, manifest: &mut CampaignManifest) {
        std::fs::create_dir_all(source.join("src")).expect("source mutation scope");
        std::fs::create_dir_all(source.join("tests/fixtures")).expect("test mutation scope");
        std::fs::write(
            source.join("src/fixture.rs"),
            b"// tracked mutation fixture\n",
        )
        .expect("source mutation fixture");
        std::fs::write(source.join("tests/fixtures/fixture.json"), b"{}\n")
            .expect("test mutation fixture");
        let adapter = b"{\"adapter\":1}";
        let evaluator = b"Write-Output evaluator";
        let no_op = b"{\"calibration\":\"no-op\"}";
        let known_bad = b"{\"calibration\":\"known-bad\"}";
        let exploration = b"{\"holdout\":\"exploration\"}";
        let containment_executor = b"{\"executor\":\"sealed-v1\"}";
        for (locator, bytes) in [
            (&manifest.adapter.implementation_locator, adapter.as_slice()),
            (&manifest.evaluator.evaluator_locator, evaluator.as_slice()),
            (
                &manifest.calibration.no_op.fixture_locator,
                no_op.as_slice(),
            ),
            (
                &manifest.calibration.known_bad.fixture_locator,
                known_bad.as_slice(),
            ),
            (
                &manifest.holdouts.tiers[1].fixture_locator,
                exploration.as_slice(),
            ),
            (
                &manifest
                    .containment_requirement
                    .as_ref()
                    .expect("containment requirement")
                    .executor_locator,
                containment_executor.as_slice(),
            ),
        ] {
            std::fs::write(source.join(locator), bytes).expect("source artifact");
        }
        manifest.adapter.implementation_sha256 = digest(adapter);
        manifest.evaluator.evaluator_sha256 = digest(evaluator);
        manifest.calibration.no_op.fixture_sha256 = digest(no_op);
        manifest.calibration.known_bad.fixture_sha256 = digest(known_bad);
        manifest.holdouts.tiers[1].fixture_sha256 = digest(exploration);
        manifest
            .containment_requirement
            .as_mut()
            .expect("containment requirement")
            .executor_sha256 = digest(containment_executor);

        let no_op_binding = crate::statistics::PairedFixtureBinding {
            locator: manifest.calibration.no_op.fixture_locator.clone(),
            sha256: manifest.calibration.no_op.fixture_sha256.clone(),
        };
        let known_bad_binding = crate::statistics::PairedFixtureBinding {
            locator: manifest.calibration.known_bad.fixture_locator.clone(),
            sha256: manifest.calibration.known_bad.fixture_sha256.clone(),
        };
        if let Some(plan) = &mut manifest.paired_analysis {
            plan.calibration_fixtures.no_op = no_op_binding;
            plan.calibration_fixtures.known_bad = known_bad_binding;
            let order_seed_protocol = b"{\"scheduler\":\"trusted-independent-v1\"}";
            let sampling_protocol = b"{\"population\":\"independent-replays-v1\"}";
            std::fs::write(
                source.join(&plan.order_seed_protocol_locator),
                order_seed_protocol,
            )
            .expect("order seed protocol");
            plan.order_seed_protocol_sha256 = digest(order_seed_protocol);
            if let crate::statistics::PairedInferenceScope::IndependentBlockPopulation {
                sampling_protocol_locator,
                sampling_protocol_sha256,
                ..
            } = &mut plan.inference_scope
            {
                std::fs::write(source.join(sampling_protocol_locator), sampling_protocol)
                    .expect("sampling protocol");
                *sampling_protocol_sha256 = digest(sampling_protocol);
            }
        }

        let mut fixtures = vec![
            fixture_entry(
                "calibration.no_op",
                &manifest.calibration.no_op.fixture_locator,
                &manifest.calibration.no_op.fixture_sha256,
            ),
            fixture_entry(
                "calibration.known_bad",
                &manifest.calibration.known_bad.fixture_locator,
                &manifest.calibration.known_bad.fixture_sha256,
            ),
            fixture_entry(
                "holdout.confirmation",
                &manifest.holdouts.tiers[0].fixture_locator,
                &manifest.holdouts.tiers[0].fixture_sha256,
            ),
            fixture_entry(
                "holdout.exploration",
                &manifest.holdouts.tiers[1].fixture_locator,
                &manifest.holdouts.tiers[1].fixture_sha256,
            ),
        ];
        if let Some(plan) = &manifest.paired_analysis {
            fixtures.extend(plan.blocks.iter().map(|block| {
                fixture_entry(
                    &format!("paired.block.{}", block.block_index),
                    &block.fixture_locator,
                    &block.fixture_sha256,
                )
            }));
        }
        let bundle = FixtureBundleDescriptor {
            schema: FIXTURE_BUNDLE_SCHEMA_V1.to_owned(),
            fixtures,
        };
        let bundle_bytes = bundle.canonical_bytes().expect("canonical bundle");
        std::fs::write(
            source.join(&manifest.evaluator.fixture_bundle_locator),
            &bundle_bytes,
        )
        .expect("fixture bundle");
        manifest.evaluator.fixture_bundle_sha256 = digest(&bundle_bytes);
    }

    fn fixture_entry(key: &str, locator: &str, sha256: &Sha256Digest) -> FixtureBundleEntry {
        FixtureBundleEntry {
            key: key.to_owned(),
            locator: locator.to_owned(),
            sha256: sha256.clone(),
        }
    }

    fn digest(bytes: &[u8]) -> Sha256Digest {
        Sha256Digest(sha256(bytes))
    }

    fn init_repository(repository: &Path) {
        git(repository, &["init"]);
        git(
            repository,
            &["config", "user.email", "mise@example.invalid"],
        );
        git(repository, &["config", "user.name", "Mise Test"]);
        git(repository, &["add", "."]);
        git(repository, &["commit", "-m", "fixture"]);
    }

    fn git(repository: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .expect("launch Git");
        assert!(
            output.status.success(),
            "Git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout(repository: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .expect("launch Git");
        assert!(
            output.status.success(),
            "Git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("Git stdout UTF-8")
            .trim()
            .to_owned()
    }
}
