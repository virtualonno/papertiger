use std::io::Write as _;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use papertiger_mise::budget::{BudgetRequest, BudgetResource, BudgetSettlement, SettlementMode};
use papertiger_mise::improvement;
use papertiger_mise::manifest::{CampaignManifest, Sha256Digest};
use papertiger_mise::{
    CandidateProposal, DerivePairedNominationSpec, DomainShadowAdapterBinding,
    FIXTURE_BUNDLE_SCHEMA_V1, FixtureBundleDescriptor, FixtureBundleEntry, PairedAdapterBinding,
    PreparePairedCohortSpec, PreservedObject, PromotionGateBinding, SupervisedTrialSpec,
    TrustedContainmentPolicy, abandon_materialization_attempt, abandon_owned_trial,
    adjudicate_deterministic_candidate, adjudicate_paired_cohort, admit_verified_campaign,
    admit_verified_successor, authority_status, bind_candidate, budget_balances,
    build_git_change_set_material, campaign, candidate, derive_candidate_planner_projection,
    derive_nomination_planner_projection, derive_paired_nomination, derive_promotion_proof,
    domain_shadow, execute_next_paired_run, execute_workspace_trial, historical_shadow,
    host_execution_status, init, inspect_source_binding, materialize_candidate, nominations,
    object_locator, open_existing, open_existing_read_only, open_for_init, paired_cohort,
    paired_cohorts, paired_run, paired_runs, portable_absolute, preflight_campaign_admission,
    prepare_paired_cohort, preserve_object, preserve_parent_promotion_proof, read_object,
    record_candidate, record_domain_shadow, record_historical_shadow, recover_paired_run,
    recover_workspace_trial, reserve_budget, reserve_paired_analysis_slot, settle_budget, sha256,
    successor_admission, trial, verify_campaign_admission, verify_nomination_integrity,
    verify_parent_promotion_gate, verify_promotion_gate, verify_successor_admission,
};
use serde::Serialize;

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
    #[arg(long, default_value = "state/papertiger-mise.sqlite")]
    db: PathBuf,
    #[arg(long, env = "PAPERTIGER_ACTOR", default_value = "operator")]
    actor: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read-only orientation over the project-owned campaign authority.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Report portable supervision availability and non-authoritative native diagnostics.
    ExecutionStatus,
    /// Explicitly create or migrate the independent Mise authority.
    Init,
    /// Prepare and validate non-admitted campaign inputs without opening authority state.
    #[command(subcommand)]
    Improvement(ImprovementCommand),
    #[command(subcommand)]
    Campaign(CampaignCommand),
    #[command(subcommand)]
    Budget(BudgetCommand),
    #[command(subcommand)]
    Candidate(CandidateCommand),
    #[command(subcommand)]
    Trial(TrialCommand),
    #[command(subcommand)]
    Paired(PairedCommand),
    #[command(subcommand)]
    Object(ObjectCommand),
    #[command(subcommand)]
    Evidence(EvidenceCommand),
    #[command(subcommand)]
    Promotion(PromotionCommand),
    #[command(subcommand)]
    Projection(ProjectionCommand),
}

