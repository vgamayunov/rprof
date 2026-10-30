use crate::{
    capture_sink::CaptureSink,
    native_collector::{Mapping, Outcome, pump},
    record::monotonic_ns,
};
use anyhow::{Result, ensure};
use rprof_core::{
    Category, CounterSample, CounterValue, Span, TrackKind,
    stream::{Attribute, Event, Payload},
};
use rprof_cuda_protocol::*;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    os::unix::net::UnixDatagram,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
};

type ApiKey = (u64, u32, u64);
const MAX_METADATA: usize = 65_536;
#[derive(Clone, Copy)]
struct Api {
    tid: u32,
    parent_runtime: u64,
}
#[derive(Default)]
struct Decode {
    devices: HashMap<(u32, u64, u32), u64>,
    streams: HashMap<(u32, u64, u32, u32, u32), u64>,
    apis: HashMap<ApiKey, Api>,
    recent: VecDeque<ApiKey>,
    invalid_timing: u64,
    unattributed: u64,
    unresolved: u64,
    ambiguous: u64,
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.iter().position(|v| *v == 0).unwrap_or(bytes.len())])
        .into_owned()
}
fn role(kind: u16) -> &'static str {
    match kind {
        RUNTIME => "runtime",
        CAPABILITY => "capability",
        DEVICE => "device",
        CONTEXT => "context",
        STREAM => "stream",
        API_BEGIN | API_END => "api",
        KERNEL => "kernel",
        MEMCPY => "memcpy",
        MEMSET => "memset",
        SYNC => "synchronization",
        FINALIZE => "finalize",
        ERROR => "error",
        DROPPED => "source_drop",
        EXTERNAL_CORRELATION => "external_correlation",
        _ => "unknown",
    }
}
fn domain(value: u32) -> &'static str {
    match value {
        DOMAIN_RUNTIME => "runtime",
        DOMAIN_DRIVER => "driver",
        _ => "unknown",
    }
}
fn api_namespace(value: u32) -> &'static str {
    if value == DOMAIN_RUNTIME {
        "cuda.runtime"
    } else {
        "cuda.driver"
    }
}
fn attrs(raw: &CudaEvent) -> BTreeMap<String, Attribute> {
    let mut a = BTreeMap::from([
        ("cuda.role".into(), Attribute::Text(role(raw.kind).into())),
        ("cuda.name".into(), Attribute::Text(text(&raw.name))),
        ("cuda.activity".into(), Attribute::Unsigned(raw.activity_id)),
        (
            "cuda.correlation".into(),
            Attribute::Unsigned(raw.correlation),
        ),
        (
            "cuda.runtime_correlation".into(),
            Attribute::Unsigned(raw.runtime_correlation),
        ),
        (
            "cuda.parent_correlation".into(),
            Attribute::Unsigned(raw.parent_correlation),
        ),
        (
            "cuda.api_domain".into(),
            Attribute::Text(domain(raw.api_domain).into()),
        ),
        ("cuda.bytes".into(), Attribute::Unsigned(raw.bytes)),
        (
            "cuda.operation".into(),
            Attribute::Unsigned(raw.operation as u64),
        ),
        ("cuda.graph".into(), Attribute::Unsigned(raw.graph_id)),
        (
            "cuda.graph_node".into(),
            Attribute::Unsigned(raw.graph_node_id),
        ),
        (
            "cuda.name_truncated".into(),
            Attribute::Bool(raw.flags & NAME_TRUNCATED != 0),
        ),
        (
            "cuda.timing_available".into(),
            Attribute::Bool(raw.flags & TIMING_UNAVAILABLE == 0),
        ),
        ("cuda.async".into(), Attribute::Bool(raw.flags & ASYNC != 0)),
        ("cuda.value".into(), Attribute::Signed(raw.value)),
        ("cuda.aux0".into(), Attribute::Unsigned(raw.aux0)),
        ("cuda.aux1".into(), Attribute::Unsigned(raw.aux1)),
        ("cuda.aux2".into(), Attribute::Unsigned(raw.aux2)),
        (
            "cuda.source_start_ns".into(),
            Attribute::Unsigned(raw.start_ns),
        ),
        ("cuda.source_end_ns".into(), Attribute::Unsigned(raw.end_ns)),
        ("process_instance".into(), Attribute::Unsigned(raw.instance)),
        ("pid".into(), Attribute::Unsigned(raw.pid as u64)),
    ]);
    if raw.tid != 0 {
        a.insert("tid".into(), Attribute::Unsigned(raw.tid as u64));
    }
    for (name, value) in [
        ("device", raw.device),
        ("context", raw.context),
        ("stream", raw.stream),
        ("src_device", raw.src_device),
        ("dst_device", raw.dst_device),
    ] {
        if value != UNKNOWN {
            a.insert(format!("cuda.{name}"), Attribute::Unsigned(value as u64));
        }
    }
    if raw.flags & RESULT_KNOWN != 0 {
        a.insert("cuda.result".into(), Attribute::Signed(raw.result as i64));
    }
    let detail = text(&raw.detail);
    if !detail.is_empty() {
        a.insert("cuda.detail".into(), Attribute::Text(detail));
    }
    if raw.kind == KERNEL {
        for (prefix, dimensions) in [
            ("grid", raw.grid),
            ("block", raw.block),
            ("cluster", raw.cluster),
        ] {
            for (axis, value) in ["x", "y", "z"].into_iter().zip(dimensions) {
                a.insert(
                    format!("cuda.{prefix}.{axis}"),
                    Attribute::Unsigned(value as u64),
                );
            }
        }
        a.insert(
            "cuda.registers_per_thread".into(),
            Attribute::Unsigned(raw.registers as u64),
        );
        a.insert(
            "cuda.static_shared_bytes".into(),
            Attribute::Unsigned(raw.static_shared),
        );
        a.insert(
            "cuda.dynamic_shared_bytes".into(),
            Attribute::Unsigned(raw.dynamic_shared),
        );
    }
    a
}

