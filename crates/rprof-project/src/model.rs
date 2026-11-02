use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub kind: ProvenanceKind,
    pub source: String,
    pub date: String,
    pub conditions: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceKind {
    Measured,
    Calibrated,
    SpecifiedPeak,
    Assumed,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    pub unit: Unit,
    pub low: f64,
    pub central: f64,
    pub high: f64,
    pub provenance: Provenance,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    WorkPerSecond,
    BytesPerSecond,
    Nanoseconds,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Cpu,
    Gpu,
    Network,
    Storage,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    pub kind: ResourceKind,
    pub description: String,
    /// Effective concurrent service slots, not a claim about physical utilization.
    pub slots_per_node: u32,
    pub capacity_bytes_per_node: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub metrics: BTreeMap<String, Metric>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub ranks_per_node: u32,
    pub threads_per_rank: u32,
    pub gpus_per_rank: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareProfile {
    pub version: u32,
    pub id: String,
    pub name: String,
    pub nodes: u32,
    pub placement: Placement,
    pub provenance: Provenance,
    pub resources: BTreeMap<String, Resource>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedSpan {
    pub id: String,
    pub name: String,
    pub category: String,
    pub lane: String,
    pub start_ns: String,
    pub duration_ns: String,
    pub incomplete: bool,
    /// Known enclosing workload scopes cannot be combined with their contents.
    pub enclosing: bool,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub capture_sha256: String,
    pub session_id: String,
    pub domain: String,
    pub duration_ns: String,
    pub complete: bool,
    pub dropped: String,
    pub omitted: String,
    pub clock_uncertainty_ns: Option<String>,
    pub warnings: Vec<String>,
    pub metadata: BTreeMap<String, String>,
    pub spans: Vec<ObservedSpan>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scaling {
    #[default]
    Fixed,
    Strong,
    Weak,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeScaling {
    #[default]
    Fixed,
    /// Ideal per-partition redistribution: source/target nodes for strong; 1 for weak.
    Distributed,
    /// Explicit overhead model (target_nodes/source_nodes)^exponent, -2..=2.
    Power { exponent: i8 },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PhaseModel {
    Unchanged,
    Throughput {
        metric: String,
    },
    Transfer {
        bandwidth_metric: String,
        latency_metric: Option<String>,
        bytes: String,
        operations: String,
    },
    Roofline {
        compute_metric: String,
        bandwidth_metric: String,
        work: String,
        bytes: String,
    },
    Unsupported {
        reason: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Phase {
    pub id: String,
    pub span_id: String,
    pub name: String,
    pub resource: Option<String>,
    pub model: PhaseModel,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub node_scaling: NodeScaling,
    pub assumption: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unmodeled {
    #[default]
    Reject,
    Unchanged,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRequirement {
    pub bytes_per_source_node: String,
    #[serde(default)]
    pub distributed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadModel {
    pub version: u32,
    pub name: String,
    pub baseline_sha256: String,
    pub domain: String,
    pub window_start_ns: String,
    pub window_duration_ns: String,
    pub assumptions: String,
    #[serde(default)]
    pub unmodeled: Unmodeled,
    #[serde(default)]
    pub unmodeled_assumption: String,
    #[serde(default)]
    pub allow_partial_evidence: bool,
    #[serde(default)]
    pub scaling_assumption: String,
    #[serde(default)]
    pub placement_assumption: String,
    #[serde(default)]
    pub memory: BTreeMap<String, MemoryRequirement>,
    #[serde(default)]
    pub required_features: BTreeMap<String, Vec<String>>,
    pub phases: Vec<Phase>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationObservation {
    pub target_id: String,
    pub nodes: u32,
    pub observed_ns: String,
    pub workload_sha256: String,
    pub target_profile_sha256: String,
    pub provenance: Provenance,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub baseline: Baseline,
    pub source: HardwareProfile,
    pub targets: Vec<HardwareProfile>,
    pub workload: WorkloadModel,
    #[serde(default)]
    pub nodes: Vec<u32>,
    #[serde(default)]
    pub scaling: Scaling,
    #[serde(default)]
    pub validation: Vec<ValidationObservation>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Projection {
    pub kind: String,
    pub version: u32,
    pub engine_version: String,
    pub hashes: BTreeMap<String, String>,
    pub request: Request,
    pub baseline_runtime_ns: String,
    pub covered_ns: String,
    pub coverage_percent: String,
    pub source_reconstructed_ns: String,
    pub assumptions: Vec<String>,
    pub warnings: Vec<String>,
    pub scenarios: Vec<Scenario>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Estimate {
    pub optimistic_ns: String,
    pub central_ns: String,
    pub pessimistic_ns: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub target_id: String,
    pub target_name: String,
    pub nodes: u32,
    pub scaling: Scaling,
    /// estimated | unsupported | infeasible. Non-estimated scenarios have no runtime.
    pub status: String,
    pub issues: Vec<String>,
    pub warnings: Vec<String>,
    pub runtime: Option<Estimate>,
    pub speedup: Option<String>,
    pub efficiency: Option<String>,
    pub tasks: Vec<ProjectedTask>,
    pub resources: Vec<ResourceLoad>,
    pub critical_path: Vec<String>,
    pub validation: Vec<ValidationResult>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectedTask {
    pub id: String,
    pub span_id: String,
    pub name: String,
    pub resource: Option<String>,
    pub model: String,
    pub assumption: String,
    pub bottleneck: String,
    pub baseline_start_ns: String,
    pub baseline_duration_ns: String,
    pub start_ns: String,
    pub duration_ns: String,
    pub preserved_lag_ns: String,
    pub critical: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct ResourceLoad {
    pub resource: String,
    pub slots: u32,
    pub busy_ns: String,
    pub modeled_busy_percent: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct ValidationResult {
    pub observed_ns: String,
    pub signed_error_percent: String,
    pub provenance: Provenance,
}