#[derive(Subcommand)]
enum ImprovementCommand {
    /// List the built-in versioned paradigm registry.
    Paradigms {
        #[arg(long)]
        json: bool,
    },
    /// Show one exact built-in paradigm template.
    Show { key: String },
    /// Validate an external registry without mutating campaign state.
    Verify { file: PathBuf },
    /// Validate a read-first project improvement brief as planning input only.
    BriefVerify { file: PathBuf },
    /// Compile an approved brief into a non-admitted campaign draft.
    Compile {
        #[arg(long)]
        brief: PathBuf,
        #[arg(long)]
        approval: PathBuf,
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
    authority: Option<papertiger_mise::AuthorityStatus>,
    corrective_command: Option<String>,
}

#[derive(Subcommand)]
enum ProjectionCommand {
    /// Reopen terminal authority and CAS evidence into a planner-safe projection document.
    Inspect {
        #[arg(
            long,
            required_unless_present = "candidate",
            conflicts_with = "candidate"
        )]
        nomination: Option<String>,
        #[arg(
            long,
            required_unless_present = "nomination",
            conflicts_with = "nomination"
        )]
        candidate: Option<String>,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
        /// Write the exact projection to a new file instead of emitting it on stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum CampaignCommand {
    /// Inspect the exact clean Git source binding used to author a manifest.
    SourceBinding { repository: PathBuf },
    /// Report every independently checkable admission defect without touching an authority.
    Preflight { manifest: PathBuf },
    /// Validate, canonicalize, and immutably admit a tracked campaign manifest.
    Admit { manifest: PathBuf },
    /// Admit a descendant after rederiving its parent proof and independent gate.
    AdmitSuccessor {
        manifest: PathBuf,
        #[arg(long)]
        parent_nomination: String,
        #[arg(long)]
        gate_binding: PathBuf,
        #[arg(long, default_value = "state/papertiger.sqlite")]
        papertiger_db: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Read one exact admitted campaign.
    Show { campaign_id: String },
    /// Read one immutable successor admission and parent-ledger receipt.
    ShowSuccessor { campaign_id: String },
    /// Write an exact canonical fixture bundle from repository files.
    FixtureBundle {
        repository: PathBuf,
        output: PathBuf,
        #[arg(long = "entry", required = true)]
        entries: Vec<String>,
    },
}

#[derive(Subcommand)]
enum BudgetCommand {
    /// Reserve cumulative resources before any candidate side effect.
    Reserve {
        campaign_id: String,
        reservation_id: String,
        #[arg(long = "amount", required = true)]
        amounts: Vec<String>,
    },
    /// Settle measured use, or conservatively charge the full reservation.
    Settle {
        campaign_id: String,
        reservation_id: String,
        #[arg(long = "amount")]
        amounts: Vec<String>,
        #[arg(long, conflicts_with = "amounts")]
        charge_reservation: bool,
        #[arg(long)]
        note: Option<String>,
    },
    /// Display the cumulative ledger for one campaign.
    Show { campaign_id: String },
}