impl Decode {
    fn activity_domain(&mut self, raw: &CudaEvent) -> Option<u32> {
        if raw.correlation == 0 {
            return None;
        }
        if matches!(raw.kind, MEMCPY | MEMSET) {
            return Some(DOMAIN_DRIVER);
        }
        let driver = self
            .apis
            .get(&(raw.instance, DOMAIN_DRIVER, raw.correlation));
        let runtime = self
            .apis
            .get(&(raw.instance, DOMAIN_RUNTIME, raw.correlation));
        match (driver, runtime) {
            (Some(_), None) => Some(DOMAIN_DRIVER),
            (None, Some(_)) => Some(DOMAIN_RUNTIME),
            (Some(driver), Some(_)) if driver.parent_runtime == raw.correlation => {
                Some(DOMAIN_DRIVER)
            }
            (Some(_), Some(_)) => {
                self.ambiguous += 1;
                None
            }
            (None, None) => {
                self.unresolved += 1;
                None
            }
        }
    }
    fn remember(&mut self, raw: &CudaEvent) {
        if raw.correlation == 0 {
            return;
        }
        let key = (raw.instance, raw.api_domain, raw.correlation);
        if !self.apis.contains_key(&key) {
            if self.apis.len() >= MAX_METADATA {
                if let Some(old) = self.recent.pop_front() {
                    self.apis.remove(&old);
                }
            }
            self.recent.push_back(key);
        }
        self.apis.insert(
            key,
            Api {
                tid: raw.tid,
                parent_runtime: raw.parent_correlation,
            },
        );
    }
    fn api_corr(out: &mut CaptureSink, raw: &CudaEvent, api_domain: u32, id: u64) -> Result<u64> {
        out.correlation(api_namespace(api_domain), raw.instance, id)
    }
    fn edge(out: &mut CaptureSink, at: u64, from: u64, to: u64, label: &str) -> Result<()> {
        if from != to {
            out.emit(Event::new(
                at,
                Payload::Dependency {
                    from_correlation_id: from,
                    to_correlation_id: to,
                    relation: label.into(),
                },
            ))?;
        }
        Ok(())
    }
    fn metadata(
        raw: &CudaEvent,
        out: &mut CaptureSink,
        process: u64,
        at: u64,
        mut a: BTreeMap<String, Attribute>,
    ) -> Result<()> {
        if raw.kind == RUNTIME {
            a.insert(
                "cuda.initialized".into(),
                Attribute::Bool(raw.flags & SOURCE_ERROR == 0 && raw.flags & MONOTONIC_CLOCK != 0),
            );
            a.insert(
                "cuda.clock_method".into(),
                Attribute::Text(
                    if raw.flags & MONOTONIC_CLOCK != 0 {
                        "CUPTI GPU normalization to host CLOCK_MONOTONIC"
                    } else {
                        "unavailable"
                    }
                    .into(),
                ),
            );
            a.insert(
                "cuda.clock_uncertainty".into(),
                Attribute::Text("not provided by CUPTI".into()),
            );
        }
        if raw.kind == DEVICE {
            a.insert("cuda.device_name".into(), Attribute::Text(text(&raw.name)));
            a.insert(
                "cuda.visible".into(),
                Attribute::Bool(raw.flags & DEVICE_VISIBLE != 0),
            );
        }
        if raw.kind == CAPABILITY {
            a.insert("cuda.capability".into(), Attribute::Text(text(&raw.name)));
            a.insert("cuda.supported".into(), Attribute::Bool(raw.result == 0));
            a.insert(
                "cuda.optional".into(),
                Attribute::Bool(raw.flags & OPTIONAL != 0),
            );
        }
        out.reliable(Event {
            timestamp_ns: at,
            attributes: a,
            payload: Payload::Counter(CounterSample {
                track_id: process,
                timestamp_ns: at,
                name: format!("cuda.meta.{}", text(&raw.name)),
                value: CounterValue::Integer(raw.value.max(0) as u64),
                unit: "metadata".into(),
            }),
        })
    }
    fn gpu_track(
        &mut self,
        raw: &CudaEvent,
        out: &mut CaptureSink,
        process: u64,
        at: u64,
    ) -> Result<u64> {
        let device_key = (raw.pid, raw.instance, raw.device);
        let device = if let Some(id) = self.devices.get(&device_key) {
            *id
        } else {
            let id = out.track(
                at,
                process,
                format!("GPU {}", raw.device),
                TrackKind::GpuDevice { device: raw.device },
            )?;
            self.devices.insert(device_key, id);
            id
        };
        let stream_key = (raw.pid, raw.instance, raw.device, raw.context, raw.stream);
        if let Some(id) = self.streams.get(&stream_key) {
            return Ok(*id);
        }
        let id = out.track(
            at,
            device,
            format!("context {} / stream {}", raw.context, raw.stream),
            TrackKind::GpuStream {
                stream: raw.stream as u64,
            },
        )?;
        self.streams.insert(stream_key, id);
        Ok(id)
    }
    fn event(&mut self, raw: CudaEvent, out: &mut CaptureSink, origin: u64, at: u64) -> Result<()> {
        let process = out.native_process(at, raw.pid, raw.instance, None)?;
        let mut a = attrs(&raw);
        let name = text(&raw.name);
        if matches!(
            raw.kind,
            RUNTIME
                | CAPABILITY
                | DEVICE
                | CONTEXT
                | STREAM
                | FINALIZE
                | ERROR
                | DROPPED
                | EXTERNAL_CORRELATION
        ) {
            return Self::metadata(&raw, out, process, at, a);
        }
        let valid = raw.flags & TIMING_UNAVAILABLE == 0
            && raw.start_ns >= origin
            && raw.end_ns >= raw.start_ns
            && raw.end_ns <= origin.saturating_add(at);
        if matches!(raw.kind, API_BEGIN | API_END) {
            ensure!(
                matches!(raw.api_domain, DOMAIN_RUNTIME | DOMAIN_DRIVER),
                "unknown CUDA API callback domain"
            );
            if raw.tid == 0 {
                self.unattributed += 1;
                return Self::metadata(&raw, out, process, at, a);
            }
            self.remember(&raw);
            let track = out.native_thread(at, raw.pid, raw.instance, raw.tid, None)?;
            let correlation = if raw.correlation != 0 {
                Some(Self::api_corr(out, &raw, raw.api_domain, raw.correlation)?)
            } else {
                None
            };
            if raw.kind == API_BEGIN {
                if raw.start_ns >= origin && raw.start_ns <= origin.saturating_add(at) {
                    if let Some(correlation) = correlation {
                        out.emit(Event {
                            timestamp_ns: at,
                            attributes: a,
                            payload: Payload::ActivityStart {
                                track_id: track,
                                name,
                                correlation_id: correlation,
                                start_ns: Some(raw.start_ns - origin),
                            },
                        })?;
                        if raw.api_domain == DOMAIN_DRIVER && raw.parent_correlation != 0 {
                            let parent =
                                Self::api_corr(out, &raw, DOMAIN_RUNTIME, raw.parent_correlation)?;
                            Self::edge(out, at, parent, correlation, "runtime calls driver API")?;
                        }
                    }
                } else {
                    self.invalid_timing += 1;
                }
            } else if valid {
                out.emit(Event {
                    timestamp_ns: at,
                    attributes: a,
                    payload: Payload::Span(Span {
                        track_id: track,
                        name,
                        category: Category::Cuda,
                        start_ns: raw.start_ns - origin,
                        duration_ns: raw.end_ns - raw.start_ns,
                        correlation_id: correlation,
                    }),
                })?;
            } else {
                self.invalid_timing += 1;
                a.insert("cuda.timing_available".into(), Attribute::Bool(false));
                Self::metadata(&raw, out, process, at, a)?;
            }
            return Ok(());
        }
        if !valid {
            self.invalid_timing += 1;
            a.insert("cuda.timing_available".into(), Attribute::Bool(false));
            return Self::metadata(&raw, out, process, at, a);
        }
        let correlation = out.correlation("cuda.activity", raw.instance, raw.activity_id)?;
        let activity_domain = self.activity_domain(&raw);
        let track = if raw.kind == SYNC {
            let api = activity_domain
                .and_then(|domain| self.apis.get(&(raw.instance, domain, raw.correlation)));
            if let Some(api) = api {
                out.native_thread(at, raw.pid, raw.instance, api.tid, None)?
            } else {
                self.unattributed += 1;
                a.insert("cuda.unattributed".into(), Attribute::Bool(true));
                return Self::metadata(&raw, out, process, at, a);
            }
        } else {
            if raw.device == UNKNOWN || raw.context == UNKNOWN || raw.stream == UNKNOWN {
                self.unattributed += 1;
                a.insert("cuda.unattributed".into(), Attribute::Bool(true));
                return Self::metadata(&raw, out, process, at, a);
            }
            self.gpu_track(&raw, out, process, at)?
        };
        out.emit(Event {
            timestamp_ns: at,
            attributes: a,
            payload: Payload::Span(Span {
                track_id: track,
                name,
                category: Category::Cuda,
                start_ns: raw.start_ns - origin,
                duration_ns: raw.end_ns - raw.start_ns,
                correlation_id: Some(correlation),
            }),
        })?;
        if raw.runtime_correlation != 0 {
            let api = Self::api_corr(out, &raw, DOMAIN_RUNTIME, raw.runtime_correlation)?;
            Self::edge(
                out,
                at,
                api,
                correlation,
                "CUPTI runtime activity correlation",
            )?;
        }
        if raw.correlation != 0 {
            if let Some(api_domain) = activity_domain {
                let api = Self::api_corr(out, &raw, api_domain, raw.correlation)?;
                Self::edge(out, at, api, correlation, "CUPTI activity correlation")?;
            }
        } else if raw.runtime_correlation == 0 {
            self.unresolved += 1;
        }
        Ok(())
    }
    fn finish(&self, out: &mut CaptureSink, at: u64) -> Result<()> {
        for (name, value) in [
            ("cuda.invalid_timing_records", self.invalid_timing),
            ("cuda.unattributed_records", self.unattributed),
            ("cuda.unresolved_correlations", self.unresolved),
            ("cuda.ambiguous_correlations", self.ambiguous),
        ] {
            let mut event = Event::new(
                at,
                Payload::Counter(CounterSample {
                    track_id: 0,
                    timestamp_ns: at,
                    name: name.into(),
                    value: CounterValue::Integer(value),
                    unit: "records".into(),
                }),
            );
            event
                .attributes
                .insert("cuda.role".into(), Attribute::Text("diagnostic".into()));
            out.reliable(event)?;
        }
        Ok(())
    }
}

