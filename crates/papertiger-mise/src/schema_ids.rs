//! Schema-identifier refusals. Every Mise-owned schema, protocol, and
//! domain-separation identifier is `papertiger-mise.<snake_case>.v<N>`, and
//! each reader accepts exactly the identifiers it implements. Any other value
//! is refused with the expected identifier and a corrective action; no reader
//! converts a document between identifiers.

use anyhow::anyhow;

/// Corrective action for stored evidence whose schema no current reader
/// implements. Stored evidence is never rewritten in place.
pub(crate) const STORED_EVIDENCE_REMEDY: &str = "stored evidence is never rewritten; restore the authority from verified recovery evidence, or record new evidence under the expected schema";

/// Refusal for a document whose schema is not the one this reader implements.
pub(crate) fn schema_refusal(
    document: &str,
    found: &str,
    expected: &str,
    remedy: &str,
) -> anyhow::Error {
    anyhow!("unsupported {document} schema '{found}'; expected '{expected}'; {remedy}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNKNOWN: &str = "papertiger-mise.unknown_document.v1";

    #[test]
    fn unsupported_stored_manifest_names_expected_schema() {
        let mut manifest = crate::manifest::tests::valid_manifest();
        manifest.schema = UNKNOWN.to_owned();
        let stored = serde_json::to_string(&manifest).expect("manifest JSON");
        let error = crate::manifest::CampaignManifest::from_stored_json(&stored)
            .expect_err("unsupported stored manifest must be refused")
            .to_string();
        assert!(
            error.contains(&format!(
                "unsupported stored campaign manifest schema '{UNKNOWN}'; expected 'papertiger-mise.campaign.v4'"
            )),
            "{error}"
        );
        assert!(error.contains(STORED_EVIDENCE_REMEDY), "{error}");

        let error = manifest
            .validate()
            .expect_err("unsupported authored manifest must be refused")
            .to_string();
        assert!(
            error.contains("expected 'papertiger-mise.campaign.v4'"),
            "{error}"
        );
        assert!(
            error.contains("author a campaign manifest with schema papertiger-mise.campaign.v4"),
            "{error}"
        );
    }

    #[test]
    fn unsupported_containment_policy_names_expected_schema() {
        let policy = crate::attestation::ContainmentPolicy {
            schema: UNKNOWN.to_owned(),
            protocol: crate::attestation::SEALED_ATTESTATION_PROTOCOL_V3.to_owned(),
            issuer_identity: "operator".to_owned(),
            public_key_ed25519: "0".repeat(64),
            executor_sha256: "0".repeat(64),
            profile_sha256: "0".repeat(64),
        };
        let error = policy
            .validate()
            .expect_err("unsupported policy")
            .to_string();
        assert!(
            error.contains("expected 'papertiger-mise.containment_policy.v3'"),
            "{error}"
        );
        assert!(
            error.contains("reissue the operator-owned containment policy"),
            "{error}"
        );
    }

    #[test]
    fn unsupported_brief_names_expected_schema() {
        let error = crate::improvement::validate_project_improvement_brief(
            format!(r#"{{"schema":"{UNKNOWN}"}}"#).as_bytes(),
        )
        .expect_err("unsupported brief")
        .to_string();
        assert!(
            error.contains("expected 'papertiger-mise.project_improvement_brief.v3'"),
            "{error}"
        );
    }

    #[test]
    fn refusal_names_found_expected_and_remedy() {
        let error = schema_refusal(
            "containment policy",
            UNKNOWN,
            "papertiger-mise.containment_policy.v3",
            "reissue the policy",
        )
        .to_string();
        assert_eq!(
            error,
            format!(
                "unsupported containment policy schema '{UNKNOWN}'; expected 'papertiger-mise.containment_policy.v3'; reissue the policy"
            )
        );
    }
}
