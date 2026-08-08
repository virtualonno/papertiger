//! Typed vocabulary for durable Mise state-machine columns and reason codes.

use std::fmt::{Display, Formatter};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

macro_rules! stored_vocabulary {
    ($name:ident { $($variant:ident => $value:literal $(| $alias:literal)*),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[serde(alias = $alias)])* $variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }

            pub fn parse_column(column: &str, value: &str) -> Result<Self> {
                match value {
                    $($value $(| $alias)* => Ok(Self::$variant)),+,
                    _ => bail!("unknown stored {column} value '{value}'"),
                }
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

stored_vocabulary!(TrialStatus {
    Owned => "owned",
    Launched => "launched",
    Succeeded => "succeeded",
    Rejected => "rejected",
    InfrastructureFailed => "infrastructure_failed",
    IntegrityFailed => "integrity_failed",
});

impl TrialStatus {
    pub const fn has_live_ownership(self) -> bool {
        matches!(self, Self::Owned | Self::Launched)
    }
}

stored_vocabulary!(PairedCohortStatus {
    Prepared => "prepared",
    Running => "running",
    Qualified => "qualified",
    Rejected => "rejected",
    Inconclusive => "inconclusive",
    Calibrated => "calibrated",
    InfrastructureFailed => "infrastructure_failed",
    IntegrityFailed => "integrity_failed",
});

impl PairedCohortStatus {
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Prepared | Self::Running)
    }

    pub const fn is_successful_terminal(self) -> bool {
        matches!(
            self,
            Self::Qualified | Self::Rejected | Self::Inconclusive | Self::Calibrated
        )
    }
}

stored_vocabulary!(PairedRunStatus {
    Prepared => "prepared",
    Launched => "launched",
    Succeeded => "succeeded",
    InfrastructureFailed => "infrastructure_failed",
    IntegrityFailed => "integrity_failed",
});

stored_vocabulary!(PairedCohortReasonCode {
    ResearchQualified => "research_qualified",
    ResearchRejected => "research_rejected",
    ResearchInconclusive => "research_inconclusive",
    NoOpCalibrationPassed => "no_op_calibration_passed",
    KnownBadCalibrationRejected => "known_bad_calibration_rejected" | "known_bad_canary_rejected",
    NoOpCalibrationFailed => "no_op_calibration_failed",
    KnownBadCalibrationNotRejected => "known_bad_calibration_not_rejected" | "known_bad_canary_not_rejected",
    AdjudicationEvidenceInvalid => "adjudication_evidence_invalid",
});

stored_vocabulary!(BudgetReservationStatus {
    Reserved => "reserved",
    Settled => "settled",
    Charged => "charged",
});

stored_vocabulary!(EvidenceGrade {
    DeterministicDevelopment => "deterministic_development",
    WorkspaceOnlyDevelopment => "workspace_only_development",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_stored_vocabulary_names_the_column_and_value() {
        let error = TrialStatus::parse_column("trials.status", "banana")
            .expect_err("unknown state must fail closed");
        assert_eq!(
            error.to_string(),
            "unknown stored trials.status value 'banana'"
        );
        assert_eq!(
            PairedCohortReasonCode::parse_column(
                "paired_cohorts.reason_code",
                "research_qualified"
            )
            .unwrap(),
            PairedCohortReasonCode::ResearchQualified
        );
    }

    #[test]
    fn historical_known_bad_reason_reopens_but_new_writes_use_calibration_vocabulary() {
        let reason = PairedCohortReasonCode::parse_column(
            "paired_cohorts.reason_code",
            "known_bad_canary_rejected",
        )
        .expect("historical reason remains readable");
        assert_eq!(reason, PairedCohortReasonCode::KnownBadCalibrationRejected);
        assert_eq!(reason.as_str(), "known_bad_calibration_rejected");
        assert_eq!(
            serde_json::from_str::<PairedCohortReasonCode>("\"known_bad_canary_not_rejected\"")
                .expect("historical receipt remains readable"),
            PairedCohortReasonCode::KnownBadCalibrationNotRejected
        );
    }
}
