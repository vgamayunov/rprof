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
