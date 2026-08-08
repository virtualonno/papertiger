//! Repeatable public-API dogfood of durable paired authority against Contextmink.
//!
//! The campaign deliberately uses a tracked synthetic score fixture. It is a
//! lifecycle/authority probe, not a Contextmink performance result.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use papertiger_mise::manifest::{
    AdapterBinding, CalibrationRequirements, CampaignManifest, CandidateMaterialContract,
    ContainmentGrade, CumulativeBudgetCaps, EvaluatorBinding, ExecutionLimits, GenerationBinding,
    GitObjectFormat, HoldoutDisclosure, HoldoutPolicy, HoldoutTier, HoldoutTierKind,
    KnownBadCalibration, MutationScope, NetworkPolicy, NoOpCalibration, ObjectiveDirection,
    ObjectiveRole, ObjectiveSpec, Sha256Digest, StopRules,
};
use papertiger_mise::{
    BudgetLimit, BudgetRequest, BudgetResource, CandidateProposal, FIXTURE_BUNDLE_SCHEMA_V1,
    FixtureBundleDescriptor, FixtureBundleEntry, GIT_CHANGE_SET_MEDIA_TYPE,
    GIT_CHANGE_SET_PROTOCOL_V1, Hypothesis, PAIRED_ADAPTER_BINDING_SCHEMA_V1,
    PAIRED_ANALYSIS_SCHEMA_V2, PAIRED_MEASUREMENT_PROTOCOL_V1, PairedAdapterBinding,
    PairedAnalysisMethod, PairedAnalysisPlan, PairedCalibrationFixtureBindings, PairedCohort,
    PairedCohortAdjudication, PairedExecutionParticipant, PairedExecutionParticipants,
    PairedFixtureBinding, PairedObjectivePolicy, PairedRunOutcome, PairedSlotSeedCommitment,
    PreparePairedCohortSpec, RationalThreshold, adjudicate_paired_cohort, admit_verified_campaign,
    bind_candidate, budget_balances, build_git_change_set_material, execute_next_paired_run, init,
    inspect_source_binding, materialize_candidate, open_for_init, prepare_paired_cohort,
    preserve_object, record_candidate, reserve_budget, reserve_paired_analysis_slot,
    verify_campaign_admission,
};
use serde_json::json;

const ACTOR: &str = "contextmink-paired-dogfood";
const CAMPAIGN_ID: &str = "contextmink-paired-synthetic-a01";
const SCORE_PATH: &str = ".mise-paired-score";

struct SeedMaterial {
    no_op: [u8; 32],
    known_bad: [u8; 32],
    research: [u8; 32],
    workloads: Vec<String>,
}

#[derive(Parser)]
struct Args {
    /// Canonical Contextmink repository. Only its committed HEAD is cloned.
    #[arg(long)]
    contextmink: PathBuf,
    /// New state directory; the dogfood refuses to overwrite any prior evidence.
    #[arg(long)]
    state: PathBuf,
}

struct CandidateFixture {
    candidate: papertiger_mise::BoundCandidate,
    worktree: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.state.exists() {
        bail!(
            "dogfood state already exists at {}; choose a new path so prior evidence is preserved",
            args.state.display()
        );
    }
    let source_input = std::fs::canonicalize(&args.contextmink)
        .with_context(|| format!("canonicalize Contextmink {}", args.contextmink.display()))?;
    require_git_clean_head_exists(&source_input)?;
    std::fs::create_dir_all(&args.state)?;
    let state = std::fs::canonicalize(&args.state)?;
    let source = state.join("source");
    let control = state.join("control");
    let objects = state.join("objects");
    let runs = state.join("runs");
    std::fs::create_dir_all(&runs)?;
    git_external(&[
        "clone",
        "--no-hardlinks",
        &portable(&source_input)?,
        &portable_lexical(&source)?,
    ])?;
    git(&source, &["config", "core.autocrlf", "false"])?;
    git(&source, &["config", "core.eol", "lf"])?;