pub(crate) fn receive(
    socket: &UnixDatagram,
    mapping: &Mapping,
    stop: &AtomicBool,
    sink: &Mutex<CaptureSink>,
    origin: u64,
    capacity: usize,
) -> Result<Outcome> {
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let worker = thread::Builder::new()
            .name("rprof-cuda-ingress".into())
            .spawn_scoped(scope, || pump(socket, mapping, stop, sender))?;
        let result = consume(receiver, mapping, sink, origin);
        if result.is_err() {
            stop.store(true, Ordering::Release);
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("CUDA ingress thread panicked"))??;
        result
    })
}
fn consume(
    receiver: Receiver<CudaEvent>,
    mapping: &Mapping,
    sink: &Mutex<CaptureSink>,
    origin: u64,
) -> Result<Outcome> {
    let mut decode = Decode::default();
    let mut received = 0;
    for raw in receiver {
        if raw.magic != MAGIC
            || raw.version != VERSION
            || !(RUNTIME..=EXTERNAL_CORRELATION).contains(&raw.kind)
        {
            continue;
        }
        received += 1;
        let at = monotonic_ns()?.saturating_sub(origin);
        let mut out = sink
            .lock()
            .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
        out.set_ingress_loss(
            "cuda",
            mapping
                .stats()
                .dropped
                .load(Ordering::Relaxed)
                .saturating_add(mapping.stats().source_dropped.load(Ordering::Relaxed)),
        );
        decode.event(raw, &mut out, origin, at)?;
    }
    let stats = mapping.stats();
    let attempted = stats.attempted.load(Ordering::Acquire);
    let dropped = attempted.saturating_sub(received);
    let source_dropped = stats.source_dropped.load(Ordering::Acquire);
    let mut out = sink
        .lock()
        .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
    out.set_ingress_loss("cuda", dropped.saturating_add(source_dropped));
    decode.finish(&mut out, monotonic_ns()?.saturating_sub(origin))?;
    Ok(Outcome {
        attempted,
        received,
        dropped,
        initialized: stats.initialized.load(Ordering::Acquire),
        source_dropped,
        source_errors: stats.source_errors.load(Ordering::Acquire),
        finalized: stats.finalized.load(Ordering::Acquire),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rprof_capture::{Recorder, RecorderConfig};
    use rprof_core::{
        Track,
        stream::{Collector, End, EndReason, Header, Loss, Shard},
    };
    use uuid::Uuid;

    fn setup() -> (tempfile::TempDir, std::path::PathBuf, Uuid, CaptureSink) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cuda.jsonl");
        let id = Uuid::new_v4();
        let config = RecorderConfig::default();
        let recorder = Recorder::create(
            &path,
            Header::new(Uuid::new_v4()),
            Shard {
                shard_id: id,
                parent_shard_id: None,
                hostname: "host".into(),
                boot_id: "boot".into(),
                monotonic_origin_ns: 100,
                started_unix_ns: 0,
                command: vec![],
                supervisor_pid: 1,
                collectors: vec![Collector {
                    name: "cuda".into(),
                    version: "test".into(),
                }],
                config: config.metadata(),
            },
            config,
        )
        .unwrap();
        let mut sink = CaptureSink::new(recorder);
        sink.reliable(Event::new(
            0,
            Payload::Track(Track {
                id: 0,
                parent_id: None,
                name: "root".into(),
                rank: None,
                kind: TrackKind::Process { pid: 1 },
            }),
        ))
        .unwrap();
        (dir, path, id, sink)
    }
    fn raw(kind: u16, id: u64, correlation: u64) -> CudaEvent {
        let mut r: CudaEvent = unsafe { std::mem::zeroed() };
        r.kind = kind;
        r.pid = 11;
        r.instance = 12;
        r.activity_id = id;
        r.correlation = correlation;
        r.start_ns = 110;
        r.end_ns = 140;
        r.device = 0;
        r.context = 2;
        r.stream = 3;
        r.name[..4].copy_from_slice(b"test");
        r
    }
    fn finish(decode: Decode, mut sink: CaptureSink, id: Uuid) {
        decode.finish(&mut sink, 1000).unwrap();
        sink.recorder
            .finish(End {
                shard_id: id,
                duration_ns: 1000,
                exit_code: Some(0),
                signal: None,
                interrupted_by: None,
                reason: EndReason::Completed,
                loss: Loss::default(),
            })
            .unwrap();
    }
    #[test]
    fn gpu_identity_correlations_and_exact_summary_survive_round_trip() {
        let (_dir, path, id, mut sink) = setup();
        let mut decode = Decode::default();
        let mut api = raw(API_END, 1, 7);
        api.api_domain = DOMAIN_RUNTIME;
        api.tid = 42;
        decode.event(api, &mut sink, 100, 1000).unwrap();
        let mut kernel = raw(KERNEL, 2, 7);
        decode.event(kernel, &mut sink, 100, 1000).unwrap();
        kernel.activity_id = 3;
        kernel.stream = 4;
        kernel.start_ns = 120;
        decode.event(kernel, &mut sink, 100, 1000).unwrap();
        let mut copy = raw(MEMCPY, 4, 0);
        copy.runtime_correlation = 7;
        copy.bytes = 9_007_199_254_740_993;
        decode.event(copy, &mut sink, 100, 1000).unwrap();
        finish(decode, sink, id);
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        let cuda = report.cuda.as_ref().unwrap();
        assert_eq!(cuda.devices[0].kernels, "2");
        assert_eq!(cuda.devices[0].kernel_ns, "50");
        assert_eq!(cuda.devices[0].window_ns, "30");
        assert_eq!(cuda.devices[0].copy_bytes, "9007199254740993");
        let gpu: Vec<_> = report
            .spans
            .iter()
            .filter(|s| s.attributes.get("cuda.role").is_some_and(|r| r == "kernel"))
            .collect();
        assert_eq!(gpu.len(), 2);
        assert_ne!(gpu[0].track, gpu[1].track);
        for span in gpu {
            assert!(!span.attributes.contains_key("tid"));
            assert!(
                report
                    .dependencies
                    .iter()
                    .any(|e| Some(&e.to) == span.correlation.as_ref())
            );
        }
        assert!(report.tracks.iter().any(|t| t.name == "thread 42"));
        assert!(!report.tracks.iter().any(|t| t.name == "thread 0"));
    }
    #[test]
    fn missing_timing_and_unattributed_sync_never_create_gpu_spans() {
        let (_dir, path, id, mut sink) = setup();
        let mut decode = Decode::default();
        let mut kernel = raw(KERNEL, 1, 0);
        kernel.start_ns = 0;
        kernel.flags = TIMING_UNAVAILABLE;
        decode.event(kernel, &mut sink, 100, 1000).unwrap();
        decode
            .event(raw(SYNC, 2, 81), &mut sink, 100, 1000)
            .unwrap();
        let mut api = raw(API_BEGIN, 3, 7);
        api.api_domain = DOMAIN_RUNTIME;
        api.tid = 42;
        decode.event(api, &mut sink, 100, 1000).unwrap();
        api.kind = API_END;
        api.start_ns = 0;
        api.flags = TIMING_UNAVAILABLE;
        decode.event(api, &mut sink, 100, 1000).unwrap();
        finish(decode, sink, id);
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        assert_eq!(report.spans.len(), 1);
        assert!(report.spans[0].incomplete);
        assert_eq!(report.spans[0].category, "cuda");
        assert!(report.cuda.as_ref().unwrap().devices.is_empty());
        let source = &report.cuda.as_ref().unwrap().sources[0];
        assert_eq!(source.invalid_timing, "2");
        assert_eq!(source.unattributed, "1");
        assert!(!source.finalized);
    }
    #[test]
    fn synchronization_uses_the_same_domain_for_thread_and_dependency() {
        let (_dir, path, id, mut sink) = setup();
        let mut decode = Decode::default();
        let mut api = raw(API_END, 1, 7);
        api.api_domain = DOMAIN_RUNTIME;
        api.tid = 42;
        decode.event(api, &mut sink, 100, 1000).unwrap();
        decode.event(raw(SYNC, 2, 7), &mut sink, 100, 1000).unwrap();
        api.api_domain = DOMAIN_DRIVER;
        api.tid = 43;
        decode.event(api, &mut sink, 100, 1000).unwrap();
        decode.event(raw(SYNC, 3, 7), &mut sink, 100, 1000).unwrap();
        finish(decode, sink, id);
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        let sync: Vec<_> = report
            .spans
            .iter()
            .filter(|s| s.attributes["cuda.role"] == "synchronization")
            .collect();
        assert_eq!(sync.len(), 1);
        let runtime = report
            .spans
            .iter()
            .find(|s| {
                s.attributes["cuda.role"] == "api" && s.attributes["cuda.api_domain"] == "runtime"
            })
            .unwrap();
        assert_eq!(sync[0].track, runtime.track);
        assert!(
            report
                .dependencies
                .iter()
                .any(|e| Some(&e.from) == runtime.correlation.as_ref()
                    && Some(&e.to) == sync[0].correlation.as_ref())
        );
        assert_eq!(report.cuda.unwrap().sources[0].ambiguous_correlations, "1");
    }
    #[test]
    fn api_correlation_cache_is_bounded_and_eviction_is_reported() {
        let mut decode = Decode::default();
        let mut api = raw(API_END, 1, 1);
        api.api_domain = DOMAIN_RUNTIME;
        api.tid = 42;
        for id in 1..=MAX_METADATA as u64 + 2 {
            api.correlation = id;
            decode.remember(&api);
        }
        assert_eq!(decode.apis.len(), MAX_METADATA);
        assert_eq!(decode.recent.len(), MAX_METADATA);
        assert_eq!(decode.activity_domain(&raw(KERNEL, 2, 1)), None);
        assert_eq!(decode.unresolved, 1);
    }

    #[test]
    fn source_drops_and_partial_multi_image_finalization_remain_distinct() {
        let (_dir, path, id, mut sink) = setup();
        for (name, value) in [
            ("cuda.initialized_images", 2),
            ("cuda.finalized", 1),
            ("cuda.attempted_events", 100),
            ("cuda.received_events", 93),
            ("cuda.ingress_dropped_events", 7),
            ("cuda.cupti_dropped_records", 11),
        ] {
            let mut event = Event::new(
                1000,
                Payload::Counter(CounterSample {
                    track_id: 0,
                    timestamp_ns: 1000,
                    name: name.into(),
                    value: CounterValue::Integer(value),
                    unit: "count".into(),
                }),
            );
            event
                .attributes
                .insert("cuda.role".into(), Attribute::Text("diagnostic".into()));
            sink.reliable(event).unwrap();
        }
        sink.set_ingress_loss("cuda", 18);
        finish(Decode::default(), sink, id);
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        let source = &report.cuda.as_ref().unwrap().sources[0];
        assert_eq!(source.ingress_dropped, "7");
        assert_eq!(source.cupti_dropped, "11");
        assert!(
            !source.finalized,
            "one flushed image cannot finalize another image"
        );
        assert_eq!(report.summary.dropped, "18");
    }
}
