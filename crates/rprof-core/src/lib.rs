//! Shared, versioned capture format for collectors, reports, and the timeline UI.
//!
//! Timestamps are integer nanoseconds relative to capture start. Unix time is
//! metadata only; it must not be used to measure durations or align remote hosts.

use serde::{Deserialize, Serialize};

pub mod stream;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Capture {
    pub schema_version: u32,
    pub metadata: Metadata,
    pub tracks: Vec<Track>,
    pub spans: Vec<Span>,
    pub counters: Vec<CounterSample>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub command: Vec<String>,
    pub started_unix_ns: u64,
    pub duration_ns: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// Collectors that actually contributed data, not merely requested features.
    pub collectors: Vec<String>,
}

/// IDs are capture-local. A distributed merger must remap IDs across shards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    pub parent_id: Option<u64>,
    pub name: String,
    pub rank: Option<u32>,
    pub kind: TrackKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrackKind {
    Process { pid: u32 },
    Thread { tid: u64 },
    GpuDevice { device: u32 },
    GpuStream { stream: u64 },
    Io { resource: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Process,
    Mpi,
    Openmp,
    Cuda,
    Io,
    Python,
    Pytorch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub track_id: u64,
    pub name: String,
    pub category: Category,
    pub start_ns: u64,
    pub duration_ns: u64,
    /// Connects related operations, e.g. a host launch and GPU activity.
    pub correlation_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterSample {
    pub track_id: u64,
    pub timestamp_ns: u64,
    pub name: String,
    pub value: CounterValue,
    pub unit: String,
}

/// Preserve exact integer PMU counts instead of rounding them through f64.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum CounterValue {
    Integer(u64),
    Float(f64),
}
