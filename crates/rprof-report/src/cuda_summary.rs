use crate::model::*;
use rprof_core::stream::{Attribute, Payload, Record};
use std::collections::{BTreeMap, HashMap};

type Image = (String, u64);
type DeviceKey = (String, u64, u64);
type ActivityKey = (String, u64, u64, u64, u64, String, String);
#[derive(Default)]
struct Runtime {
    pid: u64,
    cupti: u64,
    cuda: u64,
    initialized: bool,
    clock: String,
    capabilities: BTreeMap<String, CudaCapability>,
    errors: Vec<String>,
}
#[derive(Default)]
struct Device {
    pid: u64,
    name: String,
    identity: String,
    visible: Option<bool>,
    memory: Option<u64>,
    major: Option<u64>,
    minor: Option<u64>,
    sms: Option<u64>,
    kernels: u64,
    copies: u64,
    memsets: u64,
    kernel_ns: u128,
    copy_ns: u128,
    copy_bytes: u128,
    memset_bytes: u128,
    start: Option<u64>,
    end: u64,
}
#[derive(Default)]
struct Totals {
    count: u64,
    errors: u64,
    total: u128,
    max: u64,
    bytes: u128,
}
#[derive(Default)]
pub(crate) struct CudaAnalysis {
    runtimes: BTreeMap<Image, Runtime>,
    devices: BTreeMap<DeviceKey, Device>,
    activities: BTreeMap<ActivityKey, Totals>,
    apis: BTreeMap<(String, u64, String, String), Totals>,
    sources: BTreeMap<String, BTreeMap<String, u64>>,
    pub omitted: u64,
}
fn text<'a>(a: &'a BTreeMap<String, Attribute>, key: &str) -> Option<&'a str> {
    if let Some(Attribute::Text(v)) = a.get(key) {
        Some(v)
    } else {
        None
    }
}
fn number(a: &BTreeMap<String, Attribute>, key: &str) -> Option<u64> {
    if let Some(Attribute::Unsigned(v)) = a.get(key) {
        Some(*v)
    } else {
        None
    }
}
fn signed(a: &BTreeMap<String, Attribute>, key: &str) -> Option<i64> {
    if let Some(Attribute::Signed(v)) = a.get(key) {
        Some(*v)
    } else {
        None
    }
}
fn flag(a: &BTreeMap<String, Attribute>, key: &str) -> bool {
    matches!(a.get(key), Some(Attribute::Bool(true)))
}
fn bounded(text: &str) -> String {
    text.chars().take(256).collect()
}