    let fixtures = source.join("fixtures/mise");
    std::fs::create_dir_all(&fixtures)?;
    let adapter_bytes =
        br#"{"kind":"contextmink-synthetic-score-adapter","performance_claim":false}"#;
    let evaluator_bytes = br#"{"kind":"paired-only-placeholder"}"#;
    let no_op_fixture = br#"{"cohort":"no-op","schema":"contextmink.synthetic-fixture.v1"}"#;
    let known_bad_fixture =
        br#"{"cohort":"known-bad","schema":"contextmink.synthetic-fixture.v1"}"#;
    let exploration_fixture =
        br#"{"cohort":"research","schema":"contextmink.synthetic-fixture.v1"}"#;
    let sampling_protocol = br#"{"independence":"the score outcome is constant over the population, so threshold indicators are degenerate independent Bernoulli variables","population":"all 256-bit synthetic workload labels","sampling":"one independent OS-CSPRNG draw per frozen block after candidate identity","schema":"contextmink.synthetic-sampling.v1"}"#;
    let order_seed_protocol = br#"{"commit":"sha256(OS-CSPRNG seed bytes)","generation":"after candidate identity","reveal":"only in the admitted cohort","schema":"papertiger-mise.order-seed-protocol.v1"}"#;
    for (name, bytes) in [
        ("adapter.json", adapter_bytes.as_slice()),
        ("evaluator.json", evaluator_bytes.as_slice()),
        ("calibration-no-op.json", no_op_fixture.as_slice()),
        ("calibration-known-bad.json", known_bad_fixture.as_slice()),
        ("exploration.json", exploration_fixture.as_slice()),
        ("sampling-protocol.json", sampling_protocol.as_slice()),
        ("order-seed-protocol.json", order_seed_protocol.as_slice()),
    ] {
        std::fs::write(fixtures.join(name), bytes)?;
    }
    std::fs::write(source.join(SCORE_PATH), b"10000\n")?;

    let no_op_sha = digest(no_op_fixture);
    let known_bad_fixture_sha = digest(known_bad_fixture);
    let exploration_sha = digest(exploration_fixture);
    let bundle = FixtureBundleDescriptor {
        schema: FIXTURE_BUNDLE_SCHEMA_V1.to_owned(),
        fixtures: required_fixture_entries(&no_op_sha, &known_bad_fixture_sha, &exploration_sha),
    };
    let bundle_bytes = bundle.canonical_bytes()?;
    std::fs::write(fixtures.join("bundle.json"), &bundle_bytes)?;
    git(&source, &["add", "--all"])?;
    git_commit(&source, "add frozen Mise paired dogfood fixtures")?;
    let source_binding = inspect_source_binding(&source)?;
    if source_binding.git_object_format != GitObjectFormat::Sha1 {
        bail!("dogfood currently expects Contextmink's SHA-1 Git object format");
    }

    let proposal_policy = br#"{"policy":"external-agent-proposals-only","schema":"contextmink.synthetic-proposal-policy.v1"}"#;
    let policy_sha = digest(proposal_policy);
    let adapter_sha = digest(adapter_bytes);
    let known_bad_patch = score_patch(20_000);
    let research_patch = score_patch(8_000);
    let no_op = candidate_fixture(
        &source_binding,
        &policy_sha,
        &adapter_sha,
        Vec::new(),
        "calibration-no-op",
        "An empty patch establishes exact flatness of the tracked synthetic score.",
        state.join("runs/no-op"),
    )?;
    let known_bad = candidate_fixture(
        &source_binding,
        &policy_sha,
        &adapter_sha,
        known_bad_patch,
        "calibration-known-bad",
        "A tracked score of 20000 is an intentional direction-normalized regression.",
        state.join("runs/known-bad"),
    )?;
    let research = candidate_fixture(
        &source_binding,
        &policy_sha,
        &adapter_sha,
        research_patch,
        "synthetic-score-improvement",
        "A tracked score of 8000 is a deterministic practical improvement probe.",
        state.join("runs/research"),
    )?;
    let seeds = seed_material()?;

