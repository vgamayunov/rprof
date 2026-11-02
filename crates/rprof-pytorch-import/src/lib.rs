//! Bounded, SDK-free import of torch.profiler/Kineto Chrome trace JSON into a
//! separate rprof v2 clock domain. Chrome `ts`/`dur` are microseconds regardless
//! of `displayTimeUnit`; sub-nanosecond values round half away from zero.
//!
//! Limits: input 256 MiB, 1,000,000 source records, 65,000 tracks, 200,000
//! indexed spans, 200,000 flow endpoints, 65,000 open B slices, 4,096
//! potentially overlapping intervals examined per flow lookup. Excess
//! records/links are diagnosed, never fabricated.

use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{BufReader, Read, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use rprof_capture::{Recorder, RecorderConfig};
use rprof_core::{
    Category, CounterSample, CounterValue, Span, Track, TrackKind,
    stream::{Attribute, Collector, End, EndReason, Event, Header, Loss, Payload, Shard},
};
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use uuid::Uuid;

const MAX_INPUT: u64 = 256 * 1024 * 1024;
const MAX_RECORDS: u64 = 1_000_000;
const MAX_TRACKS: usize = 65_000;
const MAX_INDEX: usize = 200_000;
const MAX_OPEN: usize = 65_000;
const MAX_CANDIDATES: usize = 4_096;
const MAX_TEXT: usize = 8_192;

#[derive(Debug, Default, Clone)]
pub struct ImportSummary {
    pub source_records: u64,
    pub spans: u64,
    pub skipped_records: u64,
    pub unmatched_endpoints: u64,
    pub unresolved_flows: u64,
    pub ambiguous_flows: u64,
    pub precision_rounded: u64,
    pub writer_loss: u64,
}

#[derive(Deserialize, Default)]
struct SourceEvent {
    #[serde(default)]
    ph: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    cat: String,
    ts: Option<Box<RawValue>>,
    dur: Option<Box<RawValue>>,
    pid: Option<Box<RawValue>>,
    tid: Option<Box<RawValue>>,
    id: Option<Box<RawValue>>,
    id2: Option<Box<RawValue>>,
    scope: Option<Box<RawValue>>,
    args: Option<Box<RawValue>>,
}

// The seed visits array elements individually; neither the root object nor its
// traceEvents array is ever materialized as a serde_json::Value.
struct Root<'a, F>(&'a mut F);
impl<'de, F: FnMut(SourceEvent) -> Result<()>> DeserializeSeed<'de> for Root<'_, F> {
    type Value = BTreeMap<String, String>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        struct RootVisitor<'a, F>(&'a mut F);
        impl<'de, F: FnMut(SourceEvent) -> Result<()>> Visitor<'de> for RootVisitor<'_, F> {
            type Value = BTreeMap<String, String>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("Chrome trace object or event array")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while let Some(event) = seq.next_element::<SourceEvent>()? {
                    (self.0)(event).map_err(de::Error::custom)?;
                }
                Ok(BTreeMap::new())
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                struct Events<'a, F>(&'a mut F);
                impl<'de, F: FnMut(SourceEvent) -> Result<()>> DeserializeSeed<'de> for Events<'_, F> {
                    type Value = ();
                    fn deserialize<D: Deserializer<'de>>(
                        self,
                        d: D,
                    ) -> std::result::Result<(), D::Error> {
                        struct Entries<'a, F>(&'a mut F);
                        impl<'de, F: FnMut(SourceEvent) -> Result<()>> Visitor<'de> for Entries<'_, F> {
                            type Value = ();
                            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                                f.write_str("traceEvents array")
                            }
                            fn visit_seq<A: SeqAccess<'de>>(
                                self,
                                mut seq: A,
                            ) -> std::result::Result<(), A::Error> {
                                while let Some(event) = seq.next_element::<SourceEvent>()? {
                                    (self.0)(event).map_err(de::Error::custom)?;
                                }
                                Ok(())
                            }
                        }
                        d.deserialize_seq(Entries(self.0))
                    }
                }
                let mut meta = BTreeMap::new();
                let mut seen = false;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "traceEvents" {
                        if seen {
                            return Err(de::Error::custom("duplicate traceEvents"));
                        }
                        seen = true;
                        map.next_value_seed(Events(self.0))?;
                    } else {
                        let raw = map.next_value::<Box<RawValue>>()?;
                        if meta.len() < 64
                            && key.len() <= 128
                            && (meta.len() < 40 || metadata_key(&key) || key == "hostname")
                        {
                            meta.insert(key, bounded(raw.get()));
                        }
                    }
                }
                if !seen {
                    return Err(de::Error::custom("missing traceEvents"));
                }
                Ok(meta)
            }
        }
        deserializer.deserialize_any(RootVisitor(self.0))
    }
}

fn visit_file(
    path: &Path,
    mut on_event: impl FnMut(SourceEvent) -> Result<()>,
) -> Result<BTreeMap<String, String>> {
    let file = File::open(path)?;
    ensure!(file.metadata()?.len() <= MAX_INPUT, "trace exceeds 256 MiB");
    let mut de = serde_json::Deserializer::from_reader(BufReader::new(file));
    let metadata = Root(&mut on_event)
        .deserialize(&mut de)
        .context("invalid or incomplete Chrome trace")?;
    de.end().context("trailing data in Chrome trace")?;
    Ok(metadata)
}