impl CudaAnalysis {
    pub fn observe(&mut self, record: &Record) {
        let Record::Chunk(chunk) = record else {
            return;
        };
        let shard = chunk.shard_id.to_string();
        for event in &chunk.events {
            let a = &event.attributes;
            let Some(role) = text(a, "cuda.role") else {
                continue;
            };
            let source = self.sources.entry(shard.clone()).or_default();
            if role == "diagnostic" {
                if let Payload::Counter(counter) = &event.payload {
                    if let rprof_core::CounterValue::Integer(value) = counter.value {
                        if [
                            "cuda.initialized_images",
                            "cuda.finalized",
                            "cuda.attempted_events",
                            "cuda.received_events",
                            "cuda.ingress_dropped_events",
                            "cuda.cupti_dropped_records",
                            "cuda.source_errors",
                            "cuda.invalid_timing_records",
                            "cuda.unattributed_records",
                            "cuda.unresolved_correlations",
                            "cuda.ambiguous_correlations",
                        ]
                        .contains(&counter.name.as_str())
                        {
                            source.insert(counter.name.clone(), value);
                        }
                    }
                }
                continue;
            }
            let image = number(a, "process_instance").unwrap_or(0);
            let image_key = (shard.clone(), image);
            let pid = number(a, "pid").unwrap_or(0);
            if !self.runtimes.contains_key(&image_key) && self.runtimes.len() >= 4096 {
                self.omitted += 1;
                continue;
            }
            let runtime = self.runtimes.entry(image_key).or_default();
            runtime.pid = pid;
            let name = bounded(text(a, "cuda.name").unwrap_or("unknown"));
            match role {
                "runtime" => {
                    runtime.cupti = signed(a, "cuda.value").unwrap_or(0).max(0) as u64;
                    runtime.cuda = number(a, "cuda.aux0").unwrap_or(0);
                    runtime.initialized = flag(a, "cuda.initialized");
                    runtime.clock = bounded(text(a, "cuda.clock_method").unwrap_or("unknown"));
                }
                "capability" => {
                    if runtime.capabilities.len() < 64 || runtime.capabilities.contains_key(&name) {
                        runtime.capabilities.insert(
                            name.clone(),
                            CudaCapability {
                                name: name.clone(),
                                supported: flag(a, "cuda.supported"),
                                optional: flag(a, "cuda.optional"),
                                result: signed(a, "cuda.result").unwrap_or(-1).to_string(),
                                detail: bounded(text(a, "cuda.detail").unwrap_or("")),
                            },
                        );
                    } else {
                        self.omitted += 1;
                    }
                }
                "error" => {
                    if runtime.errors.len() < 32 {
                        runtime.errors.push(format!(
                            "{name}: {}",
                            bounded(text(a, "cuda.detail").unwrap_or("CUPTI error"))
                        ));
                    } else {
                        self.omitted += 1;
                    }
                }
                _ => (),
            }
            if role == "device" {
                if let Some(device) = number(a, "cuda.device") {
                    let key = (shard.clone(), image, device);
                    if !self.devices.contains_key(&key) && self.devices.len() >= 4096 {
                        self.omitted += 1;
                        continue;
                    }
                    let d = self.devices.entry(key).or_default();
                    d.pid = pid;
                    d.name = name;
                    d.identity = bounded(text(a, "cuda.detail").unwrap_or(""));
                    d.visible = Some(flag(a, "cuda.visible"));
                    d.memory = number(a, "cuda.bytes");
                    d.major = number(a, "cuda.aux0");
                    d.minor = number(a, "cuda.aux1");
                    d.sms = number(a, "cuda.aux2");
                }
            }
            let Payload::Span(span) = &event.payload else {
                continue;
            };
            if role == "api" {
                let key = (
                    shard.clone(),
                    image,
                    bounded(text(a, "cuda.api_domain").unwrap_or("unknown")),
                    bounded(&span.name),
                );
                if !self.apis.contains_key(&key) && self.apis.len() >= 8192 {
                    self.omitted += 1;
                    continue;
                }
                let t = self.apis.entry(key).or_default();
                t.count += 1;
                t.total += span.duration_ns as u128;
                t.max = t.max.max(span.duration_ns);
                if signed(a, "cuda.result").is_some_and(|r| r != 0) {
                    t.errors += 1;
                }
            } else if matches!(role, "kernel" | "memcpy" | "memset") {
                let (Some(device), Some(context), Some(stream)) = (
                    number(a, "cuda.device"),
                    number(a, "cuda.context"),
                    number(a, "cuda.stream"),
                ) else {
                    continue;
                };
                let dk = (shard.clone(), image, device);
                if !self.devices.contains_key(&dk) && self.devices.len() >= 4096 {
                    self.omitted += 1;
                    continue;
                }
                let d = self.devices.entry(dk).or_default();
                d.pid = pid;
                d.start = Some(
                    d.start
                        .map_or(span.start_ns, |start| start.min(span.start_ns)),
                );
                d.end = d.end.max(span.start_ns.saturating_add(span.duration_ns));
                let bytes = number(a, "cuda.bytes").unwrap_or(0) as u128;
                match role {
                    "kernel" => {
                        d.kernels += 1;
                        d.kernel_ns += span.duration_ns as u128;
                    }
                    "memcpy" => {
                        d.copies += 1;
                        d.copy_ns += span.duration_ns as u128;
                        d.copy_bytes += bytes;
                    }
                    "memset" => {
                        d.memsets += 1;
                        d.memset_bytes += bytes;
                    }
                    _ => (),
                }
                let mut name = bounded(&span.name);
                if flag(a, "cuda.name_truncated") {
                    name.push_str(" [truncated name group]");
                }
                let key = (
                    shard.clone(),
                    image,
                    device,
                    context,
                    stream,
                    role.into(),
                    name,
                );
                if !self.activities.contains_key(&key) && self.activities.len() >= 8192 {
                    self.omitted += 1;
                    continue;
                }
                let t = self.activities.entry(key).or_default();
                t.count += 1;
                t.total += span.duration_ns as u128;
                t.max = t.max.max(span.duration_ns);
                t.bytes += bytes;
            }
        }
    }
    pub fn finish(self, shards: &[ReportShard], warnings: &mut Vec<String>) -> Option<CudaSummary> {
        if self.sources.is_empty() {
            return None;
        }
        let domains: HashMap<_, _> = shards
            .iter()
            .map(|s| (s.id.as_str(), s.domain.as_str()))
            .collect();
        if self.omitted > 0 {
            warnings.push(format!(
                "{} CUDA summary observations omitted by metadata limits.",
                self.omitted
            ));
        }
        let runtimes = self
            .runtimes
            .into_iter()
            .map(|((shard, image), r)| {
                for error in &r.errors {
                    warnings.push(format!("CUDA process {}: {error}", r.pid));
                }
                let unavailable: Vec<_> = r
                    .capabilities
                    .values()
                    .filter(|c| !c.supported)
                    .map(|c| c.name.as_str())
                    .collect();
                if !unavailable.is_empty() {
                    warnings.push(format!(
                        "CUDA process {} unavailable activities/capabilities: {}",
                        r.pid,
                        unavailable.join(", ")
                    ));
                }
                CudaRuntime {
                    domain: domains[shard.as_str()].into(),
                    shard,
                    instance: image.to_string(),
                    pid: r.pid.to_string(),
                    cupti_version: r.cupti.to_string(),
                    cuda_version: r.cuda.to_string(),
                    initialized: r.initialized,
                    clock_method: r.clock,
                    clock_uncertainty: None,
                    capabilities: r.capabilities.into_values().collect(),
                    errors: r.errors,
                }
            })
            .collect();
        let devices = self
            .devices
            .into_iter()
            .map(|((shard, image, device), d)| CudaDevice {
                domain: domains[shard.as_str()].into(),
                shard,
                instance: image.to_string(),
                pid: d.pid.to_string(),
                device: device.to_string(),
                name: if d.name.is_empty() {
                    format!("GPU {device} (metadata unavailable)")
                } else {
                    d.name
                },
                identity: d.identity,
                visible: d.visible,
                memory_bytes: d.memory.map(|n| n.to_string()),
                compute_major: d.major.map(|n| n.to_string()),
                compute_minor: d.minor.map(|n| n.to_string()),
                multiprocessors: d.sms.map(|n| n.to_string()),
                kernels: d.kernels.to_string(),
                copies: d.copies.to_string(),
                memsets: d.memsets.to_string(),
                kernel_ns: d.kernel_ns.to_string(),
                copy_ns: d.copy_ns.to_string(),
                copy_bytes: d.copy_bytes.to_string(),
                memset_bytes: d.memset_bytes.to_string(),
                window_ns: d.start.map_or(0, |s| d.end.saturating_sub(s)).to_string(),
            })
            .collect();
        let activities = self
            .activities
            .into_iter()
            .map(
                |((shard, image, device, context, stream, kind, name), t)| CudaActivity {
                    domain: domains[shard.as_str()].into(),
                    shard,
                    instance: image.to_string(),
                    device: device.to_string(),
                    context: context.to_string(),
                    stream: stream.to_string(),
                    kind,
                    name,
                    count: t.count.to_string(),
                    total_ns: t.total.to_string(),
                    max_ns: t.max.to_string(),
                    bytes: t.bytes.to_string(),
                },
            )
            .collect();
        let apis = self
            .apis
            .into_iter()
            .map(|((shard, image, api_domain, name), t)| CudaApi {
                domain: domains[shard.as_str()].into(),
                shard,
                instance: image.to_string(),
                api_domain,
                name,
                calls: t.count.to_string(),
                errors: t.errors.to_string(),
                total_ns: t.total.to_string(),
                max_ns: t.max.to_string(),
            })
            .collect();
        let sources = self.sources.into_iter().map(|(shard, values)| {
            let get = |key: &str| values.get(key).copied().unwrap_or(0);
            if !values.contains_key("cuda.finalized") {
                warnings.push(format!("CUDA diagnostics are incomplete for shard {shard}; absent counts are lower bounds."));
            }
            if get("cuda.initialized_images") == 0 {
                warnings.push(format!("No confirmed CUPTI initialization for shard {shard}."));
            }
            let finalized = get("cuda.finalized") >= get("cuda.initialized_images").max(1);
            if !finalized {
                warnings.push(format!("CUDA final activity flush is unconfirmed for shard {shard}; GPU work may be missing."));
            }
            for (key, label) in [
                ("cuda.cupti_dropped_records", "CUPTI activity drops"),
                ("cuda.source_errors", "source errors"),
                ("cuda.invalid_timing_records", "records without usable timing"),
                ("cuda.unattributed_records", "unattributed records"),
                ("cuda.unresolved_correlations", "unresolved activity correlations"),
                ("cuda.ambiguous_correlations", "ambiguous correlations"),
            ] {
                if get(key) > 0 {
                    warnings.push(format!("CUDA shard {shard}: {} {label}.", get(key)));
                }
            }
            CudaSource {
                domain: domains[shard.as_str()].into(), shard,
                initialized_images: get("cuda.initialized_images").to_string(), finalized,
                attempted: get("cuda.attempted_events").to_string(),
                received: get("cuda.received_events").to_string(),
                ingress_dropped: get("cuda.ingress_dropped_events").to_string(),
                cupti_dropped: get("cuda.cupti_dropped_records").to_string(),
                source_errors: get("cuda.source_errors").to_string(),
                invalid_timing: get("cuda.invalid_timing_records").to_string(),
                unattributed: get("cuda.unattributed_records").to_string(),
                unresolved_correlations: get("cuda.unresolved_correlations").to_string(),
                ambiguous_correlations: get("cuda.ambiguous_correlations").to_string(),
            }
        }).collect();
        Some(CudaSummary {
            runtimes,
            devices,
            activities,
            apis,
            sources,
        })
    }
}
