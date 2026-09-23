use std::io::Write as _;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use papertiger_mise::budget::{BudgetRequest, BudgetResource, BudgetSettlement, SettlementMode};
use papertiger_mise::cancellation::{
    CancellationTarget, cancellation_request, request_cancellation,
};
use papertiger_mise::improvement;
use papertiger_mise::manifest::{CampaignManifest, Sha256Digest};
use papertiger_mise::{
    AuthorityInitOutcome, CandidateProposal, ContainmentPolicy, DerivePairedNominationSpec,
    DomainShadowAdapterBinding, FIXTURE_BUNDLE_SCHEMA_V2, FixtureBundleDescriptor,
    FixtureBundleEntry, PairedAdapterBinding, PreparePairedCohortSpec, PreservedObject,
    PromotionGateBinding, SupervisedTrialSpec, abandon_materialization_attempt,
    abandon_owned_trial, adjudicate_deterministic_candidate, adjudicate_paired_cohort,
    admit_verified_campaign, admit_verified_successor, authority_status, bind_candidate,
    budget_balances, build_git_change_set_material, campaign, candidate,
    derive_candidate_planner_projection, derive_nomination_planner_projection,
    derive_paired_nomination, derive_promotion_proof, domain_shadow, execute_next_paired_execution,
    execute_workspace_trial, historical_shadow, host_execution_status, init_at,
    inspect_source_binding, materialize_candidate, nominations, object_locator, open_existing,
    open_existing_read_only, open_for_init, paired_cohort, paired_cohorts, paired_execution,
    paired_executions, portable_absolute, preflight_campaign_admission, prepare_paired_cohort,
    preserve_object, preserve_parent_promotion_proof, read_object, record_candidate,
    record_domain_shadow, record_historical_shadow, recover_paired_execution,
    recover_workspace_trial, reserve_budget, reserve_paired_analysis_slot, settle_budget, sha256,
    successor_admission, trial, verify_campaign_admission, verify_nomination_integrity,
    verify_parent_promotion_gate, verify_promotion_gate, verify_successor_admission,
};
use serde::Serialize;

mod why_input;
use why_input::WhyArgs;

#[derive(Parser)]
#[command(
    name = "papertiger-mise",
    version,
    about = "Evidence-controlled candidate evaluation campaigns"
)]
struct Cli {
    /// Consuming project root. Every relative database, object, manifest, and
    /// workspace path resolves from this directory.
    #[arg(long, global = true, value_name = "DIR")]
    project_root: Option<PathBuf>,
    /// Mise authority database, relative to the project root.
    #[arg(long, default_value = "state/papertiger-mise.sqlite")]
    db: PathBuf,
    /// Event author recorded on every mutation.
    #[arg(long, env = "PAPERTIGER_ACTOR", default_value = "operator")]
    actor: String,
    #[command(subcommand)]
    command: Command,
}

/// Content-addressed object root shared by every command that reads or writes CAS evidence.
const OBJECTS_HELP: &str = "Content-addressed object root, relative to the project root";

/// Independent Papertiger planning database whose closed gate is read, never written.
const PAPERTIGER_DB_HELP: &str = "Papertiger planning database holding the independently closed gate; opened read-only. Relative paths resolve from the project root";

#[derive(Subcommand)]
enum Command {
    /// Print the bundled agent workflow, or with --reference the complete MISE.md reference.
    Guide {
        /// Print the complete MISE.md operating reference instead of the workflow.
        #[arg(long)]
        reference: bool,
    },
    /// Read-only orientation over the project-owned campaign authority.
    Status {
        /// Emit the project status as JSON.
        #[arg(long)]
        json: bool,
        /// Object root to inspect. Required with a custom database; presence
        /// does not prove that its content matches the authority's CAS objects.
        #[arg(long)]
        objects: Option<PathBuf>,
    },
    /// Print host supervision capabilities as JSON: the portable local-supervision
    /// contract, plus native cleanup-backend diagnostics that never affect admission,
    /// classification, or nomination.
    ExecutionStatus,
    /// Explicitly create or migrate the independent Mise authority.
    Init,
    /// Prepare and validate non-admitted campaign inputs without opening authority state.
    #[command(subcommand)]
    Improvement(ImprovementCommand),
    /// Admit and inspect immutable campaign definitions and successor lineage.
    #[command(subcommand)]
    Campaign(CampaignCommand),
    /// Reserve, settle, release, and inspect finite campaign resources.
    #[command(subcommand)]
    Budget(BudgetCommand),
    /// Record, materialize, adjudicate, and inspect exact candidate changes.
    #[command(subcommand)]
    Candidate(CandidateCommand),
    /// Run, cancel, recover, abandon, and inspect deterministic candidate trials.
    #[command(subcommand)]
    Trial(TrialCommand),
    /// Prepare, execute, cancel, recover, and adjudicate predeclared paired-analysis cohorts.
    #[command(subcommand)]
    Paired(PairedCommand),
    /// Reverify and print one content-addressed campaign object.
    #[command(subcommand)]
    Object(ObjectCommand),
    /// Record and inspect decision-ineligible historical or domain observations.
    #[command(subcommand)]
    Evidence(EvidenceCommand),
    /// List and rederive nominations; derive and verify successor and promotion proofs.
    #[command(subcommand)]
    Promotion(PromotionCommand),
    /// Export terminal candidate or nomination evidence as a planner projection document.
    #[command(subcommand)]
    Projection(ProjectionCommand),
}

