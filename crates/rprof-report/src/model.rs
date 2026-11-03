use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Derived browser model v1. All large integers are decimal strings. See
/// docs/report-model.md; these types do not change the on-disk capture schema.
#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    #[serde(skip)]
    pub(crate) source_bytes: u64,
    pub version: u32,
    pub provenance: Provenance,
    pub summary: ReportSummary,
    pub domains: Vec<Domain>,
    pub shards: Vec<ReportShard>,
    pub tracks: Vec<ReportTrack>,
    pub spans: Vec<ReportSpan>,
    pub counters: Vec<ReportCounter>,
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub io: Vec<IoSummary>,
    #[serde(default)]
    pub mpi: Vec<MpiCallSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openmp: Option<OpenmpSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuda: Option<CudaSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pmu: Option<PmuSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pytorch: Option<PytorchSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PythonSummary {
    pub runtimes: Vec<PythonRuntime>,
    pub functions: Vec<PythonFunction>,
    pub receiver_dropped: String,
    pub malformed: String,
}
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PythonRuntime {
    pub shard: String,
    pub domain: String,
    pub source: String,
    pub pid: String,
    pub instance: String,
    pub interpreter: String,
    pub backend: String,
    pub version: String,
    pub clock: String,
    pub detail: String,
    pub initialized: bool,
    pub finalized: bool,
    pub attempted: String,
    pub received: String,
    pub source_dropped: String,
    pub omitted_callbacks: String,
    pub sequence_gaps: String,
    pub unmatched: String,
    pub abandoned: String,
    pub inflight: String,
    pub source_errors: String,
    pub errors: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PythonFunction {
    pub shard: String,
    pub domain: String,
    pub source: String,
    pub kind: String,
    pub symbol: String,
    pub name: String,
    pub file: String,
    pub line: String,
    pub execution_kind: String,
    pub segments: String,
    pub total_ns: String,
    pub self_ns: Option<String>,
    pub max_ns: String,
    pub yields: String,
    pub unwinds: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PytorchSummary {
    pub sources: Vec<PytorchSource>,
    pub activities: Vec<PytorchActivity>,
    pub memory: Vec<PytorchMemory>,
}
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PytorchSource {
    pub shard: String,
    pub domain: String,
    pub metadata: BTreeMap<String, String>,
    pub diagnostics: BTreeMap<String, String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PytorchActivity {
    pub shard: String,
    pub domain: String,
    pub role: String,
    pub name: String,
    pub device: Option<String>,
    pub count: String,
    pub total_ns: String,
    pub self_ns: Option<String>,
    pub max_ns: String,
    pub bytes: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PytorchMemory {
    pub shard: String,
    pub domain: String,
    pub track: String,
    pub name: String,
    pub device: Option<String>,
    pub samples: String,
    pub peak_bytes: String,
    pub last_bytes: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PmuSummary {
    pub hosts: Vec<PmuHost>,
    pub events: Vec<PmuEvent>,
    pub metrics: Vec<PmuMetrics>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PmuHost {
    pub shard: String,
    pub domain: String,
    pub metadata: BTreeMap<String, String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PmuEvent {
    pub shard: String,
    pub domain: String,
    pub name: String,
    pub pid: String,
    pub scope: String,
    pub privilege: String,
    pub event_type: String,
    pub config: String,
    pub unit: String,
    pub status: String,
    pub error: Option<String>,
    pub raw: Option<String>,
    pub time_enabled_ns: Option<String>,
    pub time_running_ns: Option<String>,
    pub scaled_estimate: Option<String>,
    pub running_percent: Option<String>,
    pub multiplexed: bool,
    pub samples: String,
    pub final_read: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PmuMetrics {
    pub shard: String,
    pub domain: String,
    pub ipc_estimate: Option<String>,
    pub cache_miss_percent_estimate: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CudaSummary {
    pub runtimes: Vec<CudaRuntime>,
    pub devices: Vec<CudaDevice>,
    pub activities: Vec<CudaActivity>,
    pub apis: Vec<CudaApi>,
    pub sources: Vec<CudaSource>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CudaRuntime {
    pub shard: String,
    pub domain: String,
    pub instance: String,
    pub pid: String,
    pub cupti_version: String,
    pub cuda_version: String,
    pub initialized: bool,
    pub clock_method: String,
    pub clock_uncertainty: Option<String>,
    pub capabilities: Vec<CudaCapability>,
    pub errors: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CudaCapability {
    pub name: String,
    pub supported: bool,
    pub optional: bool,
    pub result: String,
    pub detail: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CudaDevice {
    pub shard: String,
    pub domain: String,
    pub instance: String,
    pub pid: String,
    pub device: String,
    pub name: String,
    pub identity: String,
    pub visible: Option<bool>,
    pub memory_bytes: Option<String>,
    pub compute_major: Option<String>,
    pub compute_minor: Option<String>,
    pub multiprocessors: Option<String>,
    pub kernels: String,
    pub copies: String,
    pub memsets: String,
    pub kernel_ns: String,
    pub copy_ns: String,
    pub copy_bytes: String,
    pub memset_bytes: String,
    pub window_ns: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CudaActivity {
    pub shard: String,
    pub domain: String,
    pub instance: String,
    pub device: String,
    pub context: String,
    pub stream: String,
    pub kind: String,
    pub name: String,
    pub count: String,
    pub total_ns: String,
    pub max_ns: String,
    pub bytes: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct CudaApi {
    pub shard: String,
    pub domain: String,
    pub instance: String,
    pub api_domain: String,
    pub name: String,
    pub calls: String,
    pub errors: String,
    pub total_ns: String,
    pub max_ns: String,
}
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct CudaSource {
    pub shard: String,
    pub domain: String,
    pub initialized_images: String,
    pub finalized: bool,
    pub attempted: String,
    pub received: String,
    pub ingress_dropped: String,
    pub cupti_dropped: String,
    pub source_errors: String,
    pub invalid_timing: String,
    pub unattributed: String,
    pub unresolved_correlations: String,
    pub ambiguous_correlations: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OpenmpSummary {
    pub runtimes: Vec<OpenmpRuntime>,
    pub regions: Vec<OpenmpRegion>,
    pub threads: Vec<OpenmpThread>,
    pub tasks_created: String,
    pub tasks_completed: String,
    pub execution_segments: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gomp: Option<GompSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GompSummary {
    pub apis: Vec<GompApi>,
    pub bodies: Vec<GompBody>,
    pub task_creations: String,
    pub task_body_spans: String,
    pub task_lifetimes_to_body_return: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct GompApi {
    pub shard: String,
    pub domain: String,
    pub instance: String,
    pub name: String,
    pub calls: String,
    pub total_ns: String,
    pub max_ns: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct GompBody {
    pub shard: String,
    pub domain: String,
    pub instance: String,
    pub tid: String,
    pub kind: String,
    pub codeptr: String,
    pub count: String,
    pub total_ns: String,
    pub max_ns: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenmpRuntime {
    pub shard: String,
    pub domain: String,
    pub instance: String,
    pub pid: String,
    #[serde(default)]
    pub backend: String,
    pub version: String,
    pub omp_version: String,
    pub initialized: bool,
    pub finalized: bool,
    pub callbacks: Vec<OpenmpCallback>,
    pub sequence_gaps: String,
    pub unmatched_endpoints: String,
    pub abandoned_intervals: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenmpCallback {
    pub name: String,
    pub status: String,
    pub observed: Option<String>,
    pub observed_complete: bool,
    #[serde(default)]
    pub detail: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenmpRegion {
    pub shard: String,
    pub domain: String,
    pub instance: String,
    pub pid: String,
    pub parallel_id: String,
    pub requested_threads: String,
    pub observed_threads: String,
    pub duration_ns: Option<String>,
    pub work_ns: String,
    pub wait_ns: String,
    pub work_min_ns: Option<String>,
    pub work_max_ns: Option<String>,
    pub work_spread_ns: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenmpThread {
    pub shard: String,
    pub domain: String,
    pub instance: String,
    pub pid: String,
    pub tid: String,
    pub parallel_id: String,
    pub team_index: Option<String>,
    pub work_ns: String,
    pub wait_ns: String,
    pub sync_ns: String,
    pub scheduled_ns: String,
    pub execution_segments: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MpiCallSummary {
    pub shard: String,
    pub rank: i32,
    pub name: String,
    pub calls: String,
    pub errors: String,
    pub total_ns: String,
    pub max_ns: String,
    pub logical_bytes: String,
    pub calls_with_bytes: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IoSummary {
    pub domain: String,
    pub host: String,
    pub file: String,
    pub operations: String,
    pub errors: String,
    pub read_bytes: String,
    pub write_bytes: String,
    pub requested_read_bytes: String,
    pub requested_write_bytes: String,
    pub total_ns: String,
    pub window_ns: String,
    pub by_operation: Vec<IoOperation>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IoOperation {
    pub name: String,
    pub count: String,
    pub errors: String,
    pub total_ns: String,
    pub max_ns: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Provenance {
    pub kind: String,
    pub label: String,
    pub source: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportSummary {
    pub format: String,
    pub session_id: String,
    pub complete: bool,
    pub truncated_tail: bool,
    pub recovered_inputs: String,
    pub tracks: String,
    pub spans: String,
    pub counters: String,
    pub dependencies: String,
    pub dropped: String,
    pub omitted_events: String,
    pub warnings: Vec<String>,
    pub categories: Vec<CategorySummary>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CategorySummary {
    pub name: String,
    pub count: String,
    pub total_ns: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Domain {
    pub id: String,
    pub name: String,
    pub origin_ns: String,
    pub end_ns: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<ClockCalibration>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClockCalibration {
    pub method: String,
    pub uncertainty_ns: String,
    pub max_drift_ns: String,
    pub complete: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportShard {
    pub id: String,
    pub domain: String,
    pub name: String,
    pub hostname: String,
    pub command: Vec<String>,
    pub collectors: Vec<String>,
    pub origin_ns: String,
    pub end_ns: String,
    pub complete: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub interrupted_by: Option<i32>,
    pub end_reason: String,
    pub dropped: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportTrack {
    pub id: String,
    pub shard: String,
    pub parent: Option<String>,
    pub name: String,
    pub kind: String,
    pub rank: Option<u32>,
    pub details: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportSpan {
    pub id: usize,
    /// Stable source observation key; unlike the retained-array index, this does
    /// not change when report retention limits change.
    #[serde(default)]
    pub source_id: String,
    pub track: String,
    pub name: String,
    pub category: String,
    pub start_ns: String,
    pub duration_ns: String,
    pub correlation: Option<String>,
    pub incomplete: bool,
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReportCounter {
    pub track: String,
    pub name: String,
    pub timestamp_ns: String,
    pub value: String,
    pub value_type: String,
    pub unit: String,
    pub attributes: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Dependency {
    pub from: String,
    pub to: String,
    pub relation: String,
}