    let judge = std::fs::canonicalize(std::env::current_exe()?)?;
    let adapter_executable = adapter_executable(&judge)?;
    let judge_locator = portable(&judge)?;
    let adapter_locator = portable(&adapter_executable)?;
    let trial_adapter = PairedAdapterBinding {
        schema: PAIRED_ADAPTER_BINDING_SCHEMA_V1.to_owned(),
        executable_locator: adapter_locator.clone(),
        executable_sha256: Sha256Digest(digest(&std::fs::read(&adapter_executable)?)),
        argv: vec![adapter_locator],
        working_directory: source_binding.repository_locator.clone(),
        environment: BTreeMap::from([
            (
                "MISE_BASELINE_ID".to_owned(),
                no_op.candidate.candidate_id.clone(),
            ),
            (
                "MISE_BASELINE_ROOT".to_owned(),
                portable_lexical(&no_op.worktree)?,
            ),
            (
                "MISE_KNOWN_BAD_ID".to_owned(),
                known_bad.candidate.candidate_id.clone(),
            ),
            (
                "MISE_KNOWN_BAD_ROOT".to_owned(),
                portable_lexical(&known_bad.worktree)?,
            ),
            (
                "MISE_RESEARCH_ID".to_owned(),
                research.candidate.candidate_id.clone(),
            ),
            (
                "MISE_RESEARCH_ROOT".to_owned(),
                portable_lexical(&research.worktree)?,
            ),
        ]),
        result_schema: "contextmink.synthetic-paired-trial-result.v1".to_owned(),
        maximum_wall_time_ms: 5_000,
        maximum_output_bytes: 1_048_576,
    };
    let objectives = objectives();
    let manifest = CampaignManifest {
        schema: "papertiger-mise.campaign.v1".to_owned(),
        campaign_id: CAMPAIGN_ID.to_owned(),
        source: source_binding.clone(),
        mutation_scope: MutationScope {
            allowlist: vec![SCORE_PATH.to_owned()],
            protected_paths: vec!["fixtures/mise".to_owned()],
        },
        candidate_material: Some(CandidateMaterialContract {
            kind: "git_change_set".to_owned(),
            protocol: GIT_CHANGE_SET_PROTOCOL_V1.to_owned(),
            media_type: GIT_CHANGE_SET_MEDIA_TYPE.to_owned(),
        }),
        adapter: AdapterBinding {
            name: "contextmink-synthetic-score".to_owned(),
            protocol: "papertiger-mise.adapter.v1".to_owned(),
            implementation_locator: "fixtures/mise/adapter.json".to_owned(),
            implementation_sha256: Sha256Digest(adapter_sha.clone()),
        },
        evaluator: EvaluatorBinding {
            argv: vec![
                judge_locator.clone(),
                "fixtures/mise/evaluator.json".to_owned(),
            ],
            working_directory: ".".to_owned(),
            environment: BTreeMap::new(),
            launcher_locator: judge_locator.clone(),
            launcher_sha256: Sha256Digest(digest(&std::fs::read(&judge)?)),
            evaluator_locator: "fixtures/mise/evaluator.json".to_owned(),
            evaluator_sha256: Sha256Digest(digest(evaluator_bytes)),
            fixture_bundle_locator: "fixtures/mise/bundle.json".to_owned(),
            fixture_bundle_sha256: Sha256Digest(digest(&bundle_bytes)),
            protocol: PAIRED_MEASUREMENT_PROTOCOL_V1.to_owned(),
            rust_build_environment: None,
            judge_build: None,
        },
        execution_limits: ExecutionLimits {
            workspace_root_locator: portable_lexical(&runs)?,
            runtime_root_locator: None,
            maximum_trial_wall_time_ms: 5_000,
            maximum_trial_output_bytes: 1_048_576,
            network: NetworkPolicy::Unrestricted,
        },
        objectives: objectives.clone(),
        paired_analysis: Some(paired_plan(
            trial_adapter,
            &no_op_sha,
            &known_bad_fixture_sha,
            &exploration_sha,
            sampling_protocol,
            order_seed_protocol,
            &seeds,
        )),
        budgets: CumulativeBudgetCaps {
            caps: vec![
                BudgetLimit {
                    resource: BudgetResource::Candidates,
                    hard_limit: 3,
                },
                BudgetLimit {
                    resource: BudgetResource::Trials,
                    hard_limit: 48,
                },
                BudgetLimit {
                    resource: BudgetResource::Failures,
                    hard_limit: 3,
                },
                BudgetLimit {
                    resource: BudgetResource::HoldoutDisclosures,
                    hard_limit: 16,
                },
                BudgetLimit {
                    resource: BudgetResource::WallTimeMilliseconds,
                    hard_limit: 300_000,
                },
                BudgetLimit {
                    resource: BudgetResource::DiskBytesWritten,
                    hard_limit: 512 * 1024 * 1024,
                },
                BudgetLimit {
                    resource: BudgetResource::ArtifactBytes,
                    hard_limit: 4 * 1024 * 1024,
                },
            ],
        },
        stop_rules: StopRules {
            not_before_unix_ms: 1,
            deadline_unix_ms: 4_000_000_000_000,
            max_consecutive_infrastructure_failures: 1,
            max_trials_without_qualified_improvement: 16,
            stop_on_evaluator_integrity_failure: true,
        },
        containment: ContainmentGrade::WorkspaceOnly,
        containment_requirement: None,
        holdouts: HoldoutPolicy {
            disclosure_cap: 16,
            tiers: vec![HoldoutTier {
                key: "exploration".to_owned(),
                kind: HoldoutTierKind::Exploration,
                fixture_locator: "fixtures/mise/exploration.json".to_owned(),
                fixture_sha256: Sha256Digest(exploration_sha),
                maximum_disclosures: 16,
                minimum_repetitions: 16,
                disclosure: HoldoutDisclosure::Aggregate,
            }],
        },
        generation: GenerationBinding {
            runtime_generation: 1,
            recursion_depth: 0,
            maximum_recursion_depth: 3,
            outer_judge_executable_sha256: Sha256Digest(digest(&std::fs::read(&judge)?)),
            outer_judge_executable_locator: judge_locator,
            proposal_policy_sha256: Sha256Digest(policy_sha),
            proposal_policy_locator: "policy.json".to_owned(),
            parent_campaign: None,
        },
        calibration: CalibrationRequirements {
            no_op: NoOpCalibration {
                candidate_patch_sha256: None,
                candidate_material_sha256: Some(Sha256Digest(
                    no_op.candidate.material_sha256.clone(),
                )),
                fixture_locator: "fixtures/mise/calibration-no-op.json".to_owned(),
                fixture_sha256: Sha256Digest(no_op_sha),
                minimum_repetitions: 16,
            },
            known_bad: KnownBadCalibration {
                candidate_patch_sha256: None,
                candidate_material_sha256: Some(Sha256Digest(
                    known_bad.candidate.material_sha256.clone(),
                )),
                fixture_locator: "fixtures/mise/calibration-known-bad.json".to_owned(),
                fixture_sha256: Sha256Digest(known_bad_fixture_sha),
                minimum_repetitions: 16,
                expected_rejection_code: "known-regression".to_owned(),
            },
        },
    };
    manifest.validate()?;