#[derive(Subcommand)]
enum CandidateCommand {
    /// Build canonical Git change-set material from two exact trees.
    BuildMaterial {
        #[arg(long)]
        repository: PathBuf,
        #[arg(long)]
        base_tree: String,
        #[arg(long)]
        result_tree: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Bind and durably record one typed proposal plus exact candidate material.
    Record {
        #[arg(long)]
        proposal: PathBuf,
        #[arg(long)]
        material: PathBuf,
        #[arg(long)]
        reservation: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Materialize one durable candidate into its exact confined worktree.
    Materialize {
        candidate_id: String,
        #[arg(long)]
        reservation: String,
        #[arg(long)]
        worktree: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Charge and close an interrupted materialization before retrying it.
    AbandonMaterialization {
        candidate_id: String,
        #[arg(long)]
        reservation: String,
        #[arg(long)]
        reason: String,
    },
    /// Read one exact durable candidate.
    Show { candidate_id: String },
    /// Derive a terminal deterministic result and optional nomination.
    Adjudicate { candidate_id: String },
}

#[derive(Subcommand)]
enum TrialCommand {
    /// Execute one typed deterministic trial through the owned supervisor.
    Run {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Heal a reverified succeeded settlement or reconcile an absent launched process.
    Recover {
        trial_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Charge and retire an ambiguous pre-launch trial intent without claiming process absence.
    Abandon {
        trial_id: String,
        #[arg(long)]
        reason: String,
    },
    /// Read one exact durable trial, including terminal evidence pointers.
    Show { trial_id: String },
}

#[derive(Subcommand)]
enum PairedCommand {
    /// Irrevocably bind one research candidate to a finite confirmation slot.
    ReserveSlot {
        campaign_id: String,
        candidate_id: String,
        slot: u32,
        /// File containing the committed order-seed reveal bytes.
        #[arg(long)]
        seed: PathBuf,
    },
    /// Freeze every ordered request and reserve the complete cohort before launch.
    Prepare {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Execute exactly the next predeclared run, or report adjudication readiness.
    RunNext {
        cohort_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Reopen every run from CAS and apply the sole fixed classifier.
    Adjudicate {
        cohort_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Derive one development nomination from exact CAS-reverified paired cohorts.
    DeriveNomination {
        research_cohort_id: String,
        #[arg(long)]
        no_op: String,
        #[arg(long)]
        known_bad: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Reconcile an exact launched run only after its birth-bound process is absent.
    Recover {
        execution_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Read one durable cohort and its terminal evidence pointers.
    ShowCohort { cohort_id: String },
    /// Enumerate every durable cohort in one campaign.
    ListCohorts { campaign_id: String },
    /// Read one durable paired run and its evidence pointers.
    ShowRun { execution_id: String },
    /// Enumerate every predeclared run in exact cohort order.
    ListRuns { cohort_id: String },
}

#[derive(Subcommand)]
enum ObjectCommand {
    /// Reverify and emit exact CAS bytes for one typed object pointer.
    Read {
        sha256: String,
        bytes: u64,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
}

#[derive(Subcommand)]
enum EvidenceCommand {
    /// Execute and retain one read-only observation with unchanged domain state.
    #[command(name = "domain-shadow")]
    RecordDomain {
        #[arg(long)]
        binding: PathBuf,
        #[arg(long)]
        request: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Reopen one exact, permanently decision-ineligible domain shadow.
    #[command(name = "show-domain-shadow")]
    ReadDomain {
        evidence_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Execute and retain adapter-backed legacy evidence without decision authority.
    #[command(name = "historical-shadow")]
    RecordHistorical {
        #[arg(long)]
        binding: PathBuf,
        #[arg(long)]
        request: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Reopen one exact historical-shadow receipt and all of its CAS objects.
    #[command(name = "show-historical-shadow")]
    ReadHistorical {
        evidence_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
}

#[derive(Subcommand)]
enum PromotionCommand {
    /// Enumerate durable nominations for operator review.
    List {
        #[arg(long)]
        campaign: Option<String>,
    },
    /// Rederive one nomination from its retained CAS evidence.
    Inspect {
        nomination_id: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Preserve the successor-only parent promotion proof for independent gate review.
    DeriveParent {
        #[arg(long)]
        nomination: String,
        #[arg(long)]
        successor_manifest: PathBuf,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Run a read-only parent promotion proof preflight against an independent gate.
    VerifyParent {
        #[arg(long, default_value = "state/papertiger.sqlite")]
        papertiger_db: PathBuf,
        #[arg(long)]
        nomination: String,
        #[arg(long)]
        successor_manifest: PathBuf,
        #[arg(long)]
        task: i64,
        #[arg(long)]
        gate: String,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        sha256: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
    },
    /// Attempt exact proof derivation; raw-observation confirmation fails closed.
    Derive {
        #[arg(long)]
        nomination: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
        #[arg(long)]
        containment_policy: PathBuf,
    },
    /// Verify a derivable proof against a separately closed Papertiger gate read-only.
    Verify {
        #[arg(long)]
        papertiger_db: PathBuf,
        #[arg(long)]
        nomination: String,
        #[arg(long)]
        task: i64,
        #[arg(long)]
        gate: String,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        sha256: String,
        #[arg(long, default_value = "state/papertiger-mise-objects")]
        objects: PathBuf,
        /// Operator-owned trust policy; it must not come from the candidate repository.
        #[arg(long)]
        containment_policy: PathBuf,
    },
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let project_root = bind_project_root(cli.project_root.as_deref())?;
    match cli.command {
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
            ImprovementCommand::Verify { file } => {
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
            ImprovementCommand::BriefVerify { file } => {
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
        Command::Status { json } => {
            let database = absolute_from(&project_root, &cli.db);
            let object_store = project_root.join("state/papertiger-mise-objects");
            if database.exists() && !database.is_file() {
                bail!(
                    "Mise database path {} exists but is not a file; pass the intended database with --db",
                    database.display()
                );
            }
            if object_store.exists() && !object_store.is_dir() {
                bail!(
                    "Mise object-store path {} exists but is not a directory; move it aside or restore the intended object store",
                    object_store.display()
                );
            }
            let initialized = database.is_file();
            let authority = if initialized {
                let connection = open_existing_read_only(&database)?;
                Some(authority_status(&connection, 10)?)
            } else {
                None
            };
            let project_root_identity = portable_absolute(&project_root)?;
            let corrective_command = (!initialized).then(|| {
                format!(
                    "papertiger-mise --project-root \"{}\" init",
                    project_root_identity
                )
            });
            let status = ProjectStatus {
                schema: "papertiger-mise.project-status.v1",
                version: env!("CARGO_PKG_VERSION"),
                project_root: project_root_identity,
                database: portable_absolute(&database)?,
                object_store: portable_absolute(&object_store)?,
                initialized,
                object_store_present: object_store.is_dir(),
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
            init(&connection)?;
            println!("initialized {}", cli.db.display());
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
                schema: FIXTURE_BUNDLE_SCHEMA_V1.to_owned(),
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
            reason,
        }) => {
            let connection = open_existing(&cli.db)?;
            let outcome = abandon_materialization_attempt(
                &connection,
                &cli.actor,
                &candidate_id,
                &reservation,
                &reason,
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
        Command::Trial(TrialCommand::Abandon { trial_id, reason }) => {
            let connection = open_existing(&cli.db)?;
            let outcome = abandon_owned_trial(&connection, &cli.actor, &trial_id, &reason)?;
            println!("trial {trial_id} {outcome:?}");
        }
        Command::Trial(TrialCommand::Show { trial_id }) => {
            let connection = open_existing(&cli.db)?;
            let record = trial(&connection, &trial_id)?
                .with_context(|| format!("unknown trial '{trial_id}'"))?;
            println!("{}", serde_json::to_string_pretty(&record)?);
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
        Command::Paired(PairedCommand::RunNext { cohort_id, objects }) => {
            let connection = open_existing(&cli.db)?;
            let outcome = execute_next_paired_run(&connection, &cli.actor, &objects, &cohort_id)?;
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
            let record = recover_paired_run(&connection, &cli.actor, &objects, &execution_id)?;
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
        Command::Paired(PairedCommand::ShowRun { execution_id }) => {
            let connection = open_existing(&cli.db)?;
            let record = paired_run(&connection, &execution_id)?
                .with_context(|| format!("unknown paired run '{execution_id}'"))?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::Paired(PairedCommand::ListRuns { cohort_id }) => {
            let connection = open_existing(&cli.db)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&paired_runs(&connection, &cohort_id)?)?
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
        Command::Projection(ProjectionCommand::Inspect {
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
                    bail!("projection inspect requires exactly one of --nomination or --candidate")
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
            let trusted_policy = read_trusted_policy(&containment_policy)?;
            let proof = verify_promotion_gate(
                &cli.db,
                &objects,
                &trusted_policy,
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
        Command::Promotion(PromotionCommand::Inspect {
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
            let trusted_policy = read_trusted_policy(&containment_policy)?;
            let proof = derive_promotion_proof(&cli.db, &objects, &trusted_policy, &nomination)?;
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
    println!("object store {}", status.object_store);
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

fn read_trusted_policy(path: &std::path::Path) -> Result<TrustedContainmentPolicy> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read trusted containment policy {}", path.display()))?;
    let trusted_policy: TrustedContainmentPolicy = serde_json::from_slice(&bytes)?;
    if trusted_policy.canonical_bytes()? != bytes {
        bail!("trusted containment policy must use canonical compact JSON");
    }
    Ok(trusted_policy)
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
