pub mod authorship;
mod digest;
pub mod process_identity;
pub use authorship::{
    Authorship, AuthorshipFlags, HUMAN_MODEL, ModelSource, RUNTIME_MODEL, resolve_authorship,
    resolve_authorship_from_env, set_authorship, validate_model, validate_reasoning_effort,
};
mod stdout;
pub use digest::{sha256, sha256_bytes, validate_sha256};
pub use stdout::{OutputClosed, is_output_closed, print_stdout, write_stdout};
mod atomic_file;
pub use atomic_file::{atomic_create_file, atomic_replace_file};
mod export_file;
pub use export_file::{ExportFileReceipt, write_export_file};
mod backup;
pub use backup::{BackupReceipt, backup_authority};
mod evidence;
pub use evidence::{
    CorrectiveCommand, EvidenceBindingVerification, EvidenceClassification,
    validate_new_file_evidence,
};
mod evidence_projection;
pub use evidence_projection::{
    DEFAULT_EVIDENCE_PAGE, EvidenceClassificationFilter, EvidenceTaskStateFilter,
    EvidenceVerificationOptions, EvidenceVerificationProjection, EvidenceVerificationReport,
    EvidenceVerificationSummary, MAX_EVIDENCE_PAGE, verify_evidence,
};
mod path_identity;
pub use path_identity::portable_absolute;
pub mod history_recovery;
mod inventory;
mod mutation;
mod pickup;
mod read_model;
mod write_guard;
pub use inventory::{plan_inventory, task_inventory};
pub use mutation::{MutationEvent, MutationReceipt, MutationRecorder};
pub use pickup::{
    OwnerState, PickupOwner, TaskPickup, host_name, observe_owner, owner_state, validate_session,
};
pub use write_guard::{GuardDrift, GuardDriftState};
mod plan_move;
pub use plan_move::move_tasks_to_plan;
mod task_graph;
mod task_outline;
pub use read_model::{
    ActivityEvent, AuthorityInfo, EventCursor, EventLog, EventRecord, PlanIdentity, PlanStatus,
    StatusInProgress, StatusProjection, StatusReadyTask, StatusResponse, StatusTask, TaskActivity,
    TaskCounts, TaskListItem, TaskListResponse, TaskSummary, authority_info, event_cursor,
    event_head, event_log, status_response, task_activity, task_list_response,
};
pub use task_outline::{
    MAX_OUTLINE_BYTES, MAX_OUTLINE_CHILDREN, OutlineChild, OutlineEntry, TASK_OUTLINE_SCHEMA,
    TaskOutline, decompose_task, parse_task_outline,
};
mod search;
pub use search::{SearchExcerpt, SearchHit, SearchResponse, search_tasks};
mod commit_association;
mod external_reference;
pub use commit_association::{
    CommitAssociation, CommitAssociationMatch, add_commit_association, commit_associations,
    find_commit_associations, remove_commit_association,
};
pub use external_reference::{
    ExternalReference, REFERENCE_KINDS, ReferenceMatch, add_external_reference,
    external_references, find_external_references, new_external_reference,
    remove_external_reference,
};
mod mise_projection;
mod mise_projection_contract;
pub use mise_projection::{
    MiseProjectionDump, MiseProjectionRecordOutcome, TaskMiseProjection, TaskMiseProjectionSummary,
    mise_projection, parse_mise_planner_projection, record_mise_projection,
    task_mise_projection_summaries, task_mise_projections,
};
pub use mise_projection_contract::{
    MISE_PLANNER_PROJECTION_SCHEMA, MISE_PLANNER_PROJECTION_SCHEMA_V1, MiseBudgetProjection,
    MiseMutationProjection, MisePlannerProjection, MisePlannerProjectionSummary,
    MiseProjectionDisposition, MiseSourceProjection,
};

pub const SCHEMA_VERSION: i64 = 14;
pub const AUTHORITY_IDENTITY: &str = "papertiger.planner";
const AUTHORITY_IDENTITY_KEY: &str = "authority";
pub const TASK_DEFINITION_REVISION_SCHEMA: &str = "papertiger.task_definition_revision.v1";
pub const MAX_TASK_TITLE_CHARS: usize = 160;
pub const MAX_TAG_CHARS: usize = 64;
const SQLITE_LOCK_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

