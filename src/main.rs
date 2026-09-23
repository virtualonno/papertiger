use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use papertiger as pt;
use rusqlite::{Connection, params};
use std::io::Read;

mod project_bundle;
mod project_setup;
mod text_input;
mod user_setup;

use text_input::{IntentArgs, NoteTextArgs, ResultArgs, WhyArgs, reject_multiple_stdin};

#[derive(Parser)]
#[command(
    name = "papertiger",
    version,
    about = "Local task planning for cross-session engineering work"
)]
struct Cli {
    /// Planning database path (default: PAPERTIGER_DB, project receipt, or installed personal store); invalid with integration commands
    #[arg(long, global = true)]
    db: Option<String>,
    /// Receipt-bound project root used for authority selection or project inspection; evidence verify also uses it for file: locators
    #[arg(long = "project-root", global = true, value_name = "DIR")]
    authority_project_root: Option<std::path::PathBuf>,
    /// Actor recorded on events (default: PAPERTIGER_ACTOR or 'operator'); invalid with project integration commands
    #[arg(long, global = true)]
    actor: Option<String>,
    /// Advisory pickup identity (default: PAPERTIGER_SESSION); never an exclusive lock or liveness signal
    #[arg(long, global = true)]
    session: Option<String>,
    /// Emit JSON: versioned reads or exact committed mutation receipts.
    #[arg(long, global = true)]
    json: bool,
    /// Caller-reported event author model; unknown attribution remains absent.
    #[arg(long, global = true)]
    model: Option<String>,
    /// Caller-reported configured reasoning effort; requires a known model.
    #[arg(long, global = true)]
    reasoning_effort: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(flatten)]
    Personal(user_setup::Command),
    /// Print the bundled JSON Schema for local planner reads, recovery, and mutation receipts; never opens authority
    Schema,
    /// Install a project-local native binary, receipt, ignore policy, and agent contract; does not accept --db or --actor
    #[command(after_help = "JSON schema: papertiger.project_setup.v5")]
    SetupProject {
        /// Existing consuming project directory
        project_root: std::path::PathBuf,
        /// Report the complete action plan without writing
        #[arg(long)]
        dry_run: bool,
        /// Replace divergent release-managed files after review
        #[arg(long)]
        replace_managed: bool,
        /// Project-relative canonical authority path (preserved by later upgrades)
        #[arg(long, value_name = "PATH")]
        authority_path: Option<std::path::PathBuf>,
        /// Skill target selection; omitted upgrades preserve the receipt selection
        #[arg(long, value_enum, value_name = "auto|agents|claude|both|none")]
        skill_target: Option<project_setup::SkillTargetRequest>,
    },
    /// Inspect repository-owned AGENTS.md and CLAUDE.md without editing them or opening the planning authority
    #[command(after_help = "JSON schema: papertiger.project_guidance.v1")]
    InspectProjectGuidance {},
    /// Remove only receipt-owned project integration files; preserves authority and repository policy
    UninstallProject {
        /// Existing consuming project directory
        project_root: std::path::PathBuf,
        /// Report the complete removal plan without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Create or upgrade a Papertiger database; refuses nonempty foreign databases
    Init,
    /// One-screen orientation: authority, active plans, current work, ready work, recent notes
    Status {},
    /// Plan management
    Plan {
        #[command(subcommand)]
        cmd: PlanCmd,
    },
    /// Add a task
    Add {
        /// Concise outcome-oriented task title
        title: String,
        /// Owning plan slug; optional when exactly one plan is active
        #[arg(long)]
        plan: Option<String>,
        #[command(flatten)]
        intent: IntentArgs,
        /// Who supplied the stored meaning: user, agent, or external
        #[arg(long, value_name = "user|agent|external")]
        intent_source: Option<String>,
        /// Work kind: work, probe, or decision
        #[arg(long, default_value = "work")]
        kind: String,
        /// Parent task sequence (bare N is shell-portable; quoted #N also works)
        #[arg(long)]
        parent: Option<String>,
        /// Dependencies, comma-separated task refs
        #[arg(long, value_delimiter = ',')]
        dep: Vec<String>,
        /// Searchable task tags, comma-separated or repeated
        #[arg(long, value_delimiter = ',')]
        tag: Vec<String>,
        /// Scheduling priority; higher values run first within readiness order
        #[arg(long, default_value_t = 0)]
        priority: i64,
        #[command(flatten)]
        why: WhyArgs,
        /// Atomically create and transition the task to in_progress; requires --why
        #[arg(long)]
        start: bool,
    },
    /// Show one task in full
    Show {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
        /// Current state without historical payloads, as JSON (implies --json); use log --task N --json for rationale/history
        #[arg(long)]
        no_history: bool,
    },
    /// List tasks (compact)
    List {
        /// Explicit inventory across every plan state, ordered by sequence
        #[arg(long, conflicts_with_all = ["plan", "sort"])]
        all_plans: bool,
        /// Maximum inventory rows (1..500); requires --all-plans
        #[arg(long, requires = "all_plans")]
        limit: Option<usize>,
        /// Continue after the previous next_after_seq
        #[arg(long, requires_all = ["all_plans", "snapshot"])]
        after_seq: Option<i64>,
        /// Authority snapshot returned by the preceding page; changes require restart
        #[arg(long, requires = "all_plans")]
        snapshot: Option<String>,
        /// Plan slug; optional when exactly one plan is active
        #[arg(long)]
        plan: Option<String>,
        /// Filter by task status
        #[arg(long)]
        status: Option<String>,
        /// Filter by exact tag
        #[arg(long)]
        tag: Option<String>,
        /// Ordering: seq or activity
        #[arg(long, default_value = "seq")]
        sort: String,
    },
    /// Search durable task context with deterministic field ranking
    Search {
        /// Literal words to find across title, intent, result, tags, and rationale
        query: String,
        /// Restrict results to one plan slug; all plans are searched by default
        #[arg(long)]
        plan: Option<String>,
        /// Restrict results to one task status
        #[arg(long)]
        status: Option<String>,
        /// Maximum ranked results to return
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Structured identity, ranking and excerpt without full task bodies (implies --json)
        #[arg(long)]
        compact: bool,
    },
    /// Edit a task
    Edit {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
        /// Replacement title
        #[arg(long)]
        title: Option<String>,
        #[command(flatten)]
        intent: IntentArgs,
        /// Who supplied the replacement intent meaning: user, agent, or external
        #[arg(long, value_name = "user|agent|external")]
        intent_source: Option<String>,
        /// Remove a previously recorded intent source
        #[arg(long, conflicts_with = "intent_source")]
        clear_intent_source: bool,
        /// Replace the parent with this task
        #[arg(long, conflicts_with = "clear_parent")]
        parent: Option<String>,
        /// Remove the current parent
        #[arg(long)]
        clear_parent: bool,
        /// Replacement work kind: work, probe, or decision
        #[arg(long)]
        kind: Option<String>,
        /// Replacement scheduling priority
        #[arg(long)]
        priority: Option<i64>,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Move an explicit complete related set to an active plan without rewriting history
    MovePlan {
        /// Explicit complete set of tasks to relocate, including related tasks
        #[arg(required = true)]
        tasks: Vec<String>,
        /// Active destination plan
        #[arg(long)]
        plan: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Start or resume unfinished work; record advisory session pickup without locking the task
    Start {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Complete a task after dependencies, blockers, gates, and children are closed
    Done {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
        #[command(flatten)]
        result: ResultArgs,
        /// Who supplied the durable result: user, agent, or external
        #[arg(long, value_name = "user|agent|external")]
        result_source: Option<String>,
    },
    /// Reopen a finished task
    Reopen {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Retire a task (no longer worth doing)
    Retire {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
        /// Durable same-plan task that replaces this work
        #[arg(long, value_name = "TASK")]
        into: Option<String>,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Reject a task/approach (records why, prevents re-litigation)
    Reject {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Gate management
    Gate {
        #[command(subcommand)]
        cmd: GateCmd,
    },
    /// External task blocker management
    Blocker {
        #[command(subcommand)]
        cmd: BlockerCmd,
    },
    /// Manage exact task reference locators without fetching external state
    Reference {
        #[command(subcommand)]
        cmd: ReferenceCmd,
    },
    /// Manage caller-resolved local commit associations without invoking Git
    Commit {
        #[command(subcommand)]
        cmd: CommitCmd,
    },
    /// Dependency management
    Dep {
        #[command(subcommand)]
        cmd: DepCmd,
    },
    /// Actionable leaf work ranked by readiness and dependency unlock impact
    Focus {
        /// Plan slug; optional when exactly one plan is active
        #[arg(long)]
        plan: Option<String>,
        /// Maximum tasks to return
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Include proposed work that is currently blocked
        #[arg(long)]
        all: bool,
    },
    /// Task tag management
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
    },
    /// Task hierarchy for a plan
    Tree {
        /// Plan slug; optional when exactly one plan is active
        #[arg(long)]
        plan: Option<String>,
    },
    /// Record a free-standing evented note (course changes, decisions)
    Note {
        #[command(flatten)]
        text: NoteTextArgs,
        /// Who supplied the note meaning: user, agent, or external
        #[arg(long, value_name = "user|agent|external")]
        source: Option<String>,
        /// Attach the note to this task sequence
        #[arg(long)]
        task: Option<String>,
    },
    /// Event history
    Log {
        /// Restrict history to this task sequence
        #[arg(long)]
        task: Option<String>,
        /// Maximum events to return
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Page backward from an event-v1 cursor emitted by JSON output
        #[arg(long, conflicts_with = "after_cursor")]
        before_cursor: Option<String>,
        /// Read new events after an event-v1 cursor emitted by JSON output
        #[arg(long)]
        after_cursor: Option<String>,
    },
    /// Advisory integrity findings
    Audit,
    /// Inspect or explicitly quarantine structurally invalid legacy history
    History {
        #[command(subcommand)]
        cmd: HistoryCmd,
    },
    /// Verify stored evidence bindings without changing authority state
    Evidence {
        #[command(subcommand)]
        cmd: EvidenceCmd,
    },
    /// Dump plans/tasks/gates as JSON
    Export {
        /// Export only this plan slug and its scoped history
        #[arg(long)]
        plan: Option<String>,
        /// Atomically write canonical UTF-8 JSON to this file instead of stdout
        #[arg(long, value_name = "PATH")]
        output: Option<std::path::PathBuf>,
        /// Replace an existing regular output file after review
        #[arg(long, requires = "output")]
        replace: bool,
    },
    /// Reinstall missing or altered write guards and remove foreign triggers, recording the drift
    RepairGuards {
        /// Explain what altered the guards and why reinstalling them is safe
        #[arg(long)]
        why: Option<String>,
        /// Report drift without changing the authority
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a consistent standalone SQLite recovery copy without migrating
    Backup {
        /// New destination; existing files and SQLite sidecars always refuse
        #[arg(long, value_name = "PATH")]
        output: std::path::PathBuf,
    },
    /// Import a papertiger.dump.v9 JSON file
    Import {
        /// Dump file to validate and import atomically
        file: String,
    },
    /// Record and inspect verified terminal Mise evidence without changing task state
    Mise {
        #[command(subcommand)]
        cmd: MiseCmd,
    },
}

#[derive(Subcommand)]
enum HistoryCmd {
    /// Read every original field and its digest, without inferring missing provenance
    Inspect { event_id: i64 },
    /// Preserve an invalid row verbatim in an audited, exportable quarantine event
    Quarantine {
        event_id: i64,
        /// Exact digest returned by history inspect; refuses a changed record
        #[arg(long)]
        expect_sha256: String,
        /// Explain why this original history is untrusted
        #[arg(long)]
        why: String,
    },
}

#[derive(Subcommand)]
enum ReferenceCmd {
    /// Record an inward locator without importing external status or fetching bytes
    Add {
        task: String,
        locator: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        sha256: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Remove one exact reference with retained rationale
    Remove {
        task: String,
        locator: String,
        #[arg(long)]
        kind: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// List inward references on one task
    List { task: String },
    /// Find tasks that name an exact locator
    Find { locator: String },
}

#[derive(Subcommand)]
enum EvidenceCmd {
    /// Summarize every stored binding and page filtered verification details
    Verify {
        /// Restrict verification to one task sequence
        #[arg(long)]
        task: Option<String>,
        /// Detail classification to show; summary counts always cover the full task scope
        #[arg(long, value_enum, default_value = "incomplete")]
        outcome: pt::EvidenceOutcomeFilter,
        /// Filter details by owning task lifecycle state
        #[arg(long, value_enum, default_value = "all")]
        task_state: pt::EvidenceTaskStateFilter,
        /// Maximum detail bindings to return
        #[arg(long, default_value_t = pt::DEFAULT_EVIDENCE_PAGE)]
        limit: usize,
        /// Continue a live evidence-v1 projection emitted by the same scope and filters
        #[arg(long)]
        after_cursor: Option<String>,
    },
}

#[derive(Subcommand)]
enum CommitCmd {
    /// Associate one full commit object ID with a local task
    Add {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
        /// Caller-resolved lowercase 40- or 64-hex commit object ID
        commit_oid: String,
        /// Stable repository label; '.' denotes the authority's project root
        #[arg(long, default_value = ".")]
        repo: String,
        /// Optional context explaining why this snapshot is useful
        #[arg(long)]
        note: Option<String>,
    },
    /// Remove an incorrect association while retaining an evented correction
    Remove {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
        /// Previously recorded full commit object ID
        commit_oid: String,
        /// Repository label used when the association was added
        #[arg(long, default_value = ".")]
        repo: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// List commit associations on one task
    List {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
    },
    /// Reverse lookup local tasks associated with one full commit object ID
    Find {
        /// Caller-resolved lowercase 40- or 64-hex commit object ID
        commit_oid: String,
        /// Restrict lookup to this stable repository label
        #[arg(long)]
        repo: Option<String>,
    },
}

#[derive(Subcommand)]
enum MiseCmd {
    /// Idempotently attach one verified projection document to its owning task
    Project {
        /// Task sequence that owns the projection
        task: String,
        /// Projection JSON path, or '-' to read a Mise inspector pipeline from stdin
        projection: String,
    },
    /// List every reverified Mise projection attached to one task
    List {
        /// Task sequence that owns the projections
        task: String,
    },
    /// Show one exact reverified projection by its SHA-256 identity
    Show {
        /// Full lowercase SHA-256 projection identity
        projection_sha256: String,
    },
}

#[derive(Subcommand)]
enum PlanCmd {
    /// Create a plan for durable work
    Add {
        /// Stable local plan selector
        slug: String,
        /// Human-readable plan title
        title: String,
        #[command(flatten)]
        intent: IntentArgs,
    },
    /// List every plan with its current status
    List {
        /// Retrieve one known plan's orientation
        #[arg(long)]
        plan: Option<String>,
    },
    /// Edit plan orientation without replacing its task/event history
    Edit {
        /// Plan slug to edit
        slug: String,
        /// Replacement plan title
        #[arg(long)]
        title: Option<String>,
        #[command(flatten)]
        intent: IntentArgs,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Set plan status (active|paused|done|retired)
    Set {
        /// Plan slug to transition
        slug: String,
        /// New status: active, paused, done, or retired
        status: String,
        #[command(flatten)]
        why: WhyArgs,
    },
}

#[derive(Subcommand)]
enum GateCmd {
    /// Add a named proof obligation to a task
    Add {
        /// Task sequence that owns the gate
        task: String,
        /// Stable gate name within the task
        name: String,
        /// Evidence kind: test, benchmark, review, capture, fixture, build, doc, or other
        #[arg(long)]
        kind: String,
        /// Exact condition required to close the gate
        #[arg(long)]
        requirement: String,
    },
    /// Close an open gate with an evidence locator
    Close {
        /// Task sequence that owns the gate
        task: String,
        /// Name of the open gate
        name: String,
        /// Evidence locator, scheme:value (file:, receipt:, claim:, spec:, commit:, url:, note:)
        #[arg(long)]
        evidence: String,
        /// Optional lowercase SHA-256 digest binding the evidence bytes
        #[arg(long)]
        sha256: Option<String>,
        /// Optional concise evidence context
        #[arg(long)]
        note: Option<String>,
    },
    /// Waive an open gate with durable rationale
    Waive {
        /// Task sequence that owns the gate
        task: String,
        /// Name of the open gate
        name: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Reopen a closed or waived gate
    Reopen {
        /// Task sequence that owns the gate
        task: String,
        /// Name of the terminal gate
        name: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Remove an open gate that no longer models required proof
    Remove {
        /// Task sequence that owns the gate
        task: String,
        /// Name of the open gate
        name: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// List every gate on one task
    List {
        /// Task sequence that owns the gates
        task: String,
    },
}

#[derive(Subcommand)]
enum BlockerCmd {
    /// Add a named external blocker to a task
    Add {
        /// Task sequence that owns the blocker
        task: String,
        /// Stable blocker name within the task
        name: String,
        /// External condition preventing progress
        #[arg(long)]
        reason: String,
    },
    /// Resolve an open blocker with external evidence
    Resolve {
        /// Task sequence that owns the blocker
        task: String,
        /// Name of the open blocker
        name: String,
        /// Evidence locator proving the blocker cleared
        #[arg(long)]
        evidence: String,
        /// Optional lowercase SHA-256 digest binding the evidence bytes
        #[arg(long)]
        sha256: Option<String>,
        /// Optional concise evidence context
        #[arg(long)]
        note: Option<String>,
    },
    /// Waive an open blocker with durable rationale
    Waive {
        /// Task sequence that owns the blocker
        task: String,
        /// Name of the open blocker
        name: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Reopen a resolved or waived blocker
    Reopen {
        /// Task sequence that owns the blocker
        task: String,
        /// Name of the terminal blocker
        name: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Remove an open blocker that no longer models reality
    Remove {
        /// Task sequence that owns the blocker
        task: String,
        /// Name of the open blocker
        name: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// List every blocker on one task
    List {
        /// Task sequence that owns the blockers
        task: String,
    },
}

#[derive(Subcommand)]
enum TagCmd {
    /// Add a searchable tag to a task
    Add {
        /// Task sequence to tag
        task: String,
        /// Tag value
        tag: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Remove a tag from a task
    Remove {
        /// Task sequence to untag
        task: String,
        /// Existing tag value
        tag: String,
        #[command(flatten)]
        why: WhyArgs,
    },
}

#[derive(Subcommand)]
enum DepCmd {
    /// Make one task depend on another task
    Add {
        /// Dependent task sequence
        task: String,
        /// Prerequisite task sequence
        on: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Remove a dependency edge
    Remove {
        /// Dependent task sequence
        task: String,
        /// Prerequisite task sequence
        on: String,
        #[command(flatten)]
        why: WhyArgs,
    },
}

impl Cmd {
    /// Projection flags that only exist as JSON select it without `--json`.
    fn implies_json(&self) -> bool {
        matches!(
            self,
            Self::Search { compact: true, .. }
                | Self::Show {
                    no_history: true,
                    ..
                }
        )
    }

    fn opens_authority_read_only(&self) -> bool {
        matches!(
            self,
            Self::Schema
                | Self::Status { .. }
                | Self::Show { .. }
                | Self::List { .. }
                | Self::Search { .. }
                | Self::Focus { .. }
                | Self::Tree { .. }
                | Self::Log { .. }
                | Self::Audit
                | Self::History {
                    cmd: HistoryCmd::Inspect { .. }
                }
                | Self::Evidence { .. }
                | Self::Export { .. }
                | Self::Backup { .. }
                | Self::Plan {
                    cmd: PlanCmd::List { .. }
                }
                | Self::Gate {
                    cmd: GateCmd::List { .. }
                }
                | Self::Blocker {
                    cmd: BlockerCmd::List { .. }
                }
                | Self::Commit {
                    cmd: CommitCmd::List { .. } | CommitCmd::Find { .. }
                }
                | Self::Reference {
                    cmd: ReferenceCmd::List { .. } | ReferenceCmd::Find { .. }
                }
                | Self::Mise {
                    cmd: MiseCmd::List { .. } | MiseCmd::Show { .. }
                }
        )
    }
}

fn status_glyph(s: &str) -> &'static str {
    match s {
        "proposed" => "·",
        "in_progress" => ">",
        "done" => "x",
        "retired" => "-",
        "rejected" => "!",
        _ => "?",
    }
}

fn print_task_line(conn: &Connection, t: &pt::Task) -> Result<()> {
    let deps = pt::open_deps(conn, t.task_id)?;
    let blockers = pt::task_blockers(conn, t.task_id)?
        .into_iter()
        .filter(|blocker| blocker.status == "open")
        .map(|blocker| blocker.name)
        .collect::<Vec<_>>();
    let open_gate_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM gates WHERE task_id=?1 AND status='open'",
        params![t.task_id],
        |r| r.get(0),
    )?;
    let mut extra = String::new();
    if !deps.is_empty() {
        extra.push_str(&format!(
            " [deps: {}]",
            deps.iter()
                .map(|d| format!("#{d}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if !blockers.is_empty() {
        extra.push_str(&format!(" [blockers: {}]", blockers.join(", ")));
    }
    if open_gate_count > 0 {
        extra.push_str(&format!(" [gates open: {open_gate_count}]"));
    }
    if t.kind != "work" {
        extra.push_str(&format!(" [{}]", t.kind));
    }
    println!(
        "{} #{} {}{}",
        status_glyph(&t.status),
        t.seq,
        t.title,
        extra
    );
    Ok(())
}

fn pickup_label(pickup: Option<&pt::TaskPickup>) -> String {
    match pickup {
        Some(p) => format!(
            " [last pickup {} at {}]",
            p.session.as_deref().unwrap_or("unattributed"),
            p.at
        ),
        None => String::new(),
    }
}

fn print_task_context(context: &pt::TaskContext) {
    let task = &context.task;
    if let Some(pickup) = &task.pickup {
        println!(
            "pickup:{}; advisory, liveness unknown",
            pickup_label(Some(pickup))
        );
    }
    let open_dependencies = context
        .dependencies
        .iter()
        .filter(|dependency| dependency.status != "done")
        .collect::<Vec<_>>();
    let open_blockers = context
        .blockers
        .iter()
        .filter(|blocker| blocker.status == "open")
        .collect::<Vec<_>>();
    let open_gate_count = context
        .gates
        .iter()
        .filter(|gate| gate.status == "open")
        .count();
    let mut extra = String::new();
    if !open_dependencies.is_empty() {
        extra.push_str(&format!(
            " [deps: {}]",
            open_dependencies
                .iter()
                .map(|dependency| format!("#{}", dependency.seq))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if !open_blockers.is_empty() {
        extra.push_str(&format!(
            " [blockers: {}]",
            open_blockers
                .iter()
                .map(|blocker| blocker.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if open_gate_count > 0 {
        extra.push_str(&format!(" [gates open: {open_gate_count}]"));
    }
    if task.kind != "work" {
        extra.push_str(&format!(" [{}]", task.kind));
    }
    println!(
        "{} #{} {}{}",
        status_glyph(&task.status),
        task.seq,
        task.title,
        extra
    );
    if !task.intent.is_empty() {
        let label = task
            .intent_source
            .as_deref()
            .map(|source| format!("intent [{source}]"))
            .unwrap_or_else(|| "intent".to_owned());
        println!("  {label}: {}", task.intent.replace('\n', "\n  "));
    }
    if let Some(result) = task.result.as_deref() {
        let label = task
            .result_source
            .as_deref()
            .map(|source| format!("result [{source}]"))
            .unwrap_or_else(|| "result".to_owned());
        println!("  {label}: {}", result.replace('\n', "\n  "));
    }
    println!("  kind: {}", task.kind);
    for (label, event) in [
        ("created", context.activity.created_event.as_ref()),
        ("last event", context.activity.last_event.as_ref()),
        ("started event", context.activity.started_event.as_ref()),
        ("completed event", context.activity.completed_event.as_ref()),
    ] {
        if let Some(event) = event {
            println!(
                "  {label}: {} by {}{} (@{})",
                event.at,
                event.actor,
                event
                    .model
                    .as_ref()
                    .map(|model| format!(" [model {model}]"))
                    .unwrap_or_default(),
                event.event_id
            );
        }
    }
    if let Some(parent) = &context.parent {
        println!(
            "  parent {} #{} {}",
            status_glyph(&parent.status),
            parent.seq,
            parent.title
        );
    }
    if let Some(replacement) = &context.replacement {
        println!(
            "  replacement {} #{} {}",
            status_glyph(&replacement.status),
            replacement.seq,
            replacement.title
        );
    }
    if task.priority != 0 {
        println!("  priority: {}", task.priority);
    }
    if !context.tags.is_empty() {
        println!("  tags: {}", context.tags.join(", "));
    }
    for dependency in &context.dependencies {
        println!(
            "  depends {} #{} {}",
            status_glyph(&dependency.status),
            dependency.seq,
            dependency.title
        );
    }
    for gate in &context.gates {
        let evidence = gate
            .evidence_locator
            .as_ref()
            .map(|locator| format!(" -> {locator}"))
            .unwrap_or_default();
        let digest = gate
            .evidence_sha256
            .as_ref()
            .map(|digest| format!(" [sha256 {digest}]"))
            .unwrap_or_default();
        let note = gate
            .note
            .as_ref()
            .map(|note| format!(" ({note})"))
            .unwrap_or_default();
        println!(
            "  gate [{}] {} ({}): {}{}{}{}",
            gate.status, gate.name, gate.kind, gate.requirement, evidence, digest, note
        );
    }
    for blocker in &context.blockers {
        let evidence = blocker
            .evidence_locator
            .as_ref()
            .map(|locator| format!(" -> {locator}"))
            .unwrap_or_default();
        let digest = blocker
            .evidence_sha256
            .as_ref()
            .map(|digest| format!(" [sha256 {digest}]"))
            .unwrap_or_default();
        let note = blocker
            .note
            .as_ref()
            .map(|note| format!(" ({note})"))
            .unwrap_or_default();
        println!(
            "  blocker [{}] {}: {}{}{}{}",
            blocker.status, blocker.name, blocker.reason, evidence, digest, note
        );
    }
    for commit in &context.commit_associations {
        let note = commit
            .note
            .as_ref()
            .map(|note| format!(" ({note})"))
            .unwrap_or_default();
        println!(
            "  commit {} {}{}",
            commit.repository, commit.commit_oid, note
        );
    }
    for reference in &context.external_references {
        println!(
            "  reference [{}] {}{}",
            reference.kind,
            reference.locator,
            reference
                .sha256
                .as_ref()
                .map(|sha| format!(" [sha256 {sha}]"))
                .unwrap_or_default()
        );
    }
    for projection in &context.mise_projections {
        println!(
            "  Mise [{}] {} candidate {} campaign {}{}",
            projection.projection.disposition.as_str(),
            projection.projection_sha256,
            projection.projection.candidate_id,
            projection.projection.campaign_id,
            projection
                .projection
                .nomination_id
                .as_ref()
                .map(|id| format!(" nomination {id}"))
                .unwrap_or_default()
        );
    }
    for child in &context.children {
        println!(
            "  child {} #{} {}",
            status_glyph(&child.status),
            child.seq,
            child.title
        );
    }
    let notes = context
        .recent_events
        .iter()
        .filter(|event| event.kind == "note")
        .take(5)
        .collect::<Vec<_>>();
    for event in notes.into_iter().rev() {
        println!(
            "  note {} {}: {}",
            short_event_time(&event.at),
            event.actor,
            event
                .why
                .as_deref()
                .unwrap_or_default()
                .replace('\n', "\n    ")
        );
    }
    if context.recent_events_truncated {
        println!(
            "  (older events omitted: papertiger log --task {})",
            task.seq
        );
    }
}

/// Windows gives the main thread 1 MiB, which clap's derived command tree for
/// this CLI exceeds in unoptimized builds. Run on an explicitly sized stack.
const CLI_STACK_BYTES: usize = 16 * 1024 * 1024;

fn main() -> Result<()> {
    std::thread::Builder::new()
        .name("papertiger".into())
        .stack_size(CLI_STACK_BYTES)
        .spawn(cli_main)
        .context("start papertiger command thread")?
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn cli_main() -> Result<()> {
    user_setup::verify_runtime()?;
    let cli = Cli::parse();
    run(cli).map_err(pt::normalize_sqlite_lock_error)
}

fn run(cli: Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::Personal(user_setup::Command::Setup {
            home,
            dry_run,
            replace_managed,
        }) => {
            if cli.db.is_some()
                || cli.authority_project_root.is_some()
                || cli.actor.is_some()
                || cli.model.is_some()
                || cli.reasoning_effort.is_some()
                || cli.session.is_some()
            {
                bail!(
                    "setup-user selects its own installation and personal authority; omit planning selectors and use --home <directory>"
                );
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&user_setup::run(
                    home.as_deref(),
                    *dry_run,
                    *replace_managed,
                    false
                )?)?
            );
            return Ok(());
        }
        Cmd::Personal(user_setup::Command::Uninstall { home, dry_run }) => {
            if cli.db.is_some()
                || cli.authority_project_root.is_some()
                || cli.actor.is_some()
                || cli.model.is_some()
                || cli.reasoning_effort.is_some()
                || cli.session.is_some()
            {
                bail!(
                    "uninstall-user preserves planning data; omit planning selectors and use --home <directory>"
                );
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&user_setup::run(
                    home.as_deref(),
                    *dry_run,
                    false,
                    true
                )?)?
            );
            return Ok(());
        }
        _ => {}
    }
    run_planner(cli)
}

fn run_planner(cli: Cli) -> Result<()> {
    let json = cli.json || cli.cmd.implies_json();
    let session = cli
        .session
        .or_else(|| std::env::var("PAPERTIGER_SESSION").ok());
    if let Some(session) = &session {
        pt::validate_session(session)?;
    }
    if matches!(cli.cmd, Cmd::Schema) {
        println!(
            "{}",
            include_str!("../docs/schemas/planner.json").trim_end()
        );
        return Ok(());
    }
    let mutation = !cli.cmd.opens_authority_read_only()
        && !matches!(
            cli.cmd,
            Cmd::SetupProject { .. } | Cmd::UninstallProject { .. } | Cmd::Init
        );
    if cli.model.is_some() && !mutation {
        bail!(
            "--model records planning event authorship; omit --model for commands that do not record events"
        );
    }
    if cli.reasoning_effort.is_some() && !mutation {
        bail!(
            "--reasoning-effort records planning event authorship; omit --reasoning-effort for commands that do not record events"
        );
    }
    let model = if mutation {
        cli.model.or_else(|| std::env::var("PAPERTIGER_MODEL").ok())
    } else {
        None
    };
    if let Some(model) = &model {
        pt::validate_model(model)?;
    }
    let reasoning_effort = if mutation {
        cli.reasoning_effort
            .or_else(|| std::env::var("PAPERTIGER_REASONING_EFFORT").ok())
    } else {
        None
    };
    if let Some(effort) = &reasoning_effort {
        pt::validate_reasoning_effort(effort)?;
        if model.is_none() {
            bail!(
                "reasoning effort requires a model identifier; supply --model <model-id> or PAPERTIGER_MODEL, or omit --reasoning-effort and PAPERTIGER_REASONING_EFFORT"
            );
        }
    }
    if json
        && matches!(
            cli.cmd,
            Cmd::Init
                | Cmd::Tree { .. }
                | Cmd::Gate {
                    cmd: GateCmd::List { .. }
                }
                | Cmd::Blocker {
                    cmd: BlockerCmd::List { .. }
                }
        )
    {
        bail!(
            "this command has no JSON projection; omit --json or use status/show --json for planner context"
        );
    }
    macro_rules! mutation_output {
        ($($args:tt)*) => { if !json { println!($($args)*); } };
    }

    if let Cmd::SetupProject {
        project_root,
        dry_run,
        replace_managed,
        authority_path,
        skill_target,
    } = &cli.cmd
    {
        if cli.db.is_some() {
            bail!(
                "setup-project does not accept --db; omit --db and select a nondefault project authority with --authority-path <project-relative-path>"
            );
        }
        if cli.actor.is_some() {
            bail!(
                "setup-project does not accept --actor because installation records no planning events; omit --actor"
            );
        }
        if cli.authority_project_root.is_some() {
            bail!(
                "setup-project does not accept global --project-root; pass the consuming project root as its positional <PROJECT_ROOT> argument"
            );
        }
        let result = project_setup::setup_project(project_setup::SetupProjectRequest {
            project_root,
            source_binary: None,
            dry_run: *dry_run,
            replace_managed: *replace_managed,
            authority_path: authority_path.as_deref(),
            skill_target: *skill_target,
        })?;
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            let mode = if result.dry_run { "planned" } else { "applied" };
            println!(
                "papertiger {} project setup at {}",
                mode, result.project_root
            );
            for action in &result.actions {
                println!("  {:?} {}", action.action, action.path);
            }
            println!("  repository guidance (observed only; never managed):");
            for file in &result.project_guidance.files {
                println!("    {}: {}", file.path, file.classification.as_str());
            }
            println!("next:");
            for action in &result.next_actions {
                println!("  - {action}");
            }
        }
        return Ok(());
    }

    if let Cmd::InspectProjectGuidance {} = &cli.cmd {
        if cli.db.is_some() {
            bail!(
                "inspect-project-guidance does not accept --db because it never opens the planning authority; omit --db"
            );
        }
        if cli.actor.is_some() {
            bail!(
                "inspect-project-guidance does not accept --actor because it records no planning events; omit --actor"
            );
        }
        let root = if let Some(root) = cli.authority_project_root.as_deref() {
            root.to_path_buf()
        } else {
            let current = std::env::current_dir().context("resolve current directory")?;
            project_setup::discover_project_root(&current)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "no project-install receipt was discovered from {}; run from an installed project or pass --project-root <DIR>. For a first installation, inspect setup without writing with: papertiger setup-project \"{}\" --dry-run --json",
                    current.display(),
                    current.display()
                )
            })?
        };
        let result = project_setup::inspect_installed_project_guidance(&root)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("papertiger repository guidance at {}", result.project_root);
            println!(
                "scope: {} ({} bytes per file)",
                result.scope, result.max_file_bytes
            );
            for file in &result.files {
                println!("  {}: {}", file.path, file.classification.as_str());
                println!("    {}", file.detail);
                println!(
                    "    repository-owned trigger example: {}",
                    file.corrective_trigger
                );
            }
            println!("pair: {}", result.pair.detail);
            println!("limitations:");
            for limitation in &result.limitations {
                println!("  - {limitation}");
            }
        }
        return Ok(());
    }

    if let Cmd::UninstallProject {
        project_root,
        dry_run,
    } = &cli.cmd
    {
        if cli.db.is_some() {
            bail!(
                "uninstall-project does not accept --db; the project-install receipt selects the preserved authority"
            );
        }
        if cli.actor.is_some() {
            bail!(
                "uninstall-project does not accept --actor because removal records no planning events; omit --actor"
            );
        }
        if cli.authority_project_root.is_some() {
            bail!(
                "uninstall-project does not accept global --project-root; pass the installed project root as its positional <PROJECT_ROOT> argument"
            );
        }
        let result = project_setup::uninstall_project(project_setup::UninstallProjectRequest {
            project_root,
            source_binary: None,
            dry_run: *dry_run,
        })?;
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            let mode = if result.dry_run { "planned" } else { "applied" };
            println!(
                "papertiger {} project uninstall at {}",
                mode, result.project_root
            );
            for action in &result.actions {
                println!("  {:?} {}", action.action, action.path);
            }
            println!("retained:");
            for retained in &result.retained {
                println!("  - {retained}");
            }
            println!("next:");
            for action in &result.next_actions {
                println!("  - {action}");
            }
        }
        return Ok(());
    }

    let project_root = cli.authority_project_root.clone();
    let db_override = cli.db.or_else(|| std::env::var("PAPERTIGER_DB").ok());
    let evidence_db_override = db_override.clone();
    let evidence_verify = matches!(
        &cli.cmd,
        Cmd::Evidence {
            cmd: EvidenceCmd::Verify { .. }
        }
    );
    if project_root.is_some() && db_override.is_some() && !evidence_verify {
        bail!(
            "ordinary planner commands do not accept --project-root together with --db or PAPERTIGER_DB; remove the database override so the project-install receipt selects one canonical authority. `evidence verify` alone retains this combination so an explicitly selected database can verify file: locators beneath a supplied project root"
        );
    }
    let db_path = match (db_override, project_root.as_deref()) {
        (Some(path), _) => path,
        (None, Some(root)) => project_setup::project_authority(root)?
            .to_string_lossy()
            .into_owned(),
        (None, None) => project_setup::discover_project_authority(&std::env::current_dir()?)?
            .or(user_setup::fallback_authority()?)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "state/papertiger.sqlite".into()),
    };
    let actor = cli
        .actor
        .or_else(|| std::env::var("PAPERTIGER_ACTOR").ok())
        .unwrap_or_else(|| "operator".into());

    if let Cmd::Init = cli.cmd {
        if let Some(dir) = std::path::Path::new(&db_path).parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let conn = pt::open_for_init(&db_path)?;
        match pt::init_at(&conn, &db_path)? {
            pt::InitOutcome::Created => {
                println!("initialized {db_path} (schema v{})", pt::SCHEMA_VERSION)
            }
            pt::InitOutcome::Migrated { from, to } => {
                println!("migrated {db_path} from schema v{from} to v{to}")
            }
            pt::InitOutcome::Current => println!(
                "{db_path} is already a Papertiger authority at schema v{}; nothing changed",
                pt::SCHEMA_VERSION
            ),
        }
        return Ok(());
    }

    if let Cmd::Backup { output } = &cli.cmd {
        let receipt = pt::backup_authority(std::path::Path::new(&db_path), output)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&receipt)?);
        } else {
            println!(
                "backed up {} to {} (schema v{}, {} bytes, SHA-256 {}); historical task and evidence semantics were preserved without validation",
                receipt.source,
                receipt.output,
                receipt.source_schema_version,
                receipt.bytes,
                receipt.sha256
            );
        }
        return Ok(());
    }

    if let Cmd::RepairGuards { why, dry_run } = &cli.cmd {
        let conn = pt::open_existing_for_guard_repair(&db_path)?;
        let drifted = if *dry_run {
            pt::write_guard_drift(&conn)?
        } else {
            let why = why.as_deref().ok_or_else(|| {
                anyhow::anyhow!(
                    "repair-guards requires --why <reason>; preview first with --dry-run"
                )
            })?;
            pt::repair_write_guards(&conn, &actor, why)?
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema": "papertiger.guard_repair.v1",
                    "dry_run": dry_run,
                    "changed": !dry_run && !drifted.is_empty(),
                    "guards": drifted,
                }))?
            );
        } else if drifted.is_empty() {
            println!("write guards are intact; nothing changed");
        } else {
            let verb = if *dry_run {
                "would reinstall"
            } else {
                "reinstalled"
            };
            for guard in &drifted {
                let action = match (guard.state, *dry_run) {
                    (pt::GuardDriftState::Foreign, true) => "would remove",
                    (pt::GuardDriftState::Foreign, false) => "removed",
                    _ => verb,
                };
                println!("{action} {} trigger {}", guard.state.as_str(), guard.name);
            }
        }
        return Ok(());
    }

    let conn = if cli.cmd.opens_authority_read_only() {
        let conn = pt::open_existing_read_only(&db_path)?;
        let drifted = pt::write_guard_drift(&conn)?;
        if !drifted.is_empty() {
            eprintln!(
                "warning: {} write guard(s) are missing, altered or foreign; mutations refuse until `papertiger repair-guards --why <reason>` restores them",
                drifted.len()
            );
        }
        conn
    } else {
        pt::open_existing(&db_path)?
    };
    if cli.cmd.opens_authority_read_only() {
        conn.execute_batch("BEGIN DEFERRED TRANSACTION")
            .context("begin read-only Papertiger authority snapshot")?;
    }

    let recorder = if mutation && (json || model.is_some()) {
        Some(pt::MutationRecorder::with_reasoning_effort(
            &conn,
            model.as_deref(),
            reasoning_effort.as_deref(),
        )?)
    } else {
        None
    };
    match cli.cmd {
        Cmd::SetupProject { .. } | Cmd::Personal(_) => {
            unreachable!()
        }
        Cmd::InspectProjectGuidance { .. } => unreachable!(),
        Cmd::UninstallProject { .. } => unreachable!(),
        Cmd::Init => unreachable!(),
        Cmd::Schema => unreachable!(),
        Cmd::Backup { .. } => unreachable!(),
        Cmd::RepairGuards { .. } => unreachable!(),
        Cmd::Status {} => {
            let status = pt::status_response(&conn, &db_path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
                return Ok(());
            }
            println!(
                "papertiger {} | schema v{} | {}",
                status.authority.papertiger_version,
                status.authority.schema_version,
                status.authority.resolved_path
            );
            if status.active_plans.is_empty() {
                println!(
                    "no active plans; inspect `papertiger plan list` or create one with `papertiger plan add <slug> <title>`"
                );
            }
            for active in &status.active_plans {
                let counts = &active.counts;
                println!(
                    "plan {}: {} [proposed {}, in_progress {}, done {}, retired {}, rejected {}]",
                    active.plan.slug,
                    active.plan.title,
                    counts.proposed,
                    counts.in_progress,
                    counts.done,
                    counts.retired,
                    counts.rejected
                );
                for entry in &active.in_progress.parents.entries {
                    println!(
                        "> parent #{} {}{}",
                        entry.task.seq,
                        entry.task.title,
                        pickup_label(entry.pickup.as_ref())
                    );
                }
                for entry in &active.in_progress.leaves.entries {
                    println!(
                        "> leaf #{} {}{}",
                        entry.task.seq,
                        entry.task.title,
                        pickup_label(entry.pickup.as_ref())
                    );
                }
                for entry in &active.ready.entries {
                    println!("· #{} {}", entry.task.seq, entry.task.title);
                }
                if !active.ready.complete {
                    println!(
                        "  ... {} ready task(s) omitted; run `{}`",
                        active.ready.omitted_count,
                        active
                            .ready
                            .continuation_command
                            .as_deref()
                            .unwrap_or_default()
                    );
                }
            }
            for note in &status.recent_notes.entries {
                println!(
                    "note @{} {} {}: {}",
                    note.event_id,
                    short_event_time(&note.at),
                    note.actor,
                    note.why.as_deref().unwrap_or_default()
                );
            }
            if !status.recent_notes.complete {
                println!(
                    "... {} older note(s) omitted; run `{}` and follow its event cursor",
                    status.recent_notes.omitted_count,
                    status
                        .recent_notes
                        .continuation_command
                        .as_deref()
                        .unwrap_or_default()
                );
            }
        }
        Cmd::Plan { cmd } => match cmd {
            PlanCmd::Add {
                slug,
                title,
                intent,
            } => {
                let intent = intent.optional()?.unwrap_or_default();
                pt::add_plan(&conn, &actor, &slug, &title, &intent)?;
                mutation_output!("plan {slug} created");
            }
            PlanCmd::List { plan } => {
                let plans = pt::plan_inventory(&conn, plan.as_deref())?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema": "papertiger.plan_list.v1", "total": plans.len(), "plans": plans
                        }))?
                    );
                } else {
                    for plan in plans {
                        println!("{:8} {}: {}", plan.status, plan.slug, plan.title);
                    }
                }
            }
            PlanCmd::Edit {
                slug,
                title,
                intent,
                why,
            } => {
                reject_multiple_stdin(&[
                    ("intent", intent.reads_stdin()),
                    ("why", why.reads_stdin()),
                ])?;
                let intent = intent.optional()?;
                let why = why.required()?;
                let changed = pt::edit_plan(
                    &conn,
                    &actor,
                    &slug,
                    title.as_deref(),
                    intent.as_deref(),
                    &why,
                )?;
                mutation_output!("plan {slug} updated ({})", changed.join(", "));
            }
            PlanCmd::Set { slug, status, why } => {
                let why = why.required()?;
                pt::set_plan_status(&conn, &actor, &slug, &status, &why)?;
                mutation_output!("plan {slug} -> {status}");
            }
        },
        Cmd::Add {
            title,
            plan,
            intent,
            intent_source,
            kind,
            parent,
            dep,
            tag,
            priority,
            why,
            start,
        } => {
            reject_multiple_stdin(&[("intent", intent.reads_stdin()), ("why", why.reads_stdin())])?;
            let intent = intent.optional()?.unwrap_or_default();
            let why = why.optional()?;
            let parent = parent.map(|p| pt::parse_task_ref(&p)).transpose()?;
            let deps: Vec<i64> = dep
                .iter()
                .map(|d| pt::parse_task_ref(d))
                .collect::<Result<_>>()?;
            let (seq, slug) = pt::add_task_for_plan_with_options(
                &conn,
                &actor,
                plan.as_deref(),
                pt::TaskCreation {
                    title: &title,
                    intent: &intent,
                    intent_source: intent_source.as_deref(),
                    kind: &kind,
                    parent,
                    deps: &deps,
                    tags: &tag,
                    priority,
                    why: why.as_deref(),
                    start,
                    session: session.as_deref(),
                },
            )?;
            if start {
                mutation_output!("#{seq} added to {slug} and in progress");
            } else {
                mutation_output!("#{seq} added to {slug}");
            }
        }
        Cmd::Show { task, no_history } => {
            let seq = pt::parse_task_ref(&task)?;
            if no_history {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&pt::task_current(&conn, seq)?)?
                );
            } else {
                let context = pt::task_context(&conn, seq)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&context)?);
                } else {
                    print_task_context(&context);
                }
            }
        }
        Cmd::List {
            all_plans,
            limit,
            after_seq,
            snapshot,
            plan,
            status,
            tag,
            sort,
        } => {
            if all_plans {
                let response = pt::task_inventory(
                    &conn,
                    status.as_deref(),
                    tag.as_deref(),
                    limit.unwrap_or(100),
                    after_seq,
                    snapshot.as_deref(),
                )?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&response)?);
                } else {
                    for item in &response.tasks {
                        println!(
                            "{} #{} [{}; {}] {}",
                            status_glyph(&item.task.status),
                            item.task.seq,
                            item.plan.slug,
                            item.plan.status,
                            item.task.title
                        );
                    }
                    println!("{} total; {} remaining", response.total, response.remaining);
                    if let Some(seq) = response.next_after_seq {
                        println!(
                            "continue with the same filters: list --all-plans --after-seq {seq} --snapshot {}",
                            response.snapshot
                        );
                    }
                }
                return Ok(());
            }
            if json {
                let response = pt::task_list_response(
                    &conn,
                    plan.as_deref(),
                    status.as_deref(),
                    tag.as_deref(),
                    &sort,
                )?;
                println!("{}", serde_json::to_string_pretty(&response)?);
                return Ok(());
            }
            let (plan_id, _) = pt::resolve_plan(&conn, plan.as_deref())?;
            let tasks = match sort.as_str() {
                "seq" => pt::list_tasks(&conn, plan_id, status.as_deref(), tag.as_deref())?,
                "activity" => {
                    pt::list_tasks_by_activity(&conn, plan_id, status.as_deref(), tag.as_deref())?
                }
                _ => bail!("unknown list sort '{sort}' (expected seq|activity)"),
            };
            for t in &tasks {
                print_task_line(&conn, t)?;
            }
        }
        Cmd::Search {
            query,
            plan,
            status,
            limit,
            compact,
        } => {
            let response =
                pt::search_tasks(&conn, &query, plan.as_deref(), status.as_deref(), limit)?;
            if json {
                if compact {
                    println!("{}", serde_json::to_string_pretty(&response.compact())?);
                    return Ok(());
                }
                println!("{}", serde_json::to_string_pretty(&response)?);
                return Ok(());
            }
            if response.results.is_empty() {
                println!("no matching tasks");
            }
            for result in &response.results {
                println!(
                    "{} #{} [{}] {}",
                    status_glyph(&result.task.status),
                    result.task.seq,
                    result.plan,
                    result.task.title
                );
                println!(
                    "  matched {} (score {}); {}: {}",
                    result.matched_fields.join(", "),
                    result.score,
                    result.excerpt.field,
                    result.excerpt.text.replace('\n', " ")
                );
            }
            if response.truncated {
                println!(
                    "{} matches; showing {}. Increase --limit up to 200.",
                    response.total_matches,
                    response.results.len()
                );
            }
        }
        Cmd::Edit {
            task,
            title,
            intent,
            intent_source,
            clear_intent_source,
            parent,
            clear_parent,
            kind,
            priority,
            why,
        } => {
            reject_multiple_stdin(&[("intent", intent.reads_stdin()), ("why", why.reads_stdin())])?;
            let intent = intent.optional()?;
            let why = why.required()?;
            let parent = if clear_parent {
                Some(None)
            } else {
                parent
                    .map(|parent| pt::parse_task_ref(&parent).map(Some))
                    .transpose()?
            };
            let seq = pt::parse_task_ref(&task)?;
            let intent_source = if clear_intent_source {
                Some(None)
            } else {
                intent_source.as_deref().map(Some)
            };
            let changed = pt::edit_task(
                &conn,
                &actor,
                seq,
                pt::TaskEdit {
                    title: title.as_deref(),
                    intent: intent.as_deref(),
                    intent_source,
                    parent,
                    kind: kind.as_deref(),
                    priority,
                },
                &why,
            )?;
            mutation_output!("#{seq} updated ({})", changed.join(", "));
        }
        Cmd::Start { task, why } => {
            let why = why.optional()?;
            let seq = pt::parse_task_ref(&task)?;
            pt::start_task(&conn, &actor, seq, why.as_deref(), session.as_deref())?;
            mutation_output!("#{seq} in progress");
        }
        Cmd::MovePlan { tasks, plan, why } => {
            let why = why.required()?;
            let sequences = tasks
                .iter()
                .map(|task| pt::parse_task_ref(task))
                .collect::<Result<Vec<_>>>()?;
            pt::move_tasks_to_plan(&conn, &actor, &sequences, &plan, &why)?;
            mutation_output!("moved {} task(s) to {plan}", sequences.len());
        }
        Cmd::Done {
            task,
            result,
            result_source,
        } => {
            let result = result.optional()?;
            let seq = pt::parse_task_ref(&task)?;
            pt::complete_task_with_source(
                &conn,
                &actor,
                seq,
                result.as_deref(),
                result_source.as_deref(),
            )?;
            mutation_output!("#{seq} done");
        }
        Cmd::Reopen { task, why } => {
            let why = why.required()?;
            let seq = pt::parse_task_ref(&task)?;
            pt::reopen_task(&conn, &actor, seq, &why)?;
            mutation_output!("#{seq} reopened");
        }
        Cmd::Retire { task, into, why } => {
            let why = why.required()?;
            let seq = pt::parse_task_ref(&task)?;
            let replacement_seq = into.as_deref().map(pt::parse_task_ref).transpose()?;
            pt::retire_task(&conn, &actor, seq, replacement_seq, &why)?;
            if let Some(replacement_seq) = replacement_seq {
                mutation_output!("#{seq} retired into #{replacement_seq}");
            } else {
                mutation_output!("#{seq} retired");
            }
        }
        Cmd::Reject { task, why } => {
            let why = why.required()?;
            let seq = pt::parse_task_ref(&task)?;
            pt::reject_task(&conn, &actor, seq, &why)?;
            mutation_output!("#{seq} rejected");
        }
        Cmd::Gate { cmd } => match cmd {
            GateCmd::Add {
                task,
                name,
                kind,
                requirement,
            } => {
                let seq = pt::parse_task_ref(&task)?;
                pt::add_gate(&conn, &actor, seq, &name, &kind, &requirement)?;
                mutation_output!("gate '{name}' added to #{seq}");
            }
            GateCmd::Close {
                task,
                name,
                evidence,
                sha256,
                note,
            } => {
                let seq = pt::parse_task_ref(&task)?;
                pt::close_gate(
                    &conn,
                    &actor,
                    seq,
                    &name,
                    &evidence,
                    sha256.as_deref(),
                    note.as_deref(),
                )?;
                mutation_output!("gate '{name}' on #{seq} closed");
            }
            GateCmd::Waive { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::waive_gate(&conn, &actor, seq, &name, &why)?;
                mutation_output!("gate '{name}' on #{seq} waived");
            }
            GateCmd::Reopen { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::reopen_gate(&conn, &actor, seq, &name, &why)?;
                mutation_output!("gate '{name}' on #{seq} reopened");
            }
            GateCmd::Remove { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::remove_open_gate(&conn, &actor, seq, &name, &why)?;
                mutation_output!("gate '{name}' removed from #{seq}");
            }
            GateCmd::List { task } => {
                let t = pt::get_task(&conn, pt::parse_task_ref(&task)?)?;
                let mut st = conn.prepare(
                    "SELECT name, kind, status, requirement FROM gates WHERE task_id=?1 ORDER BY gate_id",
                )?;
                let rows: Vec<(String, String, String, String)> = st
                    .query_map(params![t.task_id], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                for (name, kind, status, req) in rows {
                    println!("[{status}] {name} ({kind}): {req}");
                }
            }
        },
        Cmd::Blocker { cmd } => match cmd {
            BlockerCmd::Add { task, name, reason } => {
                let seq = pt::parse_task_ref(&task)?;
                pt::add_task_blocker(&conn, &actor, seq, &name, &reason)?;
                mutation_output!("blocker '{name}' added to #{seq}");
            }
            BlockerCmd::Resolve {
                task,
                name,
                evidence,
                sha256,
                note,
            } => {
                let seq = pt::parse_task_ref(&task)?;
                pt::resolve_task_blocker(
                    &conn,
                    &actor,
                    seq,
                    &name,
                    &evidence,
                    sha256.as_deref(),
                    note.as_deref(),
                )?;
                mutation_output!("blocker '{name}' on #{seq} resolved");
            }
            BlockerCmd::Waive { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::waive_task_blocker(&conn, &actor, seq, &name, &why)?;
                mutation_output!("blocker '{name}' on #{seq} waived");
            }
            BlockerCmd::Reopen { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::reopen_task_blocker(&conn, &actor, seq, &name, &why)?;
                mutation_output!("blocker '{name}' on #{seq} reopened");
            }
            BlockerCmd::Remove { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::remove_open_task_blocker(&conn, &actor, seq, &name, &why)?;
                mutation_output!("blocker '{name}' removed from #{seq}");
            }
            BlockerCmd::List { task } => {
                let task = pt::get_task(&conn, pt::parse_task_ref(&task)?)?;
                for blocker in pt::task_blockers(&conn, task.task_id)? {
                    println!(
                        "[{}] {}: {}{}",
                        blocker.status,
                        blocker.name,
                        blocker.reason,
                        blocker
                            .evidence_locator
                            .map(|locator| format!(" -> {locator}"))
                            .unwrap_or_default()
                    );
                }
            }
        },
        Cmd::Reference { cmd } => match cmd {
            ReferenceCmd::Add {
                task,
                locator,
                kind,
                sha256,
                note,
            } => {
                let seq = pt::parse_task_ref(&task)?;
                let reference = pt::new_external_reference(
                    &kind,
                    &locator,
                    sha256.as_deref(),
                    note.as_deref(),
                )?;
                pt::add_external_reference(&conn, &actor, seq, &reference)?;
                mutation_output!("reference {kind} '{locator}' recorded on #{seq}");
            }
            ReferenceCmd::Remove {
                task,
                locator,
                kind,
                why,
            } => {
                let seq = pt::parse_task_ref(&task)?;
                pt::remove_external_reference(
                    &conn,
                    &actor,
                    seq,
                    &kind,
                    &locator,
                    &why.required()?,
                )?;
                mutation_output!("reference {kind} '{locator}' removed from #{seq}");
            }
            ReferenceCmd::List { task } => {
                let references = pt::external_references(&conn, pt::parse_task_ref(&task)?)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&references)?);
                } else {
                    for reference in references {
                        println!("[{}] {}", reference.kind, reference.locator);
                    }
                }
            }
            ReferenceCmd::Find { locator } => {
                let matches = pt::find_external_references(&conn, &locator)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&matches)?);
                } else {
                    for item in matches {
                        println!(
                            "#{} [{}] {}",
                            item.task_seq, item.reference.kind, item.task_title
                        );
                    }
                }
            }
        },
        Cmd::Commit { cmd } => match cmd {
            CommitCmd::Add {
                task,
                commit_oid,
                repo,
                note,
            } => {
                let seq = pt::parse_task_ref(&task)?;
                pt::add_commit_association(
                    &conn,
                    &actor,
                    seq,
                    &repo,
                    &commit_oid,
                    note.as_deref(),
                )?;
                mutation_output!("commit {commit_oid} ({repo}) recorded on #{seq}");
            }
            CommitCmd::Remove {
                task,
                commit_oid,
                repo,
                why,
            } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::remove_commit_association(&conn, &actor, seq, &repo, &commit_oid, &why)?;
                mutation_output!("commit {commit_oid} ({repo}) removed from #{seq}");
            }
            CommitCmd::List { task } => {
                let seq = pt::parse_task_ref(&task)?;
                let commits = pt::commit_associations(&conn, seq)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&commits)?);
                } else {
                    for commit in commits {
                        let note = commit
                            .note
                            .map(|note| format!(" - {note}"))
                            .unwrap_or_default();
                        println!(
                            "{} {} {}{}",
                            short_event_time(&commit.recorded_at),
                            commit.repository,
                            commit.commit_oid,
                            note
                        );
                    }
                }
            }
            CommitCmd::Find { commit_oid, repo } => {
                let matches = pt::find_commit_associations(&conn, &commit_oid, repo.as_deref())?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&matches)?);
                } else {
                    for found in matches {
                        println!(
                            "#{} {} ({}) {}",
                            found.task_seq,
                            found.task_title,
                            found.commit.repository,
                            found.commit.recorded_at
                        );
                    }
                }
            }
        },
        Cmd::Dep { cmd } => match cmd {
            DepCmd::Add { task, on, why } => {
                let why = why.required()?;
                let (a, b) = (pt::parse_task_ref(&task)?, pt::parse_task_ref(&on)?);
                pt::add_dep(&conn, &actor, a, b, &why)?;
                mutation_output!("#{a} now depends on #{b}");
            }
            DepCmd::Remove { task, on, why } => {
                let why = why.required()?;
                let (a, b) = (pt::parse_task_ref(&task)?, pt::parse_task_ref(&on)?);
                pt::remove_dep(&conn, &actor, a, b, &why)?;
                mutation_output!("#{a} no longer depends on #{b}");
            }
        },
        Cmd::Focus { plan, limit, all } => {
            let selected_plan = match plan.as_deref() {
                Some(slug) => Some(pt::resolve_plan(&conn, Some(slug))?),
                None if json => pt::active_plan(&conn)?,
                None => Some(pt::resolve_plan(&conn, None)?),
            };
            let Some((plan_id, slug)) = selected_plan else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&pt::FocusResponse::no_active_plan())?
                );
                return Ok(());
            };
            let response = pt::focus(&conn, plan_id, limit, all, session.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else if response.projection.entries.is_empty() {
                println!("nothing actionable");
            } else {
                println!("focus {slug}");
                let mut current = None::<String>;
                for entry in response.projection.entries {
                    if current.as_deref() != Some(entry.readiness.as_str()) {
                        println!("{}:", entry.readiness);
                        current = Some(entry.readiness.clone());
                    }
                    let blockers = if entry.blockers.is_empty() {
                        String::new()
                    } else {
                        format!(" [blocked by {}]", entry.blockers.join(" "))
                    };
                    println!(
                        "  #{} {} [priority {} unlocks {} downstream {} gates {}]{}{}",
                        entry.task.seq,
                        entry.task.title,
                        entry.task.priority,
                        entry.immediate_unlock_count,
                        entry.unfinished_downstream_count,
                        entry.open_gate_count,
                        blockers,
                        pickup_label(entry.pickup.as_ref()),
                    );
                }
                if response.projection.omitted_count > 0
                    && let Some(command) = response.projection.continuation_command
                {
                    println!(
                        "  ... {} actionable task(s) omitted; run `{command}`",
                        response.projection.omitted_count
                    );
                }
            }
        }
        Cmd::Tag { cmd } => match cmd {
            TagCmd::Add { task, tag, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::add_tag(&conn, &actor, seq, &tag, &why)?;
                mutation_output!("tag '{tag}' added to #{seq}");
            }
            TagCmd::Remove { task, tag, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::remove_tag(&conn, &actor, seq, &tag, &why)?;
                mutation_output!("tag '{tag}' removed from #{seq}");
            }
        },
        Cmd::Tree { plan } => {
            let (plan_id, slug) = pt::resolve_plan(&conn, plan.as_deref())?;
            println!("{slug}");
            print_tree(&conn, plan_id, None, 1)?;
        }
        Cmd::Note { text, source, task } => {
            let text = text.required()?;
            let task_seq = task.map(|task| pt::parse_task_ref(&task)).transpose()?;
            pt::add_note_with_source(&conn, &actor, task_seq, &text, source.as_deref())?;
            mutation_output!("noted");
        }
        Cmd::Log {
            task,
            limit,
            before_cursor,
            after_cursor,
        } => {
            let task_seq = task.map(|task| pt::parse_task_ref(&task)).transpose()?;
            let log = pt::event_log(
                &conn,
                task_seq,
                limit,
                before_cursor.as_deref(),
                after_cursor.as_deref(),
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&log)?);
                return Ok(());
            }
            for event in &log.events {
                let why = event
                    .why
                    .as_ref()
                    .map(|why| format!(" - {why}"))
                    .unwrap_or_default();
                let payload = event
                    .payload
                    .as_ref()
                    .map(|payload| format!(" {payload}"))
                    .unwrap_or_default();
                println!(
                    "@{} {} {} {}/{}{}{}",
                    event.event_id,
                    short_event_time(&event.at),
                    event.actor,
                    event.entity,
                    event.kind,
                    payload,
                    why
                );
            }
            if log.truncated
                && let Some(cursor) = &log.continuation
            {
                let flag = if log.direction == "after" {
                    "--after-cursor"
                } else {
                    "--before-cursor"
                };
                println!(
                    "more events available: papertiger log {flag} {}{}",
                    cursor.token,
                    task_seq
                        .map(|seq| format!(" --task {seq}"))
                        .unwrap_or_default()
                );
            }
        }
        Cmd::Audit => {
            let findings = pt::audit(&conn)?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"schema": "papertiger.audit.v1", "findings": findings})
                );
                return Ok(());
            }
            if findings.is_empty() {
                println!("no findings");
            }
            for f in findings {
                println!("[{}] {}", f.kind, f.detail);
            }
        }
        Cmd::History { cmd } => match cmd {
            HistoryCmd::Inspect { event_id } => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&pt::history_recovery::inspect(&conn, event_id)?)?
                );
            }
            HistoryCmd::Quarantine {
                event_id,
                expect_sha256,
                why,
            } => {
                let recovery = pt::history_recovery::quarantine(
                    &conn,
                    &actor,
                    event_id,
                    &expect_sha256,
                    &why,
                )?;
                if !json {
                    println!(
                        "quarantined event {event_id}; original retained verbatim in recovery event {recovery}; no historical timestamp, association, or task state was inferred"
                    );
                }
            }
        },
        Cmd::Evidence {
            cmd:
                EvidenceCmd::Verify {
                    task,
                    outcome,
                    task_state,
                    limit,
                    after_cursor,
                },
        } => {
            let task_seq = task.map(|task| pt::parse_task_ref(&task)).transpose()?;
            let project_root = match project_root {
                Some(root) => root,
                None => project_setup::discover_project_root(&std::env::current_dir()?)?
                    .context(
                        "no project-install receipt was found; pass `papertiger evidence verify --project-root <project-root>`",
                    )?,
            };
            let options = pt::EvidenceVerificationOptions {
                task_seq,
                outcome,
                task_state,
                limit,
                after_cursor,
            };
            let mut report = pt::verify_evidence(&conn, &project_root, &options)?;
            if let Some(cursor) = report.projection.next_cursor.clone() {
                let mut arguments = Vec::new();
                if evidence_db_override.is_some() {
                    let database = std::fs::canonicalize(&db_path)
                        .with_context(|| format!("resolve evidence authority {}", db_path))?;
                    arguments.extend(["--db".to_owned(), pt::portable_absolute(&database)?]);
                }
                arguments.extend([
                    "--project-root".to_owned(),
                    report.project_root.clone(),
                    "evidence".to_owned(),
                    "verify".to_owned(),
                ]);
                if let Some(task_seq) = task_seq {
                    arguments.extend(["--task".to_owned(), task_seq.to_string()]);
                }
                arguments.extend([
                    "--outcome".to_owned(),
                    outcome.as_str().to_owned(),
                    "--task-state".to_owned(),
                    task_state.as_str().to_owned(),
                    "--limit".to_owned(),
                    limit.to_string(),
                    "--after-cursor".to_owned(),
                    cursor,
                ]);
                if json {
                    arguments.push("--json".to_owned());
                }
                report.projection.continuation_command = Some(pt::CorrectiveCommand {
                    program: "papertiger".into(),
                    arguments,
                });
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "evidence verification: {} total, {} verified, {} failed, {} unsupported ({})",
                    report.summary.binding_count,
                    report.summary.verified_count,
                    report.summary.failed_count,
                    report.summary.unsupported_count,
                    if report.summary.verification_complete {
                        "complete"
                    } else {
                        "incomplete"
                    }
                );
                println!(
                    "status counts: {}",
                    serde_json::to_string(&report.summary.status_counts)?
                );
                if !report.summary.unsupported_scheme_counts.is_empty() {
                    println!(
                        "unsupported scheme counts: {}",
                        serde_json::to_string(&report.summary.unsupported_scheme_counts)?
                    );
                }
                println!(
                    "details: {} of {} eligible shown from index {}; {} remaining; outcome={}, task-state={}",
                    report.projection.returned_count,
                    report.projection.eligible_count,
                    report.projection.page_start,
                    report.projection.remaining_count,
                    report.projection.outcome.as_str(),
                    report.projection.task_state.as_str()
                );
                for binding in &report.projection.bindings {
                    println!(
                        "  #{} {} '{}' [{}/{}] {}",
                        binding.task_seq,
                        binding.entity,
                        binding.name,
                        binding.status,
                        binding.classification.as_str(),
                        binding.locator
                    );
                    if let Some(detail) = &binding.detail {
                        println!("    {detail}");
                    }
                    for command in &binding.corrective_commands {
                        println!(
                            "    corrective argv: {} {}",
                            command.program,
                            serde_json::to_string(&command.arguments)?
                        );
                    }
                }
                if let Some(command) = &report.projection.continuation_command {
                    println!(
                        "  continuation argv: {} {}",
                        command.program,
                        serde_json::to_string(&command.arguments)?
                    );
                }
            }
            if !report.summary.verification_complete {
                bail!(
                    "evidence verification is incomplete across the full task scope; use --outcome failed or --outcome unsupported for bounded details, follow failed bindings' corrective argv, and do not count unsupported schemes as verified"
                );
            }
        }
        Cmd::Export {
            plan,
            output,
            replace,
        } => {
            let dump = pt::export(&conn, plan.as_deref())?;
            if let Some(output) = output {
                if output.exists()
                    && std::fs::canonicalize(&output)? == std::fs::canonicalize(&db_path)?
                {
                    bail!(
                        "export --output resolves to the live authority {}; choose a separate recovery file",
                        output.display()
                    );
                }
                let receipt = pt::write_export_file(&output, &dump, replace)?;
                println!("{}", serde_json::to_string_pretty(&receipt)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&dump)?);
            }
        }
        Cmd::Import { file } => {
            let text = std::fs::read_to_string(&file).with_context(|| format!("read {file}"))?;
            let dump = pt::parse_dump_json(&text)?;
            let (tasks, deps) = pt::import(&conn, &actor, &dump)?;
            mutation_output!("imported {tasks} task(s), {deps} dependency edge(s)");
        }
        Cmd::Mise { cmd } => match cmd {
            MiseCmd::Project { task, projection } => {
                let task_seq = pt::parse_task_ref(&task)?;
                let bytes = if projection == "-" {
                    let mut bytes = Vec::new();
                    std::io::stdin()
                        .read_to_end(&mut bytes)
                        .context("read Mise planner projection from stdin")?;
                    bytes
                } else {
                    std::fs::read(&projection)
                        .with_context(|| format!("read Mise planner projection {projection}"))?
                };
                let (outcome, record) =
                    pt::record_mise_projection(&conn, &actor, task_seq, &bytes)?;
                mutation_output!(
                    "{} {} task #{} [{}] campaign {} candidate {}{}",
                    outcome.as_str(),
                    record.projection_sha256,
                    record.task_seq,
                    record.projection.disposition.as_str(),
                    record.projection.campaign_id,
                    record.projection.candidate_id,
                    record
                        .projection
                        .nomination_id
                        .as_ref()
                        .map(|id| format!(" nomination {id}"))
                        .unwrap_or_default()
                );
            }
            MiseCmd::List { task } => {
                let task_seq = pt::parse_task_ref(&task)?;
                let records = pt::task_mise_projection_summaries(&conn, task_seq)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&records)?);
                } else if records.is_empty() {
                    println!("task #{task_seq} has no Mise projections");
                } else {
                    for record in records {
                        println!(
                            "{} [{}] campaign {} candidate {}{}",
                            record.projection_sha256,
                            record.projection.disposition.as_str(),
                            record.projection.campaign_id,
                            record.projection.candidate_id,
                            record
                                .projection
                                .nomination_id
                                .as_ref()
                                .map(|id| format!(" nomination {id}"))
                                .unwrap_or_default()
                        );
                    }
                }
            }
            MiseCmd::Show { projection_sha256 } => {
                let record =
                    pt::mise_projection(&conn, &projection_sha256)?.with_context(|| {
                        format!("unknown Mise planner projection '{projection_sha256}'")
                    })?;
                println!("{}", serde_json::to_string_pretty(&record)?);
            }
        },
    }
    if json && let Some(recorder) = recorder {
        println!("{}", serde_json::to_string_pretty(&recorder.receipt()?)?);
    }
    Ok(())
}

