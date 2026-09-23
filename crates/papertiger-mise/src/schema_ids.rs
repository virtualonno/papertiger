//! The 0.18.0 schema-id cutover boundary. Every Mise-owned schema, protocol,
//! and domain-separation identifier is `papertiger-mise.<snake_case>.v<N>`.
//! Retired identifiers are refused with a corrective action; no reader converts
//! them, because admitted campaigns and their CAS evidence are frozen.

use anyhow::anyhow;

/// Every identifier retired by the cutover, paired with its replacement.
pub const RETIRED_SCHEMA_IDS: &[(&str, &str)] = &[
    ("papertiger-mise.adapter.v1", "papertiger-mise.adapter.v2"),
    (
        "papertiger-mise.authority-status.v1",
        "papertiger-mise.authority_status.v2",
    ),
    (
        "papertiger-mise.campaign-inspection.v1",
        "papertiger-mise.campaign_inspection.v2",
    ),
    (
        "papertiger-mise.campaign-preflight.v1",
        "papertiger-mise.campaign_preflight.v2",
    ),
    ("papertiger-mise.campaign.v1", "papertiger-mise.campaign.v3"),
    ("papertiger-mise.campaign.v2", "papertiger-mise.campaign.v4"),
    (
        "papertiger-mise.candidate-identity.v1",
        "papertiger-mise.candidate_identity.v2",
    ),
    (
        "papertiger-mise.candidate-identity.v2",
        "papertiger-mise.candidate_identity.v3",
    ),
    (
        "papertiger-mise.candidate-material.v1",
        "papertiger-mise.candidate_material.v2",
    ),
    (
        "papertiger-mise.candidate-result.v1",
        "papertiger-mise.candidate_result.v2",
    ),
    (
        "papertiger-mise.deterministic-evaluator-output.v1",
        "papertiger-mise.deterministic_evaluator_output.v2",
    ),
    (
        "papertiger-mise.deterministic-evaluator-output.v2",
        "papertiger-mise.deterministic_evaluator_output.v3",
    ),
    (
        "papertiger-mise.deterministic-evaluator-request.v1",
        "papertiger-mise.deterministic_evaluator_request.v2",
    ),
    (
        "papertiger-mise.deterministic-evaluator-request.v2",
        "papertiger-mise.deterministic_evaluator_request.v3",
    ),
    (
        "papertiger-mise.dogfood-adapter.v1",
        "papertiger-mise.dogfood_adapter.v2",
    ),
    (
        "papertiger-mise.domain-shadow-adapter-binding.v1",
        "papertiger-mise.domain_shadow_adapter_binding.v2",
    ),
    (
        "papertiger-mise.domain-shadow-fixture-request.v1",
        "papertiger-mise.domain_shadow_fixture_request.v2",
    ),
    (
        "papertiger-mise.domain-shadow-fixture-result.v1",
        "papertiger-mise.domain_shadow_fixture_result.v2",
    ),
    (
        "papertiger-mise.domain-shadow-receipt.v1",
        "papertiger-mise.domain_shadow_receipt.v2",
    ),
    (
        "papertiger-mise.ed25519-sealed.v2",
        "papertiger-mise.ed25519_sealed.v3",
    ),
    (
        "papertiger-mise.fixture-bundle.v1",
        "papertiger-mise.fixture_bundle.v2",
    ),
    (
        "papertiger-mise.git-change-set.v1",
        "papertiger-mise.git_change_set.v2",
    ),
    (
        "papertiger-mise.git-repository.v1",
        "papertiger-mise.git_repository.v2",
    ),
    (
        "papertiger-mise.historical-shadow-receipt.v1",
        "papertiger-mise.historical_shadow_receipt.v2",
    ),
    (
        "papertiger-mise.host-execution-status.v1",
        "papertiger-mise.host_execution_status.v2",
    ),
    (
        "papertiger-mise.integrity-failure.v2",
        "papertiger-mise.integrity_failure.v3",
    ),
    (
        "papertiger-mise.judge-build-receipt.v1",
        "papertiger-mise.judge_build_receipt.v2",
    ),
    (
        "papertiger-mise.materialization.v1",
        "papertiger-mise.materialization.v3",
    ),
    (
        "papertiger-mise.materialization.v2",
        "papertiger-mise.materialization.v4",
    ),
    (
        "papertiger-mise.measurement-contract.v1",
        "papertiger-mise.measurement_contract.v2",
    ),
    (
        "papertiger-mise.measurement-sample.v1",
        "papertiger-mise.measurement_sample.v2",
    ),
    (
        "papertiger-mise.measurement.v1",
        "papertiger-mise.measurement.v2",
    ),
    (
        "papertiger-mise.negative-fingerprint.v1",
        "papertiger-mise.negative_fingerprint.v2",
    ),
    (
        "papertiger-mise.nomination.v1",
        "papertiger-mise.nomination.v2",
    ),
    (
        "papertiger-mise.order-seed-protocol.v1",
        "papertiger-mise.order_seed_protocol.v2",
    ),
    (
        "papertiger-mise.os-process-observer.v1",
        "papertiger-mise.os_process_observer.v2",
    ),
    (
        "papertiger-mise.paired-adapter-binding.v1",
        "papertiger-mise.paired_adapter_binding.v2",
    ),
    (
        "papertiger-mise.paired-analysis.v1",
        "papertiger-mise.paired_analysis.v2",
    ),
    (
        "papertiger-mise.paired-analysis.v2",
        "papertiger-mise.paired_analysis.v3",
    ),
    (
        "papertiger-mise.paired-candidate-result.v1",
        "papertiger-mise.paired_candidate_result.v2",
    ),
    (
        "papertiger-mise.paired-cohort-failure-receipt.v1",
        "papertiger-mise.paired_cohort_failure_receipt.v2",
    ),
    (
        "papertiger-mise.paired-cohort-receipt.v1",
        "papertiger-mise.paired_cohort_receipt.v2",
    ),
    (
        "papertiger-mise.paired-execution-failure-receipt.v1",
        "papertiger-mise.paired_execution_failure_receipt.v2",
    ),
    (
        "papertiger-mise.paired-execution-receipt.v1",
        "papertiger-mise.paired_execution_receipt.v2",
    ),
    (
        "papertiger-mise.paired-measurement.v1",
        "papertiger-mise.paired_measurement.v2",
    ),
    (
        "papertiger-mise.paired-nomination.v1",
        "papertiger-mise.paired_nomination.v2",
    ),
    (
        "papertiger-mise.paired-observations.v1",
        "papertiger-mise.paired_observations.v2",
    ),
    (
        "papertiger-mise.paired-order.v1",
        "papertiger-mise.paired_order.v2",
    ),
    (
        "papertiger-mise.paired-plan-identity.v1",
        "papertiger-mise.paired_plan_identity.v2",
    ),
    (
        "papertiger-mise.paired-schedule.v1",
        "papertiger-mise.paired_schedule.v2",
    ),
    (
        "papertiger-mise.paired-trial-request.v2",
        "papertiger-mise.paired_trial_request.v3",
    ),
    (
        "papertiger-mise.paired-trial-request.v3",
        "papertiger-mise.paired_trial_request.v4",
    ),
    (
        "papertiger-mise.paired-trial.v1",
        "papertiger-mise.paired_trial.v2",
    ),
    (
        "papertiger-mise.parent-promotion-proof.v1",
        "papertiger-mise.parent_promotion_proof.v2",
    ),
    (
        "papertiger-mise.parent-promotion-proof.v2",
        "papertiger-mise.parent_promotion_proof.v3",
    ),
    (
        "papertiger-mise.portable-local-supervision.v1",
        "papertiger-mise.portable_local_supervision.v2",
    ),
    (
        "papertiger-mise.process-absence-evidence.v1",
        "papertiger-mise.process_absence_evidence.v2",
    ),
    (
        "papertiger-mise.project-status.v2",
        "papertiger-mise.project_status.v3",
    ),
    (
        "papertiger-mise.promotion-proof.v1",
        "papertiger-mise.promotion_proof.v2",
    ),
    (
        "papertiger-mise.proposal-policy.v1",
        "papertiger-mise.proposal_policy.v2",
    ),
    (
        "papertiger-mise.runtime-input-integrity.v2",
        "papertiger-mise.runtime_input_integrity.v3",
    ),
    (
        "papertiger-mise.sealed-attestation.v2",
        "papertiger-mise.sealed_attestation.v3",
    ),
    (
        "papertiger-mise.trial-abandonment.v1",
        "papertiger-mise.trial_abandonment.v2",
    ),
    (
        "papertiger-mise.trial-outcome.v1",
        "papertiger-mise.trial_outcome.v2",
    ),
    (
        "papertiger-mise.trial-path.v1",
        "papertiger-mise.trial_path.v2",
    ),
    (
        "papertiger-mise.trial-receipt.v1",
        "papertiger-mise.trial_receipt.v2",
    ),
    (
        "papertiger-mise.trial-receipt.v2",
        "papertiger-mise.trial_receipt.v3",
    ),
    (
        "papertiger-mise.trial-receipt.v3",
        "papertiger-mise.trial_receipt.v4",
    ),
    (
        "papertiger-mise.trial-receipt.v4",
        "papertiger-mise.trial_receipt.v5",
    ),
    (
        "papertiger-mise.trusted-containment-policy.v2",
        "papertiger-mise.containment_policy.v3",
    ),
    (
        "papertiger-mise.workspace-owner.v1",
        "papertiger-mise.workspace_owner.v2",
    ),
    (
        "papertiger-mise.workspace-supervisor-failure.v1",
        "papertiger-mise.workspace_supervisor_failure.v2",
    ),
    (
        "papertiger-mise.workspace-supervisor.v1",
        "papertiger-mise.workspace_supervisor.v2",
    ),
    (
        "papertiger.compiled-improvement-draft.v2",
        "papertiger-mise.compiled_improvement_draft.v3",
    ),
    (
        "papertiger.improvement-paradigm-registry.v1",
        "papertiger-mise.improvement_paradigm_registry.v2",
    ),
    (
        "papertiger.improvement-paradigm-template.v1",
        "papertiger-mise.improvement_paradigm_template.v2",
    ),
    (
        "papertiger.project-improvement-brief-approval.v1",
        "papertiger-mise.project_improvement_brief_approval.v2",
    ),
    (
        "papertiger.project-improvement-brief.v2",
        "papertiger-mise.project_improvement_brief.v3",
    ),
];