    std::fs::create_dir_all(&control)?;
    git(&control, &["init"])?;
    git(&control, &["config", "core.autocrlf", "false"])?;
    std::fs::write(control.join("policy.json"), proposal_policy)?;
    let manifest_path = control.join("campaign.json");
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    git(&control, &["add", "--all"])?;
    git_commit(&control, "freeze Contextmink paired dogfood campaign")?;
    let verified = verify_campaign_admission(&manifest_path)?;

    let database = state.join("mise.sqlite");
    let connection = open_for_init(&database)?;
    init(&connection)?;
    admit_verified_campaign(&connection, ACTOR, &verified)?;
    for fixture in [&no_op, &known_bad, &research] {
        record_and_materialize(&connection, &objects, fixture)?;
    }

    let baseline_revision = materialization_revision(&connection, &objects, &no_op)?;
    let known_bad_revision = materialization_revision(&connection, &objects, &known_bad)?;
    let research_revision = materialization_revision(&connection, &objects, &research)?;
    let no_op_record = run_cohort(
        &connection,
        &objects,
        "contextmink-no-op",
        &no_op,
        PairedCohort::NoOpCalibration,
        &seeds.no_op,
        &no_op.candidate.candidate_id,
        &baseline_revision,
        &baseline_revision,
    )?;
    let known_bad_record = run_cohort(
        &connection,
        &objects,
        "contextmink-known-bad",
        &known_bad,
        PairedCohort::KnownBadCalibration,
        &seeds.known_bad,
        &no_op.candidate.candidate_id,
        &baseline_revision,
        &known_bad_revision,
    )?;
    reserve_paired_analysis_slot(
        &connection,
        ACTOR,
        CAMPAIGN_ID,
        &research.candidate.candidate_id,
        1,
        &seeds.research,
    )?;
    let research_record = run_cohort(
        &connection,
        &objects,
        "contextmink-research",
        &research,
        PairedCohort::Research {
            candidate_analysis_slot: 1,
        },
        &seeds.research,
        &no_op.candidate.candidate_id,
        &baseline_revision,
        &research_revision,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "campaign_id": CAMPAIGN_ID,
            "state": portable(&state)?,
            "source_original_head": git_text(&source_input, &["rev-parse", "HEAD^{commit}"] )?,
            "source_campaign_commit": manifest.source.base_commit,
            "no_op": no_op_record,
            "known_bad": known_bad_record,
            "research": research_record,
            "budgets": budget_balances(&connection, CAMPAIGN_ID)?,
            "claim": "durable paired lifecycle proof over a synthetic tracked Contextmink fixture; not a performance result",
        }))?
    );
    Ok(())
}