/// Copy exactly the initially observed length into a private, bounded snapshot.
/// All parsing and the hash use this same immutable input, even when the caller's
/// original file is replaced or appended to during conversion.
fn snapshot(input: &Path) -> Result<(NamedTempFile, String)> {
    let mut source = File::open(input)?;
    let len = source.metadata()?.len();
    ensure!(len <= MAX_INPUT, "trace exceeds 256 MiB");
    let preferred = Path::new("/tmp/opencode");
    let mut copy = if preferred.is_dir() {
        NamedTempFile::new_in(preferred)?
    } else {
        NamedTempFile::new()?
    };
    let mut remaining = len;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    while remaining > 0 {
        let n = remaining.min(buf.len() as u64) as usize;
        source
            .read_exact(&mut buf[..n])
            .context("source trace truncated during snapshot")?;
        copy.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    copy.flush()?;
    Ok((copy, format!("{:x}", hasher.finalize())))
}

// Convert a JSON numeric token directly to integer nanoseconds, without ever
// converting an absolute timestamp to f64 (including exponent notation).
fn micro_ns(raw: &str) -> Result<(i128, bool)> {
    let token = raw.trim();
    let (negative, token) = if let Some(t) = token.strip_prefix('-') {
        (true, t)
    } else {
        (false, token)
    };
    let (mantissa, exponent) = token.split_once(['e', 'E']).unwrap_or((token, "0"));
    let exp: i32 = exponent.parse().context("invalid timestamp exponent")?;
    ensure!(exp.unsigned_abs() <= 100, "timestamp exponent out of range");
    let mut digits: i128 = 0;
    let mut fraction = 0i32;
    let mut dot = false;
    let mut count = 0;
    for ch in mantissa.bytes() {
        if ch == b'.' && !dot {
            dot = true;
            continue;
        }
        ensure!(ch.is_ascii_digit(), "invalid timestamp number");
        count += 1;
        ensure!(count <= 38, "timestamp exceeds precision limit");
        digits = digits
            .checked_mul(10)
            .and_then(|n| n.checked_add((ch - b'0') as i128))
            .context("timestamp overflow")?;
        if dot {
            fraction += 1;
        }
    }
    ensure!(count > 0, "empty timestamp");
    let power = exp - fraction + 3;
    let (ns, rounded) = if power >= 0 {
        let multiplier = 10i128
            .checked_pow(power as u32)
            .context("timestamp overflow")?;
        (
            digits
                .checked_mul(multiplier)
                .context("timestamp overflow")?,
            false,
        )
    } else {
        if let Some(divisor) = 10i128.checked_pow((-power) as u32) {
            let quotient = digits / divisor;
            let rem = digits % divisor;
            (
                quotient + i128::from(rem >= (divisor / 2 + divisor % 2)),
                rem != 0,
            )
        } else {
            // 10^39 already exceeds twice any accepted 38-digit mantissa.
            (0, digits != 0)
        }
    };
    Ok((if negative { -ns } else { ns }, rounded))
}

fn raw_text(value: Option<&RawValue>) -> String {
    bounded(
        &value
            .map(|v| {
                let s = v.get();
                if s.starts_with('"') {
                    serde_json::from_str::<String>(s).unwrap_or_else(|_| s.into())
                } else {
                    s.to_owned()
                }
            })
            .unwrap_or_default(),
    )
}
fn bounded(text: &str) -> String {
    if text.len() <= MAX_TEXT {
        return text.into();
    }
    let mut n = MAX_TEXT;
    while !text.is_char_boundary(n) {
        n -= 1;
    }
    text[..n].to_owned()
}
fn safe_text(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end]
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
fn metadata_key(k: &str) -> bool {
    k.starts_with("rprof")
        || matches!(
            k,
            "ERROR"
                | "WARNING"
                | "schemaVersion"
                | "profile_memory"
                | "record_shapes"
                | "with_stack"
                | "with_flops"
                | "baseTimeNanoseconds"
                | "displayTimeUnit"
                | "distributedInfo"
                | "backend"
                | "rank"
                | "deviceProperties"
                | "host_name"
                | "trace_id"
                | "torch_version"
                | "torchVersion"
        )
}
fn field<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let obj = args.as_object()?;
    keys.iter().find_map(|k| obj.get(*k))
}
fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}
fn role(e: &SourceEvent) -> &'static str {
    let cat = e.cat.to_ascii_lowercase();
    let name = e.name.to_ascii_lowercase();
    // Source category distinguishes host API time from device execution, even
    // when API/function names contain "memcpy", "memset", or "kernel".
    if cat == "gpu_user_annotation" {
        "gpu_range"
    } else if cat.contains("runtime") || cat.contains("driver") {
        "runtime_api"
    } else if cat.contains("python") {
        "python"
    } else if cat.contains("user_annotation") || cat.contains("user_range") {
        "user_range"
    } else if cat.contains("cpu_op") || cat.contains("operator") {
        "operator"
    } else if cat.contains("kernel") || name.contains("kernel") && cat.contains("gpu") {
        "kernel"
    } else if cat.contains("memcpy") || name.contains("memcpy") && cat.contains("gpu") {
        "memcpy"
    } else if cat.contains("memset") || name.contains("memset") && cat.contains("gpu") {
        "memset"
    } else if e.name == "[memory]" || cat.contains("memory") {
        "memory"
    } else if e.ph == "C" {
        "counter"
    } else if e.ph == "i" || e.ph == "I" {
        "marker"
    } else {
        "other"
    }
}
fn attrs(e: &SourceEvent, index: u64, role: &str, args: &Value) -> BTreeMap<String, Attribute> {
    let mut a = BTreeMap::new();
    let mut put = |k: &str, v: String| {
        // A single hostile event cannot exceed the recorder's 64 KiB frame.
        let limit = if k == "pytorch.args" { MAX_TEXT } else { 2_048 };
        let mut end = v.len().min(limit);
        while !v.is_char_boundary(end) {
            end -= 1;
        }
        a.insert(
            k.into(),
            Attribute::Text(if k == "pytorch.args" {
                v[..end].into()
            } else {
                safe_text(&v[..end], end)
            }),
        );
    };
    put("pytorch.role", role.into());
    put("pytorch.name", e.name.clone());
    put("pytorch.source_category", e.cat.clone());
    put("pytorch.source_pid", raw_text(e.pid.as_deref()));
    put("pytorch.source_tid", raw_text(e.tid.as_deref()));
    if let Some(ts) = &e.ts {
        put("pytorch.source_ts_us", ts.get().into());
    }
    if let Some(dur) = &e.dur {
        put("pytorch.source_dur_us", dur.get().into());
    }
    for (attr, keys) in [
        (
            "device",
            &["device", "device_id", "Device Id", "deviceId"][..],
        ),
        ("device_type", &["Device Type", "device_type"]),
        ("stream", &["stream", "stream_id", "Stream Id", "streamId"]),
        ("context", &["context", "context_id", "Context Id"]),
        ("bytes", &["bytes", "Bytes", "size"]),
        (
            "external_id",
            &["External id", "external id", "external_id"],
        ),
        (
            "correlation",
            &["correlation", "correlation_id", "correlationId"],
        ),
        (
            "input_shapes",
            &["Input Dims", "Input Shapes", "input_shapes"],
        ),
        ("input_types", &["Input type", "Input Types", "input_types"]),
        ("call_stack", &["Call stack", "call_stack", "stack"]),
    ] {
        if let Some(v) = field(args, keys) {
            put(&format!("pytorch.{attr}"), value_text(v));
        }
    }
    let coordinates = lane(e, args, role);
    let inferred = coordinates.gpu
        && (field(args, &["device", "device_id", "Device Id", "deviceId"]).is_none()
            || field(args, &["stream", "stream_id", "Stream Id", "streamId"]).is_none());
    if coordinates.gpu {
        if !coordinates.device.is_empty() {
            put("pytorch.device", coordinates.device);
        }
        if !coordinates.stream.is_empty() {
            put("pytorch.stream", coordinates.stream);
        }
    }
    if let Some(raw) = &e.args {
        put("pytorch.args", raw.get().into());
        if raw.get().len() > MAX_TEXT {
            a.insert("pytorch.args_truncated".into(), Attribute::Bool(true));
        }
    }
    a.insert(
        "pytorch.source_event_index".into(),
        Attribute::Unsigned(index),
    );
    if inferred {
        a.insert("pytorch.gpu_lane_coordinates".into(), Attribute::Bool(true));
    }
    a
}