pub const TASK_STATUSES: [&str; 5] = ["proposed", "in_progress", "done", "retired", "rejected"];
/// What sort of outcome a task is. Each kind has the letter its handle shows;
/// `work` is the legacy kind of tasks recorded before typed kinds.
pub const TASK_KINDS: [&str; 8] = [
    "goal", "feature", "change", "bug", "probe", "decision", "release", "work",
];
/// Kinds a task can be given; `work` survives only on existing records.
pub const ASSIGNABLE_TASK_KINDS: [&str; 7] = [
    "goal", "feature", "change", "bug", "probe", "decision", "release",
];
pub const LEGACY_TASK_KIND: &str = "work";
/// Sources older events recorded for intent, result and note text; only
/// history validation reads them now.
pub const MEANING_SOURCES: [&str; 3] = ["user", "agent", "external"];
/// Evidence a gate requires. An `external` gate is a condition outside the
/// task, such as a vendor fix, and keeps the task from starting while open.
pub const GATE_KINDS: [&str; 9] = [
    "test",
    "benchmark",
    "review",
    "capture",
    "fixture",
    "build",
    "doc",
    "external",
    "other",
];

pub fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

mod authority;
pub use authority::{
    InitOutcome, begin_mutation, normalize_sqlite_lock_error, open_existing,
    open_existing_for_guard_repair, open_existing_read_only, open_for_init, repair_write_guards,
    verify_write_guards, write_guard_drift,
};
pub(crate) use authority::{
    authority_identity, configure_connection, has_table, require_planner_identity, schema_version,
};
mod schema;
pub use schema::{init, init_at};
mod events;
pub use events::add_note;
pub(crate) use events::record_event_in_mutation;
mod tasks;
pub use tasks::{
    Priority, Task, get_task, kind_requires_result, leaf_tasks_with_status, list_tasks,
    list_tasks_by_activity, resolve_task_ref, suggested_task_kind, task_handle, task_kind_letter,
    validate_meaning_source,
};
pub(crate) use tasks::{
    TASK_COLS, parse_task_ref, require_nonblank, task_from_row, task_tags_by_id,
    validate_assignable_task_kind, validate_kind_filter, validate_tag, validate_task_kind,
    validate_task_title,
};
mod plans;
pub(crate) use plans::plan_status;
pub use plans::{Plan, active_plan, add_plan, edit_plan, get_plan, resolve_plan, set_plan_status};
mod task_definition;
pub use task_definition::{
    PlanSelector, TaskCreation, TaskEdit, add_dep, add_tag, add_task, edit_task, remove_dep,
    remove_tag,
};
pub(crate) use task_definition::{
    add_dep_inner, add_task_in_mutation, valid_task_definition_revision_payload,
};
mod focus;
pub use focus::{FocusEntry, FocusResponse, ReadyEntry, focus, ready_tasks};
pub(crate) use focus::{entry_blockers, immediate_unlock_count, unfinished_downstream_count};
mod lifecycle;
pub use lifecycle::{complete_task, reject_task, reopen_task, retire_task, start_task};
pub(crate) use lifecycle::{live_child_sequences, open_gates, start_task_in_mutation};
mod gates;
pub use gates::{
    add_gate, remove_gate, reopen_gate, resolve_gate, validate_commit_oid, waive_gate,
};
pub(crate) use gates::{
    validate_evidence_locator, validate_new_evidence_locator, validate_optional_sha256,
};
pub mod model_report;
pub use model_report::{ModelOutcomes, ModelReport, model_report};
pub mod review;
pub use review::{
    ClaimReview, ReviewState, ReviewTarget, ReviewVerdict, TaskReview, normalized_model,
    review_task, task_review,
};
mod task_context;
pub use task_context::{
    GateRecord, TaskContext, open_deps, task_ancestry, task_context, task_current,
};
mod audit;
pub use audit::{AuditFinding, audit};
pub(crate) use audit::{
    find_cycle, invalid_replacement_terminals, replacement_repair_instruction,
    terminal_replacement_repair_instruction,
};
mod transfer;
pub use transfer::{
    DUMP_SCHEMA, Dump, EventDump, GateDump, PlanDump, TaskDump, convert_dump_v10, export, import,
    parse_dump_json,
};