fn short_event_time(at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|timestamp| timestamp.format("%Y-%m-%dT%H:%M").to_string())
        .unwrap_or_else(|_| at.chars().take(16).collect())
}

fn print_tree(conn: &Connection, plan_id: i64, parent: Option<i64>, depth: usize) -> Result<()> {
    let sql = match parent {
        Some(_) => {
            "SELECT task_id, seq, title, status FROM tasks WHERE plan_id=?1 AND parent_id=?2 ORDER BY seq"
        }
        None => {
            "SELECT task_id, seq, title, status FROM tasks WHERE plan_id=?1 AND parent_id IS NULL ORDER BY seq"
        }
    };
    let mut st = conn.prepare(sql)?;
    let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(i64, i64, String, String)> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    };
    let rows: Vec<(i64, i64, String, String)> = match parent {
        Some(p) => st
            .query_map(params![plan_id, p], map)?
            .collect::<rusqlite::Result<_>>()?,
        None => st
            .query_map(params![plan_id], map)?
            .collect::<rusqlite::Result<_>>()?,
    };
    for (task_id, seq, title, status) in rows {
        let done: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE parent_id=?1 AND status='done'",
            params![task_id],
            |r| r.get(0),
        )?;
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE parent_id=?1",
            params![task_id],
            |r| r.get(0),
        )?;
        let progress = if total > 0 {
            format!(" ({done}/{total})")
        } else {
            String::new()
        };
        println!(
            "{}{} #{seq} {title}{progress}",
            "  ".repeat(depth),
            status_glyph(&status)
        );
        print_tree(conn, plan_id, Some(task_id), depth + 1)?;
    }
    Ok(())
}