#[derive(Clone, Hash, Eq, PartialEq)]
struct Lane {
    pid: String,
    tid: String,
    pid_numeric: bool,
    tid_numeric: bool,
    gpu: bool,
    device: String,
    context: String,
    stream: String,
}
fn lane(e: &SourceEvent, args: &Value, role: &str) -> Lane {
    let get = |keys: &[&str]| field(args, keys).map(value_text).unwrap_or_default();
    let mut lane = Lane {
        pid: raw_text(e.pid.as_deref()),
        tid: raw_text(e.tid.as_deref()),
        pid_numeric: e
            .pid
            .as_ref()
            .is_some_and(|v| v.get().bytes().all(|c| c.is_ascii_digit())),
        tid_numeric: e
            .tid
            .as_ref()
            .is_some_and(|v| v.get().bytes().all(|c| c.is_ascii_digit())),
        gpu: matches!(role, "kernel" | "memcpy" | "memset" | "gpu_range"),
        device: get(&["device", "device_id", "Device Id", "deviceId"]),
        context: get(&["context", "context_id", "Context Id"]),
        stream: get(&["stream", "stream_id", "Stream Id", "streamId"]),
    };
    if lane.gpu {
        // Kineto's GPU lanes use numeric pid/device and tid/stream coordinates.
        // Context remains unknown when the record does not supply it.
        if lane.device.is_empty() && lane.pid_numeric {
            lane.device = lane.pid.clone();
        }
        if lane.stream.is_empty() && lane.tid_numeric {
            lane.stream = lane.tid.clone();
        }
    }
    lane
}
#[derive(Clone)]
struct Indexed {
    start: u64,
    end: u64,
    correlation: u64,
    prefix_max_end: u64,
}
struct Flow {
    index: u64,
    key: String,
    phase: String,
    lane: Lane,
    time: u64,
    name: String,
    cat: String,
}
struct Open {
    start: u64,
    event: SourceEvent,
    index: u64,
    args: Value,
    role: &'static str,
    track: u64,
    lane: Lane,
}

