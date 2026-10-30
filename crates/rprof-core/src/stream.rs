//! Version 2 streaming schema. Each record is one bounded, newline-terminated JSON
//! frame. A complete prefix is recoverable without a footer. Version 1 remains
//! available at the crate root for reading bootstrap captures.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CounterSample, Span, Track};

pub const VERSION: u32 = 2;
pub const FORMAT: &str = "rprof-stream";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub format: String,
    pub schema_version: u32,
    pub session_id: Uuid,
    /// Number of truncated input tails discarded by merges (provenance, not loss).
    #[serde(default)]
    pub recovered_inputs: u64,
}

impl Header {
    pub fn new(session_id: Uuid) -> Self {
        Self {
            format: FORMAT.into(),
            schema_version: VERSION,
            session_id,
            recovered_inputs: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shard {
    pub shard_id: Uuid,
    pub parent_shard_id: Option<Uuid>,
    pub hostname: String,
    /// Linux boot UUID: together with hostname, identifies a monotonic clock domain.
    pub boot_id: String,
    /// CLOCK_MONOTONIC nanoseconds at local capture origin. Same-domain shards can
    /// be aligned with this value; cross-host clocks are deliberately not aligned.
    pub monotonic_origin_ns: u64,
    pub started_unix_ns: u64,
    pub command: Vec<String>,
    pub supervisor_pid: u32,
    pub collectors: Vec<Collector>,
    pub config: CaptureConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collector {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureConfig {
    pub chunk_events: usize,
    pub queue_chunks: usize,
    pub flush_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Observation time, monotonic within each shard. Completed spans can have
    /// earlier starts; ordering is by observation, not span start.
    pub timestamp_ns: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, Attribute>,
    #[serde(flatten)]
    pub payload: Payload,
}

impl Event {
    pub fn new(timestamp_ns: u64, payload: Payload) -> Self {
        Self {
            timestamp_ns,
            attributes: BTreeMap::new(),
            payload,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Payload {
    Track(Track),
    Span(Span),
    Counter(CounterSample),
    /// An open activity remains useful even if the process or profiler is killed.
    ActivityStart {
        track_id: u64,
        name: String,
        correlation_id: u64,
        /// Optional operation start, distinct from the event's ingestion time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_ns: Option<u64>,
    },
    /// Local correlation IDs identify logical operations, possibly across tracks.
    /// Forward references are allowed; dropped operations can leave dangling links.
    Dependency {
        from_correlation_id: u64,
        to_correlation_id: u64,
        relation: String,
    },
    Heartbeat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Attribute {
    Text(String),
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Bool(bool),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Loss {
    pub queue_full: u64,
    pub oversized: u64,
    /// Events lost before entering the capture writer (e.g. native IPC saturation).
    #[serde(default)]
    pub ingress: u64,
}

impl Loss {
    pub fn total(&self) -> u64 {
        self.queue_full
            .saturating_add(self.oversized)
            .saturating_add(self.ingress)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub shard_id: Uuid,
    pub sequence: u64,
    /// Cumulative loss counters, checkpointed even before shard completion.
    pub loss: Loss,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct End {
    pub shard_id: Uuid,
    pub duration_ns: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub interrupted_by: Option<i32>,
    pub reason: EndReason,
    pub loss: Loss,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Completed,
    Interrupted,
    LaunchFailed,
    CollectorFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", content = "data", rename_all = "snake_case")]
pub enum Record {
    Header(Header),
    Shard(Shard),
    Chunk(Chunk),
    End(End),
}