#[derive(Subcommand)]
enum ImprovementCommand {
    /// List the built-in versioned paradigm registry.
    Paradigms {
        /// Emit the registry schema, digest, and templates as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show one exact built-in paradigm template.
    Show {
        /// Paradigm key, as listed by `improvement paradigms`.
        key: String,
    },
    /// Validate an external paradigm registry file without mutating campaign state.
    VerifyRegistry {
        /// Registry JSON file to validate.
        file: PathBuf,
    },
    /// Validate a read-first project improvement brief as planning input only.
    VerifyBrief {
        /// Brief JSON file to validate.
        file: PathBuf,
    },
    /// Compile an approved brief into a non-admitted campaign draft.
    Compile {
        /// Project improvement brief JSON file.
        #[arg(long)]
        brief: PathBuf,
        /// Operator approval JSON binding the exact brief bytes.
        #[arg(long)]
        approval: PathBuf,
        /// New draft file to write; an existing path is refused.
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Serialize)]
struct ProjectStatus {
    schema: &'static str,
    version: &'static str,
    project_root: String,
    database: String,
    object_store: String,
    initialized: bool,
    object_store_present: bool,
    object_store_check: &'static str,
    authority: Option<papertiger_mise::AuthorityStatus>,
    corrective_command: Option<String>,
}

#[derive(Subcommand)]
enum ProjectionCommand {
    /// Open the authority read-only, reopen the exact candidate material and relied-upon
    /// CAS evidence, rederive budgets, and emit one planner projection document.
    Export {
        /// Nomination to export.
        #[arg(
            long,
            required_unless_present = "candidate",
            conflicts_with = "candidate"
        )]
        nomination: Option<String>,
        /// Terminal non-nominated candidate to export.
        #[arg(
            long,
            required_unless_present = "nomination",
            conflicts_with = "nomination"
        )]
        candidate: Option<String>,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
        /// Write the exact projection to a new file instead of emitting it on stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum CampaignCommand {
    /// Discover recorded work in bounded live pages; does not reverify CAS or execution readiness.
    Inspect {
        /// Admitted campaign ID.
        campaign_id: String,
        /// Page section: candidates, trials, cohorts, or reservations.
        #[arg(long, default_value = "candidates")]
        section: papertiger_mise::inspection::InspectionSection,
        /// Page size, 1-100.
        #[arg(long, default_value_t = 20)]
        limit: u64,
        /// Entities to skip; use the emitted continuation arguments instead of computing it.
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// Inspect the exact clean Git source binding used to author a manifest.
    SourceBinding {
        /// Clean Git repository to bind.
        repository: PathBuf,
    },
    /// Report every independently checkable admission defect without touching an authority.
    Preflight {
        /// Campaign manifest JSON file.
        manifest: PathBuf,
    },
    /// Validate, canonicalize, and immutably admit a tracked campaign manifest.
    Admit {
        /// Campaign manifest JSON file.
        manifest: PathBuf,
    },
    /// Admit a descendant after rederiving its parent proof and independent gate.
    AdmitSuccessor {
        /// Successor campaign manifest JSON file.
        manifest: PathBuf,
        /// Parent nomination whose proof the successor relies on.
        #[arg(long)]
        parent_nomination: String,
        /// JSON binding naming the closed Papertiger gate (task, gate, evidence, sha256).
        #[arg(long)]
        gate_binding: PathBuf,
        #[arg(long, default_value = "state/papertiger.sqlite", help = PAPERTIGER_DB_HELP)]
        papertiger_db: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Read one exact admitted campaign.
    Show {
        /// Admitted campaign ID.
        campaign_id: String,
    },
    /// Read one immutable successor admission and parent-ledger receipt.
    ShowSuccessor {
        /// Admitted successor campaign ID.
        campaign_id: String,
    },
    /// Write an exact canonical fixture bundle from repository files.
    FixtureBundle {
        /// Repository containing the fixture files.
        repository: PathBuf,
        /// Bundle descriptor file to write.
        output: PathBuf,
        /// Fixture as key=repository-relative-locator; repeat for each fixture.
        #[arg(long = "entry", required = true)]
        entries: Vec<String>,
    },
}

#[derive(Subcommand)]
enum BudgetCommand {
    /// Reserve cumulative resources before any candidate side effect.
    Reserve {
        /// Admitted campaign ID.
        campaign_id: String,
        /// New reservation ID; released or settled IDs cannot be reused.
        reservation_id: String,
        /// Resource amount as resource=integer; repeat per resource. Resources: candidates,
        /// trials, failures, holdout_disclosures, wall_time_milliseconds, cpu_time_milliseconds,
        /// gpu_time_milliseconds, disk_bytes_written, network_bytes, artifact_bytes,
        /// model_tokens, cost_microunits.
        #[arg(long = "amount", required = true)]
        amounts: Vec<String>,
    },
    /// Settle measured use, or conservatively charge the full reservation.
    Settle {
        /// Admitted campaign ID.
        campaign_id: String,
        /// Reservation to settle.
        reservation_id: String,
        /// Measured use as resource=integer; repeat per resource (same names as `budget reserve`).
        #[arg(long = "amount")]
        amounts: Vec<String>,
        /// Charge every reserved amount in full instead of measured use.
        #[arg(long, conflicts_with = "amounts")]
        charge_reservation: bool,
        /// Settlement note recorded with the ledger entry.
        #[arg(long)]
        note: Option<String>,
    },
    /// Display the cumulative ledger for one campaign.
    Show {
        /// Admitted campaign ID.
        campaign_id: String,
    },
    /// Release every resource at zero only if no lifecycle operation bound it.
    Release {
        /// Admitted campaign ID.
        campaign_id: String,
        /// Unbound reservation to release.
        reservation_id: String,
        #[command(flatten)]
        why: WhyArgs,
    },
}

#[derive(Subcommand)]
enum CandidateCommand {
    /// Build canonical Git change-set material from two exact trees.
    BuildMaterial {
        /// Git repository containing both trees.
        #[arg(long)]
        repository: PathBuf,
        /// Base tree ID (the campaign's frozen base tree).
        #[arg(long)]
        base_tree: String,
        /// Result tree ID containing the candidate change.
        #[arg(long)]
        result_tree: String,
        /// Material file to write.
        #[arg(long)]
        output: PathBuf,
    },
    /// Bind and durably record one typed proposal plus exact candidate material.
    Record {
        /// Candidate proposal JSON file.
        #[arg(long)]
        proposal: PathBuf,
        /// Canonical material file from `candidate build-material`.
        #[arg(long)]
        material: PathBuf,
        /// Reservation charged for the candidate.
        #[arg(long)]
        reservation: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Materialize one durable candidate into its exact confined worktree.
    Materialize {
        /// Recorded candidate ID.
        candidate_id: String,
        /// Reservation charged for materialization.
        #[arg(long)]
        reservation: String,
        /// New detached worktree path.
        #[arg(long)]
        worktree: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Charge and close an interrupted materialization before retrying it.
    AbandonMaterialization {
        /// Candidate whose interrupted materialization is closed.
        candidate_id: String,
        /// Reservation bound by the interrupted attempt.
        #[arg(long)]
        reservation: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Read one exact durable candidate.
    Show {
        /// Recorded candidate ID.
        candidate_id: String,
    },
    /// Derive a terminal deterministic result and optional nomination.
    Adjudicate {
        /// Recorded candidate ID.
        candidate_id: String,
    },
}

#[derive(Subcommand)]
enum TrialCommand {
    /// Ask the live supervisor to stop a launched trial and retain failure evidence.
    Cancel {
        /// Launched trial ID.
        trial_id: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Execute one typed deterministic trial through the owned supervisor.
    Run {
        /// Supervised trial spec JSON file.
        #[arg(long)]
        spec: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Settle a succeeded trial whose CAS receipt reverifies but whose reservation is still
    /// open, or record OS-observed absence of a launched trial's process and charge it.
    Recover {
        /// Trial ID to recover.
        trial_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Charge and retire an ambiguous pre-launch trial intent without claiming process absence.
    Abandon {
        /// Owned (never launched) trial ID.
        trial_id: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Read one exact durable trial, including terminal evidence pointers.
    Show {
        /// Trial ID.
        trial_id: String,
    },
}

#[derive(Subcommand)]
enum PairedCommand {
    /// Ask the live supervisor to stop a launched execution and conservatively settle its cohort.
    Cancel {
        /// Launched paired execution ID.
        execution_id: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Irrevocably bind one research candidate to a finite confirmation slot.
    ReserveSlot {
        /// Admitted campaign ID.
        campaign_id: String,
        /// Research candidate ID.
        candidate_id: String,
        /// Research slot index.
        slot: u32,
        /// File containing the committed order-seed reveal bytes.
        #[arg(long)]
        seed: PathBuf,
    },
    /// Freeze every ordered request and reserve the complete cohort before launch.
    Prepare {
        /// Paired cohort spec JSON file.
        #[arg(long)]
        spec: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Execute exactly the next predeclared execution, or report adjudication readiness.
    ExecuteNext {
        /// Prepared cohort ID.
        cohort_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Reopen every execution's request, result, and receipts from CAS, classify the cohort
    /// under the admitted paired-analysis plan, and record its terminal result.
    Adjudicate {
        /// Cohort whose every execution succeeded.
        cohort_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Derive one development nomination from exact CAS-reverified paired cohorts.
    DeriveNomination {
        /// Qualified research cohort ID.
        research_cohort_id: String,
        /// Adjudicated no-op calibration cohort ID.
        #[arg(long)]
        no_op: String,
        /// Adjudicated known-bad calibration cohort ID.
        #[arg(long)]
        known_bad: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Record OS-observed absence of a launched execution's birth-bound process and fail its cohort.
    Recover {
        /// Launched paired execution ID.
        execution_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Read one durable cohort and its terminal evidence pointers.
    ShowCohort {
        /// Cohort ID.
        cohort_id: String,
    },
    /// Enumerate every durable cohort in one campaign.
    ListCohorts {
        /// Admitted campaign ID.
        campaign_id: String,
    },
    /// Read one durable paired execution, its evidence pointers, and any cancellation request.
    ShowExecution {
        /// Paired execution ID.
        execution_id: String,
    },
    /// Enumerate every predeclared execution in exact cohort order.
    ListExecutions {
        /// Cohort ID.
        cohort_id: String,
    },
}

#[derive(Subcommand)]
enum ObjectCommand {
    /// Reverify and emit exact CAS bytes for one typed object pointer.
    Read {
        /// Object SHA-256.
        sha256: String,
        /// Exact object length in bytes.
        bytes: u64,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
}

#[derive(Subcommand)]
enum EvidenceCommand {
    /// Execute and retain one read-only observation with unchanged domain state.
    #[command(name = "domain-shadow")]
    RecordDomain {
        /// Domain-shadow adapter binding JSON file.
        #[arg(long)]
        binding: PathBuf,
        /// Request JSON passed to the adapter on stdin.
        #[arg(long)]
        request: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Reopen one exact, permanently decision-ineligible domain shadow.
    #[command(name = "show-domain-shadow")]
    ReadDomain {
        /// Domain-shadow evidence ID.
        evidence_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Execute and retain adapter-backed historical evidence without decision authority.
    #[command(name = "historical-shadow")]
    RecordHistorical {
        /// Paired adapter binding JSON file.
        #[arg(long)]
        binding: PathBuf,
        /// Request JSON passed to the adapter on stdin.
        #[arg(long)]
        request: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Reopen one exact historical-shadow receipt and all of its CAS objects.
    #[command(name = "show-historical-shadow")]
    ReadHistorical {
        /// Historical-shadow evidence ID.
        evidence_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
}

#[derive(Subcommand)]
enum PromotionCommand {
    /// Enumerate durable nominations for operator review.
    List {
        /// Restrict the list to one campaign.
        #[arg(long)]
        campaign: Option<String>,
    },
    /// Rederive one nomination from its retained CAS evidence and print the verified result.
    Rederive {
        /// Nomination ID.
        nomination_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Preserve the successor-only parent promotion proof for independent gate review.
    DeriveParent {
        /// Parent nomination ID.
        #[arg(long)]
        nomination: String,
        /// Successor campaign manifest JSON file.
        #[arg(long)]
        successor_manifest: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Run a read-only parent promotion proof preflight against an independent gate.
    VerifyParent {
        #[arg(long, default_value = "state/papertiger.sqlite", help = PAPERTIGER_DB_HELP)]
        papertiger_db: PathBuf,
        /// Parent nomination ID.
        #[arg(long)]
        nomination: String,
        /// Successor campaign manifest JSON file.
        #[arg(long)]
        successor_manifest: PathBuf,
        /// Papertiger task number owning the gate.
        #[arg(long)]
        task: i64,
        /// Closed gate name.
        #[arg(long)]
        gate: String,
        /// Evidence locator recorded on the gate.
        #[arg(long)]
        evidence: String,
        /// Evidence SHA-256 recorded on the gate.
        #[arg(long)]
        sha256: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
    },
    /// Unavailable until sealed confirmation attestation lands: always refuses, because no
    /// current trial receipt is genuinely verdict-only.
    Derive {
        /// Nomination ID.
        #[arg(long)]
        nomination: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
        /// Operator-owned canonical containment policy JSON (papertiger-mise.containment_policy.v3).
        #[arg(long)]
        containment_policy: PathBuf,
    },
    /// Unavailable until sealed confirmation attestation lands: always refuses, because the
    /// production proof it verifies cannot yet be derived.
    Verify {
        #[arg(long, default_value = "state/papertiger.sqlite", help = PAPERTIGER_DB_HELP)]
        papertiger_db: PathBuf,
        /// Nomination ID.
        #[arg(long)]
        nomination: String,
        /// Papertiger task number owning the gate.
        #[arg(long)]
        task: i64,
        /// Closed gate name.
        #[arg(long)]
        gate: String,
        /// Evidence locator recorded on the gate.
        #[arg(long)]
        evidence: String,
        /// Evidence SHA-256 recorded on the gate.
        #[arg(long)]
        sha256: String,
        #[arg(long, default_value = "state/papertiger-mise-objects", help = OBJECTS_HELP)]
        objects: PathBuf,
        /// Operator-owned canonical containment policy JSON (papertiger-mise.containment_policy.v3); it must
        /// not come from the candidate repository.
        #[arg(long)]
        containment_policy: PathBuf,
    },
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("error: {}", render_error(&error));
        std::process::exit(1);
    }
}

/// Render an error chain as `{:#}` does, except that an authority trigger's
/// `RAISE(ABORT, ...)` refusal ends the chain: its message is the complete
/// refusal, and SQLite's generic constraint code beneath it adds no input.
fn render_error(error: &anyhow::Error) -> String {
    let mut rendered = Vec::new();
    for cause in error.chain() {
        rendered.push(cause.to_string());
        if let Some(rusqlite::Error::SqliteFailure(failure, Some(_))) =
            cause.downcast_ref::<rusqlite::Error>()
            && failure.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
        {
            break;
        }
    }
    rendered.join(": ")
}

fn run(cli: Cli) -> Result<()> {
    let project_root = bind_project_root(cli.project_root.as_deref())?;
    match cli.command {
        Command::Guide { reference } => {
            print!(
                "{}",
                if reference {
                    include_str!("../../../MISE.md")
                } else {
                    include_str!("../agent_guide.md")
                }
            );
        }
        Command::Campaign(CampaignCommand::Inspect {
            campaign_id,
            section,
            limit,
            offset,
        }) => {
            let connection = open_existing_read_only(&cli.db)?;
            let inspection = papertiger_mise::inspection::inspect_campaign(
                &connection,
                &campaign_id,
                section,
                limit,
                offset,
            )?;
            let mut output = serde_json::to_value(&inspection)?;
            let prefix = vec![
                "--project-root".to_owned(),
                portable_absolute(&project_root)?,
                "--db".to_owned(),
                portable_absolute(&absolute_from(&project_root, &cli.db))?,
            ];
            output["command_prefix_arguments"] = serde_json::to_value(&prefix)?;
            output["continuation_arguments"] =
                serde_json::to_value(inspection.next_offset.map(|next| {
                    let mut args = prefix;
                    args.extend([
                        "campaign".to_owned(),
                        "inspect".to_owned(),
                        campaign_id,
                        "--section".to_owned(),
                        section.as_str().to_owned(),
                        "--limit".to_owned(),
                        limit.to_string(),
                        "--offset".to_owned(),
                        next.to_string(),
                    ]);
                    args
                }))?;
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        Command::Improvement(command) => match command {
            ImprovementCommand::Paradigms { json } => {
                let (registry, digest) = improvement::builtin_paradigm_registry()?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema": registry.schema,
                            "sha256": digest,
                            "templates": registry.templates,
                        }))?
                    );
                } else {
                    println!("{} {}", registry.schema, digest);
                    for template in registry.templates {
                        println!("{} v{}", template.key, template.version);
                    }
                }
            }
            ImprovementCommand::Show { key } => {
                let (registry, digest) = improvement::builtin_paradigm_registry()?;
                let template = registry
                    .templates
                    .into_iter()
                    .find(|template| template.key == key)
                    .with_context(|| format!("unknown improvement paradigm '{key}'"))?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "registry_sha256": digest,
                        "template": template,
                    }))?
                );
            }
            ImprovementCommand::VerifyRegistry { file } => {
                let bytes = std::fs::read(&file)
                    .with_context(|| format!("read improvement registry {}", file.display()))?;
                let registry = improvement::validate_paradigm_registry(&bytes)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema": registry.schema,
                        "sha256": improvement::paradigm_registry_sha256(&bytes),
                        "templates": registry.templates.len(),
                        "valid": true,
                    }))?
                );
            }
            ImprovementCommand::VerifyBrief { file } => {
                let bytes = std::fs::read(&file).with_context(|| {
                    format!("read project improvement brief {}", file.display())
                })?;
                let brief = improvement::validate_project_improvement_brief(&bytes)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "authority": brief.authority,
                        "brief_id": brief.brief_id,
                        "evidence_items": brief.evidence.len(),
                        "schema": brief.schema,
                        "template": brief.template,
                        "valid": true,
                    }))?
                );
            }
            ImprovementCommand::Compile {
                brief,
                approval,
                output,
            } => {
                if output.exists() {
                    bail!(
                        "brief compiler output already exists at {}",
                        output.display()
                    );
                }
                let brief_bytes = std::fs::read(&brief).with_context(|| {
                    format!("read project improvement brief {}", brief.display())
                })?;
                let approval_bytes = std::fs::read(&approval).with_context(|| {
                    format!("read project improvement approval {}", approval.display())
                })?;
                let draft =
                    improvement::compile_project_improvement_brief(&brief_bytes, &approval_bytes)?;
                let bytes = serde_json::to_vec_pretty(&draft)?;
                std::fs::write(&output, &bytes).with_context(|| {
                    format!("write non-admitted improvement draft {}", output.display())
                })?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "authority": draft.authority,
                        "brief_sha256": draft.brief_sha256,
                        "output": portable_absolute(&output)?,
                        "schema": draft.schema,
                    }))?
                );
            }
        },
        Command::Status { json, objects } => {
            let database = absolute_from(&project_root, &cli.db);
            let object_store = match objects {
                Some(path) => absolute_from(&project_root, &path),
                None => {
                    let default_database = project_root.join("state/papertiger-mise.sqlite");
                    if portable_absolute(&database)? != portable_absolute(&default_database)? {
                        bail!(
                            "status cannot infer an object store for custom database {}; pass `status --objects <object-root>`",
                            database.display()
                        );
                    }
                    project_root.join("state/papertiger-mise-objects")
                }
            };
            let database_metadata = status_metadata(&database)?;
            let object_metadata = status_metadata(&object_store)?;
            if database_metadata
                .as_ref()
                .is_some_and(|metadata| !metadata.is_file())
            {
                bail!(
                    "Mise database path {} exists but is not a file; pass the intended database with --db",
                    database.display()
                );
            }
            if object_metadata
                .as_ref()
                .is_some_and(|metadata| !metadata.is_dir())
            {
                bail!(
                    "Mise object-store path {} exists but is not a directory; pass `status --objects <object-root>` with the intended directory",
                    object_store.display()
                );
            }
            let initialized = database_metadata.is_some();
            let authority = if initialized {
                let connection = open_existing_read_only(&database)?;
                Some(authority_status(&connection, 10)?)
            } else {
                None
            };
            let project_root_identity = portable_absolute(&project_root)?;
            let database_identity = portable_absolute(&database)?;
            let corrective_command = (!initialized).then(|| {
                format!(
                    "papertiger-mise --project-root \"{}\" --db \"{}\" init",
                    project_root_identity, database_identity
                )
            });
            let status = ProjectStatus {
                schema: "papertiger-mise.project_status.v3",
                version: env!("CARGO_PKG_VERSION"),
                project_root: project_root_identity,
                database: database_identity,
                object_store: portable_absolute(&object_store)?,
                initialized,
                object_store_present: object_metadata.is_some(),
                object_store_check: "directory_presence_only",
                authority,
                corrective_command,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print_project_status(&status);
            }
        }
        Command::ExecutionStatus => {
            println!(
                "{}",
                serde_json::to_string_pretty(&host_execution_status()?)?
            );
        }
        Command::Init => {
            if let Some(parent) = cli.db.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            let connection = open_for_init(&cli.db)?;
            match init_at(&connection, &cli.db)? {
                AuthorityInitOutcome::Created => println!("initialized {}", cli.db.display()),
                AuthorityInitOutcome::Migrated { from, to } => {
                    println!("migrated {} from schema v{from} to v{to}", cli.db.display())
                }
                AuthorityInitOutcome::Current => println!(
                    "{} is already a papertiger-mise authority at schema v{}; nothing changed",
                    cli.db.display(),
                    papertiger_mise::SCHEMA_VERSION
                ),
            }
        }
        Command::Campaign(CampaignCommand::Admit {
            manifest: manifest_path,
        }) => {
            let admission = verify_campaign_admission(&manifest_path)?;
            let connection = open_existing(&cli.db)?;
            let outcome = admit_verified_campaign(&connection, &cli.actor, &admission)?;
            println!(
                "campaign {} {} ({})",
                admission.campaign_id(),
                match outcome {
                    papertiger_mise::AdmissionOutcome::Admitted => "admitted",
                    papertiger_mise::AdmissionOutcome::Existing => "already admitted",
                },
                admission.manifest_sha256()
            );
        }
        Command::Campaign(CampaignCommand::Preflight { manifest }) => {
            let report = preflight_campaign_admission(&manifest);
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.ready {
                bail!(
                    "campaign preflight found {} defect(s); {}",
                    report.defects.len(),
                    report.corrective_command
                );
            }
        }
        Command::Campaign(CampaignCommand::AdmitSuccessor {
            manifest,
            parent_nomination,
            gate_binding,
            papertiger_db,
            objects,
        }) => {
            let binding: PromotionGateBinding = read_json(&gate_binding)?;
            let connection = open_existing(&cli.db)?;
            let verified = verify_successor_admission(
                &connection,
                &manifest,
                &objects,
                &papertiger_db,
                &parent_nomination,
                &binding,
            )?;
            let outcome = admit_verified_successor(&connection, &cli.actor, &verified)?;
            let record = successor_admission(&connection, verified.campaign_id())?
                .context("successor admission committed without an immutable receipt")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "outcome": match outcome {
                        papertiger_mise::AdmissionOutcome::Admitted => "admitted",
                        papertiger_mise::AdmissionOutcome::Existing => "already_admitted",
                    },
                    "successor": record,
                    "proof": verified.proof(),
                    "proof_object": verified.proof_object(),
                    "gate": verified.gate(),
                }))?
            );
        }
        Command::Campaign(CampaignCommand::SourceBinding { repository }) => {
            let binding = inspect_source_binding(&repository)?;
            println!("{}", serde_json::to_string_pretty(&binding)?);
        }
        Command::Campaign(CampaignCommand::Show { campaign_id }) => {
            let connection = open_existing(&cli.db)?;
            let record = campaign(&connection, &campaign_id)?
                .with_context(|| format!("unknown campaign '{campaign_id}'"))?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Campaign(CampaignCommand::ShowSuccessor { campaign_id }) => {
            let connection = open_existing(&cli.db)?;
            let record = successor_admission(&connection, &campaign_id)?.with_context(|| {
                format!("campaign '{campaign_id}' is not an admitted successor")
            })?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Campaign(CampaignCommand::FixtureBundle {
            repository,
            output,
            entries,
        }) => {
            let fixtures = entries
                .iter()
                .map(|entry| {
                    let (key, locator) = entry.split_once('=').with_context(|| {
                        format!("fixture entry '{entry}' must be key=repository-relative-locator")
                    })?;
                    let bytes = std::fs::read(repository.join(locator))
                        .with_context(|| format!("read fixture '{}' at {locator}", key.trim()))?;
                    Ok(FixtureBundleEntry {
                        key: key.to_owned(),
                        locator: locator.to_owned(),
                        sha256: Sha256Digest(sha256(&bytes)),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let descriptor = FixtureBundleDescriptor {
                schema: FIXTURE_BUNDLE_SCHEMA_V2.to_owned(),
                fixtures,
            };
            let canonical = descriptor.canonical_bytes()?;
            std::fs::write(&output, &canonical)
                .with_context(|| format!("write fixture bundle {}", output.display()))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "output": output,
                    "bytes": canonical.len(),
                    "sha256": sha256(&canonical),
                }))?
            );
        }
        Command::Budget(BudgetCommand::Reserve {
            campaign_id,
            reservation_id,
            amounts,
        }) => {
            let requests = amounts
                .iter()
                .map(|amount| {
                    parse_amount(amount)
                        .map(|(resource, amount)| BudgetRequest { resource, amount })
                })
                .collect::<Result<Vec<_>>>()?;
            let connection = open_existing(&cli.db)?;
            let outcome = reserve_budget(
                &connection,
                &cli.actor,
                &campaign_id,
                &reservation_id,
                &requests,
            )?;
            println!("reservation {reservation_id} {outcome:?}");
        }
        Command::Budget(BudgetCommand::Settle {
            campaign_id,
            reservation_id,
            amounts,
            charge_reservation,
            note,
        }) => {
            if !charge_reservation && amounts.is_empty() {
                bail!("measured settlement requires at least one --amount");
            }
            let settlements = amounts
                .iter()
                .map(|amount| {
                    parse_amount(amount).map(|(resource, actual_amount)| BudgetSettlement {
                        resource,
                        actual_amount,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let mode = if charge_reservation {
                SettlementMode::ChargeReservation
            } else {
                SettlementMode::Measured
            };
            let connection = open_existing(&cli.db)?;
            let outcome = settle_budget(
                &connection,
                &cli.actor,
                &campaign_id,
                &reservation_id,
                mode,
                &settlements,
                note.as_deref(),
            )?;
            println!("reservation {reservation_id} {outcome:?}");
        }
        Command::Budget(BudgetCommand::Release {
            campaign_id,
            reservation_id,
            why,
        }) => {
            let why = why.required()?;
            let connection = open_existing(&cli.db)?;
            let outcome = papertiger_mise::budget::release_unused_budget(
                &connection,
                &cli.actor,
                &campaign_id,
                &reservation_id,
                &why,
            )?;
            println!("reservation {reservation_id} {outcome:?}");
        }
        Command::Budget(BudgetCommand::Show { campaign_id }) => {
            let connection = open_existing(&cli.db)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&budget_balances(&connection, &campaign_id)?)?
            );
        }
        Command::Candidate(CandidateCommand::BuildMaterial {
            repository,
            base_tree,
            result_tree,
            output,
        }) => {
            let bytes = build_git_change_set_material(&repository, &base_tree, &result_tree)?;
            if let Some(parent) = output.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&output, &bytes)
                .with_context(|| format!("write candidate material {}", output.display()))?;
            println!("{}", String::from_utf8(bytes)?);
        }
        Command::Candidate(CandidateCommand::Record {
            proposal,
            material,
            reservation,
            objects,
        }) => {
            // Validate the existing authority before a wrong-CWD invocation can
            // create or extend an object store beside an unrelated checkout.
            let connection = open_existing(&cli.db)?;
            let proposal: CandidateProposal = read_json(&proposal)?;
            let material_bytes = std::fs::read(&material)
                .with_context(|| format!("read candidate material {}", material.display()))?;
            let bound = bind_candidate(proposal, material_bytes.clone())?;
            let material_object = preserve_object(&objects, &material_bytes)?;
            let created = record_candidate(
                &connection,
                &cli.actor,
                &objects,
                &reservation,
                &bound,
                &material_object,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "created": created,
                    "candidate": bound,
                    "material": material_object,
                }))?
            );
        }
        Command::Candidate(CandidateCommand::Materialize {
            candidate_id,
            reservation,
            worktree,
            objects,
        }) => {
            let connection = open_existing(&cli.db)?;
            let record = materialize_candidate(
                &connection,
                &cli.actor,
                &objects,
                &reservation,
                &candidate_id,
                &worktree,
            )?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Candidate(CandidateCommand::AbandonMaterialization {
            candidate_id,
            reservation,
            why,
        }) => {
            let why = why.required()?;
            let connection = open_existing(&cli.db)?;
            let outcome = abandon_materialization_attempt(
                &connection,
                &cli.actor,
                &candidate_id,
                &reservation,
                &why,
            )?;
            println!("materialization {candidate_id} {outcome:?}");
        }
        Command::Candidate(CandidateCommand::Show { candidate_id }) => {
            let connection = open_existing(&cli.db)?;
            let record = candidate(&connection, &candidate_id)?
                .with_context(|| format!("unknown candidate '{candidate_id}'"))?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Candidate(CandidateCommand::Adjudicate { candidate_id }) => {
            let connection = open_existing(&cli.db)?;
            let nomination =
                adjudicate_deterministic_candidate(&connection, &cli.actor, &candidate_id)?;
            let record = candidate(&connection, &candidate_id)?
                .context("candidate disappeared during adjudication")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "candidate": record,
                    "nomination": nomination,
                }))?
            );
        }
        Command::Trial(TrialCommand::Run { spec, objects }) => {
            let spec: SupervisedTrialSpec = read_json(&spec)?;
            let connection = open_existing(&cli.db)?;
            let outcome = execute_workspace_trial(&connection, &cli.actor, &objects, &spec)?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
        }
        Command::Trial(TrialCommand::Recover { trial_id, objects }) => {
            let connection = open_existing(&cli.db)?;
            let outcome = recover_workspace_trial(&connection, &cli.actor, &objects, &trial_id)?;
            println!("trial {trial_id} {outcome:?}");
        }
        Command::Trial(TrialCommand::Abandon { trial_id, why }) => {
            let why = why.required()?;
            let connection = open_existing(&cli.db)?;
            let outcome = abandon_owned_trial(&connection, &cli.actor, &trial_id, &why)?;
            println!("trial {trial_id} {outcome:?}");
        }
        Command::Trial(TrialCommand::Cancel { trial_id, why }) => {
            let why = why.required()?;
            let connection = open_existing(&cli.db)?;
            let request = request_cancellation(
                &connection,
                &cli.actor,
                CancellationTarget::Trial,
                &trial_id,
                &why,
            )?;
            println!("{}", serde_json::to_string_pretty(&request)?);
        }
        Command::Trial(TrialCommand::Show { trial_id }) => {
            let connection = open_existing(&cli.db)?;
            let record = trial(&connection, &trial_id)?
                .with_context(|| format!("unknown trial '{trial_id}'"))?;
            let mut value = serde_json::to_value(record)?;
            value["cancellation_request"] = serde_json::to_value(cancellation_request(
                &connection,
                CancellationTarget::Trial,
                &trial_id,
            )?)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Command::Paired(PairedCommand::Prepare { spec, objects }) => {
            let spec: PreparePairedCohortSpec = read_json(&spec)?;
            let connection = open_existing(&cli.db)?;
            let (outcome, record) =
                prepare_paired_cohort(&connection, &cli.actor, &objects, &spec)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "outcome": outcome,
                    "cohort": record,
                }))?
            );
        }
        Command::Paired(PairedCommand::ReserveSlot {
            campaign_id,
            candidate_id,
            slot,
            seed,
        }) => {
            let revealed_order_seed = std::fs::read(&seed)
                .with_context(|| format!("failed to read order-seed reveal {}", seed.display()))?;
            let connection = open_existing(&cli.db)?;
            let record = reserve_paired_analysis_slot(
                &connection,
                &cli.actor,
                &campaign_id,
                &candidate_id,
                slot,
                &revealed_order_seed,
            )?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Paired(PairedCommand::ExecuteNext { cohort_id, objects }) => {
            let connection = open_existing(&cli.db)?;
            let outcome =
                execute_next_paired_execution(&connection, &cli.actor, &objects, &cohort_id)?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
        }
        Command::Paired(PairedCommand::Adjudicate { cohort_id, objects }) => {
            let connection = open_existing(&cli.db)?;
            let (cohort, adjudication) =
                adjudicate_paired_cohort(&connection, &cli.actor, &objects, &cohort_id)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "cohort": cohort,
                    "adjudication": adjudication,
                }))?
            );
        }
        Command::Paired(PairedCommand::DeriveNomination {
            research_cohort_id,
            no_op,
            known_bad,
            objects,
        }) => {
            let connection = open_existing(&cli.db)?;
            let nomination = derive_paired_nomination(
                &connection,
                &cli.actor,
                &objects,
                &DerivePairedNominationSpec {
                    research_cohort_id,
                    no_op_cohort_id: no_op,
                    known_bad_cohort_id: known_bad,
                },
            )?;
            let verified =
                verify_nomination_integrity(&connection, &objects, &nomination.nomination_id)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "nomination": nomination,
                    "verified": verified,
                }))?
            );
        }
        Command::Paired(PairedCommand::Recover {
            execution_id,
            objects,
        }) => {
            let connection = open_existing(&cli.db)?;
            let record =
                recover_paired_execution(&connection, &cli.actor, &objects, &execution_id)?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Paired(PairedCommand::ShowCohort { cohort_id }) => {
            let connection = open_existing(&cli.db)?;
            let record = paired_cohort(&connection, &cohort_id)?
                .with_context(|| format!("unknown paired cohort '{cohort_id}'"))?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Paired(PairedCommand::ListCohorts { campaign_id }) => {
            let connection = open_existing(&cli.db)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&paired_cohorts(&connection, &campaign_id)?)?
            );
        }
        Command::Paired(PairedCommand::Cancel { execution_id, why }) => {
            let why = why.required()?;
            let connection = open_existing(&cli.db)?;
            let request = request_cancellation(
                &connection,
                &cli.actor,
                CancellationTarget::PairedExecution,
                &execution_id,
                &why,
            )?;
            println!("{}", serde_json::to_string_pretty(&request)?);
        }
        Command::Paired(PairedCommand::ShowExecution { execution_id }) => {
            let connection = open_existing(&cli.db)?;
            let record = paired_execution(&connection, &execution_id)?
                .with_context(|| format!("unknown paired execution '{execution_id}'"))?;
            let mut value = serde_json::to_value(record)?;
            value["cancellation_request"] = serde_json::to_value(cancellation_request(
                &connection,
                CancellationTarget::PairedExecution,
                &execution_id,
            )?)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Command::Paired(PairedCommand::ListExecutions { cohort_id }) => {
            let connection = open_existing(&cli.db)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&paired_executions(&connection, &cohort_id)?)?
            );
        }
        Command::Object(ObjectCommand::Read {
            sha256,
            bytes,
            objects,
        }) => {
            let object = PreservedObject {
                locator: object_locator(&sha256)?,
                sha256,
                bytes,
            };
            let body = read_object(&objects, &object)?;
            std::io::stdout().write_all(&body)?;
        }
        Command::Evidence(EvidenceCommand::RecordHistorical {
            binding,
            request,
            objects,
        }) => {
            let binding: PairedAdapterBinding = read_json(&binding)?;
            let request: serde_json::Value = read_json(&request)?;
            let connection = open_existing(&cli.db)?;
            let (outcome, record) =
                record_historical_shadow(&connection, &cli.actor, &objects, &binding, &request)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "outcome": outcome,
                    "record": record,
                }))?
            );
        }
        Command::Evidence(EvidenceCommand::RecordDomain {
            binding,
            request,
            objects,
        }) => {
            let binding: DomainShadowAdapterBinding = read_json(&binding)?;
            let request: serde_json::Value = read_json(&request)?;
            let connection = open_existing(&cli.db)?;
            let (outcome, record) =
                record_domain_shadow(&connection, &cli.actor, &objects, &binding, &request)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "outcome": outcome,
                    "record": record,
                }))?
            );
        }
        Command::Evidence(EvidenceCommand::ReadDomain {
            evidence_id,
            objects,
        }) => {
            let connection = open_existing(&cli.db)?;
            let record = domain_shadow(&connection, &objects, &evidence_id)?
                .with_context(|| format!("unknown domain shadow '{evidence_id}'"))?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Evidence(EvidenceCommand::ReadHistorical {
            evidence_id,
            objects,
        }) => {
            let connection = open_existing(&cli.db)?;
            let record = historical_shadow(&connection, &objects, &evidence_id)?
                .with_context(|| format!("unknown historical shadow '{evidence_id}'"))?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Projection(ProjectionCommand::Export {
            nomination,
            candidate,
            objects,
            output,
        }) => {
            if let Some(path) = &output
                && path.exists()
            {
                bail!("projection output already exists at {}", path.display());
            }
            let connection = open_existing_read_only(&cli.db)?;
            let projection = match (nomination, candidate) {
                (Some(nomination_id), None) => {
                    derive_nomination_planner_projection(&connection, &objects, &nomination_id)?
                }
                (None, Some(candidate_id)) => {
                    derive_candidate_planner_projection(&connection, &objects, &candidate_id)?
                }
                _ => {
                    bail!("projection export requires exactly one of --nomination or --candidate")
                }
            };
            let projection_sha256 = projection.projection_sha256()?;
            let bytes = serde_json::to_vec_pretty(&projection)?;
            if let Some(path) = output {
                std::fs::write(&path, &bytes)
                    .with_context(|| format!("write Mise planner projection {}", path.display()))?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "output": path,
                        "projection_sha256": projection_sha256,
                        "schema": projection.schema,
                    }))?
                );
            } else {
                std::io::stdout().write_all(&bytes)?;
                std::io::stdout().write_all(b"\n")?;
            }
        }
        Command::Promotion(PromotionCommand::Verify {
            papertiger_db,
            nomination,
            task,
            gate,
            evidence,
            sha256,
            objects,
            containment_policy,
        }) => {
            let containment_policy = read_containment_policy(&containment_policy)?;
            let proof = verify_promotion_gate(
                &cli.db,
                &objects,
                &containment_policy,
                &papertiger_db,
                &nomination,
                &PromotionGateBinding {
                    task_seq: task,
                    gate_name: gate,
                    evidence_locator: evidence,
                    evidence_sha256: sha256,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&proof)?);
        }
        Command::Promotion(PromotionCommand::List { campaign }) => {
            let connection = open_existing(&cli.db)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&nominations(&connection, campaign.as_deref())?)?
            );
        }
        Command::Promotion(PromotionCommand::Rederive {
            nomination_id,
            objects,
        }) => {
            let connection = open_existing(&cli.db)?;
            let verified = verify_nomination_integrity(&connection, &objects, &nomination_id)?;
            println!("{}", serde_json::to_string_pretty(&verified)?);
        }
        Command::Promotion(PromotionCommand::DeriveParent {
            nomination,
            successor_manifest,
            objects,
        }) => {
            let connection = open_existing(&cli.db)?;
            let successor = existing_or_verified_successor(&connection, &successor_manifest)?;
            let preserved =
                preserve_parent_promotion_proof(&connection, &objects, &nomination, &successor)?;
            println!("{}", serde_json::to_string_pretty(&preserved)?);
        }
        Command::Promotion(PromotionCommand::VerifyParent {
            papertiger_db,
            nomination,
            successor_manifest,
            task,
            gate,
            evidence,
            sha256,
            objects,
        }) => {
            let connection = open_existing_read_only(&cli.db)?;
            let successor = existing_or_verified_successor(&connection, &successor_manifest)?;
            let (proof, proof_object, verified_gate) = verify_parent_promotion_gate(
                &connection,
                &objects,
                &papertiger_db,
                &nomination,
                &successor,
                &PromotionGateBinding {
                    task_seq: task,
                    gate_name: gate,
                    evidence_locator: evidence,
                    evidence_sha256: sha256,
                },
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "proof": proof,
                    "proof_object": proof_object,
                    "gate": verified_gate,
                }))?
            );
        }
        Command::Promotion(PromotionCommand::Derive {
            nomination,
            objects,
            containment_policy,
        }) => {
            let containment_policy = read_containment_policy(&containment_policy)?;
            let proof =
                derive_promotion_proof(&cli.db, &objects, &containment_policy, &nomination)?;
            let proof_sha256 = proof.sha256()?;
            let evidence_locator = proof.evidence_locator()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "proof": proof,
                    "sha256": proof_sha256,
                    "evidence_locator": evidence_locator,
                }))?
            );
        }
    }
    Ok(())
}