struct Converter {
    rank: Option<u32>,
    recorder: Recorder,
    origin: i128,
    last: u64,
    next_track: u64,
    next_correlation: u64,
    tracks: HashMap<Lane, u64>,
    processes: HashMap<String, u64>,
    gpu_devices: HashMap<String, u64>,
    names: HashMap<(String, String), String>,
    open: HashMap<Lane, Vec<Open>>,
    open_count: usize,
    indexed: HashMap<(String, String), Vec<Indexed>>,
    index_count: usize,
    flows: Vec<Flow>,
    summary: ImportSummary,
}
impl Converter {
    fn time(&mut self, raw: &RawValue) -> Result<u64> {
        let (ns, rounded) = micro_ns(raw.get())?;
        self.summary.precision_rounded += u64::from(rounded);
        u64::try_from(ns.checked_sub(self.origin).context("timestamp underflow")?)
            .context("timestamp out of range")
    }
    fn observe(
        &mut self,
        actual: u64,
        payload: Payload,
        attrs: BTreeMap<String, Attribute>,
    ) -> Result<()> {
        self.last = self.last.max(actual);
        let mut event = Event::new(self.last, payload);
        event.attributes = attrs;
        // Offline conversion can block without losing data on a saturated queue.
        self.recorder.reliable(event)
    }
    fn track(&mut self, lane: &Lane) -> Result<u64> {
        if let Some(id) = self.tracks.get(lane) {
            return Ok(*id);
        }
        ensure!(self.tracks.len() < MAX_TRACKS, "trace exceeds track limit");
        let id = self.next_track;
        self.next_track += 1;
        let (parent, kind, name) = if lane.gpu {
            let device = if lane.device.is_empty() {
                format!("unknown:{}", lane.pid)
            } else {
                lane.device.clone()
            };
            let parent = if let Some(id) = self.gpu_devices.get(&device) {
                *id
            } else {
                let parent = self.next_track;
                self.next_track += 1;
                let dev = device.parse::<u32>().unwrap_or(u32::MAX);
                self.recorder.reliable(Event::new(
                    self.last,
                    Payload::Track(Track {
                        id: parent,
                        parent_id: None,
                        name: bounded(&format!("GPU device {device}")),
                        rank: self.rank,
                        kind: TrackKind::GpuDevice { device: dev },
                    }),
                ))?;
                self.gpu_devices.insert(device, parent);
                parent
            };
            let stream = lane.stream.parse::<u64>().unwrap_or(u64::MAX);
            (
                Some(parent),
                TrackKind::GpuStream { stream },
                format!(
                    "GPU stream {} (source pid {}, tid {}, context {})",
                    if lane.stream.is_empty() {
                        "unknown"
                    } else {
                        &lane.stream
                    },
                    lane.pid,
                    lane.tid,
                    lane.context
                ),
            )
        } else {
            // String IDs are synthetic labels, never represented as real OS tids.
            let tid = if lane.tid_numeric {
                lane.tid.parse::<u64>().unwrap_or(u64::MAX)
            } else {
                u64::MAX
            };
            let parent = if let Some(pid) = lane
                .pid_numeric
                .then(|| lane.pid.parse::<u32>().ok())
                .flatten()
            {
                if let Some(id) = self.processes.get(&lane.pid) {
                    Some(*id)
                } else {
                    let parent = self.next_track;
                    self.next_track += 1;
                    let label = self.names.get(&(lane.pid.clone(), "0".into()));
                    self.recorder.reliable(Event::new(
                        self.last,
                        Payload::Track(Track {
                            id: parent,
                            parent_id: None,
                            name: bounded(&format!(
                                "{} (source pid {pid})",
                                label.map(String::as_str).unwrap_or("PyTorch process")
                            )),
                            rank: self.rank,
                            kind: TrackKind::Process { pid },
                        }),
                    ))?;
                    self.processes.insert(lane.pid.clone(), parent);
                    Some(parent)
                }
            } else {
                None
            };
            (
                parent,
                TrackKind::Thread { tid },
                format!(
                    "{} (source pid {}, tid {})",
                    self.names
                        .get(&(lane.pid.clone(), lane.tid.clone()))
                        .cloned()
                        .unwrap_or_else(|| "CPU".into()),
                    lane.pid,
                    lane.tid
                ),
            )
        };
        self.recorder.reliable(Event::new(
            self.last,
            Payload::Track(Track {
                id,
                parent_id: parent,
                name: safe_text(&name, 2048),
                rank: self.rank,
                kind,
            }),
        ))?;
        self.tracks.insert(lane.clone(), id);
        Ok(id)
    }
    #[allow(clippy::too_many_arguments)] // A span retains its source identity and its actual lane/timing separately.
    fn span(
        &mut self,
        e: &SourceEvent,
        index: u64,
        start: u64,
        dur: u64,
        lane: Lane,
        track: u64,
        args: &Value,
        role: &str,
    ) -> Result<()> {
        let correlation = self.next_correlation;
        self.next_correlation += 1;
        let category = if lane.gpu {
            Category::Cuda
        } else {
            Category::Pytorch
        };
        self.observe(
            start.checked_add(dur).context("span end overflow")?,
            Payload::Span(Span {
                track_id: track,
                name: safe_text(&e.name, 2048),
                category,
                start_ns: start,
                duration_ns: dur,
                correlation_id: Some(correlation),
            }),
            attrs(e, index, role, args),
        )?;
        self.summary.spans += 1;
        if self.index_count < MAX_INDEX {
            self.indexed
                .entry((lane.pid, lane.tid))
                .or_default()
                .push(Indexed {
                    start,
                    end: start + dur,
                    correlation,
                    prefix_max_end: 0,
                });
            self.index_count += 1;
        }
        Ok(())
    }
    fn event(&mut self, e: SourceEvent) -> Result<()> {
        self.summary.source_records += 1;
        ensure!(
            self.summary.source_records <= MAX_RECORDS,
            "trace exceeds source record limit"
        );
        let index = self.summary.source_records - 1;
        let args: Value = e
            .args
            .as_ref()
            .and_then(|v| serde_json::from_str(v.get()).ok())
            .unwrap_or(Value::Null);
        let r = role(&e);
        let lane = lane(&e, &args, r);
        if e.ph == "M" {
            if matches!(e.name.as_str(), "thread_name" | "process_name")
                && self.names.len() < MAX_TRACKS
            {
                if let Some(v) = field(&args, &["name"]) {
                    self.names.insert(
                        (
                            lane.pid,
                            if e.name == "process_name" {
                                "0".into()
                            } else {
                                lane.tid
                            },
                        ),
                        bounded(&value_text(v)),
                    );
                }
            }
            return Ok(());
        }
        let Some(ts) = e.ts.as_ref() else {
            self.summary.skipped_records += 1;
            return Ok(());
        };
        let time = match self.time(ts) {
            Ok(t) => t,
            Err(_) => {
                self.summary.skipped_records += 1;
                return Ok(());
            }
        };
        if matches!(e.ph.as_str(), "s" | "t" | "f") {
            let scope = raw_text(e.scope.as_deref());
            let (id, id_scope) = if let Some(raw) = &e.id2 {
                let v: Value = serde_json::from_str(raw.get()).unwrap_or(Value::Null);
                if let Some(obj) = v.as_object() {
                    if let Some(id) = obj.get("global") {
                        (value_text(id), "global")
                    } else if let Some(id) = obj.get("local") {
                        (value_text(id), "local")
                    } else {
                        (String::new(), "local")
                    }
                } else {
                    (String::new(), "local")
                }
            } else {
                (raw_text(e.id.as_deref()), "legacy")
            };
            if id.is_empty() || self.flows.len() >= MAX_INDEX {
                self.summary.unresolved_flows += 1;
                return Ok(());
            }
            let key = format!(
                "{id_scope}|{}|{}|{}|{}|{}",
                // Kineto's legacy ac2g flow IDs cross CPU process and GPU device
                // pid namespaces. Only explicit id2.local is process-scoped.
                if id_scope == "local" { &lane.pid } else { "" },
                bounded(&e.cat),
                bounded(&e.name),
                scope,
                bounded(&id)
            );
            self.flows.push(Flow {
                index,
                key,
                phase: e.ph,
                lane,
                time,
                name: bounded(&e.name),
                cat: bounded(&e.cat),
            });
            return Ok(());
        }
        let track = match e.ph.as_str() {
            "X" | "B" | "E" | "C" | "i" | "I" => self.track(&lane)?,
            _ => {
                self.summary.skipped_records += 1;
                return Ok(());
            }
        };
        match e.ph.as_str() {
            "X" => {
                let Some(raw) = &e.dur else {
                    self.summary.skipped_records += 1;
                    return Ok(());
                };
                let Ok((dur, rounded)) = micro_ns(raw.get()) else {
                    self.summary.skipped_records += 1;
                    return Ok(());
                };
                self.summary.precision_rounded += u64::from(rounded);
                if dur < 0 || dur > u64::MAX as i128 || time.checked_add(dur as u64).is_none() {
                    self.summary.skipped_records += 1;
                    return Ok(());
                }
                self.span(&e, index, time, dur as u64, lane, track, &args, r)?;
            }
            "B" => {
                if self.open_count >= MAX_OPEN {
                    self.summary.skipped_records += 1;
                    return Ok(());
                }
                self.open.entry(lane.clone()).or_default().push(Open {
                    start: time,
                    event: e,
                    index,
                    args,
                    role: r,
                    track,
                    lane,
                });
                self.open_count += 1;
            }
            "E" => {
                if let Some(open) = self.open.get_mut(&lane).and_then(Vec::pop) {
                    self.open_count -= 1;
                    if time >= open.start {
                        self.span(
                            &open.event,
                            open.index,
                            open.start,
                            time - open.start,
                            open.lane,
                            open.track,
                            &open.args,
                            open.role,
                        )?;
                    } else {
                        self.summary.unmatched_endpoints += 1;
                    }
                } else {
                    self.summary.unmatched_endpoints += 1;
                }
            }
            "C" => {
                let mut count = 0;
                if let Some(obj) = args.as_object() {
                    for (name, value) in obj.iter().take(256) {
                        let v = match value {
                            Value::Number(n) => {
                                n.as_u64().map(CounterValue::Integer).or_else(|| {
                                    n.as_f64()
                                        .filter(|v| v.is_finite())
                                        .map(CounterValue::Float)
                                })
                            }
                            _ => None,
                        };
                        if let Some(value) = v {
                            let mut a = attrs(&e, index, r, &args);
                            a.insert("pytorch.counter_key".into(), Attribute::Text(bounded(name)));
                            self.observe(
                                time,
                                Payload::Counter(CounterSample {
                                    track_id: track,
                                    timestamp_ns: time,
                                    name: safe_text(&format!("{}.{name}", e.name), 2048),
                                    value,
                                    unit: "source".into(),
                                }),
                                a,
                            )?;
                            count += 1;
                        }
                    }
                }
                if count == 0 {
                    self.summary.skipped_records += 1;
                }
            }
            "i" | "I" => {
                let mut a = attrs(&e, index, r, &args);
                if e.name == "[memory]" {
                    // RawValue retains exact signed integer tokens even outside
                    // serde_json Value's i64/u64 range; never turn a negative
                    // allocation delta into an imprecise f64 measurement.
                    let raw: BTreeMap<String, Box<RawValue>> = e
                        .args
                        .as_ref()
                        .and_then(|v| serde_json::from_str(v.get()).ok())
                        .unwrap_or_default();
                    if let Some(delta) = raw.get("Bytes") {
                        a.insert(
                            "pytorch.delta_bytes".into(),
                            Attribute::Text(bounded(delta.get())),
                        );
                    }
                    for (key, name) in [
                        ("Total Allocated", "pytorch.memory.allocated"),
                        ("Total Reserved", "pytorch.memory.reserved"),
                    ] {
                        let Some(value) = raw.get(key).and_then(|v| v.get().parse::<u64>().ok())
                        else {
                            continue;
                        };
                        let mut counter_attrs = a.clone();
                        counter_attrs
                            .insert("pytorch.counter_key".into(), Attribute::Text(key.into()));
                        self.observe(
                            time,
                            Payload::Counter(CounterSample {
                                track_id: track,
                                timestamp_ns: time,
                                name: name.into(),
                                value: CounterValue::Integer(value),
                                unit: "bytes".into(),
                            }),
                            counter_attrs,
                        )?;
                    }
                }
                self.observe(
                    time,
                    Payload::Counter(CounterSample {
                        track_id: track,
                        timestamp_ns: time,
                        name: safe_text(&e.name, 2048),
                        value: CounterValue::Integer(1),
                        unit: "marker".into(),
                    }),
                    a,
                )?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    fn endpoint(&self, flow: &Flow) -> std::result::Result<u64, &'static str> {
        let mut best: Option<&Indexed> = None;
        let mut ambiguous = false;
        let Some(spans) = self
            .indexed
            .get(&(flow.lane.pid.clone(), flow.lane.tid.clone()))
        else {
            return Err("missing");
        };
        // Sorted starts and prefix maximum ends skip arbitrary numbers of
        // disjoint spans, while examining at most MAX_CANDIDATES overlapping
        // candidates even for dense pathological lanes.
        let upper = spans.partition_point(|s| s.start <= flow.time);
        let mut scanned = 0;
        for span in spans[..upper].iter().rev() {
            if span.prefix_max_end < flow.time {
                break;
            }
            scanned += 1;
            if scanned > MAX_CANDIDATES {
                return Err("capacity");
            }
            if (span.start <= flow.time && flow.time < span.end)
                || (span.start == flow.time && span.end == flow.time)
            {
                if let Some(old) = best {
                    if span.start > old.start || span.start == old.start && span.end < old.end {
                        best = Some(span);
                        ambiguous = false;
                    } else if span.start == old.start && span.end == old.end {
                        ambiguous = true;
                    }
                } else {
                    best = Some(span);
                }
            }
        }
        if ambiguous {
            Err("ambiguous")
        } else {
            best.map(|s| s.correlation).ok_or("missing")
        }
    }
    fn finish(
        mut self,
        metadata: BTreeMap<String, String>,
        sha: String,
        shard_id: Uuid,
    ) -> Result<ImportSummary> {
        for (_, stack) in std::mem::take(&mut self.open) {
            for open in stack {
                self.summary.unmatched_endpoints += 1;
                let correlation = self.next_correlation;
                self.next_correlation += 1;
                self.observe(
                    open.start,
                    Payload::ActivityStart {
                        track_id: open.track,
                        name: safe_text(&open.event.name, 2048),
                        correlation_id: correlation,
                        start_ns: Some(open.start),
                    },
                    attrs(&open.event, open.index, open.role, &open.args),
                )?;
            }
        }
        for spans in self.indexed.values_mut() {
            spans.sort_unstable_by_key(|s| (s.start, s.end, s.correlation));
            let mut max_end = 0;
            for span in spans {
                max_end = max_end.max(span.end);
                span.prefix_max_end = max_end;
            }
        }
        let mut chains: HashMap<String, u64> = HashMap::new();
        let mut flows = std::mem::take(&mut self.flows);
        // Only bounded flow endpoints are sorted, not the source events or spans.
        flows.sort_by_key(|f| {
            (
                f.time,
                match f.phase.as_str() {
                    "s" => 0,
                    "t" => 1,
                    _ => 2,
                },
            )
        });
        for flow in flows {
            let endpoint = self.endpoint(&flow);
            if endpoint == Err("ambiguous") {
                self.summary.ambiguous_flows += 1;
            }
            let current = endpoint.ok();
            if flow.phase == "s" {
                if let Some(c) = current {
                    chains.insert(flow.key, c);
                } else {
                    self.summary.unresolved_flows += 1;
                }
            } else {
                if let (Some(from), Some(to)) = (chains.get(&flow.key).copied(), current) {
                    let mut a = BTreeMap::new();
                    a.insert("pytorch.role".into(), Attribute::Text("runtime".into()));
                    a.insert(
                        "pytorch.flow_key".into(),
                        Attribute::Text(bounded(&flow.key)),
                    );
                    a.insert(
                        "pytorch.source_event_index".into(),
                        Attribute::Unsigned(flow.index),
                    );
                    a.insert(
                        "pytorch.flow_name".into(),
                        Attribute::Text(bounded(&flow.name)),
                    );
                    a.insert(
                        "pytorch.source_category".into(),
                        Attribute::Text(bounded(&flow.cat)),
                    );
                    self.observe(
                        flow.time,
                        Payload::Dependency {
                            from_correlation_id: from,
                            to_correlation_id: to,
                            relation: "pytorch.flow".into(),
                        },
                        a,
                    )?;
                    if flow.phase == "t" {
                        chains.insert(flow.key, to);
                    } else {
                        chains.remove(&flow.key);
                    }
                } else {
                    self.summary.unresolved_flows += 1;
                    if flow.phase == "f" {
                        chains.remove(&flow.key);
                    }
                }
            }
        }
        self.summary.unresolved_flows += chains.len() as u64;
        let mut meta = BTreeMap::new();
        meta.insert("pytorch.import_source_sha256".into(), Attribute::Text(sha));
        meta.insert("pytorch.clock_method".into(), Attribute::Text("source_minimum_ts; relative microseconds converted to nanoseconds; no monotonic alignment".into()));
        meta.insert("pytorch.source_loss_known".into(), Attribute::Bool(false));
        meta.insert("pytorch.role".into(), Attribute::Text("runtime".into()));
        // The entire metadata observation, not just individual fields, must
        // remain below the recorder's 64 KiB encoded-event limit.
        let mut metadata_budget = 16 * 1024;
        let mut metadata_truncated = false;
        for (k, v) in metadata {
            if metadata_key(&k) {
                let mut end = v.len().min(metadata_budget.min(1024));
                while !v.is_char_boundary(end) {
                    end -= 1;
                }
                metadata_truncated |= end < v.len();
                metadata_budget -= end;
                meta.insert(format!("pytorch.{k}"), Attribute::Text(v[..end].into()));
            }
        }
        if metadata_truncated {
            meta.insert("pytorch.metadata_truncated".into(), Attribute::Bool(true));
        }
        self.observe(
            self.last,
            Payload::Counter(CounterSample {
                track_id: 0,
                timestamp_ns: self.last,
                name: "pytorch.import.metadata".into(),
                value: CounterValue::Integer(1),
                unit: "metadata".into(),
            }),
            meta,
        )?;
        for (name, count) in [
            ("source_records", self.summary.source_records),
            ("spans", self.summary.spans),
            ("skipped_records", self.summary.skipped_records),
            ("unmatched_endpoints", self.summary.unmatched_endpoints),
            ("unresolved_flows", self.summary.unresolved_flows),
            ("precision_rounded", self.summary.precision_rounded),
            ("ambiguous_flows", self.summary.ambiguous_flows),
        ] {
            let mut a = BTreeMap::new();
            a.insert("pytorch.role".into(), Attribute::Text("diagnostic".into()));
            self.observe(
                self.last,
                Payload::Counter(CounterSample {
                    track_id: 0,
                    timestamp_ns: self.last,
                    name: format!("pytorch.import.{name}"),
                    value: CounterValue::Integer(count),
                    unit: "records".into(),
                }),
                a,
            )?;
        }
        self.summary.writer_loss = self.recorder.loss().total();
        self.recorder.finish(End {
            shard_id,
            duration_ns: self.last,
            exit_code: None,
            signal: None,
            interrupted_by: None,
            reason: EndReason::Completed,
            loss: Loss::default(),
        })?;
        Ok(self.summary)
    }
}

/// Convert an existing Chrome trace into a new path (must not exist). The caller
/// owns temporary-file cleanup and atomic publication after successful return.
pub fn import(input: &Path, output: &Path, session: Option<Uuid>) -> Result<ImportSummary> {
    let (snapshot, sha) = snapshot(input)?;
    let source = snapshot.path();
    let mut min = None::<i128>;
    let mut max = None::<i128>;
    let mut records = 0;
    let mut names = HashMap::new();
    let source_metadata = visit_file(source, |e| {
        records += 1;
        ensure!(records <= MAX_RECORDS, "trace exceeds source record limit");
        if e.ph == "M"
            && matches!(e.name.as_str(), "process_name" | "thread_name")
            && names.len() < MAX_TRACKS
        {
            if let Some(args) = e
                .args
                .as_ref()
                .and_then(|v| serde_json::from_str::<Value>(v.get()).ok())
            {
                if let Some(label) = field(&args, &["name"]) {
                    names.insert(
                        (
                            raw_text(e.pid.as_deref()),
                            if e.name == "process_name" {
                                "0".into()
                            } else {
                                raw_text(e.tid.as_deref())
                            },
                        ),
                        bounded(&value_text(label)),
                    );
                }
            }
        }
        // Chrome metadata (including clocks with ts=0) and unknown phases do
        // not describe a measured interval in the profiler's source domain.
        if !matches!(
            e.ph.as_str(),
            "X" | "B" | "E" | "C" | "i" | "I" | "s" | "t" | "f"
        ) {
            return Ok(());
        }
        if let Some(raw) = &e.ts {
            if let Ok((ts, _)) = micro_ns(raw.get()) {
                min = Some(min.map_or(ts, |m| m.min(ts)));
                let end = e
                    .dur
                    .as_ref()
                    .and_then(|v| micro_ns(v.get()).ok())
                    .and_then(|(d, _)| if d >= 0 { ts.checked_add(d) } else { None })
                    .unwrap_or(ts);
                max = Some(max.map_or(end, |m| m.max(end)));
            }
        }
        Ok(())
    })?;
    let origin = min.unwrap_or(0);
    ensure!(
        max.unwrap_or(origin)
            .checked_sub(origin)
            .and_then(|n| u64::try_from(n).ok())
            .is_some(),
        "source time range exceeds u64 nanoseconds"
    );
    let shard_id = Uuid::new_v4();
    let config = RecorderConfig::default();
    let shard = Shard {
        shard_id,
        parent_shard_id: None,
        hostname: source_metadata
            .get("hostname")
            .or_else(|| source_metadata.get("host_name"))
            .and_then(|raw| serde_json::from_str::<String>(raw).ok())
            .map(|s| bounded(&s))
            .unwrap_or_else(|| "pytorch-import".into()),
        boot_id: format!("pytorch-trace:{sha}:{shard_id}"),
        monotonic_origin_ns: 0,
        started_unix_ns: 0,
        command: vec![],
        supervisor_pid: 0,
        collectors: vec![Collector {
            name: "pytorch-import".into(),
            version: "1".into(),
        }],
        config: config.metadata(),
    };
    let recorder = Recorder::create(
        output,
        Header::new(session.unwrap_or_else(Uuid::new_v4)),
        shard,
        config,
    )?;
    let rank = source_metadata
        .get("distributedInfo")
        .and_then(|v| serde_json::from_str::<Value>(v).ok())
        .and_then(|v| v.get("rank").and_then(Value::as_u64))
        .or_else(|| {
            source_metadata
                .get("rprof")
                .and_then(|v| serde_json::from_str::<Value>(v).ok())
                .and_then(|v| {
                    v.get("distributed")
                        .and_then(|d| d.get("rank"))
                        .and_then(Value::as_u64)
                })
        })
        .and_then(|v| u32::try_from(v).ok());
    let mut converter = Converter {
        rank,
        recorder,
        origin,
        last: 0,
        next_track: 1,
        next_correlation: 1,
        tracks: HashMap::new(),
        processes: HashMap::new(),
        gpu_devices: HashMap::new(),
        names,
        open: HashMap::new(),
        open_count: 0,
        indexed: HashMap::new(),
        index_count: 0,
        flows: Vec::new(),
        summary: ImportSummary::default(),
    };
    // Track 0 hosts source metadata and conversion diagnostics, not user timings.
    converter.recorder.reliable(Event::new(
        0,
        Payload::Track(Track {
            id: 0,
            parent_id: None,
            name: "PyTorch import diagnostics".into(),
            rank,
            kind: TrackKind::Io {
                resource: "pytorch-import".into(),
            },
        }),
    ))?;
    let metadata = visit_file(source, |e| converter.event(e))?;
    converter.finish(metadata, sha, shard_id)
}

#[cfg(test)]
mod tests {
    use super::{MAX_INPUT, micro_ns, snapshot, visit_file};
    use std::{fs, io::Write};

    #[test]
    fn decimal_scaling_never_rounds_absolute_time_through_f64() {
        assert_eq!(
            micro_ns("9007199254740993.125").unwrap(),
            (9007199254740993125, false)
        );
        assert_eq!(
            micro_ns("9.007199254740993125e15").unwrap(),
            (9007199254740993125, false)
        );
        assert_eq!(micro_ns("-1.0005e-3").unwrap(), (-1, true));
        assert_eq!(micro_ns("-0.0005").unwrap(), (-1, true));
        assert_eq!(micro_ns("0.00049").unwrap(), (0, true));
        assert!(micro_ns("1e200").is_err());
    }

    #[test]
    fn snapshot_hash_and_both_passes_ignore_later_source_mutations() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source");
        let bytes = br#"[{"ph":"X","name":"original","ts":1,"dur":2}]"#;
        fs::write(&input, bytes).unwrap();
        let (frozen, hash) = snapshot(&input).unwrap();
        assert_eq!(hash, format!("{:x}", Sha256::digest(bytes)));
        fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&input)
            .unwrap()
            .write_all(br#"[{"ph":"X","name":"changed","ts":999,"dur":2}]"#)
            .unwrap();
        for _ in 0..2 {
            let mut seen = Vec::new();
            visit_file(frozen.path(), |event| {
                seen.push(event.name);
                Ok(())
            })
            .unwrap();
            assert_eq!(seen, ["original"]);
        }
        let too_large = dir.path().join("large");
        fs::File::create(&too_large)
            .unwrap()
            .set_len(MAX_INPUT + 1)
            .unwrap();
        assert!(snapshot(&too_large).is_err());
    }
}