#[cfg(test)]
mod command_access_tests {
    use super::*;

    #[test]
    fn read_surfaces_use_read_only_authority_admission() {
        for command in [
            Cmd::Status {},
            Cmd::Audit,
            Cmd::Backup {
                output: "recovery.sqlite".into(),
            },
            Cmd::Export {
                plan: None,
                output: None,
                replace: false,
            },
            Cmd::Plan {
                cmd: PlanCmd::List { plan: None },
            },
            Cmd::Gate {
                cmd: GateCmd::List { task: "1".into() },
            },
            Cmd::Blocker {
                cmd: BlockerCmd::List { task: "1".into() },
            },
            Cmd::Commit {
                cmd: CommitCmd::Find {
                    commit_oid: "a".repeat(40),
                    repo: None,
                },
            },
            Cmd::Mise {
                cmd: MiseCmd::Show {
                    projection_sha256: "a".repeat(64),
                },
            },
        ] {
            assert!(command.opens_authority_read_only());
        }
    }

    #[test]
    fn mutation_surfaces_default_to_read_write_authority_admission() {
        assert!(!Cmd::Init.opens_authority_read_only());
        assert!(
            !Cmd::Commit {
                cmd: CommitCmd::Add {
                    task: "1".into(),
                    commit_oid: "a".repeat(40),
                    repo: ".".into(),
                    note: None,
                },
            }
            .opens_authority_read_only()
        );
        assert!(
            !Cmd::Mise {
                cmd: MiseCmd::Project {
                    task: "1".into(),
                    projection: "projection.json".into(),
                },
            }
            .opens_authority_read_only()
        );
    }
}
