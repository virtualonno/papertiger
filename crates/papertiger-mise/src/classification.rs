use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::candidate::CandidateDisposition;
use crate::manifest::{ObjectiveDirection, ObjectiveRole, ObjectiveSpec};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeterministicObservation {
    pub objective: String,
    pub baseline: f64,
    pub candidate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectiveResult {
    pub key: String,
    pub role: ObjectiveRole,
    pub direction: ObjectiveDirection,
    pub unit: String,
    pub baseline: f64,
    pub candidate: f64,
    pub improvement: f64,
    pub acceptance_passed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Classification {
    pub disposition: CandidateDisposition,
    pub reasons: Vec<String>,
    pub objectives: Vec<ObjectiveResult>,
}

/// Classify exact deterministic observations against the campaign's one
/// frozen objective schema. Tier qualification is derived by the lifecycle
/// from durable trials and cannot be supplied by the proposer.
pub fn classify_deterministic(
    definitions: &[ObjectiveSpec],
    observations: &[DeterministicObservation],
    all_required_tiers_passed: bool,
) -> Result<Classification> {
    let observation_count = observations.len();
    let observations = observations
        .iter()
        .map(|observation| (observation.objective.as_str(), observation))
        .collect::<BTreeMap<_, _>>();
    let expected = definitions
        .iter()
        .map(|definition| definition.key.as_str())
        .collect::<BTreeSet<_>>();
    if observation_count != definitions.len()
        || observations.len() != definitions.len()
        || observations.keys().copied().collect::<BTreeSet<_>>() != expected
    {
        bail!("deterministic evaluation requires exactly one observation per objective");
    }

    let mut results = Vec::with_capacity(definitions.len());
    let mut reasons = Vec::new();
    let mut rejected = false;
    let mut primary_improvement = false;
    for definition in definitions {
        let observation = observations[definition.key.as_str()];
        if !observation.baseline.is_finite() || !observation.candidate.is_finite() {
            bail!("objective '{}' contains a non-finite value", definition.key);
        }
        let improvement = improvement(definition, observation.baseline, observation.candidate)?;
        let acceptance_passed = definition
            .acceptance_threshold
            .map(|threshold| acceptance_passed(definition, observation.candidate, threshold))
            .transpose()?;

        if definition.role == ObjectiveRole::HardConstraint && acceptance_passed != Some(true) {
            rejected = true;
            reasons.push(format!("hard constraint '{}' failed", definition.key));
        }
        if matches!(
            definition.role,
            ObjectiveRole::Primary | ObjectiveRole::Protected
        ) && improvement < -definition.regression_tolerance
        {
            rejected = true;
            reasons.push(format!(
                "objective '{}' regressed beyond its frozen tolerance",
                definition.key
            ));
        }
        if definition.role == ObjectiveRole::Primary
            && improvement >= definition.minimum_practical_change
        {
            primary_improvement = true;
        }
        results.push(ObjectiveResult {
            key: definition.key.clone(),
            role: definition.role,
            direction: definition.direction,
            unit: definition.unit.clone(),
            baseline: observation.baseline,
            candidate: observation.candidate,
            improvement,
            acceptance_passed,
        });
    }

    if rejected {
        return Ok(Classification {
            disposition: CandidateDisposition::Rejected,
            reasons,
            objectives: results,
        });
    }
    if !primary_improvement {
        reasons.push("no primary objective cleared its practical improvement threshold".to_owned());
    }
    if !all_required_tiers_passed {
        reasons.push("one or more frozen qualification tiers are incomplete or failed".to_owned());
    }
    Ok(Classification {
        disposition: if primary_improvement && all_required_tiers_passed {
            CandidateDisposition::Nominated
        } else {
            CandidateDisposition::Inconclusive
        },
        reasons,
        objectives: results,
    })
}

fn improvement(definition: &ObjectiveSpec, baseline: f64, candidate: f64) -> Result<f64> {
    Ok(match definition.direction {
        ObjectiveDirection::Minimize => baseline - candidate,
        ObjectiveDirection::Maximize => candidate - baseline,
        ObjectiveDirection::Target => {
            let target = definition
                .target_value
                .ok_or_else(|| anyhow::anyhow!("target objective has no target value"))?;
            (baseline - target).abs() - (candidate - target).abs()
        }
    })
}

fn acceptance_passed(definition: &ObjectiveSpec, candidate: f64, threshold: f64) -> Result<bool> {
    Ok(match definition.direction {
        ObjectiveDirection::Minimize => candidate <= threshold,
        ObjectiveDirection::Maximize => candidate >= threshold,
        ObjectiveDirection::Target => {
            let target = definition
                .target_value
                .ok_or_else(|| anyhow::anyhow!("target objective has no target value"))?;
            (candidate - target).abs() <= threshold
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definitions() -> Vec<ObjectiveSpec> {
        vec![
            ObjectiveSpec {
                key: "correct".to_owned(),
                role: ObjectiveRole::HardConstraint,
                direction: ObjectiveDirection::Maximize,
                unit: "boolean".to_owned(),
                minimum_practical_change: 0.0,
                regression_tolerance: 0.0,
                acceptance_threshold: Some(1.0),
                target_value: None,
            },
            ObjectiveSpec {
                key: "latency".to_owned(),
                role: ObjectiveRole::Primary,
                direction: ObjectiveDirection::Minimize,
                unit: "milliseconds".to_owned(),
                minimum_practical_change: 1.0,
                regression_tolerance: 0.0,
                acceptance_threshold: None,
                target_value: None,
            },
            ObjectiveSpec {
                key: "memory".to_owned(),
                role: ObjectiveRole::Protected,
                direction: ObjectiveDirection::Minimize,
                unit: "mebibytes".to_owned(),
                minimum_practical_change: 0.0,
                regression_tolerance: 2.0,
                acceptance_threshold: None,
                target_value: None,
            },
        ]
    }

    fn observations(correct: f64, latency: f64, memory: f64) -> Vec<DeterministicObservation> {
        vec![
            DeterministicObservation {
                objective: "correct".to_owned(),
                baseline: 1.0,
                candidate: correct,
            },
            DeterministicObservation {
                objective: "latency".to_owned(),
                baseline: 10.0,
                candidate: latency,
            },
            DeterministicObservation {
                objective: "memory".to_owned(),
                baseline: 100.0,
                candidate: memory,
            },
        ]
    }

    #[test]
    fn frozen_schema_separates_wrong_flat_regressed_and_real_candidates() {
        let wrong = classify_deterministic(&definitions(), &observations(0.0, 8.0, 100.0), true)
            .expect("wrong candidate");
        assert_eq!(wrong.disposition, CandidateDisposition::Rejected);

        let flat = classify_deterministic(&definitions(), &observations(1.0, 9.5, 100.0), true)
            .expect("flat candidate");
        assert_eq!(flat.disposition, CandidateDisposition::Inconclusive);

        let regressed =
            classify_deterministic(&definitions(), &observations(1.0, 8.0, 103.0), true)
                .expect("regressed candidate");
        assert_eq!(regressed.disposition, CandidateDisposition::Rejected);

        let improved = classify_deterministic(&definitions(), &observations(1.0, 8.0, 101.0), true)
            .expect("improved candidate");
        assert_eq!(improved.disposition, CandidateDisposition::Nominated);
    }

    #[test]
    fn qualification_is_not_caller_defined_per_objective() {
        let result = classify_deterministic(&definitions(), &observations(1.0, 8.0, 100.0), false)
            .expect("incomplete tiers");
        assert_eq!(result.disposition, CandidateDisposition::Inconclusive);
    }
}