fn required_fixture_entries(
    no_op: &str,
    known_bad: &str,
    exploration: &str,
) -> Vec<FixtureBundleEntry> {
    let mut entries = vec![
        fixture_entry(
            "calibration.known_bad",
            "fixtures/mise/calibration-known-bad.json",
            known_bad,
        ),
        fixture_entry(
            "calibration.no_op",
            "fixtures/mise/calibration-no-op.json",
            no_op,
        ),
        fixture_entry(
            "holdout.exploration",
            "fixtures/mise/exploration.json",
            exploration,
        ),
    ];
    entries.extend((0..8).map(|block| {
        fixture_entry(
            &format!("paired.block.{block}"),
            "fixtures/mise/exploration.json",
            exploration,
        )
    }));
    entries
}

fn fixture_entry(key: &str, locator: &str, sha256: &str) -> FixtureBundleEntry {
    FixtureBundleEntry {
        key: key.to_owned(),
        locator: locator.to_owned(),
        sha256: Sha256Digest(sha256.to_owned()),
    }
}

fn objectives() -> Vec<ObjectiveSpec> {
    vec![
        ObjectiveSpec {
            key: "correct".to_owned(),
            role: ObjectiveRole::HardConstraint,
            direction: ObjectiveDirection::Maximize,
            unit: "boolean".to_owned(),
            minimum_practical_change: 0.0,
            regression_tolerance: 0.0,
            acceptance_threshold: Some(1.0),
            target_value: None,
        },
        ObjectiveSpec {
            key: "frame-ms".to_owned(),
            role: ObjectiveRole::Primary,
            direction: ObjectiveDirection::Minimize,
            unit: "synthetic-units".to_owned(),
            minimum_practical_change: 1.0,
            regression_tolerance: 0.5,
            acceptance_threshold: None,
            target_value: None,
        },
        ObjectiveSpec {
            key: "memory-mib".to_owned(),
            role: ObjectiveRole::Protected,
            direction: ObjectiveDirection::Minimize,
            unit: "synthetic-units".to_owned(),
            minimum_practical_change: 0.0,
            regression_tolerance: 2.0,
            acceptance_threshold: None,
            target_value: None,
        },
    ]
}