fn bind_project_root(explicit: Option<&std::path::Path>) -> Result<PathBuf> {
    let requested = match explicit {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().context("resolve current project directory")?,
    };
    let root = std::fs::canonicalize(&requested).with_context(|| {
        format!(
            "resolve project root {}; pass an existing consuming project directory to --project-root",
            requested.display()
        )
    })?;
    if !root.is_dir() {
        bail!("project root {} is not a directory", root.display());
    }
    if explicit.is_some() {
        std::env::set_current_dir(&root)
            .with_context(|| format!("enter project root {}", root.display()))?;
    }
    Ok(root)
}

fn absolute_from(root: &std::path::Path, path: &std::path::Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn print_project_status(status: &ProjectStatus) {
    println!("project {}", status.project_root);
    println!("database {}", status.database);
    println!(
        "object store {} (directory present: {}; content not verified)",
        status.object_store, status.object_store_present
    );
    let Some(authority) = &status.authority else {
        println!("authority uninitialized");
        if let Some(command) = &status.corrective_command {
            println!("initialize deliberately: {command}");
        }
        return;
    };
    println!(
        "authority schema v{}: {} campaign(s), {} nomination(s)",
        authority.schema_version, authority.campaign_count, authority.nomination_count
    );
    println!(
        "open: {} reservation(s), {} deterministic trial(s), {} paired cohort(s)",
        authority.open_reservation_count,
        authority.active_trial_count,
        authority.active_paired_cohort_count
    );
    if authority.integrity_failure_count > 0 {
        println!(
            "integrity failures: {} (inspect before continuing)",
            authority.integrity_failure_count
        );
    }
    if !authority.candidate_dispositions.is_empty() {
        println!(
            "candidates: {}",
            authority
                .candidate_dispositions
                .iter()
                .map(|(status, count)| format!("{status} {count}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for campaign in &authority.recent_campaigns {
        println!(
            "campaign {} {} {}",
            campaign.campaign_id, campaign.admitted_at, campaign.manifest_sha256
        );
    }
    if authority.recent_campaigns_truncated {
        println!("(older campaigns omitted)");
    }
}

fn status_metadata(path: &std::path::Path) -> Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| {
            format!(
                "inspect {}; pass a readable --db or --objects path",
                path.display()
            )
        }),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<T> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read JSON input {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse JSON input {}", path.display()))
}

fn existing_or_verified_successor(
    connection: &rusqlite::Connection,
    manifest_path: &std::path::Path,
) -> Result<CampaignManifest> {
    let historical: CampaignManifest = read_json(manifest_path)?;
    if successor_admission(connection, &historical.campaign_id)?.is_some() {
        return Ok(historical);
    }
    Ok(verify_campaign_admission(manifest_path)?.manifest().clone())
}

fn read_containment_policy(path: &std::path::Path) -> Result<ContainmentPolicy> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read containment policy {}", path.display()))?;
    let containment_policy: ContainmentPolicy = serde_json::from_slice(&bytes)?;
    if containment_policy.canonical_bytes()? != bytes {
        bail!(
            "containment policy must use canonical compact JSON; reissue it as the exact compact serde_json bytes of papertiger-mise.containment_policy.v3 with no trailing newline"
        );
    }
    Ok(containment_policy)
}

fn parse_amount(value: &str) -> Result<(BudgetResource, u64)> {
    let (resource, amount) = value
        .split_once('=')
        .with_context(|| format!("budget amount '{value}' must be resource=integer"))?;
    let resource = BudgetResource::from_str(resource)?;
    let amount = amount
        .parse::<u64>()
        .with_context(|| format!("invalid budget amount '{amount}'"))?;
    if amount == 0 {
        bail!("budget amount must be nonzero");
    }
    Ok((resource, amount))
}

#[cfg(test)]
mod tests {
    use super::render_error;

    fn constraint_failure(ddl: &str, statement: &str) -> anyhow::Error {
        let connection = rusqlite::Connection::open_in_memory().expect("in-memory database");
        connection.execute_batch(ddl).expect("fixture schema");
        let error = connection
            .execute(statement, [])
            .expect_err("fixture statement must be refused");
        anyhow::Error::from(error).context("record fixture")
    }

    #[test]
    fn trigger_refusal_omits_sqlite_constraint_code() {
        let error = constraint_failure(
            "CREATE TABLE fixture (id TEXT);
             CREATE TRIGGER fixture_guard BEFORE INSERT ON fixture
             BEGIN SELECT RAISE(ABORT, 'fixture refusal; run the corrective command'); END;",
            "INSERT INTO fixture (id) VALUES ('one')",
        );
        assert_eq!(
            render_error(&error),
            "record fixture: fixture refusal; run the corrective command"
        );
    }

    #[test]
    fn other_sqlite_failures_keep_their_complete_chain() {
        let error = constraint_failure(
            "CREATE TABLE fixture (id TEXT NOT NULL);",
            "INSERT INTO fixture (id) VALUES (NULL)",
        );
        assert_eq!(render_error(&error), format!("{error:#}"));
        assert!(render_error(&error).contains("Error code"), "{error:#}");
    }
}
