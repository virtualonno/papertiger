//! Frozen measurement meaning and the trusted collector's retained provenance.
//! These bindings prevent substitution; they do not attest an untrusted host.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

use crate::digest::{sha256, validate_sha256};
use crate::improvement::ObjectiveRole;
use crate::validation::validate_nonblank;

pub const MEASUREMENT_CONTRACT_SCHEMA_V2: &str = "papertiger-mise.measurement_contract.v2";
pub const MEASUREMENT_SAMPLE_SCHEMA_V2: &str = "papertiger-mise.measurement_sample.v2";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessRole {
    CandidateRuntime,
    Compiler,
    TestHarness,
    ExternalService,
    StaticAnalysis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostCategory {
    ProductBehavior,
    DevelopmentCost,
    Infrastructure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementPhase {
    Build,
    Startup,
    Runtime,
    Shutdown,
    Test,
    Analysis,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceMetric {
    Memory,
    CpuTime,
    WallTime,
    Storage,
    Io,
    Network,
    Energy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "resource", rename_all = "snake_case")]
pub enum MetricKind {
    Behavior,
    Resource(ResourceMetric),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregation {
    SingleValue,
    Count,
    Sum,
    Mean,
    Median,
    Minimum,
    Maximum,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingMethod {
    WholeWorkload,
    Exhaustive,
    FixedInterval,
    EventDriven,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheState {
    Cold,
    Warm,
    Mixed,
    NotApplicable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadScope {
    pub name: String,
    pub cardinality: u64,
    pub cardinality_unit: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceConstraintBasis {
    pub domain_rationale: String,
    pub no_op_fixture_sha256: String,
    pub known_bad_fixture_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementContract {
    pub schema: String,
    pub subject: String,
    pub process_role: ProcessRole,
    /// Exact collector-observed executable filename, including platform suffix.
    pub executable_name: String,
    pub category: CostCategory,
    pub phase: MeasurementPhase,
    pub metric_kind: MetricKind,
    pub metric_semantics: String,
    pub unit: String,
    pub workload: WorkloadScope,
    pub host_class: String,
    pub environment_class: String,
    pub aggregation: Aggregation,
    pub sampling_method: SamplingMethod,
    pub sample_count: u64,
    pub sampling_interval_ms: Option<u64>,
    pub cache_state: CacheState,
    pub rationale: String,
    pub tradeoff: String,
    pub limitations: Vec<String>,
    pub resource_constraint: Option<ResourceConstraintBasis>,
}

impl MeasurementContract {
    pub fn validate(&self, unit: &str, role: ObjectiveRole) -> Result<()> {
        if self.schema != MEASUREMENT_CONTRACT_SCHEMA_V2 {
            bail!("measurement.schema must be {MEASUREMENT_CONTRACT_SCHEMA_V2}");
        }
        for (name, value) in [
            ("subject", self.subject.as_str()),
            ("executable_name", self.executable_name.as_str()),
            ("metric_semantics", self.metric_semantics.as_str()),
            ("unit", self.unit.as_str()),
            ("workload.name", self.workload.name.as_str()),
            (
                "workload.cardinality_unit",
                self.workload.cardinality_unit.as_str(),
            ),
            ("host_class", self.host_class.as_str()),
            ("environment_class", self.environment_class.as_str()),
            ("rationale", self.rationale.as_str()),
            ("tradeoff", self.tradeoff.as_str()),
        ] {
            validate_nonblank(&format!("measurement.{name}"), value)?;
        }
        if self.unit != unit {
            bail!("measurement.unit must exactly match its objective unit '{unit}'");
        }
        if self.executable_name.contains(['/', '\\']) {
            bail!("measurement.executable_name must be an exact filename without directories");
        }
        if self.workload.cardinality == 0 || self.sample_count == 0 {
            bail!("measurement requires positive workload.cardinality and sample_count");
        }
        match (self.sampling_method, self.sampling_interval_ms) {
            (SamplingMethod::FixedInterval, Some(interval)) if interval > 0 => {}
            (SamplingMethod::FixedInterval, _) => {
                bail!("fixed_interval measurement requires positive sampling_interval_ms")
            }
            (_, None) => {}
            _ => bail!("sampling_interval_ms is only valid for fixed_interval measurement"),
        }
        if self.limitations.is_empty() || self.limitations.iter().any(|s| s.trim().is_empty()) {
            bail!("measurement.limitations must explicitly describe the evidence scope");
        }
        if self.process_role == ProcessRole::Compiler && self.phase != MeasurementPhase::Build {
            bail!("compiler measurement requires phase=build; it cannot substitute for runtime");
        }
        if matches!(self.metric_kind, MetricKind::Resource(_))
            && matches!(
                self.process_role,
                ProcessRole::Compiler | ProcessRole::TestHarness
            )
            && self.category == CostCategory::ProductBehavior
        {
            bail!(
                "compiler or test_harness resource cost cannot use category=product_behavior; measure the candidate runtime separately"
            );
        }
        if matches!(self.metric_kind, MetricKind::Resource(_))
            && role == ObjectiveRole::HardConstraint
        {
            let basis = self.resource_constraint.as_ref().ok_or_else(|| anyhow::anyhow!(
                "hard resource constraint requires measurement.resource_constraint with domain rationale and exact calibration fixture bindings"
            ))?;
            validate_nonblank(
                "resource_constraint.domain_rationale",
                &basis.domain_rationale,
            )?;
            validate_sha256(
                &basis.no_op_fixture_sha256,
                "resource_constraint.no_op_fixture_sha256",
            )?;
            validate_sha256(
                &basis.known_bad_fixture_sha256,
                "resource_constraint.known_bad_fixture_sha256",
            )?;
        } else if self.resource_constraint.is_some() {
            bail!("measurement.resource_constraint is only valid for a hard resource constraint");
        }
        Ok(())
    }

    pub fn sha256(&self) -> Result<String> {
        Ok(sha256(&serde_json::to_vec(self)?))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasuredProcess {
    pub pid: u32,
    pub birth_identity: String,
    pub executable_locator: String,
    pub executable_sha256: String,
}

impl MeasuredProcess {
    /// Capture this collector's actual process identity. Use this only for a
    /// measurement of the collector itself; child measurements need the child's
    /// independently captured identity instead.
    pub fn current() -> Result<Self> {
        let pid = std::process::id();
        let crate::process_identity::ProcessObservation::Active {
            process_birth_identity,
        } = crate::process_identity::observe_process(pid)?
        else {
            bail!(
                "measurement collector is not active; capture the measured process identity while it is running"
            );
        };
        let executable =
            std::env::current_exe().context("locate measurement collector executable")?;
        Ok(Self {
            pid,
            birth_identity: process_birth_identity,
            executable_locator: crate::path_identity::portable_absolute(&executable)?,
            executable_sha256: sha256(
                &std::fs::read(&executable).context("read measurement collector executable")?,
            ),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementSample {
    pub schema: String,
    /// Observed scope must equal the frozen contract, not merely its metric name.
    pub observed: MeasurementContract,
    pub process: MeasuredProcess,
    pub participant_revision: String,
    pub fixture_sha256: String,
    /// Runtime-selected evaluator environment (deterministic) or frozen
    /// environment profile (paired). Host compliance remains collector-trusted.
    pub environment_sha256: String,
    /// An integer for scaled paired evidence; an exact evaluator number otherwise.
    pub value: Number,
    pub scale10: u8,
    /// Raw collector evidence is retained inside the CAS-bound evaluator result.
    pub evidence: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationProvenance {
    pub baseline: MeasurementSample,
    pub candidate: MeasurementSample,
}

impl MeasurementSample {
    pub fn validate(&self, contract: &MeasurementContract) -> Result<()> {
        if self.schema != MEASUREMENT_SAMPLE_SCHEMA_V2 {
            bail!("measurement sample requires schema={MEASUREMENT_SAMPLE_SCHEMA_V2}");
        }
        if &self.observed != contract {
            bail!(
                "measurement provenance differs from the frozen process, workload, phase, category or sampling contract; supply evidence for that exact objective"
            );
        }
        if self.process.pid == 0 {
            bail!("measurement provenance requires the observed nonzero process.pid");
        }
        validate_nonblank(
            "measurement process.birth_identity",
            &self.process.birth_identity,
        )?;
        validate_nonblank(
            "measurement process.executable_locator",
            &self.process.executable_locator,
        )?;
        let filename = self.process.executable_locator.rsplit(['/', '\\']).next();
        if filename != Some(contract.executable_name.as_str()) {
            bail!(
                "measured executable must match measurement.executable_name '{}'; compiler and runtime processes are not interchangeable",
                contract.executable_name
            );
        }
        validate_sha256(
            &self.process.executable_sha256,
            "measurement process.executable_sha256",
        )?;
        validate_sha256(&self.fixture_sha256, "measurement fixture_sha256")?;
        validate_sha256(&self.environment_sha256, "measurement environment_sha256")?;
        validate_nonblank(
            "measurement participant_revision",
            &self.participant_revision,
        )?;
        if self
            .evidence
            .as_object()
            .is_none_or(|object| object.is_empty())
        {
            bail!("measurement provenance requires a nonempty object-valued raw evidence document");
        }
        Ok(())
    }

    pub fn validate_runtime_binding(
        &self,
        revision: &str,
        fixture_sha256: &str,
        environment_sha256: &str,
    ) -> Result<()> {
        if self.participant_revision != revision
            || self.fixture_sha256 != fixture_sha256
            || self.environment_sha256 != environment_sha256
        {
            bail!(
                "measurement provenance must bind the runtime-selected participant revision, fixture_sha256 and environment_sha256"
            );
        }
        Ok(())
    }
}

pub(crate) fn validate_deterministic_provenance(
    contract: Option<&MeasurementContract>,
    provenance: Option<&ObservationProvenance>,
    baseline: f64,
    candidate: f64,
) -> Result<()> {
    match (contract, provenance) {
        (None, None) => Ok(()), // Immutable historical evidence has no inferred provenance.
        (Some(contract), Some(provenance)) => {
            for (sample, expected) in [
                (&provenance.baseline, baseline),
                (&provenance.candidate, candidate),
            ] {
                sample.validate(contract)?;
                const EXACT_INTEGER_LIMIT: u64 = 1_u64 << 53;
                if sample
                    .value
                    .as_u64()
                    .is_some_and(|value| value > EXACT_INTEGER_LIMIT)
                    || sample
                        .value
                        .as_i64()
                        .is_some_and(|value| value.unsigned_abs() > EXACT_INTEGER_LIMIT)
                {
                    bail!(
                        "deterministic provenance integer exceeds the exact binary64 integer range; use paired integer measurements with an explicit scale10"
                    );
                }
                if sample.scale10 != 0 || sample.value.as_f64() != Some(expected) {
                    bail!(
                        "deterministic provenance value must equal the observed value with scale10=0"
                    );
                }
            }
            Ok(())
        }
        _ => bail!(
            "measurement contract and retained observation provenance must both be present; an observation without provenance cannot satisfy a measurement contract"
        ),
    }
}

pub(crate) fn validate_paired_sample(
    contract: Option<&MeasurementContract>,
    sample: Option<&MeasurementSample>,
    units: i64,
    scale10: u8,
) -> Result<()> {
    match (contract, sample) {
        (None, None) => Ok(()),
        (Some(contract), Some(sample)) => {
            sample.validate(contract)?;
            if sample.scale10 != scale10 || sample.value.as_i64() != Some(units) {
                bail!("paired provenance must preserve exact integer units and frozen scale10");
            }
            Ok(())
        }
        _ => bail!(
            "paired measurement requires both its frozen contract and retained provenance sample"
        ),
    }
}

pub(crate) fn validate_resource_calibration<'a>(
    results: impl IntoIterator<Item = (&'a str, Option<&'a MeasurementContract>, Option<bool>)>,
    known_bad: bool,
) -> Result<()> {
    for (key, contract, acceptance) in results {
        if contract.is_some_and(|contract| contract.resource_constraint.is_some())
            && acceptance != Some(!known_bad)
        {
            bail!(
                "hard resource constraint '{key}' requires a passing no-op and a threshold-rejecting known-bad control; supply calibrated resource observations for that exact objective"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn contract(unit: &str) -> MeasurementContract {
        MeasurementContract {
            schema: MEASUREMENT_CONTRACT_SCHEMA_V2.to_owned(),
            subject: "synthetic measurement contract fixture".to_owned(),
            process_role: ProcessRole::TestHarness,
            executable_name: "fixture-evaluator".to_owned(),
            category: CostCategory::ProductBehavior,
            phase: MeasurementPhase::Test,
            metric_kind: MetricKind::Behavior,
            metric_semantics: "fixed synthetic score, not a resource measurement".to_owned(),
            unit: unit.to_owned(),
            workload: WorkloadScope {
                name: "complete synthetic fixture".to_owned(),
                cardinality: 1,
                cardinality_unit: "case".to_owned(),
            },
            host_class: "unit-test host".to_owned(),
            environment_class: "isolated test fixture".to_owned(),
            aggregation: Aggregation::SingleValue,
            sampling_method: SamplingMethod::WholeWorkload,
            sample_count: 1,
            sampling_interval_ms: None,
            cache_state: CacheState::NotApplicable,
            rationale: "Exercise provenance refusal and preservation".to_owned(),
            tradeoff: "No domain performance inference from synthetic scores".to_owned(),
            limitations: vec!["Synthetic test data; no independent host attestation".to_owned()],
            resource_constraint: None,
        }
    }

    pub(crate) fn sample(contract: &MeasurementContract, value: i64) -> MeasurementSample {
        MeasurementSample {
            schema: MEASUREMENT_SAMPLE_SCHEMA_V2.to_owned(),
            observed: contract.clone(),
            process: MeasuredProcess {
                pid: 7,
                birth_identity: "synthetic-process-birth".to_owned(),
                executable_locator: format!("/fixture/{}", contract.executable_name),
                executable_sha256: sha256(b"fixture executable"),
            },
            participant_revision: "fixture-revision".to_owned(),
            fixture_sha256: sha256(b"fixture"),
            environment_sha256: sha256(b"environment"),
            value: Number::from(value),
            scale10: 0,
            evidence: serde_json::json!({"synthetic_fixture_value": value}),
        }
    }

    #[test]
    fn measured_process_uses_live_native_identity() {
        let process = MeasuredProcess::current().expect("live identity");
        assert_eq!(process.pid, std::process::id());
        assert!(!process.birth_identity.is_empty());
        assert_eq!(
            process.executable_sha256,
            sha256(&std::fs::read(std::env::current_exe().unwrap()).unwrap())
        );
    }

    #[test]
    fn omitted_scope_and_threshold_only_claims_are_refused() {
        let contract = contract("bytes");
        let value = serde_json::to_value(&contract).unwrap();
        for key in [
            "schema",
            "subject",
            "process_role",
            "executable_name",
            "category",
            "phase",
            "metric_kind",
            "metric_semantics",
            "unit",
            "workload",
            "host_class",
            "environment_class",
            "aggregation",
            "sampling_method",
            "sample_count",
            "cache_state",
            "rationale",
            "tradeoff",
            "limitations",
        ] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<MeasurementContract>(missing).is_err(),
                "accepted omission of {key}"
            );
        }
        assert!(
            serde_json::from_value::<MeasurementContract>(
                serde_json::json!({"maximum_bytes": 5368709120_u64})
            )
            .is_err()
        );
        for key in [
            "subject",
            "metric_semantics",
            "host_class",
            "environment_class",
            "rationale",
            "tradeoff",
        ] {
            let mut blank = value.clone();
            blank[key] = serde_json::json!(" ");
            assert!(
                serde_json::from_value::<MeasurementContract>(blank)
                    .unwrap()
                    .validate("bytes", ObjectiveRole::Diagnostic)
                    .is_err(),
                "accepted blank {key}"
            );
        }
    }

    #[test]
    fn compiler_memory_cannot_substitute_for_product_runtime_memory() {
        let mut runtime = contract("bytes");
        runtime.subject = "Ghidramink indexer runtime".to_owned();
        runtime.process_role = ProcessRole::CandidateRuntime;
        runtime.phase = MeasurementPhase::Runtime;
        runtime.executable_name = "ghidramink-indexer".to_owned();
        runtime.metric_kind = MetricKind::Resource(ResourceMetric::Memory);
        runtime.validate("bytes", ObjectiveRole::Protected).unwrap();
        let mut compiler = sample(&runtime, 5 * 1024 * 1024 * 1024);
        compiler.observed.process_role = ProcessRole::Compiler;
        compiler.observed.phase = MeasurementPhase::Build;
        compiler.observed.category = CostCategory::DevelopmentCost;
        compiler.observed.executable_name = "rustc".to_owned();
        compiler.process.executable_locator = "/toolchain/rustc".to_owned();
        assert!(compiler.validate(&runtime).is_err());
        // Relabeling scope alone still leaves an incompatible measured process.
        compiler.observed = runtime.clone();
        assert!(compiler.validate(&runtime).is_err());
        runtime.process_role = ProcessRole::Compiler;
        assert!(runtime.validate("bytes", ObjectiveRole::Protected).is_err());
        runtime.phase = MeasurementPhase::Build;
        assert!(runtime.validate("bytes", ObjectiveRole::Protected).is_err());
    }

    #[test]
    fn scope_value_and_runtime_bindings_cannot_be_substituted() {
        let contract = contract("cases");
        let valid = sample(&contract, 3);
        valid.validate(&contract).unwrap();
        let mut substituted = valid.clone();
        substituted.observed.workload.cardinality += 1;
        assert!(substituted.validate(&contract).is_err());
        substituted = valid.clone();
        substituted.observed.cache_state = CacheState::Warm;
        assert!(substituted.validate(&contract).is_err());
        for (revision, fixture, environment) in [
            (
                "other",
                valid.fixture_sha256.as_str(),
                valid.environment_sha256.as_str(),
            ),
            (
                valid.participant_revision.as_str(),
                "other",
                valid.environment_sha256.as_str(),
            ),
            (
                valid.participant_revision.as_str(),
                valid.fixture_sha256.as_str(),
                "other",
            ),
        ] {
            assert!(
                valid
                    .validate_runtime_binding(revision, fixture, environment)
                    .is_err()
            );
        }
        let provenance = ObservationProvenance {
            baseline: valid.clone(),
            candidate: valid.clone(),
        };
        validate_deterministic_provenance(Some(&contract), Some(&provenance), 3.0, 3.0).unwrap();
        assert!(
            validate_deterministic_provenance(Some(&contract), Some(&provenance), 3.0, 4.0)
                .is_err()
        );
        assert!(validate_deterministic_provenance(Some(&contract), None, 3.0, 3.0).is_err());
        assert!(validate_paired_sample(Some(&contract), Some(&valid), 3, 1).is_err());
        assert!(validate_paired_sample(Some(&contract), Some(&valid), 4, 0).is_err());
        assert!(validate_paired_sample(Some(&contract), None, 3, 0).is_err());
    }

    #[test]
    fn unrelated_behavior_rejection_cannot_calibrate_a_resource_threshold() {
        let mut contract = contract("bytes");
        contract.resource_constraint = Some(ResourceConstraintBasis {
            domain_rationale: "fixture resource ceiling".to_owned(),
            no_op_fixture_sha256: sha256(b"no-op"),
            known_bad_fixture_sha256: sha256(b"known-bad"),
        });
        validate_resource_calibration([("memory", Some(&contract), Some(true))], false).unwrap();
        validate_resource_calibration([("memory", Some(&contract), Some(false))], true).unwrap();
        assert!(
            validate_resource_calibration([("memory", Some(&contract), Some(true))], true).is_err()
        );
        assert!(validate_resource_calibration([("memory", Some(&contract), None)], true).is_err());
    }
}