fn paired_plan(
    trial_adapter: PairedAdapterBinding,
    no_op: &str,
    known_bad: &str,
    exploration: &str,
    sampling_protocol: &[u8],
    order_seed_protocol: &[u8],
    seeds: &SeedMaterial,
) -> PairedAnalysisPlan {
    PairedAnalysisPlan {
        schema: PAIRED_ANALYSIS_SCHEMA_V2.to_owned(),
        method: PairedAnalysisMethod::FixedSampleExactPairedBinomial,
        trial_adapter: Some(trial_adapter),
        inference_scope:
            papertiger_mise::statistics::PairedInferenceScope::IndependentBlockPopulation {
                population: "uniform-256-bit-contextmink-synthetic-workload-labels".to_owned(),
                sampling_protocol_locator: "fixtures/mise/sampling-protocol.json".to_owned(),
                sampling_protocol_sha256: Sha256Digest(digest(sampling_protocol)),
            },
        required_blocks: 8,
        order_seed_protocol_locator: "fixtures/mise/order-seed-protocol.json".to_owned(),
        order_seed_protocol_sha256: Sha256Digest(digest(order_seed_protocol)),
        order_seed_commitments: vec![PairedSlotSeedCommitment {
            candidate_analysis_slot: 1,
            sha256: Sha256Digest(digest(&seeds.research)),
        }],
        calibration_seed_commitments:
            papertiger_mise::statistics::PairedCalibrationSeedCommitments {
                no_op_sha256: Sha256Digest(digest(&seeds.no_op)),
                known_bad_sha256: Sha256Digest(digest(&seeds.known_bad)),
            },
        campaign_familywise_alpha: RationalThreshold {
            numerator: 1,
            denominator: 20,
        },
        maximum_candidate_analyses: 1,
        objective_policies: vec![
            PairedObjectivePolicy {
                objective: "correct".to_owned(),
                scale10: 0,
                measurement_summary_protocol: "all-pass.v1".to_owned(),
                minimum_practical_change_units: 0,
                regression_tolerance_units: 0,
                acceptance_threshold_units: Some(1),
                target_value_units: None,
                no_op_maximum_absolute_effect_units: 0,
            },
            PairedObjectivePolicy {
                objective: "frame-ms".to_owned(),
                scale10: 3,
                measurement_summary_protocol: "synthetic-point.v1".to_owned(),
                minimum_practical_change_units: 1_000,
                regression_tolerance_units: 500,
                acceptance_threshold_units: None,
                target_value_units: None,
                no_op_maximum_absolute_effect_units: 0,
            },
            PairedObjectivePolicy {
                objective: "memory-mib".to_owned(),
                scale10: 3,
                measurement_summary_protocol: "synthetic-point.v1".to_owned(),
                minimum_practical_change_units: 0,
                regression_tolerance_units: 2_000,
                acceptance_threshold_units: None,
                target_value_units: None,
                no_op_maximum_absolute_effect_units: 0,
            },
        ],
        calibration_fixtures: PairedCalibrationFixtureBindings {
            no_op: PairedFixtureBinding {
                locator: "fixtures/mise/calibration-no-op.json".to_owned(),
                sha256: Sha256Digest(no_op.to_owned()),
            },
            known_bad: PairedFixtureBinding {
                locator: "fixtures/mise/calibration-known-bad.json".to_owned(),
                sha256: Sha256Digest(known_bad.to_owned()),
            },
        },
        blocks: seeds
            .workloads
            .iter()
            .enumerate()
            .map(
                |(block_index, workload_seed)| papertiger_mise::PairedBlockDesign {
                    block_index: u32::try_from(block_index).expect("eight workload seeds fit u32"),
                    stratum: if block_index % 2 == 0 { "even" } else { "odd" }.to_owned(),
                    fixture_locator: "fixtures/mise/exploration.json".to_owned(),
                    fixture_sha256: Sha256Digest(exploration.to_owned()),
                    environment_profile_sha256: Sha256Digest(digest(
                        b"contextmink.synthetic-environment.v1",
                    )),
                    workload_seed: workload_seed.clone(),
                },
            )
            .collect(),
    }
}

fn seed_material() -> Result<SeedMaterial> {
    let no_op = random_seed()?;
    let known_bad = random_seed()?;
    let research = random_seed()?;
    let workloads = (0..8)
        .map(|_| random_seed().map(|seed| lower_hex(&seed)))
        .collect::<Result<Vec<_>>>()?;
    Ok(SeedMaterial {
        no_op,
        known_bad,
        research,
        workloads,
    })
}

