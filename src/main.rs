use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use papertiger as pt;
use rusqlite::{Connection, params};
use std::io::Read;

mod project_setup;

type EventRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

#[derive(Parser)]
#[command(
    name = "papertiger",
    version,
    about = "Lean planning surface for cross-session engineering work"
)]
struct Cli {
    /// Database path (default: PAPERTIGER_DB or state/papertiger.sqlite)
    #[arg(long, global = true)]
    db: Option<String>,
    /// Actor recorded on events (default: PAPERTIGER_ACTOR or 'operator')
    #[arg(long, global = true)]
    actor: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install a project-local binary, launchers, ignore policy, and agent contract
    SetupProject {
        /// Existing consuming project directory
        project_root: std::path::PathBuf,
        /// Report the complete action plan without writing
        #[arg(long)]
        dry_run: bool,
        /// Replace divergent release-managed files after review
        #[arg(long)]
        replace_managed: bool,
        /// Emit papertiger.project_setup.v1 JSON
        #[arg(long)]
        json: bool,
    },
    /// Create or upgrade a Papertiger database; refuses nonempty foreign databases
    Init,
    /// One-screen orientation: active plan, in-progress, ready, blocked, recent notes
    Status,
    /// Plan management
    Plan {
        #[command(subcommand)]
        cmd: PlanCmd,
    },
    /// Add a task
    Add {
        title: String,
        #[arg(long)]
        plan: Option<String>,
        /// Intent text; use --intent-file or '-' (stdin) for long prose
        #[arg(long)]
        intent: Option<String>,
        #[arg(long)]
        intent_file: Option<String>,
        /// Work kind: work, probe, or decision
        #[arg(long, default_value = "work")]
        kind: String,
        #[arg(long)]
        parent: Option<String>,
        /// Dependencies, comma-separated task refs
        #[arg(long, value_delimiter = ',')]
        dep: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        tag: Vec<String>,
        #[arg(long, default_value_t = 0)]
        priority: i64,
        /// External traceability key only; commands always select tasks by task.seq
        #[arg(long)]
        alias: Option<String>,
        #[arg(long)]
        why: Option<String>,
    },
    /// Show one task in full
    Show {
        task: String,
        #[arg(long)]
        json: bool,
    },
    /// List tasks (compact)
    List {
        #[arg(long)]
        plan: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        tag: Option<String>,
    },
    /// Edit a task
    Edit {
        task: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        intent: Option<String>,
        #[arg(long)]
        intent_file: Option<String>,
        /// Replace the parent with this task
        #[arg(long, conflicts_with = "clear_parent")]
        parent: Option<String>,
        /// Remove the current parent
        #[arg(long)]
        clear_parent: bool,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        priority: Option<i64>,
        #[arg(long)]
        why: String,
    },
    /// Mark a task in progress
    Start {
        task: String,
        #[arg(long)]
        why: Option<String>,
    },
    /// Complete a task after dependencies, blockers, gates, and children are closed
    Done {
        task: String,
        #[arg(long)]
        result: Option<String>,
    },
    /// Reopen a finished task
    Reopen {
        task: String,
        #[arg(long)]
        why: String,
    },
    /// Retire a task (no longer worth doing)
    Retire {
        task: String,
        #[arg(long)]
        why: String,
    },
    /// Reject a task/approach (records why, prevents re-litigation)
    Reject {
        task: String,
        #[arg(long)]
        why: String,
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
    /// Dependency management
    Dep {
        #[command(subcommand)]
        cmd: DepCmd,
    },
    /// Ready tasks in execution order
    Next {
        #[arg(long)]
        plan: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Also show blocked tasks with named blockers
        #[arg(long)]
        all: bool,
    },
    /// Actionable leaf work ranked by readiness and dependency unlock impact
    Focus {
        #[arg(long)]
        plan: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
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
        #[arg(long)]
        plan: Option<String>,
    },
    /// Record a free-standing evented note (course changes, decisions)
    Note {
        text: String,
        #[arg(long)]
        task: Option<String>,
    },
    /// Event history
    Log {
        #[arg(long)]
        task: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Advisory integrity findings
    Audit,
    /// Dump plans/tasks/gates as JSON
    Export {
        #[arg(long)]
        plan: Option<String>,
    },
    /// Import a papertiger.dump.v1, v2, or v3 JSON file
    Import { file: String },
    /// Record and inspect verified terminal Mise evidence without changing task state
    Mise {
        #[command(subcommand)]
        cmd: MiseCmd,
    },
}

#[derive(Subcommand)]
enum MiseCmd {
    /// Idempotently attach one verified projection document to its owning task
    Project {
        task: String,
        /// Projection JSON path, or '-' to read a Mise inspector pipeline from stdin
        projection: String,
    },
    /// List every reverified Mise projection attached to one task
    List {
        task: String,
        #[arg(long)]
        json: bool,
    },
    /// Show one exact reverified projection by its SHA-256 identity
    Show { projection_sha256: String },
}

#[derive(Subcommand)]
enum PlanCmd {
    Add {
        slug: String,
        title: String,
        #[arg(long, default_value = "")]
        intent: String,
    },
    List,
    /// Edit plan orientation without replacing its task/event history
    Edit {
        slug: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        intent: Option<String>,
        #[arg(long)]
        why: String,
    },
    /// Set plan status (active|paused|done|retired)
    Set {
        slug: String,
        status: String,
        #[arg(long)]
        why: String,
    },
}

#[derive(Subcommand)]
enum GateCmd {
    Add {
        task: String,
        name: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        requirement: String,
    },
    Close {
        task: String,
        name: String,
        /// Evidence locator, scheme:value (file:, receipt:, claim:, spec:, commit:, url:, note:)
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        sha256: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    Waive {
        task: String,
        name: String,
        #[arg(long)]
        why: String,
    },
    Reopen {
        task: String,
        name: String,
        #[arg(long)]
        why: String,
    },
    Rm {
        task: String,
        name: String,
        #[arg(long)]
        why: String,
    },
    List {
        task: String,
    },
}

#[derive(Subcommand)]
enum BlockerCmd {
    Add {
        task: String,
        name: String,
        #[arg(long)]
        reason: String,
    },
    Resolve {
        task: String,
        name: String,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        sha256: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    Waive {
        task: String,
        name: String,
        #[arg(long)]
        why: String,
    },
    Rm {
        task: String,
        name: String,
        #[arg(long)]
        why: String,
    },
    List {
        task: String,
    },
}

#[derive(Subcommand)]
enum TagCmd {
    Add {
        task: String,
        tag: String,
        #[arg(long)]
        why: String,
    },
    Rm {
        task: String,
        tag: String,
        #[arg(long)]
        why: String,
    },
}

#[derive(Subcommand)]
enum DepCmd {
    Add {
        task: String,
        on: String,
        #[arg(long)]
        why: String,
    },
    Rm {
        task: String,
        on: String,
        #[arg(long)]
        why: String,
    },
}

fn read_intent(intent: Option<String>, intent_file: Option<String>) -> Result<String> {
    match (intent, intent_file) {
        (Some(_), Some(_)) => bail!("pass --intent or --intent-file, not both"),
        (Some(i), None) => Ok(i),
        (None, Some(f)) if f == "-" => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(s.trim().to_string())
        }
        (None, Some(f)) => Ok(std::fs::read_to_string(&f)
            .with_context(|| format!("read {f}"))?
            .trim()
            .to_string()),
        (None, None) => Ok(String::new()),
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
    if let Some(a) = &t.alias {
        extra.push_str(&format!(" ({a})"));
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
    if let Some(alias) = &task.alias {
        extra.push_str(&format!(" ({alias})"));
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
    let cli = Cli::parse();

    if let Cmd::SetupProject {
        project_root,
        dry_run,
        replace_managed,
        json,
    } = &cli.cmd
    {
        let result = project_setup::setup_project(project_setup::SetupProjectRequest {
            project_root,
            source_binary: None,
            dry_run: *dry_run,
            replace_managed: *replace_managed,
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
        Cmd::Status => {
            let mut st = conn.prepare("SELECT slug, title, status FROM plans ORDER BY plan_id")?;
            let plans: Vec<(String, String, String)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            if plans.is_empty() {
                println!("no plans; start with `papertiger plan add <slug> <title>`");
                return Ok(());
            }
            for (slug, title, pstatus) in &plans {
                if pstatus != "active" {
                    continue;
                }
                let (plan_id, _) = pt::resolve_plan(&conn, Some(slug))?;
                let counts: Vec<(String, i64)> = {
                    let mut st = conn.prepare(
                        "SELECT status, COUNT(*) FROM tasks WHERE plan_id=?1 GROUP BY status",
                    )?;
                    st.query_map(params![plan_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<rusqlite::Result<_>>()?
                };
                let summary = counts
                    .iter()
                    .map(|(s, n)| format!("{s} {n}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("plan {slug}: {title} [{summary}]");
                let in_prog = pt::leaf_tasks_with_status(&conn, plan_id, "in_progress")?;
                for t in &in_prog {
                    print_task_line(&conn, t)?;
                }
                for e in pt::next(&conn, plan_id, 5, false)? {
                    print_task_line(&conn, &e.task)?;
                }
            }
            let mut st = conn.prepare(
                "SELECT at, actor, why FROM events WHERE kind='note' ORDER BY event_id DESC LIMIT 3",
            )?;
            let notes: Vec<(String, String, Option<String>)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (at, actor, why) in notes {
                println!(
                    "note {} {}: {}",
                    short_event_time(&at),
                    actor,
                    why.unwrap_or_default()
                );
            }
        }
        Cmd::Plan { cmd } => match cmd {
            PlanCmd::Add {
                slug,
                title,
                intent,
            } => {
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
                pt::set_plan_status(&conn, &actor, &slug, &status, &why)?;
                println!("plan {slug} -> {status}");
            }
        },
        Cmd::Add {
            title,
            plan,
            intent,
            intent_file,
            kind,
            parent,
            dep,
            tag,
            priority,
            alias,
            why,
        } => {
            let intent = read_intent(intent, intent_file)?;
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
                alias.as_deref(),
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
        Cmd::List { plan, status, tag } => {
            let (plan_id, _) = pt::resolve_plan(&conn, plan.as_deref())?;
            let tasks = pt::list_tasks(&conn, plan_id, status.as_deref(), tag.as_deref())?;
            for t in &tasks {
                print_task_line(&conn, t)?;
            }
        }
        Cmd::Edit {
            task,
            title,
            intent,
            intent_file,
            parent,
            clear_parent,
            kind,
            priority,
            why,
        } => {
            let intent = if intent.is_some() || intent_file.is_some() {
                Some(read_intent(intent, intent_file)?)
            } else {
                None
            };
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
            let seq = pt::parse_task_ref(&task)?;
            pt::start_task(&conn, &actor, seq, why.as_deref())?;
            println!("#{seq} in progress");
        }
        Cmd::Done { task, result } => {
            let seq = pt::parse_task_ref(&task)?;
            pt::complete_task(&conn, &actor, seq, result.as_deref())?;
            println!("#{seq} done");
        }
        Cmd::Reopen { task, why } => {
            if why.trim().is_empty() {
                bail!("reopening a task requires a nonblank reason");
            }
            let seq = pt::parse_task_ref(&task)?;
            pt::reopen_task(&conn, &actor, seq, &why)?;
            println!("#{seq} reopened");
        }
        Cmd::Retire { task, why } => {
            let seq = pt::parse_task_ref(&task)?;
            pt::retire_task(&conn, &actor, seq, &why)?;
            println!("#{seq} retired");
        }
        Cmd::Reject { task, why } => {
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
                let seq = pt::parse_task_ref(&task)?;
                pt::waive_gate(&conn, &actor, seq, &name, &why)?;
                println!("gate '{name}' on #{seq} waived");
            }
            GateCmd::Reopen { task, name, why } => {
                let seq = pt::parse_task_ref(&task)?;
                pt::reopen_gate(&conn, &actor, seq, &name, &why)?;
                println!("gate '{name}' on #{seq} reopened");
            }
            GateCmd::Rm { task, name, why } => {
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
                let seq = pt::parse_task_ref(&task)?;
                pt::waive_task_blocker(&conn, &actor, seq, &name, &why)?;
                println!("blocker '{name}' on #{seq} waived");
            }
            BlockerCmd::Rm { task, name, why } => {
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
        Cmd::Dep { cmd } => match cmd {
            DepCmd::Add { task, on, why } => {
                let (a, b) = (pt::parse_task_ref(&task)?, pt::parse_task_ref(&on)?);
                pt::add_dep(&conn, &actor, a, b, &why)?;
                println!("#{a} now depends on #{b}");
            }
            DepCmd::Rm { task, on, why } => {
                let (a, b) = (pt::parse_task_ref(&task)?, pt::parse_task_ref(&on)?);
                pt::remove_dep(&conn, &actor, a, b, &why)?;
                println!("#{a} no longer depends on #{b}");
            }
        },
        Cmd::Next { plan, limit, all } => {
            let (plan_id, _) = pt::resolve_plan(&conn, plan.as_deref())?;
            let entries = pt::next(&conn, plan_id, limit, all)?;
            if entries.is_empty() {
                println!("nothing ready");
            }
            for e in entries {
                if e.blockers.is_empty() {
                    print_task_line(&conn, &e.task)?;
                } else {
                    println!(
                        "blocked #{} {} [{}]",
                        e.task.seq,
                        e.task.title,
                        e.blockers.join(" ")
                    );
                }
            }
        }
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
                        "schema": "papertiger.focus.v3",
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
                        "schema": "papertiger.focus.v3",
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
                let seq = pt::parse_task_ref(&task)?;
                pt::add_tag(&conn, &actor, seq, &tag, &why)?;
                println!("tag '{tag}' added to #{seq}");
            }
            TagCmd::Rm { task, tag, why } => {
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
            let task_seq = task.map(|task| pt::parse_task_ref(&task)).transpose()?;
            pt::add_note(&conn, &actor, task_seq, &text)?;
            println!("noted");
        }
        Cmd::Log { task, limit } => {
            let (sql, task_seq): (String, Option<i64>) = match task {
                Some(t) => {
                    let seq = pt::parse_task_ref(&t)?;
                    pt::get_task(&conn, seq)?;
                    (
                        "SELECT at, actor, entity, kind, why, payload FROM events
                         WHERE entity_seq=?1 AND entity IN ('task','dep','gate')
                         ORDER BY event_id DESC LIMIT ?2"
                            .into(),
                        Some(seq),
                    )
                }
                None => (
                    "SELECT at, actor, entity, kind, why, payload FROM events
                     ORDER BY event_id DESC LIMIT ?1"
                        .into(),
                    None,
                ),
            };
            let mut st = conn.prepare(&sql)?;
            let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<EventRow> {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            };
            let rows: Vec<_> = match task_seq {
                Some(seq) => st
                    .query_map(params![seq, limit as i64], map)?
                    .collect::<rusqlite::Result<_>>()?,
                None => st
                    .query_map(params![limit as i64], map)?
                    .collect::<rusqlite::Result<_>>()?,
            };
            for (at, actor, entity, kind, why, payload) in rows {
                let why = why.map(|w| format!(" — {w}")).unwrap_or_default();
                let payload = payload.map(|p| format!(" {p}")).unwrap_or_default();
                println!(
                    "{} {} {}/{}{}{}",
                    short_event_time(&at),
                    actor,
                    entity,
                    kind,
                    payload,
                    why
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
        Cmd::Export { plan } => {
            let dump = pt::export(&conn, plan.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&dump)?);
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
