use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use papertiger as pt;
use rusqlite::{Connection, params};
use std::io::Read;

mod project_setup;
mod text_input;

use text_input::{IntentArgs, NoteTextArgs, ResultArgs, WhyArgs, reject_multiple_stdin};

#[derive(Parser)]
#[command(
    name = "papertiger",
    version,
    about = "Local task planning for cross-session engineering work"
)]
struct Cli {
    /// Planning database path (default: PAPERTIGER_DB or state/papertiger.sqlite); invalid with setup-project
    #[arg(long, global = true)]
    db: Option<String>,
    /// Actor recorded on events (default: PAPERTIGER_ACTOR or 'operator'); invalid with setup-project
    #[arg(long, global = true)]
    actor: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install a project-local binary, launchers, ignore policy, and agent contract; does not accept --db or --actor
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
        /// Emit papertiger.project_setup.v2 JSON
        #[arg(long)]
        json: bool,
    },
    /// Create or upgrade a Papertiger database; refuses nonempty foreign databases
    Init,
    /// One-screen orientation: authority, active plans, current work, ready work, recent notes
    Status {
        /// Emit papertiger.status.v1 JSON
        #[arg(long)]
        json: bool,
    },
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
    },
    /// Show one task in full
    Show {
        /// Task sequence (bare N is shell-portable; quoted #N also works)
        task: String,
        /// Emit papertiger.task_context.v4 JSON
        #[arg(long)]
        json: bool,
    },
    /// List tasks (compact)
    List {
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
        /// Emit papertiger.task_list.v1 JSON
        #[arg(long)]
        json: bool,
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
        /// Emit papertiger.search.v1 JSON
        #[arg(long)]
        json: bool,
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
    /// Mark a task in progress
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
        /// Emit papertiger.focus.v4 JSON
        #[arg(long)]
        json: bool,
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
        /// Emit papertiger.event_log.v1 JSON
        #[arg(long)]
        json: bool,
    },
    /// Advisory integrity findings
    Audit,
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
    /// Import a papertiger.dump.v6 JSON file
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
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Reverse lookup local tasks associated with one full commit object ID
    Find {
        /// Caller-resolved lowercase 40- or 64-hex commit object ID
        commit_oid: String,
        /// Restrict lookup to this stable repository label
        #[arg(long)]
        repo: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
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
        /// Emit JSON
        #[arg(long)]
        json: bool,
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
    List,
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

fn print_task_context(context: &pt::TaskContext) {
    let task = &context.task;
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
        println!("  intent: {}", task.intent.replace('\n', "\n  "));
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
                "  {label}: {} by {} (@{})",
                event.at, event.actor, event.event_id
            );
        }
    }
    if let Some(result) = &task.result {
        println!("  result: {}", result.replace('\n', "\n  "));
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

fn main() -> Result<()> {
    run().map_err(pt::normalize_sqlite_lock_error)
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    if let Cmd::SetupProject {
        project_root,
        dry_run,
        replace_managed,
        authority_path,
        json,
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
        let result = project_setup::setup_project(project_setup::SetupProjectRequest {
            project_root,
            source_binary: None,
            dry_run: *dry_run,
            replace_managed: *replace_managed,
            authority_path: authority_path.as_deref(),
        })?;
        if *json {
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
            if !result.agent_guidance_files_found.is_empty() {
                println!(
                    "  preserved agent guidance: {}",
                    result
                        .agent_guidance_files_found
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            println!("next:");
            for action in &result.next_actions {
                println!("  - {action}");
            }
        }
        return Ok(());
    }

    let db_path = cli
        .db
        .or_else(|| std::env::var("PAPERTIGER_DB").ok())
        .unwrap_or_else(|| "state/papertiger.sqlite".into());
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
        pt::init(&conn)?;
        println!("initialized {db_path} (schema v{})", pt::SCHEMA_VERSION);
        return Ok(());
    }

    let mut conn = pt::open_existing(&db_path)?;

    match cli.cmd {
        Cmd::SetupProject { .. } => unreachable!(),
        Cmd::Init => unreachable!(),
        Cmd::Status { json } => {
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
                for entry in &active.in_progress {
                    println!("> #{} {}", entry.task.seq, entry.task.title);
                }
                for entry in &active.ready {
                    println!("· #{} {}", entry.task.seq, entry.task.title);
                }
            }
            for note in &status.recent_notes {
                println!(
                    "note @{} {} {}: {}",
                    note.event_id,
                    short_event_time(&note.at),
                    note.actor,
                    note.why.as_deref().unwrap_or_default()
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
                println!("plan {slug} created");
            }
            PlanCmd::List => {
                let mut st =
                    conn.prepare("SELECT slug, title, status FROM plans ORDER BY plan_id")?;
                let rows: Vec<(String, String, String)> = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                for (slug, title, status) in rows {
                    println!("{status:8} {slug}: {title}");
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
                println!("plan {slug} updated ({})", changed.join(", "));
            }
            PlanCmd::Set { slug, status, why } => {
                let why = why.required()?;
                pt::set_plan_status(&conn, &actor, &slug, &status, &why)?;
                println!("plan {slug} -> {status}");
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
        } => {
            reject_multiple_stdin(&[("intent", intent.reads_stdin()), ("why", why.reads_stdin())])?;
            let intent = intent.optional()?.unwrap_or_default();
            let why = why.optional()?;
            let parent = parent.map(|p| pt::parse_task_ref(&p)).transpose()?;
            let deps: Vec<i64> = dep
                .iter()
                .map(|d| pt::parse_task_ref(d))
                .collect::<Result<_>>()?;
            let (seq, slug) = pt::add_task_for_plan(
                &conn,
                &actor,
                plan.as_deref(),
                &title,
                &intent,
                &kind,
                parent,
                &deps,
                &tag,
                priority,
                why.as_deref(),
            )?;
            println!("#{seq} added to {slug}");
        }
        Cmd::Show { task, json } => {
            let context = pt::task_context(&conn, pt::parse_task_ref(&task)?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&context)?);
                return Ok(());
            }
            print_task_context(&context);
        }
        Cmd::List {
            plan,
            status,
            tag,
            sort,
            json,
        } => {
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
            json,
        } => {
            let response =
                pt::search_tasks(&conn, &query, plan.as_deref(), status.as_deref(), limit)?;
            if json {
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
                &why,
            )?;
            println!("#{seq} updated ({})", changed.join(", "));
        }
        Cmd::Start { task, why } => {
            let why = why.optional()?;
            let seq = pt::parse_task_ref(&task)?;
            pt::start_task(&conn, &actor, seq, why.as_deref())?;
            println!("#{seq} in progress");
        }
        Cmd::Done { task, result } => {
            let result = result.optional()?;
            let seq = pt::parse_task_ref(&task)?;
            pt::complete_task(&conn, &actor, seq, result.as_deref())?;
            println!("#{seq} done");
        }
        Cmd::Reopen { task, why } => {
            let why = why.required()?;
            let seq = pt::parse_task_ref(&task)?;
            pt::reopen_task(&conn, &actor, seq, &why)?;
            println!("#{seq} reopened");
        }
        Cmd::Retire { task, into, why } => {
            let why = why.required()?;
            let seq = pt::parse_task_ref(&task)?;
            let replacement_seq = into.as_deref().map(pt::parse_task_ref).transpose()?;
            pt::retire_task(&conn, &actor, seq, replacement_seq, &why)?;
            if let Some(replacement_seq) = replacement_seq {
                println!("#{seq} retired into #{replacement_seq}");
            } else {
                println!("#{seq} retired");
            }
        }
        Cmd::Reject { task, why } => {
            let why = why.required()?;
            let seq = pt::parse_task_ref(&task)?;
            pt::reject_task(&conn, &actor, seq, &why)?;
            println!("#{seq} rejected");
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
                println!("gate '{name}' added to #{seq}");
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
                println!("gate '{name}' on #{seq} closed");
            }
            GateCmd::Waive { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::waive_gate(&conn, &actor, seq, &name, &why)?;
                println!("gate '{name}' on #{seq} waived");
            }
            GateCmd::Reopen { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::reopen_gate(&conn, &actor, seq, &name, &why)?;
                println!("gate '{name}' on #{seq} reopened");
            }
            GateCmd::Remove { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::remove_open_gate(&conn, &actor, seq, &name, &why)?;
                println!("gate '{name}' removed from #{seq}");
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
                println!("blocker '{name}' added to #{seq}");
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
                println!("blocker '{name}' on #{seq} resolved");
            }
            BlockerCmd::Waive { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::waive_task_blocker(&conn, &actor, seq, &name, &why)?;
                println!("blocker '{name}' on #{seq} waived");
            }
            BlockerCmd::Remove { task, name, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::remove_open_task_blocker(&conn, &actor, seq, &name, &why)?;
                println!("blocker '{name}' removed from #{seq}");
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
                println!("commit {commit_oid} ({repo}) recorded on #{seq}");
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
                println!("commit {commit_oid} ({repo}) removed from #{seq}");
            }
            CommitCmd::List { task, json } => {
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
            CommitCmd::Find {
                commit_oid,
                repo,
                json,
            } => {
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
                println!("#{a} now depends on #{b}");
            }
            DepCmd::Remove { task, on, why } => {
                let why = why.required()?;
                let (a, b) = (pt::parse_task_ref(&task)?, pt::parse_task_ref(&on)?);
                pt::remove_dep(&conn, &actor, a, b, &why)?;
                println!("#{a} no longer depends on #{b}");
            }
        },
        Cmd::Focus {
            plan,
            limit,
            json,
            all,
        } => {
            let selected_plan = match plan.as_deref() {
                Some(slug) => Some(pt::resolve_plan(&conn, Some(slug))?),
                None if json => pt::active_plan(&conn)?,
                None => Some(pt::resolve_plan(&conn, None)?),
            };
            let Some((plan_id, slug)) = selected_plan else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema": "papertiger.focus.v4",
                        "selection_state": "no_active_plan",
                        "plan": null,
                        "entries": [],
                    }))?
                );
                return Ok(());
            };
            let entries = pt::focus(&conn, plan_id, limit, all)?;
            if json {
                let plan = pt::get_plan(&conn, plan_id)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema": "papertiger.focus.v4",
                        "selection_state": "resolved",
                        "plan": plan,
                        "entries": entries,
                    }))?
                );
            } else if entries.is_empty() {
                println!("nothing actionable");
            } else {
                println!("focus {slug}");
                let mut current = None::<String>;
                for entry in entries {
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
                        "  #{} {} [priority {} unlocks {} downstream {} gates {}]{}",
                        entry.task.seq,
                        entry.task.title,
                        entry.task.priority,
                        entry.immediate_unlock_count,
                        entry.unfinished_downstream_count,
                        entry.open_gate_count,
                        blockers,
                    );
                }
            }
        }
        Cmd::Tag { cmd } => match cmd {
            TagCmd::Add { task, tag, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::add_tag(&conn, &actor, seq, &tag, &why)?;
                println!("tag '{tag}' added to #{seq}");
            }
            TagCmd::Remove { task, tag, why } => {
                let why = why.required()?;
                let seq = pt::parse_task_ref(&task)?;
                pt::remove_tag(&conn, &actor, seq, &tag, &why)?;
                println!("tag '{tag}' removed from #{seq}");
            }
        },
        Cmd::Tree { plan } => {
            let (plan_id, slug) = pt::resolve_plan(&conn, plan.as_deref())?;
            println!("{slug}");
            print_tree(&conn, plan_id, None, 1)?;
        }
        Cmd::Note { text, task } => {
            let text = text.required()?;
            let task_seq = task.map(|task| pt::parse_task_ref(&task)).transpose()?;
            pt::add_note(&conn, &actor, task_seq, &text)?;
            println!("noted");
        }
        Cmd::Log {
            task,
            limit,
            before_cursor,
            after_cursor,
            json,
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
            if findings.is_empty() {
                println!("no findings");
            }
            for f in findings {
                println!("[{}] {}", f.kind, f.detail);
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
            let (tasks, deps) = pt::import(&mut conn, &actor, &dump)?;
            println!("imported {tasks} task(s), {deps} dependency edge(s)");
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
                println!(
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
            MiseCmd::List { task, json } => {
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
