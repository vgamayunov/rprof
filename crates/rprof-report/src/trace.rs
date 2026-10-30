use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    path::Path,
    str::FromStr,
};

use anyhow::{Context, Result, ensure};
use rprof_core::{
    TrackKind,
    stream::{Payload, Record},
};
use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Number, Value, json};
use uuid::Uuid;

use crate::{
    Limits, analyze,
    analyze::{attributes, category, counter, key, scan},
};

const MAX_FLOW_ENDPOINTS: usize = 200_000;
const MAX_OPEN_ACTIVITIES: usize = 200_000;

#[derive(Clone)]
struct Endpoint {
    pid: u32,
    tid: u32,
    start: u128,
    end: u128,
}

struct OpenActivity {
    endpoint: Endpoint,
    name: String,
    correlation: String,
    attributes: BTreeMap<String, String>,
    shard: String,
}

fn microseconds(ns: u128) -> Result<Box<RawValue>> {
    Ok(RawValue::from_string(format!(
        "{}.{:03}",
        ns / 1000,
        ns % 1000
    ))?)
}

struct Events<W> {
    out: W,
    first: bool,
}

impl<W: Write> Events<W> {
    fn emit(&mut self, value: impl Serialize) -> Result<()> {
        if !self.first {
            self.out.write_all(b",\n")?;
        }
        self.first = false;
        serde_json::to_writer(&mut self.out, &value)?;
        Ok(())
    }

    fn timed(&mut self, fields: Value, timestamp: u128, duration: Option<u128>) -> Result<()> {
        #[derive(Serialize)]
        struct Timed {
            #[serde(flatten)]
            fields: Value,
            ts: Box<RawValue>,
            #[serde(skip_serializing_if = "Option::is_none")]
            dur: Option<Box<RawValue>>,
        }
        self.emit(Timed {
            fields,
            ts: microseconds(timestamp)?,
            dur: duration.map(microseconds).transpose()?,
        })
    }

    fn flow(&mut self, id: usize, from: &Endpoint, to: &Endpoint, name: &str) -> Result<()> {
        let start = from.end.min(to.start).max(from.start.min(to.start));
        self.timed(
            json!({"ph":"s", "cat":"rprof.correlation", "name":name, "id":id.to_string(),
            "pid":from.pid, "tid":from.tid}),
            start,
            None,
        )?;
        self.timed(
            json!({"ph":"f", "cat":"rprof.correlation", "name":name, "id":id.to_string(), "bp":"e",
            "pid":to.pid, "tid":to.tid}),
            to.start,
            None,
        )
    }
}

