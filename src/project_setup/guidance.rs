use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::Path;

#[cfg(test)]
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::Serialize;

use super::receipt::SkillTarget;
use super::{INSTALL_RECEIPT_PATH, load_running_project_receipt, normalized_path};

const GUIDANCE_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
pub(super) const MAX_GUIDANCE_FILE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GuidanceClassification {
    Missing,
    Unreadable,
    RefusedNonRegular,
    RefusedOversized,
    RefusedInvalidUtf8,
    StaleShellLauncher,
    SelectedSkillTrigger,
    PapertigerSkillTrigger,
    AgentsGuidanceReference,
    IntegrationPointerOnly,
    PapertigerMentionOnly,
    NoDiscoverableTrigger,
}

impl GuidanceClassification {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::RefusedNonRegular => "refused_non_regular",
            Self::RefusedOversized => "refused_oversized",
            Self::RefusedInvalidUtf8 => "refused_invalid_utf8",
            Self::StaleShellLauncher => "stale_shell_launcher",
            Self::SelectedSkillTrigger => "selected_skill_trigger",
            Self::PapertigerSkillTrigger => "papertiger_skill_trigger",
            Self::AgentsGuidanceReference => "agents_guidance_reference",
            Self::IntegrationPointerOnly => "integration_pointer_only",
            Self::PapertigerMentionOnly => "papertiger_mention_only",
            Self::NoDiscoverableTrigger => "no_discoverable_trigger",
        }
    }

    fn content_inspected(self) -> bool {
        !matches!(
            self,
            Self::Missing
                | Self::Unreadable
                | Self::RefusedNonRegular
                | Self::RefusedOversized
                | Self::RefusedInvalidUtf8
        )
    }

    fn inspection_complete(self) -> bool {
        !matches!(
            self,
            Self::Unreadable
                | Self::RefusedNonRegular
                | Self::RefusedOversized
                | Self::RefusedInvalidUtf8
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GuidanceFileInspection {
    pub(crate) path: String,
    pub(crate) classification: GuidanceClassification,
    pub(crate) content_inspected: bool,
    pub(crate) bytes: Option<u64>,
    pub(crate) sha256: Option<String>,
    pub(crate) matched_selected_skill_paths: Vec<String>,
    pub(crate) papertiger_skill_trigger_observed: bool,
    pub(crate) integration_pointer_observed: bool,
    pub(crate) agents_guidance_reference_observed: bool,
    pub(crate) stale_shell_launcher_terms: Vec<String>,
    pub(crate) corrective_trigger: String,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct GuidancePairInspection {
    pub(crate) comparable: bool,
    pub(crate) byte_identical: Option<bool>,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProjectGuidanceInspection {
    pub(crate) schema: &'static str,
    pub(crate) project_root: String,
    pub(crate) skill_targets: Vec<SkillTarget>,
    pub(crate) selected_skill_paths: Vec<String>,
    pub(crate) scope: &'static str,
    pub(crate) max_file_bytes: u64,
    pub(crate) inspection_complete: bool,
    pub(crate) files: Vec<GuidanceFileInspection>,
    pub(crate) pair: GuidancePairInspection,
    pub(crate) limitations: Vec<&'static str>,
}

struct InspectedFile {
    public: GuidanceFileInspection,
    bytes: Option<Vec<u8>>,
}

pub(crate) fn inspect_installed_project_guidance(
    project_root: &Path,
) -> Result<ProjectGuidanceInspection> {
    let root = fs::canonicalize(project_root).with_context(|| {
        format!(
            "resolve project guidance root {}; pass an installed project directory",
            project_root.display()
        )
    })?;
    if !root.is_dir() {
        return Err(anyhow!(
            "project guidance root is not a directory: {}; pass an installed project directory",
            root.display()
        ));
    }
    let receipt = load_running_project_receipt(&root)?.ok_or_else(|| {
        anyhow!(
            "no project-install receipt was found at {}; pass the exact installed project root, restore its receipt if prior planning existed, or inspect a first installation with: papertiger setup-project \"{}\" --dry-run --json",
            root.join(INSTALL_RECEIPT_PATH).display(),
            root.display()
        )
    })?;
    Ok(inspect_project_guidance(&root, &receipt.skill_targets))
}

pub(super) fn inspect_project_guidance(
    root: &Path,
    skill_targets: &[SkillTarget],
) -> ProjectGuidanceInspection {
    let selected_skill_paths = skill_targets
        .iter()
        .map(|target| target.managed_path().to_owned())
        .collect::<Vec<_>>();
    let inspected = GUIDANCE_FILES
        .iter()
        .map(|path| inspect_file(root, path, skill_targets, &selected_skill_paths))
        .collect::<Vec<_>>();
    let pair = compare_pair(&inspected);
    let inspection_complete = inspected
        .iter()
        .all(|file| file.public.classification.inspection_complete());

    ProjectGuidanceInspection {
        schema: "papertiger.project_guidance.v1",
        project_root: normalized_path(root),
        skill_targets: skill_targets.to_vec(),
        selected_skill_paths,
        scope: "repository-root AGENTS.md and CLAUDE.md only",
        max_file_bytes: MAX_GUIDANCE_FILE_BYTES,
        inspection_complete,
        files: inspected.into_iter().map(|file| file.public).collect(),
        pair,
        limitations: vec![
            "Classifications are lexical observations, not proof that an agent will load or follow the referenced skill.",
            "Only repository-root AGENTS.md and CLAUDE.md are inspected; nested guidance, imports, harness trust, and runtime discovery remain outside this result.",
            "Each regular file is read once with a fixed byte cap; concurrent same-length rewrites may require a rerun for a fresh observation.",
            "An AGENTS.md reference in CLAUDE.md is reported as indirection and is never promoted into semantic equivalence.",
        ],
    }
}

fn inspect_file(
    root: &Path,
    relative: &str,
    skill_targets: &[SkillTarget],
    selected_skill_paths: &[String],
) -> InspectedFile {
    let path = root.join(relative);
    let corrective_trigger = corrective_trigger(relative, skill_targets);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return uninspected(
                relative,
                GuidanceClassification::Missing,
                None,
                corrective_trigger,
                "Repository guidance file is absent; absence is fully observed for this root.",
            );
        }
        Err(error) => {
            return uninspected(
                relative,
                GuidanceClassification::Unreadable,
                None,
                corrective_trigger,
                format!("Could not inspect repository guidance metadata: {error}"),
            );
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return uninspected(
            relative,
            GuidanceClassification::RefusedNonRegular,
            Some(metadata.len()),
            corrective_trigger,
            "Repository guidance is not a regular non-symlink file; content was not followed outside the project boundary.",
        );
    }
    if metadata.len() > MAX_GUIDANCE_FILE_BYTES {
        return uninspected(
            relative,
            GuidanceClassification::RefusedOversized,
            Some(metadata.len()),
            corrective_trigger,
            format!(
                "Repository guidance exceeds the fixed {}-byte inspection cap; narrow or split the always-loaded guidance before rerunning.",
                MAX_GUIDANCE_FILE_BYTES
            ),
        );
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize + 1);
    let read_result = File::open(&path).and_then(|file| {
        file.take(MAX_GUIDANCE_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
    });
    if let Err(error) = read_result {
        return uninspected(
            relative,
            GuidanceClassification::Unreadable,
            Some(metadata.len()),
            corrective_trigger,
            format!("Could not read repository guidance within the fixed cap: {error}"),
        );
    }
    if bytes.len() as u64 > MAX_GUIDANCE_FILE_BYTES {
        return uninspected(
            relative,
            GuidanceClassification::RefusedOversized,
            Some(bytes.len() as u64),
            corrective_trigger,
            format!(
                "Repository guidance grew beyond the fixed {}-byte inspection cap while it was read; stabilize or narrow it before rerunning.",
                MAX_GUIDANCE_FILE_BYTES
            ),
        );
    }

    let sha256 = papertiger::sha256(&bytes);
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) => {
            return InspectedFile {
                public: GuidanceFileInspection {
                    path: relative.to_owned(),
                    classification: GuidanceClassification::RefusedInvalidUtf8,
                    content_inspected: false,
                    bytes: Some(bytes.len() as u64),
                    sha256: Some(sha256),
                    matched_selected_skill_paths: Vec::new(),
                    papertiger_skill_trigger_observed: false,
                    integration_pointer_observed: false,
                    agents_guidance_reference_observed: false,
                    stale_shell_launcher_terms: Vec::new(),
                    corrective_trigger,
                    detail: format!(
                        "Repository guidance is not valid UTF-8 at byte {}; lexical content was not inspected.",
                        error.valid_up_to()
                    ),
                },
                bytes: Some(bytes),
            };
        }
    };

    let normalized = normalize_text(text);
    let paragraphs = normalized.split("\n\n").collect::<Vec<_>>();
    let normalized_selected_skill_paths = selected_skill_paths
        .iter()
        .map(|path| path.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let matched_selected_skill_paths = selected_skill_paths
        .iter()
        .zip(&normalized_selected_skill_paths)
        .filter(|(_, normalized_path)| normalized.contains(normalized_path.as_str()))
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let selected_skill_trigger = paragraphs.iter().any(|paragraph| {
        normalized_selected_skill_paths
            .iter()
            .any(|path| paragraph.contains(path))
            && has_trigger_verb(paragraph)
            && has_scope_boundary(paragraph)
    });
    let papertiger_skill_trigger_observed = paragraphs.iter().any(|paragraph| {
        paragraph.contains("papertiger skill")
            && has_trigger_verb(paragraph)
            && has_scope_boundary(paragraph)
    });
    let integration_pointer_observed = normalized.contains("tools/papertiger/agent_integration.md");
    let agents_guidance_reference_observed = relative == "CLAUDE.md"
        && (normalized.contains("follow `agents.md`")
            || normalized.contains("read `agents.md`")
            || normalized.contains("follow agents.md")
            || normalized.contains("read agents.md"));
    let stale_shell_launcher_terms = stale_launcher_terms(&normalized);
    let papertiger_mentioned = normalized.contains("papertiger");

    let classification = if !stale_shell_launcher_terms.is_empty() {
        GuidanceClassification::StaleShellLauncher
    } else if selected_skill_trigger {
        GuidanceClassification::SelectedSkillTrigger
    } else if papertiger_skill_trigger_observed {
        GuidanceClassification::PapertigerSkillTrigger
    } else if agents_guidance_reference_observed {
        GuidanceClassification::AgentsGuidanceReference
    } else if integration_pointer_observed {
        GuidanceClassification::IntegrationPointerOnly
    } else if papertiger_mentioned {
        GuidanceClassification::PapertigerMentionOnly
    } else {
        GuidanceClassification::NoDiscoverableTrigger
    };
    let detail = classification_detail(classification);

    InspectedFile {
        public: GuidanceFileInspection {
            path: relative.to_owned(),
            classification,
            content_inspected: classification.content_inspected(),
            bytes: Some(bytes.len() as u64),
            sha256: Some(sha256),
            matched_selected_skill_paths,
            papertiger_skill_trigger_observed,
            integration_pointer_observed,
            agents_guidance_reference_observed,
            stale_shell_launcher_terms,
            corrective_trigger,
            detail: detail.to_owned(),
        },
        bytes: Some(bytes),
    }
}

fn uninspected(
    relative: &str,
    classification: GuidanceClassification,
    bytes: Option<u64>,
    corrective_trigger: String,
    detail: impl Into<String>,
) -> InspectedFile {
    InspectedFile {
        public: GuidanceFileInspection {
            path: relative.to_owned(),
            classification,
            content_inspected: false,
            bytes,
            sha256: None,
            matched_selected_skill_paths: Vec::new(),
            papertiger_skill_trigger_observed: false,
            integration_pointer_observed: false,
            agents_guidance_reference_observed: false,
            stale_shell_launcher_terms: Vec::new(),
            corrective_trigger,
            detail: detail.into(),
        },
        bytes: None,
    }
}

fn compare_pair(files: &[InspectedFile]) -> GuidancePairInspection {
    let [agents, claude] = files else {
        unreachable!("guidance inspection always has AGENTS.md and CLAUDE.md")
    };
    match (&agents.bytes, &claude.bytes) {
        (Some(agents), Some(claude)) => {
            let identical = agents == claude;
            GuidancePairInspection {
                comparable: true,
                byte_identical: Some(identical),
                detail: if identical {
                    "AGENTS.md and CLAUDE.md are byte-identical within the inspected scope."
                        .to_owned()
                } else {
                    "AGENTS.md and CLAUDE.md differ; this reports bytes only and does not infer whether their instructions are equivalent."
                        .to_owned()
                },
            }
        }
        _ => GuidancePairInspection {
            comparable: false,
            byte_identical: None,
            detail: "Both guidance files must be read within the fixed cap before byte identity is comparable."
                .to_owned(),
        },
    }
}

fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn has_trigger_verb(paragraph: &str) -> bool {
    paragraph
        .split(|character: char| !character.is_ascii_alphabetic())
        .any(|word| matches!(word, "read" | "load" | "follow"))
}

fn has_scope_boundary(paragraph: &str) -> bool {
    [
        "multi-outcome",
        "separate-commit",
        "separate commit",
        "durable task",
        "multiple independently reviewable outcomes",
    ]
    .iter()
    .any(|term| paragraph.contains(term))
}

fn stale_launcher_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let prose = text.replace("\n\n", ". ").replace('\n', " ");
    for term in [
        "scripts/papertiger",
        "papertiger launcher",
        "project launcher",
        "shell launcher",
    ] {
        if term_occurs_positively(&prose, term) {
            terms.push(term.to_owned());
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

fn term_occurs_positively(line: &str, term: &str) -> bool {
    line.match_indices(term).any(|(index, _)| {
        let before = &line[..index];
        let after = &line[index + term.len()..];
        let clause_start = before
            .rfind(['.', ';', ':'])
            .map_or(0, |boundary| boundary + 1);
        let clause_end = after
            .find(['.', ';', ':'])
            .map_or(line.len(), |boundary| index + term.len() + boundary);
        let clause = &line[clause_start..clause_end];
        ![
            "do not",
            "never",
            "without",
            "retired",
            "obsolete",
            "deprecated",
            "remove",
        ]
        .iter()
        .any(|marker| clause.contains(marker))
    })
}

fn corrective_trigger(relative: &str, skill_targets: &[SkillTarget]) -> String {
    let preferred = match relative {
        "AGENTS.md" => skill_targets
            .iter()
            .find(|target| **target == SkillTarget::Agents),
        "CLAUDE.md" => skill_targets
            .iter()
            .find(|target| **target == SkillTarget::Claude),
        _ => None,
    }
    .or_else(|| skill_targets.first());
    let selected = preferred.map_or("tools/papertiger/agent_integration.md", |target| {
        target.managed_path()
    });
    format!(
        "Before the first edit or commit on multi-outcome or separate-commit work, or work matching an existing durable task, read `{selected}` completely and follow it. Skip new bounded edits or read-only reviews without a durable outcome, intermediate steps, and domain-owned or shared-team lifecycle. Resume existing durable work even for a small step."
    )
}

fn classification_detail(classification: GuidanceClassification) -> &'static str {
    match classification {
        GuidanceClassification::StaleShellLauncher => {
            "Positive shell-launcher wording was observed in Papertiger guidance; the current contract requires direct native-binary invocation."
        }
        GuidanceClassification::SelectedSkillTrigger => {
            "One paragraph contains a selected skill path, a read/load/follow verb, and a durable-work scope term."
        }
        GuidanceClassification::PapertigerSkillTrigger => {
            "One paragraph tells the reader to load or read a Papertiger skill for durable-work scope, but it does not bind an exact selected skill path."
        }
        GuidanceClassification::AgentsGuidanceReference => {
            "CLAUDE.md refers the reader to AGENTS.md; inspect both file results because this command does not infer semantic inheritance."
        }
        GuidanceClassification::IntegrationPointerOnly => {
            "The canonical integration document is referenced without an observed durable-work skill trigger."
        }
        GuidanceClassification::PapertigerMentionOnly => {
            "Papertiger is mentioned, but no selected-path or generic durable-work skill trigger was observed."
        }
        GuidanceClassification::NoDiscoverableTrigger => {
            "No Papertiger trigger, canonical integration pointer, or Papertiger mention was observed."
        }
        GuidanceClassification::Missing
        | GuidanceClassification::Unreadable
        | GuidanceClassification::RefusedNonRegular
        | GuidanceClassification::RefusedOversized
        | GuidanceClassification::RefusedInvalidUtf8 => {
            unreachable!("uninspected classifications supply their own detail")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "papertiger-guidance-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create guidance fixture");
        root
    }

    #[test]
    fn classifies_selected_trigger_and_agents_indirection_without_equating_them() {
        let root = fixture("selected-and-indirect");
        fs::write(
            root.join("AGENTS.md"),
            "Before the first edit on multi-outcome work, read `.agents/skills/papertiger/SKILL.md` completely.\n",
        )
        .unwrap();
        fs::write(root.join("CLAUDE.md"), "Follow `AGENTS.md`.\n").unwrap();

        let result = inspect_project_guidance(&root, &[SkillTarget::Agents, SkillTarget::Claude]);
        assert_eq!(
            result.files[0].classification,
            GuidanceClassification::SelectedSkillTrigger
        );
        assert_eq!(
            result.files[1].classification,
            GuidanceClassification::AgentsGuidanceReference
        );
        assert!(result.files[1].agents_guidance_reference_observed);
        assert_eq!(result.pair.byte_identical, Some(false));
        assert!(result.inspection_complete);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn classifications_keep_launcher_pointer_skill_and_mentions_distinct() {
        let root = fixture("classifications");
        fs::write(
            root.join("AGENTS.md"),
            "Use the project-local Papertiger launcher after reading tools/papertiger/agent_integration.md.\n",
        )
        .unwrap();
        fs::write(
            root.join("CLAUDE.md"),
            "Before separate commits, load the project Papertiger skill completely.\n",
        )
        .unwrap();
        let result = inspect_project_guidance(&root, &[SkillTarget::Agents]);
        assert_eq!(
            result.files[0].classification,
            GuidanceClassification::StaleShellLauncher
        );
        assert_eq!(
            result.files[1].classification,
            GuidanceClassification::PapertigerSkillTrigger
        );

        fs::write(
            root.join("AGENTS.md"),
            "Planning reference: tools/papertiger/agent_integration.md\n",
        )
        .unwrap();
        fs::write(root.join("CLAUDE.md"), "Papertiger owns planning.\n").unwrap();
        let result = inspect_project_guidance(&root, &[SkillTarget::Agents]);
        assert_eq!(
            result.files[0].classification,
            GuidanceClassification::IntegrationPointerOnly
        );
        assert_eq!(
            result.files[1].classification,
            GuidanceClassification::PapertigerMentionOnly
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_and_oversized_files_are_bounded_and_explicit() {
        let root = fixture("bounded");
        fs::write(
            root.join("AGENTS.md"),
            vec![b'x'; MAX_GUIDANCE_FILE_BYTES as usize + 1],
        )
        .unwrap();
        let result = inspect_project_guidance(&root, &[]);
        assert_eq!(
            result.files[0].classification,
            GuidanceClassification::RefusedOversized
        );
        assert_eq!(
            result.files[1].classification,
            GuidanceClassification::Missing
        );
        assert!(!result.inspection_complete);
        assert!(!result.pair.comparable);
        assert!(
            result.files[1]
                .corrective_trigger
                .contains("tools/papertiger/agent_integration.md")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identical_files_are_compared_by_bytes() {
        let root = fixture("identical");
        let content = b"Before a durable task, read `.agents/skills/papertiger/SKILL.md`.\n";
        fs::write(root.join("AGENTS.md"), content).unwrap();
        fs::write(root.join("CLAUDE.md"), content).unwrap();
        let result = inspect_project_guidance(&root, &[SkillTarget::Agents]);
        assert_eq!(result.pair.byte_identical, Some(true));
        assert_eq!(result.files[0].sha256, result.files[1].sha256);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn negated_launcher_language_is_not_reported_as_stale() {
        let root = fixture("negated-launcher");
        fs::write(
            root.join("AGENTS.md"),
            "Before multi-outcome work, read `.agents/skills/papertiger/SKILL.md`. Do\nnot route Papertiger through a shell launcher.\n",
        )
        .unwrap();
        let result = inspect_project_guidance(&root, &[SkillTarget::Agents]);
        assert_eq!(
            result.files[0].classification,
            GuidanceClassification::SelectedSkillTrigger
        );
        assert!(result.files[0].stale_shell_launcher_terms.is_empty());

        fs::write(
            root.join("AGENTS.md"),
            "Do not edit the receipt by hand; use the project launcher for Papertiger.\n",
        )
        .unwrap();
        let result = inspect_project_guidance(&root, &[SkillTarget::Agents]);
        assert_eq!(
            result.files[0].classification,
            GuidanceClassification::StaleShellLauncher
        );
        assert_eq!(
            result.files[0].stale_shell_launcher_terms,
            ["project launcher"]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