fn random_seed() -> Result<[u8; 32]> {
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(|error| anyhow!("obtain OS random seed: {error}"))?;
    Ok(seed)
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

fn candidate_fixture(
    source: &papertiger_mise::manifest::SourceBinding,
    policy_sha: &str,
    adapter_sha: &str,
    patch: Vec<u8>,
    semantic_class: &str,
    mechanism: &str,
    worktree: PathBuf,
) -> Result<CandidateFixture> {
    let changed = if patch.is_empty() {
        BTreeSet::new()
    } else {
        BTreeSet::from([SCORE_PATH.to_owned()])
    };
    let authoring_name = format!(
        "{}-authoring",
        worktree
            .file_name()
            .and_then(|name| name.to_str())
            .context("candidate worktree has no UTF-8 name")?
    );
    let authoring = worktree.with_file_name(authoring_name);
    git(
        Path::new(&source.repository_locator),
        &[
            "worktree",
            "add",
            "--detach",
            authoring.to_str().context("authoring path is not UTF-8")?,
            &source.base_commit,
        ],
    )?;
    if !patch.is_empty() {
        git_apply(&authoring, &patch)?;
    }
    let result_tree = git_text(&authoring, &["write-tree"])?;
    let material = build_git_change_set_material(
        Path::new(&source.repository_locator),
        &source.base_tree,
        &result_tree,
    )?;
    let candidate = bind_candidate(
        CandidateProposal {
            campaign_id: CAMPAIGN_ID.to_owned(),
            parent_candidate_ids: BTreeSet::new(),
            base_commit: source.base_commit.clone(),
            base_tree: source.base_tree.clone(),
            proposer: ACTOR.to_owned(),
            proposal_policy_sha256: policy_sha.to_owned(),
            adapter_sha256: adapter_sha.to_owned(),
            hypothesis: Hypothesis {
                mechanism: mechanism.to_owned(),
                expected_effects: vec![
                    "The frozen paired classifier reports the predeclared direction.".to_owned(),
                ],
                possible_regressions: vec![
                    "Adapter, schedule, or CAS drift invalidates the cohort.".to_owned(),
                ],
                decisive_falsifiers: vec![
                    "Any result differs from the frozen synthetic score contract.".to_owned(),
                ],
            },
            changed_paths: changed,
            changed_symbols: BTreeSet::new(),
            semantic_class: semantic_class.to_owned(),
            differentiator: None,
        },
        material,
    )?;
    Ok(CandidateFixture {
        candidate,
        worktree,
    })
}

fn record_and_materialize(
    connection: &rusqlite::Connection,
    objects: &Path,
    fixture: &CandidateFixture,
) -> Result<()> {
    let candidate_reservation = format!("candidate-{}", &fixture.candidate.candidate_id[..12]);
    reserve_budget(
        connection,
        ACTOR,
        CAMPAIGN_ID,
        &candidate_reservation,
        &[
            BudgetRequest::new(BudgetResource::Candidates, 1)?,
            BudgetRequest::new(
                BudgetResource::ArtifactBytes,
                u64::try_from(fixture.candidate.material_bytes.len())?.max(1),
            )?,
        ],
    )?;
    let material_object = preserve_object(objects, &fixture.candidate.material_bytes)?;
    record_candidate(
        connection,
        ACTOR,
        objects,
        &candidate_reservation,
        &fixture.candidate,
        &material_object,
    )?;
    let materialization_reservation =
        format!("materialize-{}", &fixture.candidate.candidate_id[..12]);
    reserve_budget(
        connection,
        ACTOR,
        CAMPAIGN_ID,
        &materialization_reservation,
        &[
            BudgetRequest::new(BudgetResource::DiskBytesWritten, 128 * 1024 * 1024)?,
            BudgetRequest::new(BudgetResource::ArtifactBytes, 1_048_576)?,
        ],
    )?;
    materialize_candidate(
        connection,
        ACTOR,
        objects,
        &materialization_reservation,
        &fixture.candidate.candidate_id,
        &fixture.worktree,
    )?;
    Ok(())
}

fn materialization_revision(
    connection: &rusqlite::Connection,
    objects: &Path,
    fixture: &CandidateFixture,
) -> Result<String> {
    let reservation = format!("materialize-{}", &fixture.candidate.candidate_id[..12]);
    Ok(materialize_candidate(
        connection,
        ACTOR,
        objects,
        &reservation,
        &fixture.candidate.candidate_id,
        &fixture.worktree,
    )?
    .result_tree)
}

#[allow(clippy::too_many_arguments)]
fn run_cohort(
    connection: &rusqlite::Connection,
    objects: &Path,
    cohort_id: &str,
    candidate: &CandidateFixture,
    cohort: PairedCohort,
    seed: &[u8],
    baseline_id: &str,
    baseline_revision: &str,
    candidate_revision: &str,
) -> Result<serde_json::Value> {
    let spec = PreparePairedCohortSpec {
        cohort_id: cohort_id.to_owned(),
        campaign_id: CAMPAIGN_ID.to_owned(),
        candidate_id: candidate.candidate.candidate_id.clone(),
        cohort,
        revealed_order_seed: seed.to_vec(),
        participants: PairedExecutionParticipants {
            baseline: PairedExecutionParticipant {
                identity_sha256: Sha256Digest(baseline_id.to_owned()),
                revision: baseline_revision.to_owned(),
            },
            candidate: PairedExecutionParticipant {
                identity_sha256: Sha256Digest(candidate.candidate.candidate_id.clone()),
                revision: candidate_revision.to_owned(),
            },
        },
        reservation_id: format!("cohort-{cohort_id}"),
    };
    prepare_paired_cohort(connection, ACTOR, objects, &spec)?;
    loop {
        if matches!(
            execute_next_paired_run(connection, ACTOR, objects, cohort_id)?,
            PairedRunOutcome::ReadyForAdjudication
        ) {
            break;
        }
    }
    let (record, adjudication) = adjudicate_paired_cohort(connection, ACTOR, objects, cohort_id)?;
    match (&cohort, &adjudication) {
        (PairedCohort::NoOpCalibration, PairedCohortAdjudication::NoOpCalibration(result))
            if result.passed => {}
        (
            PairedCohort::KnownBadCalibration,
            PairedCohortAdjudication::KnownBadCalibration(result),
        ) if result.disposition == papertiger_mise::PairedDisposition::Rejected => {}
        (PairedCohort::Research { .. }, PairedCohortAdjudication::Research(result))
            if result.disposition == papertiger_mise::PairedDisposition::Qualified => {}
        _ => bail!("paired dogfood cohort reached an unexpected adjudication: {adjudication:?}"),
    }
    Ok(json!({ "record": record, "adjudication": adjudication }))
}

fn score_patch(score: i64) -> Vec<u8> {
    format!("diff --git a/{SCORE_PATH} b/{SCORE_PATH}\n--- a/{SCORE_PATH}\n+++ b/{SCORE_PATH}\n@@ -1 +1 @@\n-10000\n+{score}\n").into_bytes()
}

fn adapter_executable(judge: &Path) -> Result<PathBuf> {
    let suffix = std::env::consts::EXE_SUFFIX;
    let path = judge
        .parent()
        .context("dogfood executable has no parent")?
        .join(format!("paired_fixture_adapter{suffix}"));
    std::fs::canonicalize(&path).with_context(|| {
        format!(
            "missing adapter {}; run `cargo build -p papertiger-mise --examples` first",
            path.display()
        )
    })
}

fn require_git_clean_head_exists(repository: &Path) -> Result<()> {
    git_text(repository, &["rev-parse", "HEAD^{commit}"])?;
    Ok(())
}

fn git_commit(repository: &Path, message: &str) -> Result<()> {
    git(
        repository,
        &[
            "-c",
            "user.name=Mise Dogfood",
            "-c",
            "user.email=mise-dogfood.invalid",
            "commit",
            "-m",
            message,
        ],
    )
}

fn git(repository: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()?;
    if !output.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn git_apply(repository: &Path, patch: &[u8]) -> Result<()> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["apply", "--index", "--whitespace=nowarn", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("Git apply has no stdin")?
        .write_all(patch)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "Git apply failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn git_external(args: &[&str]) -> Result<()> {
    let output = Command::new("git").args(args).output()?;
    if !output.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn git_text(repository: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()?;
    if !output.status.success() {
        bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn portable(path: &Path) -> Result<String> {
    portable_lexical(&std::fs::canonicalize(path)?)
}

fn portable_lexical(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let display = absolute.to_string_lossy();
    Ok(display
        .strip_prefix(r"\\?\")
        .unwrap_or(&display)
        .replace('\\', "/"))
}

fn digest(bytes: &[u8]) -> String {
    papertiger_mise::sha256(bytes)
}