/// Export all spans/counters using a streaming second scan. Multiple independent
/// clocks require explicit domain selection; no fictitious cross-host alignment.
/// Flow reconstruction is separately bounded and omissions are reported in metadata.
pub fn write_trace(path: &Path, mut out: impl Write, domain: Option<&str>) -> Result<()> {
    let report = analyze(
        path,
        Limits {
            retain_timeline: false,
            ..Default::default()
        },
    )?;
    let selected = match domain {
        Some(id) => {
            ensure!(
                report.domains.iter().any(|d| d.id == id),
                "unknown domain {id}; list domains with rprof report"
            );
            id.to_string()
        }
        None => {
            ensure!(
                report.domains.len() <= 1,
                "capture has {} unaligned clock domains; select --domain ID (see text report)",
                report.domains.len()
            );
            report
                .domains
                .first()
                .map_or(String::new(), |d| d.id.clone())
        }
    };
    let shards: HashMap<_, _> = report
        .shards
        .iter()
        .filter(|s| s.domain == selected)
        .enumerate()
        .map(|(i, shard)| (shard.id.clone(), (i as u32 + 1, shard)))
        .collect();
    out.write_all(b"{\"traceEvents\":[\n")?;
    let mut events = Events {
        out: &mut out,
        first: true,
    };
    let mut tracks = HashMap::<(Uuid, u64), u32>::new();
    let mut correlations = BTreeMap::<String, Vec<Endpoint>>::new();
    let mut dependencies = Vec::new();
    let mut open = HashMap::<(Uuid, u64, u64), OpenActivity>::new();
    let mut endpoints = 0;
    let mut omitted_flows = 0u64;
    let mut observed = HashMap::<String, u128>::new();
    scan(path, Some(report.source_bytes), |record| {
        let id = match &record {
            Record::Header(_) => return Ok(()),
            Record::Shard(s) => s.shard_id,
            Record::Chunk(c) => c.shard_id,
            Record::End(e) => e.shard_id,
        };
        let shard_key = id.to_string();
        let Some((pid, shard)) = shards.get(&shard_key) else {
            return Ok(());
        };
        let offset: u128 = shard.origin_ns.parse()?;
        match record {
            Record::Shard(source) => {
                events.emit(json!({"ph":"M", "name":"process_name", "pid":pid, "args":{"name":format!("{} / {}", source.hostname, &shard_key[..8])}}))?;
                events.emit(json!({"ph":"M", "name":"rprof.shard", "pid":pid, "args":{
                    "shard":shard_key, "command":source.command, "clock_domain":selected,
                    "monotonic_origin_ns":source.monotonic_origin_ns.to_string(), "domain_offset_ns":offset.to_string()
                }}))?;
            }
            Record::Chunk(chunk) => {
                for event in chunk.events {
                    let now = offset + event.timestamp_ns as u128;
                    observed.insert(shard_key.clone(), now);
                    let attrs = attributes(event.attributes);
                    match event.payload {
                        Payload::Track(track) => {
                            let tid = tracks.len() as u32 + 1;
                            tracks.insert((id, track.id), tid);
                            let kind = match track.kind {
                                TrackKind::Process { .. } => "process",
                                TrackKind::Thread { .. } => "thread",
                                TrackKind::GpuDevice { .. } => "GPU",
                                TrackKind::GpuStream { .. } => "GPU stream",
                                TrackKind::Io { .. } => "I/O",
                            };
                            let name = format!(
                                "{} [{}{}]",
                                track.name,
                                kind,
                                track.rank.map_or(String::new(), |r| format!(", rank {r}"))
                            );
                            events.emit(json!({"ph":"M", "name":"thread_name", "pid":pid, "tid":tid, "args":{"name":name}}))?;
                            events.emit(json!({"ph":"M", "name":"rprof.track", "pid":pid, "tid":tid, "args":{
                            "id":track.id.to_string(), "parent":track.parent_id.map(|p|p.to_string()), "shard":shard_key, "rank":track.rank
                        }}))?;
                        }
                        Payload::Span(span) => {
                            let tid = *tracks
                                .get(&(id, span.track_id))
                                .context("unknown trace track")?;
                            let start = offset + span.start_ns as u128;
                            let endpoint = Endpoint {
                                pid: *pid,
                                tid,
                                start,
                                end: start + span.duration_ns as u128,
                            };
                            if let Some(correlation) = span.correlation_id {
                                open.remove(&(id, span.track_id, correlation));
                                if endpoints < MAX_FLOW_ENDPOINTS {
                                    correlations
                                        .entry(key(id, correlation))
                                        .or_default()
                                        .push(endpoint);
                                    endpoints += 1;
                                } else {
                                    omitted_flows += 1;
                                }
                            }
                            events.timed(json!({"ph":"X", "pid":pid, "tid":tid, "name":span.name, "cat":category(&span.category),
                            "args":{"rprof_start_ns":start.to_string(), "rprof_duration_ns":span.duration_ns.to_string(),
                                "correlation":span.correlation_id.map(|c|key(id,c)), "attributes":attrs,
                                "incomplete":attrs.get("mpi.request_state").is_some_and(|v|v=="tracking_released") || attrs.get("mpi.abort_entry").is_some_and(|v|v=="true")}}), start, Some(span.duration_ns as u128))?;
                        }
                        Payload::Counter(sample) => {
                            let tid = *tracks
                                .get(&(id, sample.track_id))
                                .context("unknown trace counter track")?;
                            let timestamp = offset + sample.timestamp_ns as u128;
                            let (value, value_type) = counter(sample.value);
                            events.timed(json!({"ph":"C", "pid":pid, "tid":tid, "name":sample.name, "cat":"counter",
                            "args":{"value":Value::Number(Number::from_str(&value)?)},
                            "rprof":{"exact_value":value, "value_type":value_type, "unit":sample.unit, "timestamp_ns":timestamp.to_string(), "attributes":attrs}}), timestamp, None)?;
                        }
                        Payload::ActivityStart {
                            track_id,
                            name,
                            correlation_id,
                            start_ns,
                        } => {
                            ensure!(
                                open.len() < MAX_OPEN_ACTIVITIES,
                                "too many simultaneous open activities for trace export"
                            );
                            let tid = *tracks
                                .get(&(id, track_id))
                                .context("unknown trace activity track")?;
                            open.insert(
                                (id, track_id, correlation_id),
                                OpenActivity {
                                    endpoint: Endpoint {
                                        pid: *pid,
                                        tid,
                                        start: offset
                                            + start_ns.unwrap_or(event.timestamp_ns) as u128,
                                        end: offset
                                            + start_ns.unwrap_or(event.timestamp_ns) as u128,
                                    },
                                    name,
                                    correlation: key(id, correlation_id),
                                    attributes: attrs,
                                    shard: shard_key.clone(),
                                },
                            );
                        }
                        Payload::Dependency {
                            from_correlation_id,
                            to_correlation_id,
                            relation,
                        } => {
                            if dependencies.len() < MAX_FLOW_ENDPOINTS {
                                dependencies.push((
                                    key(id, from_correlation_id),
                                    key(id, to_correlation_id),
                                    relation,
                                ));
                            } else {
                                omitted_flows += 1;
                            }
                        }
                        Payload::Heartbeat => (),
                    }
                }
            }
            Record::End(end) => {
                events.emit(json!({"ph":"M", "name":"rprof.outcome", "pid":pid, "args":{
                    "exit_code":end.exit_code, "signal":end.signal, "interrupted_by":end.interrupted_by,
                    "reason":format!("{:?}",end.reason), "dropped":end.loss.total().to_string()
                }}))?;
            }
            Record::Header(_) => (),
        }
        Ok(())
    })?;
    let mut open: Vec<_> = open.into_iter().collect();
    open.sort_by_key(|(key, _)| *key);
    for (_, activity) in open {
        let end = observed
            .get(&activity.shard)
            .copied()
            .unwrap_or(activity.endpoint.start);
        events.timed(json!({"ph":"X", "pid":activity.endpoint.pid, "tid":activity.endpoint.tid,
            "cat":"rprof.incomplete", "name":activity.name,
            "args":{"incomplete":true, "correlation":activity.correlation, "attributes":activity.attributes,
                "rprof_start_ns":activity.endpoint.start.to_string(), "rprof_duration_ns":end.saturating_sub(activity.endpoint.start).to_string()}}), activity.endpoint.start, Some(end.saturating_sub(activity.endpoint.start)))?;
    }
    let mut flow_id = 0;
    for endpoints in correlations.values_mut() {
        endpoints.sort_by_key(|e| (e.start, e.end, e.pid, e.tid));
        for pair in endpoints.windows(2) {
            events.flow(flow_id, &pair[0], &pair[1], "shared correlation")?;
            flow_id += 1;
        }
    }
    for (from, to, relation) in dependencies {
        if let (Some(from), Some(to)) = (
            correlations.get(&from).and_then(|e| e.first()),
            correlations.get(&to).and_then(|e| e.first()),
        ) {
            if from.start <= to.start {
                events.flow(flow_id, from, to, &relation)?;
                flow_id += 1;
            } else {
                omitted_flows += 1;
            }
        } else {
            omitted_flows += 1;
        }
    }
    out.write_all(b"\n],\"displayTimeUnit\":\"ns\",\"rprof\":")?;
    serde_json::to_writer(
        &mut out,
        &json!({
            "session":report.summary.session_id, "provenance":report.provenance,
            "clock_domain":selected, "complete":report.summary.complete, "warnings":report.summary.warnings,
            "dropped":report.summary.dropped, "omitted_flow_endpoints_or_links":omitted_flows.to_string(),
            "numeric_note":"Trace timestamps are microseconds; exact ns and integer counter values are retained as strings. Reference viewers may round numeric plots."
        }),
    )?;
    out.write_all(b"}\n")?;
    Ok(())
}