/// The replacement for an identifier retired by the 0.18.0 cutover, if any.
pub fn replacement_for_retired_schema(id: &str) -> Option<&'static str> {
    RETIRED_SCHEMA_IDS
        .iter()
        .find(|(retired, _)| *retired == id)
        .map(|(_, current)| *current)
}

/// Corrective action for evidence recorded by a pre-0.18.0 runtime.
pub(crate) const FROZEN_EVIDENCE_REMEDY: &str = "evidence recorded before papertiger-mise 0.18.0 is frozen and is never rewritten: reopen it with a papertiger-mise 0.17.x binary, or admit a new campaign under papertiger-mise.campaign.v4";

/// Refusal for a document whose schema is not the one current reader.
/// A retired identifier names its replacement and `remedy`; any other value is
/// reported as unsupported.
pub(crate) fn schema_refusal(
    document: &str,
    found: &str,
    expected: &str,
    remedy: &str,
) -> anyhow::Error {
    match replacement_for_retired_schema(found) {
        Some(current) => anyhow!(
            "{document} schema '{found}' was retired by papertiger-mise 0.18.0 (replacement '{current}'); {remedy}"
        ),
        None => anyhow!("unsupported {document} schema '{found}'; expected '{expected}'; {remedy}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_ids_never_collide_with_current_ids() {
        for (retired, current) in RETIRED_SCHEMA_IDS {
            assert!(current.starts_with("papertiger-mise."), "{current}");
            let name = current.split('.').nth(1).expect("schema name");
            assert!(!name.contains('-'), "{current} must be snake_case");
            assert!(
                replacement_for_retired_schema(current).is_none(),
                "{current} is both retired and current (from {retired})"
            );
        }
    }

    #[test]
    fn stored_pre_cutover_manifest_is_refused_as_frozen_evidence() {
        let mut manifest = crate::manifest::tests::valid_manifest();
        manifest.schema = "papertiger-mise.campaign.v2".to_owned();
        let stored = serde_json::to_string(&manifest).expect("manifest JSON");
        let error = crate::manifest::CampaignManifest::from_stored_json(&stored)
            .expect_err("pre-0.18 stored manifest must be refused")
            .to_string();
        assert!(
            error.contains("'papertiger-mise.campaign.v2' was retired"),
            "{error}"
        );
        assert!(error.contains("papertiger-mise.campaign.v4"), "{error}");
        assert!(error.contains("papertiger-mise 0.17.x"), "{error}");

        let error = manifest
            .validate()
            .expect_err("pre-0.18 authored manifest must be refused")
            .to_string();
        assert!(error.contains("author a new campaign manifest"), "{error}");
    }

    #[test]
    fn pre_cutover_containment_policy_must_be_reissued() {
        let policy = crate::attestation::ContainmentPolicy {
            schema: "papertiger-mise.trusted-containment-policy.v2".to_owned(),
            protocol: crate::attestation::SEALED_ATTESTATION_PROTOCOL_V3.to_owned(),
            issuer_identity: "operator".to_owned(),
            public_key_ed25519: "0".repeat(64),
            executor_sha256: "0".repeat(64),
            profile_sha256: "0".repeat(64),
        };
        let error = policy.validate().expect_err("retired policy").to_string();
        assert!(
            error.contains("reissue the operator-owned containment policy"),
            "{error}"
        );
        assert!(
            error.contains("papertiger-mise.containment_policy.v3"),
            "{error}"
        );
    }

    #[test]
    fn pre_cutover_brief_names_its_replacement() {
        let error = crate::improvement::validate_project_improvement_brief(
            br#"{"schema":"papertiger.project-improvement-brief.v2"}"#,
        )
        .expect_err("retired brief")
        .to_string();
        assert!(
            error.contains("replacement 'papertiger-mise.project_improvement_brief.v3'"),
            "{error}"
        );
    }

    #[test]
    fn retired_refusal_names_replacement_and_remedy() {
        let error = schema_refusal(
            "containment policy",
            "papertiger-mise.trusted-containment-policy.v2",
            "papertiger-mise.containment_policy.v3",
            "reissue the policy",
        )
        .to_string();
        assert!(
            error.contains("retired by papertiger-mise 0.18.0"),
            "{error}"
        );
        assert!(
            error.contains("papertiger-mise.containment_policy.v3"),
            "{error}"
        );
        assert!(error.contains("reissue the policy"), "{error}");
    }
}
