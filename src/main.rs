use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use papertiger as pt;
use rusqlite::{Connection, params};
use std::io::Read;

mod guide_contract;
mod project_authority;
mod text_input;

use text_input::{IntentArgs, NoteTextArgs, ResultArgs, WhyArgs, reject_multiple_stdin};

#[derive(Parser)]
#[command(
    name = "papertiger",
    version,
    about = "Local task planning for cross-session engineering work"
)]
struct Cli {
    /// Planning database path (default: PAPERTIGER_DB, else the nearest state/papertiger.sqlite above the working directory)
    #[arg(long, global = true)]
    db: Option<String>,
    /// Project root whose state/papertiger.sqlite is the authority; evidence verify and gate resolve also resolve file: locators beneath it
    #[arg(long = "project-root", global = true, value_name = "DIR")]
    authority_project_root: Option<std::path::PathBuf>,
    /// Actor label recorded on events (default: PAPERTIGER_ACTOR, else the harness and session, else 'operator' for the human model, else the model); provenance, never ownership
    #[arg(long, global = true)]
    actor: Option<String>,
    /// Advisory pickup identity (default: PAPERTIGER_SESSION, else the harness session); never an exclusive lock or liveness signal
    #[arg(long, global = true)]
    session: Option<String>,
    /// Emit JSON: versioned reads or committed mutation receipts
    #[arg(long, global = true)]
    json: bool,
    /// Model that authored this write (default: PAPERTIGER_MODEL); every write requires one, and commands that record no event ignore it
    #[arg(long, global = true)]
    model: Option<String>,
    /// Configured reasoning effort of the authoring model (default: PAPERTIGER_REASONING_EFFORT or the harness)
    #[arg(long, global = true)]
    reasoning_effort: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the bundled JSON Schema; its top-level oneOf lists every covered document. Never opens authority
    Schema,
    /// Create or upgrade a Papertiger database and report it in plain text; refuses nonempty foreign databases
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
        /// Owning plan slug; defaults to the parent's plan, else the single active plan
        #[arg(long)]
        plan: Option<String>,
        #[command(flatten)]
        intent: IntentArgs,
        /// Kind of outcome (any letter case)
        #[arg(long, value_name = "goal|feature|change|bug|probe|decision|release", value_parser = lowercase)]
        kind: String,
        /// Parent task: N, #N or its handle (e.g. G-N)
        #[arg(long)]
        parent: Option<String>,
        /// Dependencies, comma-separated task refs
        #[arg(long, value_delimiter = ',')]
        dep: Vec<String>,
        /// Searchable task tags, comma-separated or repeated
        #[arg(long, value_delimiter = ',')]
        tag: Vec<String>,
        /// Scheduling level within readiness order: now, next (default) or later; sequence work with --dep
        #[arg(long, default_value = "next")]
        priority: pt::Priority,
        #[command(flatten)]
        why: WhyArgs,
        /// Atomically create and transition the task to in_progress
        #[arg(long)]
        start: bool,
    },
    /// Create a parent's child tasks and their dependencies from a JSON outline in one atomic call
    #[command(
        after_help = "Outline: papertiger.task_outline.v2 in `papertiger schema`. Each child has a batch-local \"key\", a \"title\" and a \"kind\", and optionally \"intent\", \"why\", \"tags\", \"priority\" (now, next or later) and \"deps\" (sibling keys as strings, existing task numbers as integers). Keys are never stored. The JSON receipt lists one task create event per child, in outline order."
    )]
    Decompose {
        /// Parent task: N, #N or its handle (e.g. G-N)
        parent: String,
        /// Read the papertiger.task_outline.v2 outline as UTF-8 from PATH, or stdin with '-'
        #[arg(long, value_name = "PATH|-")]
        outline_file: String,
        /// Also start every child whose dependencies are all existing done tasks; each needs a "why"
        #[arg(long)]
        start_ready: bool,
    },
    /// Show one task in full
    Show {
        /// Task: N, #N or its handle (e.g. F-N)
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
        /// Continue from the previous page's next_cursor (same filters; refused after any authority change)
        #[arg(long, requires = "all_plans")]
        after_cursor: Option<String>,
        /// Plan slug; optional when exactly one plan is active
        #[arg(long)]
        plan: Option<String>,
        /// Filter by task status
        #[arg(long)]
        status: Option<String>,
        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,
        /// Filter by kind (any letter case)
        #[arg(long, value_name = "goal|feature|change|bug|probe|decision|release|work", value_parser = lowercase)]
        kind: Option<String>,
        /// Only tasks whose filing or result still needs someone else's review
        #[arg(long)]
        unreviewed: bool,
        /// Ordering: seq or activity
        #[arg(long, default_value = "seq")]
        sort: String,
    },
    /// Search task titles, intents, results, tags and rationale with deterministic ranking
    Search {
        /// Literal words to find across title, intent, result, tags, and rationale
        query: String,
        /// Restrict results to one plan slug; all plans are searched by default
        #[arg(long)]
        plan: Option<String>,
        /// Restrict results to one task status
        #[arg(long)]
        status: Option<String>,
        /// Filter by kind (any letter case)
        #[arg(long, value_name = "goal|feature|change|bug|probe|decision|release|work", value_parser = lowercase)]
        kind: Option<String>,
        /// Maximum ranked results (default 20; 5 with --compact)
        #[arg(long)]
        limit: Option<usize>,
        /// Identity, ranking and excerpt without task bodies, with a continuation command when truncated (implies --json)
        #[arg(long)]
        compact: bool,
    },
    /// Edit a task
    Edit {
        /// Task: N, #N or its handle (e.g. F-N)
        task: String,
        /// Replacement title
        #[arg(long)]
        title: Option<String>,
        #[command(flatten)]
        intent: IntentArgs,
        /// Replace the parent with this task
        #[arg(long, conflicts_with = "clear_parent")]
        parent: Option<String>,
        /// Remove the current parent
        #[arg(long)]
        clear_parent: bool,
        /// Replacement kind (any letter case)
        #[arg(long, value_name = "goal|feature|change|bug|probe|decision|release", value_parser = lowercase)]
        kind: Option<String>,
        /// Replacement scheduling level: now, next or later
        #[arg(long)]
        priority: Option<pt::Priority>,
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
        /// Task: N, #N or its handle (e.g. F-N)
        task: String,
        /// The rationale as a second word, like --why
        #[arg(value_name = "WHY")]
        text: Option<String>,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Complete a task after dependencies, gates, and children are closed
    Done {
        /// Task: N, #N or its handle (e.g. F-N)
        task: String,
        /// The result as a second word, like --result
        #[arg(value_name = "RESULT")]
        text: Option<String>,
        #[command(flatten)]
        result: ResultArgs,
    },
    /// Reopen a finished task
    Reopen {
        /// Task: N, #N or its handle (e.g. F-N)
        task: String,
        /// The rationale as a second word, like --why
        #[arg(value_name = "WHY")]
        text: Option<String>,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Retire a task that is no longer worth doing, optionally into its replacement
    Retire {
        /// Task: N, #N or its handle (e.g. F-N)
        task: String,
        /// The rationale as a second word, like --why
        #[arg(value_name = "WHY")]
        text: Option<String>,
        /// Same-plan task that replaces this work
        #[arg(long, value_name = "TASK")]
        into: Option<String>,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Resume a task and its whole subtree: intent, progress, next work, open work, obligations, outcomes and notes
    Brief {
        /// Task: N, #N or its handle (e.g. G-N)
        task: String,
    },
    /// Claim outcomes per authoring model: filings, results, reviews and reopens
    Models {
        /// Restrict to one plan slug; every plan by default
        #[arg(long)]
        plan: Option<String>,
        /// Only claims made at or after this RFC3339 instant
        #[arg(long)]
        since: Option<String>,
    },
    /// Record whether a task's filing (or, with --result, its result) holds up, from another session
    Review {
        /// Task: N, #N or its handle (e.g. F-N)
        task: String,
        /// confirmed or disputed
        #[arg(long)]
        verdict: String,
        /// Review the done task's result instead of its filing
        #[arg(long)]
        result: bool,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Reject a task or approach with its rationale; `reopen` can revisit it
    Reject {
        /// Task: N, #N or its handle (e.g. F-N)
        task: String,
        /// The rationale as a second word, like --why
        #[arg(value_name = "WHY")]
        text: Option<String>,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Gate management
    Gate {
        #[command(subcommand)]
        cmd: GateCmd,
    },
    /// Manage task reference locators without fetching external state
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
    /// Actionable leaf work and parents ready to close, ranked by readiness and dependency unlock impact
    Focus {
        /// Plan slug; without it, text focus ranks each active plan in turn and JSON needs exactly one active plan
        #[arg(long)]
        plan: Option<String>,
        /// Maximum tasks per plan (default 20, or 5 each when ranking several active plans)
        #[arg(long)]
        limit: Option<usize>,
        /// Include proposed work that is currently blocked
        #[arg(long)]
        include_blocked: bool,
        /// Filter by kind (any letter case)
        #[arg(long, value_name = "goal|feature|change|bug|probe|decision|release|work", value_parser = lowercase)]
        kind: Option<String>,
        /// Only tasks whose filing still needs someone else's review
        #[arg(long)]
        unreviewed: bool,
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
        /// Show tasks of this kind with the ancestors that place them (any letter case)
        #[arg(long, value_name = "goal|feature|change|bug|probe|decision|release|work", value_parser = lowercase)]
        kind: Option<String>,
    },
    /// Record a free-standing evented note (course changes, decisions)
    Note {
        /// The note text, or a task and then its text: `note "text"`, `note F-3 "text"`
        #[arg(value_name = "TASK] [TEXT", num_args = 0..=2)]
        words: Vec<String>,
        #[command(flatten)]
        text: NoteTextArgs,
        /// Attach the note to this task number
        #[arg(long)]
        task: Option<String>,
    },
    /// Event history
    Log {
        /// Restrict history to this task number
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
    /// Export plans, tasks, gates, events and Mise projections as papertiger.dump.v11 JSON
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
    /// Import a papertiger.dump.v11 JSON file (papertiger.dump.v10 is converted)
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
    Inspect {
        /// Stored event id, as shown by `log` or `audit`
        event_id: i64,
    },
    /// Preserve an invalid row verbatim in an audited, exportable quarantine event
    Quarantine {
        /// Stored event id to quarantine
        event_id: i64,
        /// Digest returned by history inspect; refuses a changed record
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
        /// Task: N, #N or its handle (e.g. F-N)
        task: String,
        /// Locator of the external item, such as a URL or repository path
        locator: String,
        /// Reference kind
        #[arg(long, value_name = "pull_request|issue|review|adr|input|other", value_parser = lowercase)]
        kind: String,
        /// Optional lowercase SHA-256 digest binding the referenced bytes
        #[arg(long)]
        sha256: Option<String>,
        /// Optional context for the reference
        #[arg(long)]
        note: Option<String>,
    },
    /// Remove one reference with retained rationale
    Remove {
        /// Task: N, #N or its handle (e.g. F-N)
        task: String,
        /// Locator recorded on the task
        locator: String,
        /// Kind recorded with the locator
        #[arg(long, value_name = "pull_request|issue|review|adr|input|other", value_parser = lowercase)]
        kind: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// List inward references on one task
    List {
        /// Task: N, #N or its handle (e.g. F-N)
        task: String,
    },
    /// Find tasks that name a locator
    Find {
        /// Locator to match exactly
        locator: String,
    },
}

#[derive(Subcommand)]
enum EvidenceCmd {
    /// Summarize every stored binding and page filtered verification details
    Verify {
        /// Restrict verification to one task number
        #[arg(long)]
        task: Option<String>,
        /// Detail classification to show; summary counts always cover the full task scope
        #[arg(long, value_enum, default_value = "incomplete")]
        classification: pt::EvidenceClassificationFilter,
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
        /// Task: N, #N or its handle (e.g. F-N)
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
        /// Task: N, #N or its handle (e.g. F-N)
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
        /// Task: N, #N or its handle (e.g. F-N)
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
    Record {
        /// Task (N, #N or handle) that owns the projection
        task: String,
        /// Projection JSON path, or '-' to read a Mise inspector pipeline from stdin
        projection: String,
    },
    /// List every reverified Mise projection attached to one task
    List {
        /// Task (N, #N or handle) that owns the projections
        task: String,
    },
    /// Show one reverified projection by its SHA-256 identity
    Show {
        /// Full lowercase SHA-256 projection identity
        projection_sha256: String,
    },
}

#[derive(Subcommand)]
enum PlanCmd {
    /// Create a plan
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
        /// Task (N, #N or handle) that owns the gate
        task: String,
        /// Stable gate name within the task
        name: String,
        /// Evidence kind; external is a condition outside the task that blocks starting it (any letter case)
        #[arg(long, value_name = "test|benchmark|review|capture|fixture|build|doc|external|other", value_parser = lowercase)]
        kind: String,
        /// Proof required to resolve the gate
        #[arg(long)]
        requirement: String,
    },
    /// Resolve an open gate with an evidence locator
    Resolve {
        /// Task (N, #N or handle) that owns the gate
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
    /// Waive an open gate with a rationale
    Waive {
        /// Task (N, #N or handle) that owns the gate
        task: String,
        /// Name of the open gate
        name: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Reopen a resolved or waived gate
    Reopen {
        /// Task (N, #N or handle) that owns the gate
        task: String,
        /// Name of the terminal gate
        name: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Remove an open gate that no longer models required proof
    Remove {
        /// Task (N, #N or handle) that owns the gate
        task: String,
        /// Name of the open gate
        name: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// List every gate on one task
    List {
        /// Task (N, #N or handle) that owns the gates
        task: String,
    },
}

#[derive(Subcommand)]
enum TagCmd {
    /// Add a searchable tag to a task
    Add {
        /// Task to tag: N, #N or its handle
        task: String,
        /// Tag value
        tag: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Remove a tag from a task
    Remove {
        /// Task to untag: N, #N or its handle
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
        /// Dependent task: N, #N or its handle
        task: String,
        /// Prerequisite task: N, #N or its handle
        on: String,
        #[command(flatten)]
        why: WhyArgs,
    },
    /// Remove a dependency edge
    Remove {
        /// Dependent task: N, #N or its handle
        task: String,
        /// Prerequisite task: N, #N or its handle
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
                | Self::Models { .. }
                | Self::Brief { .. }
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

/// Names of a task's open external gates: conditions outside the task that
/// keep it from starting.
fn open_external_gates(conn: &Connection, task_id: i64) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT name FROM gates WHERE task_id=?1 AND kind='external' AND status='open' ORDER BY gate_id",
    )?;
    Ok(statement
        .query_map(params![task_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `triage` marks unreviewed claims too, for `list --unreviewed`.
fn print_task_line(conn: &Connection, t: &pt::Task, triage: bool) -> Result<()> {
    let deps = pt::open_deps(conn, t.task_id)?;
    let external = open_external_gates(conn, t.task_id)?;
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
                .map(|d| pt::seq_handle(conn, *d))
                .collect::<Result<Vec<_>>>()?
                .join(" ")
        ));
    }
    if !external.is_empty() {
        extra.push_str(&format!(" [external: {}]", external.join(", ")));
    }
    if open_gate_count > 0 {
        extra.push_str(&format!(" [gates open: {open_gate_count}]"));
    }
    let review = pt::task_review(conn, t)?;
    match t.status.as_str() {
        "proposed" | "in_progress" => extra.push_str(&review_marker("", &review.filing, triage)),
        "done" => {
            if let Some(result) = &review.result {
                extra.push_str(&review_marker(
                    "result ",
                    result,
                    triage && !result.is_legacy(),
                ));
            }
        }
        _ => {}
    }
    pt::outln!(
        "{} {} {}{}",
        status_glyph(&t.status),
        t.handle,
        t.title,
        extra
    );
    Ok(())
}

fn print_brief(brief: &pt::Brief) -> Result<()> {
    let task = &brief.task;
    pt::outln!(
        "{} {} {}{}",
        status_glyph(&task.status),
        task.handle,
        task.title,
        review_marker("", &brief.review, false)
    );
    if !brief.ancestry.is_empty() {
        pt::outln!("  path: {}", brief.ancestry.join(" › "));
    }
    if !brief.intent.is_empty() {
        // Intents are often a few paragraph-long lines, so bound both.
        const INTENT_LINES: usize = 6;
        const INTENT_CHARS: usize = 600;
        let lines = brief.intent.lines().collect::<Vec<_>>();
        let mut excerpt = lines[..lines.len().min(INTENT_LINES)].join("\n  ");
        let mut truncated = lines.len() > INTENT_LINES;
        if let Some((cut, _)) = excerpt.char_indices().nth(INTENT_CHARS) {
            excerpt.truncate(cut);
            excerpt.push('…');
            truncated = true;
        }
        pt::outln!("  intent: {excerpt}");
        if truncated {
            pt::outln!(
                "  ... intent continues; run `papertiger show {}`",
                task.handle
            );
        }
    }
    let progress = &brief.progress;
    pt::outln!(
        "  subtree: {} task(s): {} done, {} retired, {} rejected, {} open",
        progress.tasks,
        progress.done,
        progress.retired,
        progress.rejected,
        progress.open
    );
    let omitted = |count: usize| {
        if count > 0 {
            pt::outln!("  ... {count} more");
        }
        Ok::<(), anyhow::Error>(())
    };
    if !brief.next.entries.is_empty() {
        pt::outln!("next:");
        for task in &brief.next.entries {
            pt::outln!("  {} {} [{}]", task.handle, task.title, task.priority);
        }
        omitted(brief.next.omitted)?;
    }
    if !brief.open.entries.is_empty() {
        pt::outln!("open:");
        for open in &brief.open.entries {
            let review = review_marker("", &open.review, false);
            pt::outln!(
                "  {} {} {}{}{}",
                status_glyph(&open.task.status),
                open.task.handle,
                open.task.title,
                review,
                pickup_label(open.pickup.as_ref())
            );
        }
        omitted(brief.open.omitted)?;
    }
    if !brief.obligations.entries.is_empty() {
        pt::outln!("obligations:");
        for obligation in &brief.obligations.entries {
            pt::outln!(
                "  {} {} ({}): {}",
                obligation.task,
                obligation.gate,
                obligation.kind,
                obligation.requirement
            );
        }
        omitted(brief.obligations.omitted)?;
    }
    if !brief.outcomes.entries.is_empty() {
        pt::outln!("outcomes:");
        for outcome in &brief.outcomes.entries {
            let review = outcome
                .review
                .as_ref()
                .map(|review| review_marker("result ", review, false))
                .unwrap_or_default();
            pt::outln!(
                "  {} {}{}: {}",
                outcome.task.handle,
                outcome.task.title,
                review,
                preview(&outcome.result)
            );
        }
        omitted(brief.outcomes.omitted)?;
    }
    if !brief.notes.entries.is_empty() {
        pt::outln!("notes:");
        for note in &brief.notes.entries {
            pt::outln!(
                "  @{} {} {}: {}",
                note.event_id,
                &note.at[..note.at.len().min(16)],
                note.actor,
                preview(note.why.as_deref().unwrap_or_default())
            );
        }
        omitted(brief.notes.omitted)?;
    }
    Ok(())
}

/// One line of at most 160 characters, ending in `…` when the
/// text was cut; `show` and `log` carry the full text.
fn preview(text: &str) -> String {
    clip(text, 160)
}

/// `text` on one line of at most `limit` characters, ending in `…` when cut.
fn clip(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut cut = flat.chars().take(limit - 1).collect::<String>();
    cut.push('…');
    cut
}

/// ` [disputed · deepseek-v4.1-flash]` for a disputed claim, else nothing.
/// Review is optional, so an ordinary view leaves an unreviewed claim
/// unmarked; a `--unreviewed` triage view (`triage`) marks it too.
fn review_marker(prefix: &str, claim: &pt::ClaimReview, triage: bool) -> String {
    let shown = match claim.state {
        pt::ReviewState::Disputed => true,
        pt::ReviewState::Unreviewed => triage,
        _ => false,
    };
    if !shown {
        return String::new();
    }
    format!(
        " [{prefix}{} · {}]",
        claim.state.as_str(),
        claim.author_model.as_deref().unwrap_or("model unknown")
    )
}

/// Text given as a command's second word or by its flag, but not both.
fn second_word(word: Option<String>, flag: Option<String>, field: &str) -> Result<Option<String>> {
    match (word, flag) {
        (Some(_), Some(_)) => bail!(
            "give the {field} once, as the second word or with --{field}/--{field}-file, not both"
        ),
        (word, flag) => Ok(word.filter(|text| !text.trim().is_empty()).or(flag)),
    }
}

/// A kind or similar keyword in any letter case; the lists are lowercase.
fn lowercase(value: &str) -> Result<String, std::convert::Infallible> {
    Ok(value.to_ascii_lowercase())
}

/// One readable line per event: who, which task, what changed and why. The
/// authorship fields every event carries are summarized as the model; JSON
/// keeps the full payload.
fn log_line(conn: &Connection, event: &pt::EventRecord) -> String {
    // Fields every event carries, or that only restate another field.
    const AUTHORSHIP: &[&str] = &[
        "harness",
        "model",
        "model_source",
        "reasoning_effort",
        "session",
        "seq",
        "fields",
        "revision_schema",
        "pickup",
        "previous_pickup",
    ];
    let target = match (event.task_seq, &event.gate) {
        (Some(seq), gate) => {
            let handle = pt::seq_handle(conn, seq).unwrap_or_else(|_| format!("#{seq}"));
            match gate {
                Some(gate) => format!(" {handle} gate '{gate}'"),
                None => format!(" {handle}"),
            }
        }
        (None, _) => event
            .plan
            .as_ref()
            .map(|plan| format!(" plan {plan}"))
            .unwrap_or_default(),
    };
    let details = event
        .payload
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .map(|payload| {
            payload
                .iter()
                .filter(|(key, value)| {
                    !AUTHORSHIP.contains(&key.as_str())
                        && !value.is_null()
                        // A result recorded as the rationale is shown once.
                        && value.as_str() != event.why.as_deref()
                })
                .map(|(key, value)| match value {
                    serde_json::Value::String(text) => format!("{key}={}", clip(text, 60)),
                    other => format!("{key}={}", clip(&other.to_string(), 60)),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|details| !details.is_empty())
        .map(|details| format!(" {details}"))
        .unwrap_or_default();
    let model = event
        .model
        .as_ref()
        .map(|model| format!(" [{model}]"))
        .unwrap_or_default();
    let why = event
        .why
        .as_ref()
        .map(|why| format!(" - {}", preview(why)))
        .unwrap_or_default();
    format!(
        "@{} {} {}{model}{target} {}/{}{details}{why}",
        event.event_id,
        short_event_time(&event.at),
        event.actor,
        event.entity,
        event.kind,
    )
}

/// The plans a text read covers: the named one, else every active plan (an
/// empty set falls through to the refusal naming `plan add`).
fn text_plans(conn: &Connection, plan: Option<&str>) -> Result<Vec<(i64, String)>> {
    Ok(match plan {
        Some(slug) => vec![pt::resolve_plan(conn, Some(slug))?],
        None => match pt::active_plans(conn)? {
            all if all.is_empty() => vec![pt::resolve_plan(conn, None)?],
            all => all,
        },
    })
}

/// One line describing a claim's review for `show`.
fn claim_review_line(claim: &pt::ClaimReview) -> String {
    let mut line = format!(
        "{} (by {})",
        claim.state.as_str(),
        claim.author_model.as_deref().unwrap_or("model unknown")
    );
    if claim.legacy {
        line.push_str("; recorded before 1.0");
    }
    if let Some(reviewer) = &claim.reviewer_model {
        line.push_str(&format!("; last checked by {reviewer}"));
        if claim.same_model == Some(true) {
            line.push_str(", the same model");
        }
    }
    line
}

fn pickup_label(pickup: Option<&pt::TaskPickup>) -> String {
    match pickup {
        Some(p) => format!(
            " [last pickup {} at {}{}]",
            p.session.as_deref().unwrap_or("unattributed"),
            p.at,
            match pt::owner_state(p) {
                pt::OwnerState::Ended => "; owner ended",
                pt::OwnerState::Running => "; owner running",
                pt::OwnerState::Unknown => "",
            }
        ),
        None => String::new(),
    }
}

fn print_task_context(context: &pt::TaskContext) -> Result<()> {
    let task = &context.task;
    if let Some(pickup) = &task.pickup {
        // Name the owner state in every case so the line never contradicts it.
        let owner = match pt::owner_state(pickup) {
            pt::OwnerState::Ended => "owner ended",
            pt::OwnerState::Running => "owner running",
            pt::OwnerState::Unknown => "owner state unknown",
        };
        pt::outln!(
            "pickup: last by {} at {}; {owner}; advisory, never a lock",
            pickup.session.as_deref().unwrap_or("unattributed"),
            pickup.at
        );
    }
    let open_dependencies = context
        .dependencies
        .iter()
        .filter(|dependency| dependency.status != "done")
        .collect::<Vec<_>>();
    let open_external = context
        .gates
        .iter()
        .filter(|gate| gate.kind == "external" && gate.status == "open")
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
                .map(|dependency| dependency.handle.clone())
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if !open_external.is_empty() {
        extra.push_str(&format!(
            " [external: {}]",
            open_external
                .iter()
                .map(|gate| gate.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if open_gate_count > 0 {
        extra.push_str(&format!(" [gates open: {open_gate_count}]"));
    }
    pt::outln!(
        "{} {} {}{}",
        status_glyph(&task.status),
        task.handle,
        task.title,
        extra
    );
    if !context.ancestry.is_empty() {
        pt::outln!("  path: {}", context.ancestry.join(" › "));
    }
    if !task.intent.is_empty() {
        pt::outln!("  intent: {}", task.intent.replace('\n', "\n  "));
    }
    if let Some(result) = task.result.as_deref() {
        pt::outln!("  result: {}", result.replace('\n', "\n  "));
    }
    pt::outln!("  kind: {}", task.kind);
    pt::outln!("  filing: {}", claim_review_line(&context.review.filing));
    if let Some(result) = &context.review.result {
        pt::outln!("  result review: {}", claim_review_line(result));
    }
    for (label, event) in [
        ("created", context.activity.created_event.as_ref()),
        ("last event", context.activity.last_event.as_ref()),
        ("started event", context.activity.started_event.as_ref()),
        ("completed event", context.activity.completed_event.as_ref()),
    ] {
        if let Some(event) = event {
            pt::outln!(
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
        pt::outln!(
            "  parent {} {} {}",
            status_glyph(&parent.status),
            parent.handle,
            parent.title
        );
    }
    if let Some(replacement) = &context.replacement {
        pt::outln!(
            "  replacement {} {} {}",
            status_glyph(&replacement.status),
            replacement.handle,
            replacement.title
        );
    }
    if task.priority != pt::Priority::Next {
        pt::outln!("  priority: {}", task.priority);
    }
    if !context.tags.is_empty() {
        pt::outln!("  tags: {}", context.tags.join(", "));
    }
    for dependency in &context.dependencies {
        pt::outln!(
            "  depends {} {} {}",
            status_glyph(&dependency.status),
            dependency.handle,
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
        pt::outln!(
            "  gate [{}] {} ({}): {}{}{}{}",
            gate.status,
            gate.name,
            gate.kind,
            gate.requirement,
            evidence,
            digest,
            note
        );
    }
    for commit in &context.commit_associations {
        let note = commit
            .note
            .as_ref()
            .map(|note| format!(" ({note})"))
            .unwrap_or_default();
        pt::outln!(
            "  commit {} {}{}",
            commit.repository,
            commit.commit_oid,
            note
        );
    }
    for reference in &context.external_references {
        pt::outln!(
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
        pt::outln!(
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
        pt::outln!(
            "  child {} {} {}",
            status_glyph(&child.status),
            child.handle,
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
        pt::outln!(
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
        pt::outln!(
            "  (older events omitted: papertiger log --task {})",
            task.seq
        );
    }
    Ok(())
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
    let cli = Cli::parse();
    match run_planner(cli) {
        Err(error) if pt::is_output_closed(&error) => Ok(()),
        result => result.map_err(pt::normalize_sqlite_lock_error),
    }
}

fn run_planner(cli: Cli) -> Result<()> {
    let json = cli.json || cli.cmd.implies_json();
    if matches!(cli.cmd, Cmd::Schema) {
        pt::outln!(
            "{}",
            include_str!("../docs/schemas/planner.json").trim_end()
        );
        return Ok(());
    }
    let mutation = !cli.cmd.opens_authority_read_only()
        && !matches!(
            cli.cmd,
            Cmd::Init | Cmd::Backup { .. } | Cmd::RepairGuards { dry_run: true, .. }
        );
    // Agents pass their model on every command; one that records no event
    // has nothing to attribute and simply ignores it.
    let flags = pt::AuthorshipFlags {
        actor: cli.actor.as_deref(),
        session: cli.session.as_deref(),
        model: cli.model.as_deref(),
        reasoning_effort: cli.reasoning_effort.as_deref(),
    };
    // Every write records its author; reads only need the session that
    // recognizes this caller's own pickups.
    let authorship = if mutation {
        Some(pt::resolve_authorship_from_env(flags)?)
    } else {
        None
    };
    let explicit_session = cli.session.is_some();
    let session = match &authorship {
        Some(authorship) => authorship.session.clone(),
        None => pt::authorship::resolve_session_from_env(cli.session.as_deref())?,
    };
    if json && matches!(cli.cmd, Cmd::Init) {
        bail!(
            "init reports in plain text only: one line naming the authority and whether it was initialized, migrated, or already current; omit --json and read that line, or run `status --json` afterwards"
        );
    }
    if json
        && matches!(
            cli.cmd,
            Cmd::Tree { .. }
                | Cmd::Gate {
                    cmd: GateCmd::List { .. }
                }
        )
    {
        bail!(
            "this command has no JSON projection; omit --json or use status/show --json for planner context"
        );
    }
    macro_rules! mutation_output {
        ($($args:tt)*) => { if !json { pt::outln!($($args)*); } };
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
    let evidence_resolve = matches!(
        &cli.cmd,
        Cmd::Gate {
            cmd: GateCmd::Resolve { .. }
        }
    );
    if project_root.is_some() && db_override.is_some() && !evidence_verify && !evidence_resolve {
        bail!(
            "ordinary planner commands do not accept --project-root together with --db or PAPERTIGER_DB; remove the database override so --project-root selects one canonical authority. `evidence verify` and `gate resolve` alone retain this combination so an explicitly selected database can verify or bind file: locators beneath a supplied project root"
        );
    }
    // The project root of an authority `init` may create, for its ignore rules.
    let mut init_root = None;
    let db_path = match (db_override, project_root.as_deref()) {
        (Some(path), _) => path,
        // Selected paths are shown in refusals; render them as a user would
        // type them, without Windows verbatim prefixes.
        (None, Some(root)) => {
            let selected = project_authority::at_root(root)?;
            init_root = Some(selected.root);
            pt::portable_absolute(&selected.authority)?
        }
        (None, None) => {
            let cwd = std::env::current_dir()?;
            match project_authority::discover(&cwd)? {
                Some(found) => pt::portable_absolute(&found.authority)?,
                // Creating an authority names its root: run from a
                // subdirectory, an implicit one would nest a second authority.
                None => return Err(project_authority::not_found(&cwd)),
            }
        }
    };
    let actor = authorship
        .as_ref()
        .map(|authorship| authorship.actor.clone())
        .unwrap_or_default();

    if let Cmd::Init = cli.cmd {
        if let Some(dir) = std::path::Path::new(&db_path).parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let conn = pt::open_for_init(&db_path)?;
        match pt::init_at(&conn, &db_path)? {
            pt::InitOutcome::Created => {
                pt::outln!("initialized {db_path} (schema v{})", pt::SCHEMA_VERSION);
                if let Some(root) = &init_root
                    && project_authority::write_ignore_block(root)?
                {
                    pt::outln!("added Papertiger's ignore rules to .gitignore");
                }
            }
            pt::InitOutcome::Migrated { from, to } => {
                pt::outln!("migrated {db_path} from schema v{from} to v{to}")
            }
            pt::InitOutcome::Current => pt::outln!(
                "{db_path} is already a Papertiger authority at schema v{}; nothing changed",
                pt::SCHEMA_VERSION
            ),
        }
        return Ok(());
    }

    if let Cmd::Backup { output } = &cli.cmd {
        let receipt = pt::backup_authority(std::path::Path::new(&db_path), output)?;
        if json {
            pt::outln!("{}", serde_json::to_string_pretty(&receipt)?);
        } else {
            pt::outln!(
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
            if let Some(authorship) = &authorship {
                pt::set_authorship(&conn, authorship)?;
            }
            pt::repair_write_guards(&conn, &actor, why)?
        };
        if json {
            pt::outln!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema": "papertiger.guard_repair.v1",
                    "dry_run": dry_run,
                    "changed": !dry_run && !drifted.is_empty(),
                    "guards": drifted,
                }))?
            );
        } else if drifted.is_empty() {
            pt::outln!("write guards are intact; nothing changed");
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
                pt::outln!("{action} {} trigger {}", guard.state.as_str(), guard.name);
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

    if let Some(authorship) = &authorship {
        pt::set_authorship(&conn, authorship)?;
    }
    let recorder = if mutation && json {
        Some(pt::MutationRecorder::new(&conn)?)
    } else {
        None
    };
    match cli.cmd {
        Cmd::Init => unreachable!(),
        Cmd::Schema => unreachable!(),
        Cmd::Backup { .. } => unreachable!(),
        Cmd::RepairGuards { .. } => unreachable!(),
        Cmd::Status {} => {
            let status = pt::status_response(&conn, &db_path)?;
            if json {
                pt::outln!("{}", serde_json::to_string_pretty(&status)?);
                return Ok(());
            }
            pt::outln!(
                "papertiger {} | schema v{} | {}",
                status.authority.papertiger_version,
                status.authority.schema_version,
                status.authority.resolved_path
            );
            if status.active_plans.is_empty() {
                pt::outln!(
                    "no active plans; inspect `papertiger plan list` or create one with `papertiger plan add <slug> <title>`"
                );
            }
            for active in &status.active_plans {
                let counts = &active.counts;
                pt::outln!(
                    "plan {}: {} [proposed {}, in_progress {}, done {}, retired {}, rejected {}]",
                    active.plan.slug,
                    active.plan.title,
                    counts.proposed,
                    counts.in_progress,
                    counts.done,
                    counts.retired,
                    counts.rejected
                );
                if !active.open_by_kind.is_empty() {
                    pt::outln!(
                        "  open by kind: {}",
                        active
                            .open_by_kind
                            .iter()
                            .map(|open| format!(
                                "{} {}",
                                pt::task_kind_letter(&open.kind).unwrap_or('?'),
                                open.count
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                // One screen: live owners first, then the most recent pickups.
                const IN_PROGRESS_SHOWN: usize = 8;
                let mut in_progress = active
                    .in_progress
                    .parents
                    .entries
                    .iter()
                    .map(|entry| ("parent", entry))
                    .chain(
                        active
                            .in_progress
                            .leaves
                            .entries
                            .iter()
                            .map(|entry| ("leaf", entry)),
                    )
                    .collect::<Vec<_>>();
                in_progress.sort_by_key(|(_, entry)| {
                    let running = entry
                        .pickup
                        .as_ref()
                        .is_some_and(|pickup| pt::owner_state(pickup) == pt::OwnerState::Running);
                    let at = entry.pickup.as_ref().map(|pickup| pickup.at.clone());
                    (!running, std::cmp::Reverse(at))
                });
                let hidden = in_progress.len().saturating_sub(IN_PROGRESS_SHOWN)
                    + active.in_progress.parents.omitted_count
                    + active.in_progress.leaves.omitted_count;
                for (role, entry) in in_progress.iter().take(IN_PROGRESS_SHOWN) {
                    pt::outln!(
                        "> {role} {} {}{}",
                        entry.task.handle,
                        entry.task.title,
                        pickup_label(entry.pickup.as_ref())
                    );
                }
                if hidden > 0 {
                    pt::outln!(
                        "  ... {hidden} more in progress; run `papertiger list --plan {} --status in_progress --sort activity`",
                        active.plan.slug
                    );
                }
                for entry in &active.ready.entries {
                    pt::outln!("· {} {}", entry.task.handle, entry.task.title);
                }
                if !active.ready.complete {
                    pt::outln!(
                        "  ... {} ready task(s) omitted; run `papertiger focus --plan {}`",
                        active.ready.omitted_count,
                        active.plan.slug
                    );
                }
            }
            for note in &status.recent_notes.entries {
                pt::outln!(
                    "note @{} {} {}: {}",
                    note.event_id,
                    short_event_time(&note.at),
                    note.actor,
                    preview(note.why.as_deref().unwrap_or_default())
                );
            }
            if !status.recent_notes.complete {
                pt::outln!(
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
                    pt::outln!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema": "papertiger.plan_list.v1", "total": plans.len(), "plans": plans
                        }))?
                    );
                } else {
                    for plan in plans {
                        pt::outln!("{:8} {}: {}", plan.status, plan.slug, plan.title);
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
                let why = why.optional()?;
                let changed = pt::edit_plan(
                    &conn,
                    &actor,
                    &slug,
                    title.as_deref(),
                    intent.as_deref(),
                    why.as_deref(),
                )?;
                mutation_output!("plan {slug} updated ({})", changed.join(", "));
            }
            PlanCmd::Set { slug, status, why } => {
                let why = why.optional()?;
                pt::set_plan_status(&conn, &actor, &slug, &status, why.as_deref())?;
                mutation_output!("plan {slug} -> {status}");
            }
        },
        Cmd::Add {
            title,
            plan,
            intent,
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
            let parent = parent
                .map(|p| pt::resolve_task_ref(&conn, &p))
                .transpose()?;
            let deps: Vec<i64> = dep
                .iter()
                .map(|d| pt::resolve_task_ref(&conn, d))
                .collect::<Result<_>>()?;
            let seq = pt::add_task(
                &conn,
                &actor,
                plan.as_deref(),
                pt::TaskCreation {
                    title: &title,
                    intent: &intent,
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
            let slug = pt::get_plan(&conn, pt::get_task(&conn, seq)?.plan_id)?.slug;
            let handle = pt::task_handle(seq, &kind);
            if start {
                mutation_output!("{handle} added to {slug} and in progress");
            } else {
                mutation_output!("{handle} added to {slug}");
            }
        }
        Cmd::Decompose {
            parent,
            outline_file,
            start_ready,
        } => {
            let parent = pt::resolve_task_ref(&conn, &parent)?;
            let bytes =
                read_input_bytes(&outline_file, "task outline", Some(pt::MAX_OUTLINE_BYTES))?;
            let outline = pt::parse_task_outline(&bytes)?;
            let children = pt::decompose_task(
                &conn,
                &actor,
                parent,
                &outline,
                start_ready,
                session.as_deref(),
            )?;
            let parent = pt::seq_handle(&conn, parent)?;
            mutation_output!("{parent} decomposed into {} child task(s):", children.len());
            for child in &children {
                let started = if child.status == "in_progress" {
                    " (in progress)"
                } else {
                    ""
                };
                let handle = pt::get_task(&conn, child.seq)?.handle;
                mutation_output!("  {} -> {handle}{started}", child.key);
            }
        }
        Cmd::Show { task, no_history } => {
            let seq = pt::resolve_task_ref(&conn, &task)?;
            if no_history {
                pt::outln!(
                    "{}",
                    serde_json::to_string_pretty(&pt::task_current(&conn, seq)?)?
                );
            } else {
                let context = pt::task_context(&conn, seq)?;
                if json {
                    pt::outln!("{}", serde_json::to_string_pretty(&context)?);
                } else {
                    print_task_context(&context)?;
                }
            }
        }
        Cmd::List {
            all_plans,
            limit,
            after_cursor,
            plan,
            status,
            tag,
            kind,
            unreviewed,
            sort,
        } => {
            if all_plans {
                let response = pt::task_inventory(
                    &conn,
                    status.as_deref(),
                    tag.as_deref(),
                    kind.as_deref(),
                    unreviewed,
                    limit.unwrap_or(100),
                    after_cursor.as_deref(),
                )?;
                if json {
                    pt::outln!("{}", serde_json::to_string_pretty(&response)?);
                } else {
                    for item in &response.tasks {
                        // A triage listing labels the claims it selected.
                        let review = if unreviewed {
                            let task = pt::get_task(&conn, item.task.seq)?;
                            let review = pt::task_review(&conn, &task)?;
                            match (task.status.as_str(), &review.result) {
                                ("done", Some(result)) => review_marker("result ", result, true),
                                _ => review_marker("", &review.filing, true),
                            }
                        } else {
                            String::new()
                        };
                        pt::outln!(
                            "{} {} [{}; {}] {}{}",
                            status_glyph(&item.task.status),
                            item.task.handle,
                            item.plan.slug,
                            item.plan.status,
                            item.task.title,
                            review
                        );
                    }
                    pt::outln!("{} total; {} remaining", response.total, response.remaining);
                    if let Some(command) = &response.continuation_command {
                        pt::outln!("continue with: {}", command.replacen(" --json", "", 1));
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
                    kind.as_deref(),
                    unreviewed,
                    &sort,
                )?;
                pt::outln!("{}", serde_json::to_string_pretty(&response)?);
                return Ok(());
            }
            let targets = text_plans(&conn, plan.as_deref())?;
            let several = targets.len() > 1;
            for (plan_id, slug) in targets {
                let tasks = match sort.as_str() {
                    "seq" => pt::list_tasks(
                        &conn,
                        plan_id,
                        status.as_deref(),
                        tag.as_deref(),
                        kind.as_deref(),
                    )?,
                    "activity" => pt::list_tasks_by_activity(
                        &conn,
                        plan_id,
                        status.as_deref(),
                        tag.as_deref(),
                        kind.as_deref(),
                    )?,
                    _ => bail!("unknown list sort '{sort}' (expected seq|activity)"),
                };
                let mut shown = Vec::new();
                for t in &tasks {
                    if !unreviewed || pt::review::needs_review(&conn, t)? {
                        shown.push(t);
                    }
                }
                // Several plans each get a heading; an empty one stays quiet.
                if several && !shown.is_empty() {
                    pt::outln!("{slug}");
                }
                for t in shown {
                    print_task_line(&conn, t, unreviewed)?;
                }
            }
        }
        Cmd::Search {
            query,
            plan,
            status,
            kind,
            limit,
            compact,
        } => {
            let limit = limit.unwrap_or(if compact { 5 } else { 20 });
            let response = pt::search_tasks(
                &conn,
                &query,
                plan.as_deref(),
                status.as_deref(),
                kind.as_deref(),
                limit,
            )?;
            if json {
                if compact {
                    pt::outln!("{}", serde_json::to_string_pretty(&response.compact())?);
                    return Ok(());
                }
                pt::outln!("{}", serde_json::to_string_pretty(&response)?);
                return Ok(());
            }
            if response.results.is_empty() {
                pt::outln!("no matching tasks");
            }
            for result in &response.results {
                pt::outln!(
                    "{} {} [{}] {}",
                    status_glyph(&result.task.status),
                    result.task.handle,
                    result.plan,
                    result.task.title
                );
                pt::outln!(
                    "  matched {} (score {}); {}: {}",
                    result.matched_fields.join(", "),
                    result.score,
                    result.excerpt.field,
                    result.excerpt.text.replace('\n', " ")
                );
            }
            if response.truncated {
                pt::outln!(
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
            parent,
            clear_parent,
            kind,
            priority,
            why,
        } => {
            reject_multiple_stdin(&[("intent", intent.reads_stdin()), ("why", why.reads_stdin())])?;
            let intent = intent.optional()?;
            let why = why.optional()?;
            let parent = if clear_parent {
                Some(None)
            } else {
                parent
                    .map(|parent| pt::resolve_task_ref(&conn, &parent).map(Some))
                    .transpose()?
            };
            let seq = pt::resolve_task_ref(&conn, &task)?;
            let changed = pt::edit_task(
                &conn,
                &actor,
                seq,
                pt::TaskEdit {
                    title: title.as_deref(),
                    intent: intent.as_deref(),
                    parent,
                    kind: kind.as_deref(),
                    priority,
                },
                why.as_deref(),
            )?;
            let handle = pt::seq_handle(&conn, seq)?;
            mutation_output!("{handle} updated ({})", changed.join(", "));
        }
        Cmd::Start { task, text, why } => {
            let why = second_word(text, why.optional()?, "why")?;
            let seq = pt::resolve_task_ref(&conn, &task)?;
            pt::start_task(&conn, &actor, seq, why.as_deref(), session.as_deref())?;
            let handle = pt::seq_handle(&conn, seq)?;
            mutation_output!("{handle} in progress");
        }
        Cmd::MovePlan { tasks, plan, why } => {
            let why = why.optional()?;
            let sequences = tasks
                .iter()
                .map(|task| pt::resolve_task_ref(&conn, task))
                .collect::<Result<Vec<_>>>()?;
            pt::move_tasks_to_plan(&conn, &actor, &sequences, &plan, why.as_deref())?;
            mutation_output!("moved {} task(s) to {plan}", sequences.len());
        }
        Cmd::Done { task, text, result } => {
            let result = second_word(text, result.optional()?, "result")?;
            let seq = pt::resolve_task_ref(&conn, &task)?;
            pt::complete_task(&conn, &actor, seq, result.as_deref())?;
            let handle = pt::seq_handle(&conn, seq)?;
            mutation_output!("{handle} done");
        }
        Cmd::Reopen { task, text, why } => {
            let why = second_word(text, why.optional()?, "why")?;
            let seq = pt::resolve_task_ref(&conn, &task)?;
            pt::reopen_task(&conn, &actor, seq, why.as_deref())?;
            let handle = pt::seq_handle(&conn, seq)?;
            mutation_output!("{handle} reopened");
        }
        Cmd::Retire {
            task,
            text,
            into,
            why,
        } => {
            let why = second_word(text, why.optional()?, "why")?;
            let seq = pt::resolve_task_ref(&conn, &task)?;
            let replacement_seq = into
                .as_deref()
                .map(|task| pt::resolve_task_ref(&conn, task))
                .transpose()?;
            pt::retire_task(&conn, &actor, seq, replacement_seq, why.as_deref())?;
            if let Some(replacement_seq) = replacement_seq {
                let (handle, replacement) = (
                    pt::seq_handle(&conn, seq)?,
                    pt::seq_handle(&conn, replacement_seq)?,
                );
                mutation_output!("{handle} retired into {replacement}");
            } else {
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("{handle} retired");
            }
        }
        Cmd::Brief { task } => {
            let seq = pt::resolve_task_ref(&conn, &task)?;
            let brief = pt::brief(&conn, seq)?;
            if json {
                pt::outln!("{}", serde_json::to_string_pretty(&brief)?);
                return Ok(());
            }
            print_brief(&brief)?;
        }
        Cmd::Models { plan, since } => {
            let report = pt::model_report(&conn, plan.as_deref(), since.as_deref())?;
            if json {
                pt::outln!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }
            if report.models.is_empty() {
                pt::outln!("no claims in scope");
            }
            for outcome in &report.models {
                let verdicts = |claims: &pt::model_report::ClaimCounts| {
                    let parts = [
                        (claims.cross_model.confirmed, "confirmed by other models"),
                        (claims.cross_model.disputed, "disputed by other models"),
                        (claims.same_model.confirmed, "confirmed by the same model"),
                        (claims.same_model.disputed, "disputed by the same model"),
                        (claims.unreviewed, "unreviewed"),
                    ]
                    .into_iter()
                    .filter(|(count, _)| *count > 0)
                    .map(|(count, label)| format!("{count} {label}"))
                    .collect::<Vec<_>>();
                    if parts.is_empty() {
                        "no reviews".to_owned()
                    } else {
                        parts.join(", ")
                    }
                };
                let filings = &outcome.filings;
                let results = &outcome.results;
                pt::outln!(
                    "{}{}",
                    outcome.model,
                    if outcome.small_sample {
                        " (small sample)"
                    } else {
                        ""
                    }
                );
                pt::outln!(
                    "  filings {}: {} done, {} retired, {} rejected, {} open; {} engaged; {}",
                    filings.claims.total,
                    filings.done,
                    filings.retired,
                    filings.rejected,
                    filings.open,
                    filings.engaged,
                    verdicts(&filings.claims)
                );
                pt::outln!(
                    "  results {}: {} reopened; {}",
                    results.claims.total,
                    results.reopened,
                    verdicts(&results.claims)
                );
            }
        }
        Cmd::Review {
            task,
            verdict,
            result,
            why,
        } => {
            let why = why.required()?;
            let seq = pt::resolve_task_ref(&conn, &task)?;
            let target = if result {
                pt::ReviewTarget::Result
            } else {
                pt::ReviewTarget::Filing
            };
            pt::review_task(
                &conn,
                &actor,
                seq,
                target,
                pt::ReviewVerdict::parse(&verdict)?,
                &why,
            )?;
            let handle = pt::get_task(&conn, seq)?.handle;
            let claim = if result { "result" } else { "filing" };
            mutation_output!("{handle} {claim} {verdict}");
        }
        Cmd::Reject { task, text, why } => {
            let why = second_word(text, why.optional()?, "why")?.ok_or_else(|| {
                anyhow::anyhow!(
                    "reject requires a reason: `papertiger reject <task> \"<why>\"`, or --why/--why-file"
                )
            })?;
            let seq = pt::resolve_task_ref(&conn, &task)?;
            pt::reject_task(&conn, &actor, seq, &why)?;
            let handle = pt::seq_handle(&conn, seq)?;
            mutation_output!("{handle} rejected");
        }
        Cmd::Gate { cmd } => match cmd {
            GateCmd::Add {
                task,
                name,
                kind,
                requirement,
            } => {
                let seq = pt::resolve_task_ref(&conn, &task)?;
                pt::add_gate(&conn, &actor, seq, &name, &kind, &requirement)?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("gate '{name}' added to {handle}");
            }
            GateCmd::Resolve {
                task,
                name,
                evidence,
                sha256,
                note,
            } => {
                let seq = pt::resolve_task_ref(&conn, &task)?;
                validate_new_evidence(
                    &evidence,
                    sha256.as_deref(),
                    project_root.as_deref(),
                    evidence_db_override.is_some(),
                    &db_path,
                )?;
                pt::resolve_gate(
                    &conn,
                    &actor,
                    seq,
                    &name,
                    &evidence,
                    sha256.as_deref(),
                    note.as_deref(),
                )?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("gate '{name}' on {handle} resolved");
            }
            GateCmd::Waive { task, name, why } => {
                let why = why.required()?;
                let seq = pt::resolve_task_ref(&conn, &task)?;
                pt::waive_gate(&conn, &actor, seq, &name, &why)?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("gate '{name}' on {handle} waived");
            }
            GateCmd::Reopen { task, name, why } => {
                let why = why.optional()?;
                let seq = pt::resolve_task_ref(&conn, &task)?;
                pt::reopen_gate(&conn, &actor, seq, &name, why.as_deref())?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("gate '{name}' on {handle} reopened");
            }
            GateCmd::Remove { task, name, why } => {
                let why = why.required()?;
                let seq = pt::resolve_task_ref(&conn, &task)?;
                pt::remove_gate(&conn, &actor, seq, &name, &why)?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("gate '{name}' removed from {handle}");
            }
            GateCmd::List { task } => {
                let t = pt::get_task(&conn, pt::resolve_task_ref(&conn, &task)?)?;
                let mut st = conn.prepare(
                    "SELECT name, kind, status, requirement FROM gates WHERE task_id=?1 ORDER BY gate_id",
                )?;
                let rows: Vec<(String, String, String, String)> = st
                    .query_map(params![t.task_id], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                for (name, kind, status, req) in rows {
                    pt::outln!("[{status}] {name} ({kind}): {req}");
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
                let seq = pt::resolve_task_ref(&conn, &task)?;
                let reference = pt::new_external_reference(
                    &kind,
                    &locator,
                    sha256.as_deref(),
                    note.as_deref(),
                )?;
                pt::add_external_reference(&conn, &actor, seq, &reference)?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("reference {kind} '{locator}' recorded on {handle}");
            }
            ReferenceCmd::Remove {
                task,
                locator,
                kind,
                why,
            } => {
                let seq = pt::resolve_task_ref(&conn, &task)?;
                pt::remove_external_reference(
                    &conn,
                    &actor,
                    seq,
                    &kind,
                    &locator,
                    &why.required()?,
                )?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("reference {kind} '{locator}' removed from {handle}");
            }
            ReferenceCmd::List { task } => {
                let references =
                    pt::external_references(&conn, pt::resolve_task_ref(&conn, &task)?)?;
                if json {
                    pt::outln!("{}", serde_json::to_string_pretty(&references)?);
                } else {
                    for reference in references {
                        pt::outln!("[{}] {}", reference.kind, reference.locator);
                    }
                }
            }
            ReferenceCmd::Find { locator } => {
                let matches = pt::find_external_references(&conn, &locator)?;
                if json {
                    pt::outln!("{}", serde_json::to_string_pretty(&matches)?);
                } else {
                    for item in matches {
                        pt::outln!(
                            "{} [{}] {}",
                            pt::seq_handle(&conn, item.task_seq)?,
                            item.reference.kind,
                            item.task_title
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
                let seq = pt::resolve_task_ref(&conn, &task)?;
                pt::add_commit_association(
                    &conn,
                    &actor,
                    seq,
                    &repo,
                    &commit_oid,
                    note.as_deref(),
                )?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("commit {commit_oid} ({repo}) recorded on {handle}");
            }
            CommitCmd::Remove {
                task,
                commit_oid,
                repo,
                why,
            } => {
                let why = why.optional()?;
                let seq = pt::resolve_task_ref(&conn, &task)?;
                pt::remove_commit_association(
                    &conn,
                    &actor,
                    seq,
                    &repo,
                    &commit_oid,
                    why.as_deref(),
                )?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("commit {commit_oid} ({repo}) removed from {handle}");
            }
            CommitCmd::List { task } => {
                let seq = pt::resolve_task_ref(&conn, &task)?;
                let commits = pt::commit_associations(&conn, seq)?;
                if json {
                    pt::outln!("{}", serde_json::to_string_pretty(&commits)?);
                } else {
                    for commit in commits {
                        let note = commit
                            .note
                            .map(|note| format!(" - {note}"))
                            .unwrap_or_default();
                        pt::outln!(
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
                    pt::outln!("{}", serde_json::to_string_pretty(&matches)?);
                } else {
                    for found in matches {
                        pt::outln!(
                            "{} {} ({}) {}",
                            pt::seq_handle(&conn, found.task_seq)?,
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
                let why = why.optional()?;
                let (a, b) = (
                    pt::resolve_task_ref(&conn, &task)?,
                    pt::resolve_task_ref(&conn, &on)?,
                );
                pt::add_dep(&conn, &actor, a, b, why.as_deref())?;
                let (a, b) = (pt::seq_handle(&conn, a)?, pt::seq_handle(&conn, b)?);
                mutation_output!("{a} now depends on {b}");
            }
            DepCmd::Remove { task, on, why } => {
                let why = why.optional()?;
                let (a, b) = (
                    pt::resolve_task_ref(&conn, &task)?,
                    pt::resolve_task_ref(&conn, &on)?,
                );
                pt::remove_dep(&conn, &actor, a, b, why.as_deref())?;
                let (a, b) = (pt::seq_handle(&conn, a)?, pt::seq_handle(&conn, b)?);
                mutation_output!("{a} no longer depends on {b}");
            }
        },
        Cmd::Focus {
            plan,
            limit,
            include_blocked,
            kind,
            unreviewed,
        } => {
            // Text focus without --plan ranks every active plan in turn; JSON
            // stays one plan per document, so it still asks for --plan.
            let targets = match plan.as_deref() {
                Some(slug) => vec![pt::resolve_plan(&conn, Some(slug))?],
                None if json => match pt::active_plan(&conn)? {
                    Some(selected) => vec![selected],
                    None => {
                        pt::outln!(
                            "{}",
                            serde_json::to_string_pretty(&pt::FocusResponse::no_active_plan())?
                        );
                        return Ok(());
                    }
                },
                None => text_plans(&conn, None)?,
            };
            let several = targets.len() > 1;
            let limit = limit.unwrap_or(if several { 5 } else { 20 });
            for (plan_id, slug) in targets {
                let heading = if several {
                    format!("focus {slug}: ")
                } else {
                    String::new()
                };
                let response = pt::focus(
                    &conn,
                    plan_id,
                    limit,
                    include_blocked,
                    kind.as_deref(),
                    unreviewed,
                    session.as_deref(),
                )?;
                if json {
                    pt::outln!("{}", serde_json::to_string_pretty(&response)?);
                } else if response.projection.entries.is_empty() {
                    // Say when the only open work is blocked rather than absent.
                    let hidden = if include_blocked {
                        0
                    } else {
                        pt::focus(
                            &conn,
                            plan_id,
                            usize::MAX,
                            true,
                            kind.as_deref(),
                            unreviewed,
                            session.as_deref(),
                        )?
                        .projection
                        .entries
                        .len()
                    };
                    if hidden == 0 {
                        pt::outln!("{heading}nothing actionable");
                    } else {
                        pt::outln!(
                            "{heading}nothing actionable; {hidden} blocked task(s) hidden: run `papertiger focus --plan {slug} --include-blocked`"
                        );
                    }
                } else {
                    pt::outln!("focus {slug}");
                    let mut current = None::<String>;
                    for entry in response.projection.entries {
                        if current.as_deref() != Some(entry.readiness.as_str()) {
                            pt::outln!("{}:", entry.readiness);
                            current = Some(entry.readiness.clone());
                        }
                        let blockers = if entry.blockers.is_empty() {
                            String::new()
                        } else {
                            format!(" [blocked by {}]", entry.blockers.join(" "))
                        };
                        let path = if entry.ancestry.is_empty() {
                            String::new()
                        } else {
                            format!(" [in {}]", entry.ancestry.join(" › "))
                        };
                        let review = review_marker("", &entry.review, unreviewed);
                        let closing = if entry.closed_child_count > 0 {
                            format!(" [close: all {} children closed]", entry.closed_child_count)
                        } else {
                            String::new()
                        };
                        pt::outln!(
                            "  {} {}{} [priority {} unlocks {} downstream {} gates {}]{}{}{}{}",
                            entry.task.handle,
                            entry.task.title,
                            path,
                            entry.task.priority,
                            entry.immediate_unlock_count,
                            entry.unfinished_downstream_count,
                            entry.open_gate_count,
                            review,
                            closing,
                            blockers,
                            pickup_label(entry.pickup.as_ref()),
                        );
                    }
                    if response.projection.omitted_count > 0
                        && let Some(command) = response.projection.continuation_command
                    {
                        // The text reader continues in text; a session that
                        // came from the environment needs no flag.
                        let mut command = command.replacen(" --json", "", 1);
                        if !explicit_session && let Some(session) = &session {
                            command = command.replacen(&format!(" --session {session}"), "", 1);
                        }
                        pt::outln!(
                            "  ... {} actionable task(s) omitted; run `{command}`",
                            response.projection.omitted_count
                        );
                    }
                }
            }
        }
        Cmd::Tag { cmd } => match cmd {
            TagCmd::Add { task, tag, why } => {
                let why = why.optional()?;
                let seq = pt::resolve_task_ref(&conn, &task)?;
                pt::add_tag(&conn, &actor, seq, &tag, why.as_deref())?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("tag '{tag}' added to {handle}");
            }
            TagCmd::Remove { task, tag, why } => {
                let why = why.optional()?;
                let seq = pt::resolve_task_ref(&conn, &task)?;
                pt::remove_tag(&conn, &actor, seq, &tag, why.as_deref())?;
                let handle = pt::seq_handle(&conn, seq)?;
                mutation_output!("tag '{tag}' removed from {handle}");
            }
        },
        Cmd::Tree { plan, kind } => {
            for (plan_id, slug) in text_plans(&conn, plan.as_deref())? {
                let visible = kind
                    .as_deref()
                    .map(|kind| tree_kind_scope(&conn, plan_id, kind))
                    .transpose()?;
                pt::outln!("{slug}");
                print_tree(&conn, plan_id, None, 1, visible.as_ref())?;
            }
        }
        Cmd::Note { words, text, task } => {
            // Positional words read by count: one is the text, two are a
            // task and its text; flags say the same thing explicitly.
            let (positional_task, positional_text) = match words.as_slice() {
                [text] => (None, Some(text.clone())),
                [task, text] => (Some(task.clone()), Some(text.clone())),
                _ => (None, None),
            };
            let text = match (positional_text, text.optional()?) {
                (Some(_), Some(_)) => bail!(
                    "give the note text once, positionally or with --text/--text-file, not both"
                ),
                (Some(text), None) | (None, Some(text)) => text,
                (None, None) => bail!(
                    "note needs its text: `papertiger note [<task>] \"<text>\"`, or --text/--text-file"
                ),
            };
            let task = match (positional_task, task) {
                (Some(_), Some(_)) => {
                    bail!("name the note's task once, positionally or with --task, not both")
                }
                (positional, flag) => positional.or(flag),
            };
            let task_seq = task
                .map(|task| pt::resolve_task_ref(&conn, &task))
                .transpose()?;
            pt::add_note(&conn, &actor, task_seq, &text)?;
            mutation_output!("noted");
        }
        Cmd::Log {
            task,
            limit,
            before_cursor,
            after_cursor,
        } => {
            let task_seq = task
                .map(|task| pt::resolve_task_ref(&conn, &task))
                .transpose()?;
            let log = pt::event_log(
                &conn,
                task_seq,
                limit,
                before_cursor.as_deref(),
                after_cursor.as_deref(),
            )?;
            if json {
                pt::outln!("{}", serde_json::to_string_pretty(&log)?);
                return Ok(());
            }
            for event in &log.events {
                pt::outln!("{}", log_line(&conn, event));
            }
            if log.truncated
                && let Some(cursor) = &log.continuation
            {
                let flag = if log.direction == "after" {
                    "--after-cursor"
                } else {
                    "--before-cursor"
                };
                pt::outln!(
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
                pt::outln!(
                    "{}",
                    serde_json::json!({"schema": "papertiger.audit.v1", "findings": findings})
                );
                return Ok(());
            }
            if findings.is_empty() {
                pt::outln!("no findings");
            }
            for f in findings {
                pt::outln!("[{}] {}", f.kind, f.detail);
            }
        }
        Cmd::History { cmd } => match cmd {
            HistoryCmd::Inspect { event_id } => {
                pt::outln!(
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
                    pt::outln!(
                        "quarantined event {event_id}; original retained verbatim in recovery event {recovery}; no historical timestamp, association, or task state was inferred"
                    );
                }
            }
        },
        Cmd::Evidence {
            cmd:
                EvidenceCmd::Verify {
                    task,
                    classification,
                    task_state,
                    limit,
                    after_cursor,
                },
        } => {
            let task_seq = task
                .map(|task| pt::resolve_task_ref(&conn, &task))
                .transpose()?;
            let project_root = match project_root {
                Some(root) => root,
                None => project_authority::discover(&std::env::current_dir()?)?
                    .map(|found| found.root)
                    .context(
                        "no project with state/papertiger.sqlite was found above the working directory; pass `papertiger evidence verify --project-root <project-root>`",
                    )?,
            };
            let options = pt::EvidenceVerificationOptions {
                task_seq,
                classification,
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
                    "--classification".to_owned(),
                    classification.as_str().to_owned(),
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
                pt::outln!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                pt::outln!(
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
                pt::outln!(
                    "status counts: {}",
                    serde_json::to_string(&report.summary.status_counts)?
                );
                if !report.summary.unsupported_scheme_counts.is_empty() {
                    pt::outln!(
                        "unsupported scheme counts: {}",
                        serde_json::to_string(&report.summary.unsupported_scheme_counts)?
                    );
                }
                pt::outln!(
                    "details: {} of {} eligible shown from index {}; {} remaining; classification={}, task-state={}",
                    report.projection.returned_count,
                    report.projection.eligible_count,
                    report.projection.page_start,
                    report.projection.remaining_count,
                    report.projection.classification.as_str(),
                    report.projection.task_state.as_str()
                );
                for binding in &report.projection.bindings {
                    pt::outln!(
                        "  {} {} '{}' [{}/{}] {}",
                        pt::seq_handle(&conn, binding.task_seq)?,
                        binding.entity,
                        binding.name,
                        binding.status,
                        binding.classification.as_str(),
                        binding.locator
                    );
                    if let Some(detail) = &binding.detail {
                        pt::outln!("    {detail}");
                    }
                    for command in &binding.corrective_commands {
                        pt::outln!(
                            "    corrective argv: {} {}",
                            command.program,
                            serde_json::to_string(&command.arguments)?
                        );
                    }
                }
                if let Some(command) = &report.projection.continuation_command {
                    pt::outln!(
                        "  continuation argv: {} {}",
                        command.program,
                        serde_json::to_string(&command.arguments)?
                    );
                }
            }
            if !report.summary.verification_complete {
                bail!(
                    "evidence verification is incomplete across the full task scope; use --classification failed or --classification unsupported for bounded details, follow failed bindings' corrective argv, and do not count unsupported schemes as verified"
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
                pt::outln!("{}", serde_json::to_string_pretty(&receipt)?);
            } else {
                pt::outln!("{}", serde_json::to_string_pretty(&dump)?);
            }
        }
        Cmd::Import { file } => {
            let text = std::fs::read_to_string(&file).with_context(|| format!("read {file}"))?;
            let dump = pt::parse_dump_json(&text)?;
            let (tasks, deps) = pt::import(&conn, &actor, &dump)?;
            mutation_output!("imported {tasks} task(s), {deps} dependency edge(s)");
        }
        Cmd::Mise { cmd } => match cmd {
            MiseCmd::Record { task, projection } => {
                let task_seq = pt::resolve_task_ref(&conn, &task)?;
                let bytes = read_input_bytes(&projection, "Mise planner projection", None)?;
                let (outcome, record) =
                    pt::record_mise_projection(&conn, &actor, task_seq, &bytes)?;
                mutation_output!(
                    "{} {} task {} [{}] campaign {} candidate {}{}",
                    outcome.as_str(),
                    record.projection_sha256,
                    pt::seq_handle(&conn, record.task_seq)?,
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
                let task_seq = pt::resolve_task_ref(&conn, &task)?;
                let records = pt::task_mise_projection_summaries(&conn, task_seq)?;
                if json {
                    pt::outln!("{}", serde_json::to_string_pretty(&records)?);
                } else if records.is_empty() {
                    let handle = pt::seq_handle(&conn, task_seq)?;
                    pt::outln!("task {handle} has no Mise projections");
                } else {
                    for record in records {
                        pt::outln!(
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
                pt::outln!("{}", serde_json::to_string_pretty(&record)?);
            }
        },
    }
    if json && let Some(recorder) = recorder {
        pt::outln!("{}", serde_json::to_string_pretty(&recorder.receipt()?)?);
    }
    Ok(())
}

/// A new `file:` gate locator resolves beneath the project root of
/// the selected authority: the explicit --project-root, else the discovered
/// project unless a database override bypassed discovery.
fn validate_new_evidence(
    evidence: &str,
    sha256: Option<&str>,
    project_root: Option<&std::path::Path>,
    database_override: bool,
    db_path: &str,
) -> Result<()> {
    let project_root = match project_root {
        Some(root) => Some(root.to_path_buf()),
        None if database_override => None,
        None => project_authority::discover(&std::env::current_dir()?)?.map(|found| found.root),
    };
    let authority = std::fs::canonicalize(db_path)
        .ok()
        .and_then(|path| pt::portable_absolute(&path).ok())
        .unwrap_or_else(|| db_path.to_owned());
    pt::validate_new_file_evidence(evidence, sha256, project_root.as_deref(), &authority)
}

/// Read a whole input file, or stdin for '-'.
/// Read PATH, or stdin for '-'. With `limit`, reading stops one byte past it
/// so an oversized input is never buffered whole; the parser names the limit.
fn read_input_bytes(path: &str, label: &str, limit: Option<usize>) -> Result<Vec<u8>> {
    let mut reader: Box<dyn Read> = if path == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(std::fs::File::open(path).with_context(|| format!("read {label} {path}"))?)
    };
    if let Some(limit) = limit {
        reader = Box::new(reader.take(limit as u64 + 1));
    }
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).with_context(|| {
        if path == "-" {
            format!("read {label} from stdin")
        } else {
            format!("read {label} {path}")
        }
    })?;
    Ok(bytes)
}

fn short_event_time(at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(at)
        .map(|timestamp| timestamp.format("%Y-%m-%dT%H:%M").to_string())
        .unwrap_or_else(|_| at.chars().take(16).collect())
}

/// Progress over a parent's children: `(done/total)` while every closed
/// child is done, otherwise `(closed/total closed: N done, N retired, ...)`
/// so retired and rejected children count as closed without passing as done.
fn child_progress(conn: &Connection, task_id: i64) -> Result<String> {
    let (total, done, retired, rejected): (i64, i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(status='done'), 0),
                COALESCE(SUM(status='retired'), 0),
                COALESCE(SUM(status='rejected'), 0)
           FROM tasks WHERE parent_id=?1",
        params![task_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if total == 0 {
        return Ok(String::new());
    }
    if retired == 0 && rejected == 0 {
        return Ok(format!(" ({done}/{total})"));
    }
    let closed = done + retired + rejected;
    let parts = [(done, "done"), (retired, "retired"), (rejected, "rejected")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(" ({closed}/{total} closed: {parts})"))
}

/// Tasks of `kind` in a plan plus every ancestor, so a filtered tree keeps
/// each match under the parents that place it.
fn tree_kind_scope(
    conn: &Connection,
    plan_id: i64,
    kind: &str,
) -> Result<std::collections::HashSet<i64>> {
    let matches = pt::list_tasks(conn, plan_id, None, None, Some(kind))?;
    let mut visible = std::collections::HashSet::new();
    for task in matches {
        let mut next = Some(task.task_id);
        while let Some(task_id) = next {
            if !visible.insert(task_id) {
                break;
            }
            next = conn.query_row(
                "SELECT parent_id FROM tasks WHERE task_id=?1",
                params![task_id],
                |row| row.get(0),
            )?;
        }
    }
    Ok(visible)
}

fn print_tree(
    conn: &Connection,
    plan_id: i64,
    parent: Option<i64>,
    depth: usize,
    visible: Option<&std::collections::HashSet<i64>>,
) -> Result<()> {
    let sql = match parent {
        Some(_) => {
            "SELECT task_id, seq, title, status, kind FROM tasks WHERE plan_id=?1 AND parent_id=?2 ORDER BY seq"
        }
        None => {
            "SELECT task_id, seq, title, status, kind FROM tasks WHERE plan_id=?1 AND parent_id IS NULL ORDER BY seq"
        }
    };
    let mut st = conn.prepare(sql)?;
    let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(i64, i64, String, String, String)> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    };
    let rows: Vec<(i64, i64, String, String, String)> = match parent {
        Some(p) => st
            .query_map(params![plan_id, p], map)?
            .collect::<rusqlite::Result<_>>()?,
        None => st
            .query_map(params![plan_id], map)?
            .collect::<rusqlite::Result<_>>()?,
    };
    for (task_id, seq, title, status, kind) in rows {
        if visible.is_some_and(|visible| !visible.contains(&task_id)) {
            continue;
        }
        let progress = child_progress(conn, task_id)?;
        let handle = pt::task_handle(seq, &kind);
        pt::outln!(
            "{}{} {handle} {title}{progress}",
            "  ".repeat(depth),
            status_glyph(&status)
        );
        print_tree(conn, plan_id, Some(task_id), depth + 1, visible)?;
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
                cmd: MiseCmd::Record {
                    task: "1".into(),
                    projection: "projection.json".into(),
                },
            }
            .opens_authority_read_only()
        );
    }
}
