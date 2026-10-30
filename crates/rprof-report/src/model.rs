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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OpenmpSummary {
    pub runtimes: Vec<OpenmpRuntime>,
    pub regions: Vec<OpenmpRegion>,
    pub threads: Vec<OpenmpThread>,
    pub tasks_created: String,
    pub tasks_completed: String,
    pub execution_segments: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenmpRuntime {
    pub shard: String,
    pub domain: String,
    pub instance: String,
    pub pid: String,
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
