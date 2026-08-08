use std::collections::BTreeSet;
use std::fmt::Write;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::digest::{sha256, validate_sha256};

pub const CANDIDATE_MATERIAL_SCHEMA_V1: &str = "papertiger-mise.candidate-material.v1";
pub const GIT_CHANGE_SET_SCHEMA_V1: &str = "papertiger-mise.git-change-set.v1";
pub const GIT_CHANGE_SET_PROTOCOL_V1: &str = "papertiger-mise.git-change-set.v1";
pub const GIT_CHANGE_SET_MEDIA_TYPE: &str = "application/vnd.papertiger-mise.git-change-set+json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hypothesis {
    pub mechanism: String,
    pub expected_effects: Vec<String>,
    pub possible_regressions: Vec<String>,
    pub decisive_falsifiers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateProposal {
    pub campaign_id: String,
    pub parent_candidate_ids: BTreeSet<String>,
    pub base_commit: String,
    pub base_tree: String,
    pub proposer: String,
    pub proposal_policy_sha256: String,
    pub adapter_sha256: String,
    pub hypothesis: Hypothesis,
    pub changed_paths: BTreeSet<String>,
    pub changed_symbols: BTreeSet<String>,
    pub semantic_class: String,
    /// Required when deliberately revisiting a durable negative fingerprint.
    pub differentiator: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateDisposition {
    Proposed,
    Materialized,
    Evaluating,
    Rejected,
    Inconclusive,
    InfrastructureFailed,
    Nominated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateMaterialFormat {
    LegacyGitPatchV1,
    GitChangeSetV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeOperation {
    Add,
    Modify,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitFileMode {
    #[serde(rename = "100644")]
    Regular,
    #[serde(rename = "100755")]
    Executable,
}

impl GitFileMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitFileIdentity {
    pub mode: GitFileMode,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitFileContent {
    pub mode: GitFileMode,
    pub sha256: String,
    pub content_hex: String,
}

impl GitFileContent {
    pub fn from_bytes(mode: GitFileMode, bytes: &[u8]) -> Self {
        Self {
            mode,
            sha256: sha256(bytes),
            content_hex: encode_hex(bytes),
        }
    }

    pub fn bytes(&self) -> Result<Vec<u8>> {
        decode_hex(&self.content_hex, "Git change content")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitChange {
    pub operation: GitChangeOperation,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<GitFileIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<GitFileContent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitChangeSet {
    pub schema: String,
    pub changes: Vec<GitChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitChangeSetScope {
    pub changed_paths: BTreeSet<String>,
    pub operations: BTreeSet<GitChangeOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateMaterial {
    pub schema: String,
    pub kind: String,
    pub protocol: String,
    pub media_type: String,
    pub payload_sha256: String,
    pub scope: GitChangeSetScope,
    pub change_set: GitChangeSet,
}

impl CandidateMaterial {
    pub fn from_changes(mut changes: Vec<GitChange>) -> Result<Self> {
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        let change_set = GitChangeSet {
            schema: GIT_CHANGE_SET_SCHEMA_V1.to_owned(),
            changes,
        };
        let paths = change_set
            .changes
            .iter()
            .map(|change| change.path.clone())
            .collect();
        let operations = change_set
            .changes
            .iter()
            .map(|change| change.operation)
            .collect();
        let payload_sha256 = sha256(&serde_json::to_vec(&change_set)?);
        let material = Self {
            schema: CANDIDATE_MATERIAL_SCHEMA_V1.to_owned(),
            kind: "git_change_set".to_owned(),
            protocol: GIT_CHANGE_SET_PROTOCOL_V1.to_owned(),
            media_type: GIT_CHANGE_SET_MEDIA_TYPE.to_owned(),
            payload_sha256,
            scope: GitChangeSetScope {
                changed_paths: paths,
                operations,
            },
            change_set,
        };
        material.validate()?;
        Ok(material)
    }

    pub fn parse_canonical(bytes: &[u8]) -> Result<Self> {
        let material: Self = serde_json::from_slice(bytes)
            .context("candidate material is not typed canonical JSON")?;
        material.validate()?;
        if serde_json::to_vec(&material)? != bytes {
            bail!(
                "candidate material must be canonical compact JSON; run `papertiger-mise candidate build-material --repository <repo> --base-tree <tree> --result-tree <tree> --output <file>`"
            );
        }
        Ok(material)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != CANDIDATE_MATERIAL_SCHEMA_V1
            || self.kind != "git_change_set"
            || self.protocol != GIT_CHANGE_SET_PROTOCOL_V1
            || self.media_type != GIT_CHANGE_SET_MEDIA_TYPE
        {
            bail!(
                "candidate material does not declare the exact supported Git change-set contract"
            );
        }
        if self.change_set.schema != GIT_CHANGE_SET_SCHEMA_V1 {
            bail!("candidate material has an unsupported Git change-set payload schema");
        }
        let payload = serde_json::to_vec(&self.change_set)?;
        validate_sha256(&self.payload_sha256, "candidate material payload")?;
        if sha256(&payload) != self.payload_sha256 {
            bail!("candidate material payload SHA-256 does not match its typed change set");
        }
        let mut paths = BTreeSet::new();
        let mut operations = BTreeSet::new();
        let mut previous: Option<&str> = None;
        for change in &self.change_set.changes {
            validate_portable_relative_path(&change.path)?;
            if previous.is_some_and(|value| value >= change.path.as_str()) {
                bail!("Git change-set records must be uniquely sorted by path");
            }
            previous = Some(&change.path);
            paths.insert(change.path.clone());
            operations.insert(change.operation);
            match change.operation {
                GitChangeOperation::Add if change.old.is_none() && change.new.is_some() => {}
                GitChangeOperation::Modify if change.old.is_some() && change.new.is_some() => {
                    if change.old.as_ref().map(|state| state.mode)
                        != change.new.as_ref().map(|state| state.mode)
                    {
                        bail!(
                            "Git change '{}' changes executable mode; candidate material admits content changes only",
                            change.path
                        );
                    }
                }
                GitChangeOperation::Delete if change.old.is_some() && change.new.is_none() => {}
                _ => bail!(
                    "Git change '{}' has old/new states inconsistent with its operation",
                    change.path
                ),
            }
            if let Some(old) = &change.old {
                validate_sha256(&old.sha256, "Git change old content")?;
            }
            if let Some(new) = &change.new {
                validate_sha256(&new.sha256, "Git change new content")?;
                let content = new.bytes()?;
                if sha256(&content) != new.sha256 {
                    bail!("Git change '{}' new content SHA-256 differs", change.path);
                }
            }
        }
        if paths != self.scope.changed_paths || operations != self.scope.operations {
            bail!("candidate material scope does not exactly match its Git change records");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundCandidate {
    pub candidate_id: String,
    pub material_sha256: String,
    pub material_format: CandidateMaterialFormat,
    pub negative_fingerprint: String,
    pub proposal: CandidateProposal,
    pub material_bytes: Vec<u8>,
}

#[derive(Serialize)]
struct CandidateIdentity<'a> {
    schema: &'static str,
    proposal: &'a CandidateProposal,
    material_sha256: &'a str,
}

#[derive(Serialize)]
struct LegacyCandidateIdentity<'a> {
    schema: &'static str,
    proposal: &'a CandidateProposal,
    patch_sha256: &'a str,
}

#[derive(Serialize)]
struct NegativeFingerprint<'a> {
    schema: &'static str,
    changed_paths: &'a BTreeSet<String>,
    changed_symbols: &'a BTreeSet<String>,
    semantic_class: &'a str,
}

pub fn bind_candidate(
    proposal: CandidateProposal,
    material_bytes: Vec<u8>,
) -> Result<BoundCandidate> {
    validate_proposal(&proposal)?;
    let material = CandidateMaterial::parse_canonical(&material_bytes)?;
    if material.change_set.changes.is_empty() && proposal.semantic_class != "calibration-no-op" {
        bail!("only a calibration no-op may have an empty candidate change set");
    }
    if material.scope.changed_paths != proposal.changed_paths {
        bail!("candidate declared changed paths differ from its typed material scope");
    }
    let material_sha256 = sha256(&material_bytes);
    let identity = serde_json::to_vec(&CandidateIdentity {
        schema: "papertiger-mise.candidate-identity.v2",
        proposal: &proposal,
        material_sha256: &material_sha256,
    })?;
    let negative = serde_json::to_vec(&NegativeFingerprint {
        schema: "papertiger-mise.negative-fingerprint.v1",
        changed_paths: &proposal.changed_paths,
        changed_symbols: &proposal.changed_symbols,
        semantic_class: &proposal.semantic_class,
    })?;
    Ok(BoundCandidate {
        candidate_id: sha256(&identity),
        material_sha256,
        material_format: CandidateMaterialFormat::GitChangeSetV1,
        negative_fingerprint: sha256(&negative),
        proposal,
        material_bytes,
    })
}

pub(crate) fn bind_legacy_patch_candidate(
    proposal: CandidateProposal,
    patch_bytes: Vec<u8>,
) -> Result<BoundCandidate> {
    validate_proposal(&proposal)?;
    if patch_bytes.is_empty() && proposal.semantic_class != "calibration-no-op" {
        bail!("legacy candidate patch must not be empty");
    }
    let material_sha256 = sha256(&patch_bytes);
    let identity = serde_json::to_vec(&LegacyCandidateIdentity {
        schema: "papertiger-mise.candidate-identity.v1",
        proposal: &proposal,
        patch_sha256: &material_sha256,
    })?;
    let negative = serde_json::to_vec(&NegativeFingerprint {
        schema: "papertiger-mise.negative-fingerprint.v1",
        changed_paths: &proposal.changed_paths,
        changed_symbols: &proposal.changed_symbols,
        semantic_class: &proposal.semantic_class,
    })?;
    Ok(BoundCandidate {
        candidate_id: sha256(&identity),
        material_sha256,
        material_format: CandidateMaterialFormat::LegacyGitPatchV1,
        negative_fingerprint: sha256(&negative),
        proposal,
        material_bytes: patch_bytes,
    })
}

fn validate_proposal(proposal: &CandidateProposal) -> Result<()> {
    validate_identifier("campaign_id", &proposal.campaign_id)?;
    validate_git_identity("base_commit", &proposal.base_commit)?;
    validate_git_identity("base_tree", &proposal.base_tree)?;
    for parent in &proposal.parent_candidate_ids {
        validate_sha256(parent, "parent_candidate_id")?;
    }
    for (name, value) in [
        ("proposer", proposal.proposer.as_str()),
        (
            "proposal_policy_sha256",
            proposal.proposal_policy_sha256.as_str(),
        ),
        ("adapter_sha256", proposal.adapter_sha256.as_str()),
        (
            "hypothesis.mechanism",
            proposal.hypothesis.mechanism.as_str(),
        ),
        ("semantic_class", proposal.semantic_class.as_str()),
    ] {
        if value.trim().is_empty() {
            bail!("{name} must be nonblank");
        }
    }
    validate_sha256(&proposal.proposal_policy_sha256, "proposal_policy_sha256")?;
    validate_sha256(&proposal.adapter_sha256, "adapter_sha256")?;
    if proposal
        .differentiator
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        bail!("candidate differentiator must be absent or nonblank");
    }
    if proposal.hypothesis.expected_effects.is_empty()
        || proposal.hypothesis.decisive_falsifiers.is_empty()
    {
        bail!("candidate hypothesis requires expected effects and decisive falsifiers");
    }
    if proposal.changed_paths.is_empty() && proposal.semantic_class != "calibration-no-op" {
        bail!("candidate must declare at least one changed path");
    }
    if proposal.semantic_class == "calibration-no-op" && !proposal.changed_paths.is_empty() {
        bail!("no-op calibration candidate must not declare changed paths");
    }
    for path in &proposal.changed_paths {
        validate_portable_relative_path(path)?;
    }
    Ok(())
}

fn validate_identifier(name: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("{name} must be a portable identifier");
    }
    Ok(())
}

fn validate_git_identity(name: &str, value: &str) -> Result<()> {
    if !(40..=64).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("{name} must be a lowercase full Git object identity");
    }
    Ok(())
}

pub(crate) fn validate_portable_relative_path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || value.contains(':')
    {
        bail!("candidate changed path '{value}' is not a portable confined relative path");
    }
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn decode_hex(value: &str, role: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{role} must be lowercase even-length hexadecimal");
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("validated ASCII hex");
            u8::from_str_radix(pair, 16).context("decode validated hex byte")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal() -> CandidateProposal {
        CandidateProposal {
            campaign_id: "fixture-rsi-1".to_owned(),
            parent_candidate_ids: BTreeSet::new(),
            base_commit: "b".repeat(40),
            base_tree: "c".repeat(40),
            proposer: "fixture-proposer".to_owned(),
            proposal_policy_sha256: "d".repeat(64),
            adapter_sha256: "e".repeat(64),
            hypothesis: Hypothesis {
                mechanism: "replace repeated linear lookup with indexed lookup".to_owned(),
                expected_effects: vec!["fewer comparisons".to_owned()],
                possible_regressions: vec!["higher retained memory".to_owned()],
                decisive_falsifiers: vec!["correctness fixture differs".to_owned()],
            },
            changed_paths: BTreeSet::from(["src/search.rs".to_owned()]),
            changed_symbols: BTreeSet::from(["search".to_owned()]),
            semantic_class: "lookup-index".to_owned(),
            differentiator: None,
        }
    }

    #[test]
    fn identity_binds_exact_material_and_negative_semantics() {
        let material = |body: &[u8]| {
            CandidateMaterial::from_changes(vec![GitChange {
                operation: GitChangeOperation::Modify,
                path: "src/search.rs".to_owned(),
                old: Some(GitFileIdentity {
                    mode: GitFileMode::Regular,
                    sha256: sha256(b"old"),
                }),
                new: Some(GitFileContent::from_bytes(GitFileMode::Regular, body)),
            }])
            .unwrap()
            .canonical_bytes()
            .unwrap()
        };
        let first = bind_candidate(proposal(), material(b"new")).expect("first candidate");
        let same = bind_candidate(proposal(), material(b"new")).expect("same candidate");
        let changed =
            bind_candidate(proposal(), material(b"different")).expect("changed candidate");
        assert_eq!(first.candidate_id, same.candidate_id);
        assert_eq!(first.negative_fingerprint, changed.negative_fingerprint);
        assert_ne!(first.candidate_id, changed.candidate_id);
        assert_ne!(first.material_sha256, changed.material_sha256);
    }

    #[test]
    fn hypotheses_need_falsifiers_and_paths_are_confined() {
        let mut missing_falsifier = proposal();
        missing_falsifier.hypothesis.decisive_falsifiers.clear();
        assert!(bind_legacy_patch_candidate(missing_falsifier, b"patch".to_vec()).is_err());

        let mut traversal = proposal();
        traversal.changed_paths = BTreeSet::from(["../evaluator.rs".to_owned()]);
        assert!(bind_legacy_patch_candidate(traversal, b"patch".to_vec()).is_err());
    }

    #[test]
    fn git_change_set_refuses_mode_drift_and_noncanonical_bytes() {
        let material = CandidateMaterial::from_changes(vec![GitChange {
            operation: GitChangeOperation::Modify,
            path: "src/search.rs".to_owned(),
            old: Some(GitFileIdentity {
                mode: GitFileMode::Regular,
                sha256: sha256(b"old"),
            }),
            new: Some(GitFileContent::from_bytes(GitFileMode::Executable, b"new")),
        }]);
        assert!(
            material
                .unwrap_err()
                .to_string()
                .contains("executable mode")
        );

        let canonical = CandidateMaterial::from_changes(Vec::new())
            .unwrap()
            .canonical_bytes()
            .unwrap();
        let mut noncanonical = canonical;
        noncanonical.push(b'\n');
        assert!(
            CandidateMaterial::parse_canonical(&noncanonical)
                .unwrap_err()
                .to_string()
                .contains("canonical compact JSON")
        );
    }
}
