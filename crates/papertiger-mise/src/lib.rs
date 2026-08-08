//! Independent campaign authority for project-generic recursive improvement.
//!
//! Papertiger owns planning state. This crate deliberately owns a separate
//! SQLite authority for immutable campaign admission, resource budgets,
//! executions, and evidence.

pub mod adapter;
pub mod admission;
pub mod attestation;
pub mod budget;
pub mod candidate;
pub mod classification;
mod digest;
pub mod domain_shadow;
mod executor;
mod git_materialization;
pub mod improvement;
mod lifecycle;
pub mod manifest;
pub mod object;
pub mod paired_evidence;
pub mod paired_runtime;
mod path_identity;
pub mod planner_projection;
mod process_identity;
pub mod promotion;
pub mod state;
pub mod statistics;
pub mod store;
pub mod successor;
mod validation;

pub use attestation::{
    SEALED_ATTESTATION_PROTOCOL_V2, SEALED_ATTESTATION_SCHEMA_V2, SealedAttestationPayload,
    SignedSealedAttestation, TRUSTED_CONTAINMENT_POLICY_SCHEMA_V2, TrustedContainmentPolicy,
    record_sealed_attestation,
};

pub use adapter::{
    DomainBlockMeasurement, DomainObjectiveMeasurement, DomainObservationResult, DomainParticipant,
    DomainSessionEvidence, DomainTrialMeasurement, DomainTrialResult,
    PAIRED_ADAPTER_BINDING_SCHEMA_V1, PAIRED_TRIAL_REQUEST_SCHEMA_V2, PairedAdapterBinding,
    PairedAdapterCohort, PairedExecutionParticipant, PairedExecutionParticipants,
    PairedParticipantRole, PairedTrialObjective, PairedTrialRequest, VerifiedAdapterResult,
    execute_paired_adapter, execute_paired_adapter_cohort,
};
pub use admission::{
    CAMPAIGN_PREFLIGHT_SCHEMA_V1, CampaignPreflightDefect, CampaignPreflightReport,
    FIXTURE_BUNDLE_SCHEMA_V1, FixtureBundleDescriptor, FixtureBundleEntry,
    VerifiedCampaignAdmission, admit_verified_campaign, inspect_source_binding,
    preflight_campaign_admission, verify_campaign_admission,
};
pub use budget::{
    BudgetAmount, BudgetBalance, BudgetLimit, BudgetRequest, BudgetResource, BudgetSettlement,
    ReservationOutcome, SettlementMode, SettlementOutcome, budget_balances, reserve_budget,
    settle_budget,
};
pub use candidate::{
    BoundCandidate, CandidateDisposition, CandidateMaterial, CandidateMaterialFormat,
    CandidateProposal, GIT_CHANGE_SET_MEDIA_TYPE, GIT_CHANGE_SET_PROTOCOL_V1, GitChange,
    GitChangeOperation, GitChangeSet, GitChangeSetScope, GitFileContent, GitFileIdentity,
    GitFileMode, Hypothesis, bind_candidate,
};
pub use classification::{
    Classification, DeterministicObservation, ObjectiveResult, classify_deterministic,
};
pub use digest::{sha256, validate_sha256};
pub use domain_shadow::{
    DOMAIN_SHADOW_ADAPTER_BINDING_SCHEMA_V1, DOMAIN_SHADOW_RECEIPT_SCHEMA_V1,
    DomainShadowAdapterBinding, DomainShadowOutcome, DomainShadowReceipt, DomainShadowRecord,
    DomainShadowResult, DomainShadowState, domain_shadow, record_domain_shadow,
};
pub use executor::{
    ExecutionCapabilities, HOST_EXECUTION_STATUS_SCHEMA_V1, HostExecutionStatus,
    PORTABLE_LOCAL_SUPERVISION_CONTRACT_V1, host_execution_status,
};
pub use git_materialization::build_git_change_set_material;
pub use lifecycle::{
    CandidateRecord, ColdRecoveryOutcome, DeterministicEvaluatorOutput,
    DeterministicEvaluatorRequest, EvaluatorJudgeBuild, JudgeBuildReceipt, MaterializationReceipt,
    MaterializationRecord, NominationRecord, SupervisedTrialOutcome, SupervisedTrialSpec,
    TrialReceipt, TrialRecord, VerifiedCandidateEvidence, VerifiedNominationEvidence,
    abandon_materialization_attempt, abandon_owned_trial, adjudicate_deterministic_candidate,
    candidate, execute_workspace_trial, materialize_candidate, negative_fingerprint_candidates,
    nominations, record_candidate, recover_workspace_trial, trial, verify_candidate_integrity,
    verify_nomination_integrity,
};
pub use object::{PreservedObject, object_locator, preserve_object, read_object, verify_object};
pub use paired_evidence::{
    HISTORICAL_SHADOW_RECEIPT_SCHEMA_V1, HistoricalBlockBinding, HistoricalShadowReceipt,
    HistoricalShadowRecord, ObservedRunOrder, PairedEvidenceOutcome, historical_shadow,
    record_historical_shadow,
};
pub use paired_runtime::{
    DerivePairedNominationSpec, PAIRED_COHORT_RECEIPT_SCHEMA_V1,
    PAIRED_EXECUTION_RECEIPT_SCHEMA_V1, PAIRED_NOMINATION_RECEIPT_SCHEMA_V1,
    PairedCohortAdjudication, PairedCohortReceipt, PairedCohortRecord, PairedPreparationOutcome,
    PairedRunOutcome, PairedRunRecord, PreparePairedCohortSpec, VerifiedPairedCohortEvidence,
    adjudicate_paired_cohort, derive_paired_nomination, execute_next_paired_run, paired_cohort,
    paired_cohorts, paired_run, paired_runs, prepare_paired_cohort, recover_paired_run,
    verify_paired_cohort_integrity,
};
pub use path_identity::portable_absolute;
pub use planner_projection::{
    derive_candidate_planner_projection, derive_nomination_planner_projection,
};
pub use promotion::{
    PromotionEvidenceDigest, PromotionGateBinding, PromotionProof, VerifiedPromotionGate,
    derive_promotion_proof, verify_promotion_gate,
};
pub use state::{
    BudgetReservationStatus, EvidenceGrade, PairedCohortReasonCode, PairedCohortStatus,
    PairedRunStatus, TrialStatus,
};
pub use statistics::{
    ExactPValue, MedianOrderStatistics, NoOpCalibrationResult, PAIRED_ANALYSIS_SCHEMA_V1,
    PAIRED_ANALYSIS_SCHEMA_V2, PAIRED_MEASUREMENT_PROTOCOL_V1, PairedAnalysisMethod,
    PairedAnalysisPlan, PairedAnalysisSlotRecord, PairedBlockDesign, PairedBlockObservation,
    PairedCalibrationFixtureBindings, PairedCandidateContext, PairedClassification, PairedCohort,
    PairedDisposition, PairedFixtureBinding, PairedHypothesisKind, PairedHypothesisResult,
    PairedObjectiveObservation, PairedObjectivePolicy, PairedObjectiveResult, PairedRunOrder,
    PairedSlotSeedCommitment, RationalThreshold, assess_known_bad_cohort, assess_no_op_cohort,
    classify_paired_fixed, paired_analysis_slot, paired_observations_sha256, paired_run_order,
    paired_schedule_sha256, reserve_paired_analysis_slot,
};
pub use store::{
    AdmissionOutcome, AuthorityStatus, CampaignRecord, CampaignSummary, EventRecord,
    SCHEMA_VERSION, SuccessorAdmissionRecord, append_campaign_event, authority_status, campaign,
    campaign_events, init, open_existing, open_existing_read_only, open_for_init,
    successor_admission,
};
pub use successor::{
    PARENT_PROMOTION_PROOF_SCHEMA_V1, PARENT_PROMOTION_PROOF_SCHEMA_V2, ParentPromotionProof,
    PreservedParentPromotionProof, SUCCESSOR_ADMISSION_SCOPE_V1, VerifiedParentPromotionGate,
    VerifiedSuccessorAdmission, admit_verified_successor, derive_parent_promotion_proof,
    preserve_parent_promotion_proof, verify_parent_promotion_gate, verify_successor_admission,
};
