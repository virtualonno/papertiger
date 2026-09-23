//! Honest scope for synthetic lifecycle fixtures; never domain performance proof.
use papertiger_mise::measurement::{
    Aggregation, CacheState, CostCategory, MEASUREMENT_CONTRACT_SCHEMA_V2, MeasurementContract,
    MeasurementPhase, MetricKind, ProcessRole, SamplingMethod, WorkloadScope,
};

pub fn contract(unit: &str, executable_name: &str) -> MeasurementContract {
    MeasurementContract {
        schema: MEASUREMENT_CONTRACT_SCHEMA_V2.to_owned(),
        subject: "Synthetic lifecycle score fixture".to_owned(),
        process_role: ProcessRole::TestHarness,
        executable_name: executable_name.to_owned(),
        category: CostCategory::ProductBehavior,
        phase: MeasurementPhase::Test,
        metric_kind: MetricKind::Behavior,
        metric_semantics: "Read the complete fixed score fixture; units are synthetic and measure no time or memory".to_owned(),
        unit: unit.to_owned(),
        workload: WorkloadScope {
            name: "Complete tracked synthetic score fixture".to_owned(),
            cardinality: 1,
            cardinality_unit: "case".to_owned(),
        },
        host_class: format!("{}-{} native host", std::env::consts::OS, std::env::consts::ARCH),
        environment_class: "Mise runtime-selected fixture environment".to_owned(),
        aggregation: Aggregation::SingleValue,
        sampling_method: SamplingMethod::WholeWorkload,
        sample_count: 1,
        sampling_interval_ms: None,
        cache_state: CacheState::NotApplicable,
        rationale: "Exercise finite evaluation and negative evidence on a controlled fixture".to_owned(),
        tradeoff: "A synthetic score establishes no product performance improvement".to_owned(),
        limitations: vec!["Trusted collector over disclosed synthetic data; no independent host attestation or product performance claim".to_owned()],
        resource_constraint: None,
    }
}
