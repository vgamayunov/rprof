use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Cursor, Read},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use rprof_capture::{Reader, Summary};
use rprof_core::{
    Capture, Category, CounterValue, TrackKind,
    stream::{
        Attribute, CaptureConfig, Chunk, Collector, End, EndReason, Event, Header, Loss, Payload,
        Record, Shard,
    },
};
use uuid::Uuid;

use crate::model::*;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Text summaries don't retain timeline events or track metadata.
    pub retain_timeline: bool,
    pub max_events: usize,
    pub max_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            retain_timeline: true,
            max_events: 200_000,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

pub(crate) struct Scan {
    pub summary: Summary,
    pub legacy: bool,
    pub source_bytes: u64,
}

/// Scan a fixed-length snapshot. A growing live capture cannot turn report
/// generation into an unbounded follow operation.
pub(crate) fn scan(
    path: &Path,
    snapshot: Option<u64>,
    mut on_record: impl FnMut(Record) -> Result<()>,
) -> Result<Scan> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let source_bytes = snapshot.unwrap_or(file.metadata()?.len());
    let mut probe = BufReader::new(file.take(source_bytes));
    let mut first = String::new();
    (&mut probe)
        .take(rprof_capture::MAX_FRAME_BYTES as u64)
        .read_line(&mut first)?;
    let stream = serde_json::from_str::<serde_json::Value>(&first)
        .ok()
        .is_some_and(|v| v.get("record").is_some());
    let input: Box<dyn BufRead> = if stream {
        Box::new(BufReader::new(File::open(path)?.take(source_bytes)))
    } else {
        ensure!(
            source_bytes <= 16 * 1024 * 1024,
            "legacy v1 capture exceeds 16 MiB (or stream header is invalid)"
        );
        let capture: Capture =
            serde_json::from_reader(BufReader::new(File::open(path)?.take(source_bytes)))
                .context("reading legacy capture (or invalid stream header)")?;
        ensure!(
            capture.schema_version == rprof_core::SCHEMA_VERSION,
            "unsupported legacy schema version {}",
            capture.schema_version
        );
        Box::new(Cursor::new(convert_legacy(capture)?))
    };
    let mut reader = Reader::new(input);
    while let Some(record) = reader.next_record()? {
        on_record(record)?;
    }
    Ok(Scan {
        summary: reader.summary()?,
        legacy: !stream,
        source_bytes,
    })
}

fn convert_legacy(capture: Capture) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut write = |record| -> Result<()> {
        serde_json::to_writer(&mut output, &record)?;
        output.push(b'\n');
        Ok(())
    };
    let id = Uuid::nil();
    write(Record::Header(Header::new(id)))?;
    write(Record::Shard(Shard {
        shard_id: id,
        parent_shard_id: None,
        hostname: "legacy (unknown host)".into(),
        boot_id: "legacy-unknown".into(),
        monotonic_origin_ns: 0,
        started_unix_ns: capture.metadata.started_unix_ns,
        command: capture.metadata.command,
        supervisor_pid: 0,
        collectors: capture
            .metadata
            .collectors
            .into_iter()
            .map(|name| Collector {
                name,
                version: "legacy".into(),
            })
            .collect(),
        config: CaptureConfig {
            chunk_events: 1,
            queue_chunks: 1,
            flush_interval_ms: 100,
        },
    }))?;
    let mut events = Vec::new();
    for track in capture.tracks {
        events.push(Event::new(0, Payload::Track(track)));
    }
    for span in capture.spans {
        let end = span
            .start_ns
            .checked_add(span.duration_ns)
            .context("legacy span overflow")?;
        events.push(Event::new(end, Payload::Span(span)));
    }
    for counter in capture.counters {
        events.push(Event::new(counter.timestamp_ns, Payload::Counter(counter)));
    }
    events.sort_by_key(|e| e.timestamp_ns);
    for (sequence, event) in events.into_iter().enumerate() {
        write(Record::Chunk(Chunk {
            shard_id: id,
            sequence: sequence as u64,
            loss: Loss::default(),
            events: vec![event],
        }))?;
    }
    write(Record::End(End {
        shard_id: id,
        duration_ns: capture.metadata.duration_ns,
        exit_code: capture.metadata.exit_code,
        signal: capture.metadata.signal,
        interrupted_by: None,
        reason: EndReason::Completed,
        loss: Loss::default(),
    }))?;
    Ok(output)
}

pub(crate) fn category(category: &Category) -> &'static str {
    match category {
        Category::Process => "process",
        Category::Mpi => "mpi",
        Category::Openmp => "openmp",
        Category::Cuda => "cuda",
        Category::Io => "io",
    }
}

pub(crate) fn key(shard: Uuid, id: u64) -> String {
    format!("{shard}:{id}")
}

pub(crate) fn attributes(values: BTreeMap<String, Attribute>) -> BTreeMap<String, String> {
    values
        .into_iter()
        .map(|(name, value)| {
            (
                name,
                match value {
                    Attribute::Text(v) => v,
                    Attribute::Unsigned(v) => v.to_string(),
                    Attribute::Signed(v) => v.to_string(),
                    Attribute::Float(v) => v.to_string(),
                    Attribute::Bool(v) => v.to_string(),
                },
            )
        })
        .collect()
}

pub(crate) fn counter(value: CounterValue) -> (String, String) {
    match value {
        CounterValue::Integer(v) => (v.to_string(), "integer".into()),
        CounterValue::Float(v) => (v.to_string(), "float".into()),
    }
}

struct Pending {
    shard: Uuid,
    track: u64,
    timestamp: u64,
    correlation: u64,
    name: String,
    category: String,
    attributes: BTreeMap<String, String>,
}

pub fn analyze(path: &Path, limits: Limits) -> Result<Report> {
    ensure!(
        limits.max_events <= 2_000_000 && limits.max_bytes <= 512 * 1024 * 1024,
        "report limits exceed 2M events or 512 MiB"
    );
    let mut tracks = Vec::new();
    let mut spans = Vec::new();
    let mut counters = Vec::new();
    let mut dependencies = Vec::new();
    let mut pending = HashMap::<(Uuid, u64, u64), Pending>::new();
    let mut categories = BTreeMap::<String, (u64, u128)>::new();
    let mut retained = 0;
    let mut used_bytes = 0;
    let mut omitted = 0u64;
    let mut metadata_bytes = 0;
    let mut io = crate::io_summary::IoAnalysis::default();
    let mut clocks = crate::clock::Clocks::default();
    let mut mpi = crate::mpi_summary::MpiAnalysis::default();
    let mut openmp = crate::openmp_summary::OpenmpAnalysis::default();
    let mut cuda = crate::cuda_summary::CudaAnalysis::default();
    let mut pmu = crate::pmu_summary::PmuAnalysis::default();
    let data = scan(path, None, |record| {
        io.observe(&record);
        clocks.observe(&record);
        mpi.observe(&record);
        openmp.observe(&record);
        cuda.observe(&record);
        pmu.observe(&record);
        if let Record::Chunk(chunk) = record {
            for event in chunk.events {
                if let Payload::Span(span) = &event.payload {
                    let stats = categories
                        .entry(category(&span.category).into())
                        .or_default();
                    stats.0 = stats.0.checked_add(1).context("span count overflow")?;
                    stats.1 = stats
                        .1
                        .checked_add(span.duration_ns as u128)
                        .context("duration total overflow")?;
                    // Even omitted completions close retained open activities.
                    if let Some(correlation) = span.correlation_id {
                        pending.remove(&(chunk.shard_id, span.track_id, correlation));
                    }
                }
                if !limits.retain_timeline || matches!(event.payload, Payload::Heartbeat) {
                    continue;
                }
                let cost = serde_json::to_vec(&event)?.len();
                if matches!(event.payload, Payload::Track(_)) {
                    // Never silently drop structural declarations. Bound metadata
                    // separately so late tracks don't disappear after data saturation.
                    metadata_bytes += cost;
                    ensure!(
                        metadata_bytes <= 16 * 1024 * 1024,
                        "report track metadata exceeds 16 MiB; use a text summary"
                    );
                } else if retained >= limits.max_events
                    || cost > limits.max_bytes.saturating_sub(used_bytes)
                {
                    omitted += 1;
                    continue;
                } else {
                    retained += 1;
                    used_bytes += cost;
                }
                let attrs = attributes(event.attributes);
                match event.payload {
                    Payload::Track(track) => {
                        let (kind, details) = match track.kind {
                            TrackKind::Process { pid } => {
                                ("process", [("pid".into(), pid.to_string())])
                            }
                            TrackKind::Thread { tid } => {
                                ("thread", [("tid".into(), tid.to_string())])
                            }
                            TrackKind::GpuDevice { device } => {
                                ("gpu_device", [("device".into(), device.to_string())])
                            }
                            TrackKind::GpuStream { stream } => {
                                ("gpu_stream", [("stream".into(), stream.to_string())])
                            }
                            TrackKind::Io { resource } => ("io", [("resource".into(), resource)]),
                        };
                        tracks.push(ReportTrack {
                            id: key(chunk.shard_id, track.id),
                            shard: chunk.shard_id.to_string(),
                            parent: track.parent_id.map(|id| key(chunk.shard_id, id)),
                            name: track.name,
                            kind: kind.into(),
                            rank: track.rank,
                            details: details.into_iter().chain(attrs).collect(),
                        });
                    }
                    Payload::Span(span) => spans.push(ReportSpan {
                        id: spans.len(),
                        track: key(chunk.shard_id, span.track_id),
                        name: span.name,
                        category: category(&span.category).into(),
                        start_ns: span.start_ns.to_string(),
                        duration_ns: span.duration_ns.to_string(),
                        correlation: span.correlation_id.map(|id| key(chunk.shard_id, id)),
                        incomplete: attrs
                            .get("mpi.request_state")
                            .is_some_and(|state| state == "tracking_released")
                            || attrs
                                .get("mpi.abort_entry")
                                .is_some_and(|flag| flag == "true"),
                        attributes: attrs,
                    }),
                    Payload::Counter(sample) => {
                        let (value, value_type) = counter(sample.value);
                        counters.push(ReportCounter {
                            track: key(chunk.shard_id, sample.track_id),
                            name: sample.name,
                            timestamp_ns: sample.timestamp_ns.to_string(),
                            value,
                            value_type,
                            unit: sample.unit,
                            attributes: attrs,
                        });
                    }
                    Payload::ActivityStart {
                        track_id,
                        name,
                        correlation_id,
                        start_ns,
                    } => {
                        pending.insert(
                            (chunk.shard_id, track_id, correlation_id),
                            Pending {
                                shard: chunk.shard_id,
                                track: track_id,
                                timestamp: start_ns.unwrap_or(event.timestamp_ns),
                                correlation: correlation_id,
                                name,
                                category: if attrs.contains_key("openmp.role") {
                                    "openmp"
                                } else if attrs.contains_key("mpi.record_type") {
                                    "mpi"
                                } else if attrs.contains_key("cuda.role") {
                                    "cuda"
                                } else {
                                    "activity"
                                }
                                .into(),
                                attributes: attrs,
                            },
                        );
                    }
                    Payload::Dependency {
                        from_correlation_id,
                        to_correlation_id,
                        relation,
                    } => dependencies.push(Dependency {
                        from: key(chunk.shard_id, from_correlation_id),
                        to: key(chunk.shard_id, to_correlation_id),
                        relation,
                    }),
                    Payload::Heartbeat => (),
                }
            }
        }
        Ok(())
    })?;
    let summary = data.summary;
    // Stable ordering even though activities are stored in a hash table.
    let mut pending: Vec<_> = pending.into_iter().collect();
    pending.sort_by_key(|(key, _)| *key);
    for (_, open) in pending {
        let observed = summary.shards[&open.shard].observed_ns;
        spans.push(ReportSpan {
            id: spans.len(),
            track: key(open.shard, open.track),
            name: open.name,
            category: open.category,
            start_ns: open.timestamp.to_string(),
            duration_ns: observed.saturating_sub(open.timestamp).to_string(),
            correlation: Some(key(open.shard, open.correlation)),
            incomplete: true,
            attributes: open.attributes,
        });
    }
    let clock_layout = clocks.finish(&summary);
    let mut domains = clock_layout.domains;
    let lookup = clock_layout.raw_domains;
    let mut shards = Vec::new();
    let mut offsets = HashMap::new();
    let mut totals = [0u128; 5];
    let mut synthetic = false;
    for (id, shard) in &summary.shards {
        let domain = lookup[&(shard.shard.hostname.clone(), shard.shard.boot_id.clone())];
        let offset = clock_layout.offsets[id];
        let duration = shard
            .end
            .as_ref()
            .map_or(shard.observed_ns, |e| e.duration_ns);
        let end = (offset as u128) + (duration as u128);
        let current: u128 = domains[domain].end_ns.parse()?;
        domains[domain].end_ns = current.max(end).to_string();
        offsets.insert(id.to_string(), offset as u128);
        totals[0] += shard.tracks as u128;
        totals[1] += shard.spans as u128;
        totals[2] += shard.counters as u128;
        totals[3] += shard.dependencies as u128;
        totals[4] += shard.loss.total() as u128;
        synthetic |= shard
            .shard
            .collectors
            .iter()
            .any(|c| c.name.starts_with("synthetic"));
        shards.push(ReportShard {
            id: id.to_string(),
            domain: domain.to_string(),
            name: format!("shard {}", &id.to_string()[..8]),
            hostname: shard.shard.hostname.clone(),
            command: shard.shard.command.clone(),
            collectors: shard
                .shard
                .collectors
                .iter()
                .map(|c| c.name.clone())
                .collect(),
            origin_ns: offset.to_string(),
            end_ns: end.to_string(),
            complete: shard.end.is_some(),
            exit_code: shard.end.as_ref().and_then(|e| e.exit_code),
            signal: shard.end.as_ref().and_then(|e| e.signal),
            interrupted_by: shard.end.as_ref().and_then(|e| e.interrupted_by),
            end_reason: shard
                .end
                .as_ref()
                .map_or("unknown (missing footer)".into(), |e| {
                    format!("{:?}", e.reason)
                }),
            dropped: shard.loss.total().to_string(),
        });
    }
    let track_shards: HashMap<_, _> = tracks
        .iter()
        .map(|t| (t.id.as_str(), t.shard.as_str()))
        .collect();
    for span in &mut spans {
        let local: u64 = span.start_ns.parse()?;
        span.start_ns = (offsets[track_shards[span.track.as_str()]] + local as u128).to_string();
    }
    for counter in &mut counters {
        let local: u64 = counter.timestamp_ns.parse()?;
        counter.timestamp_ns =
            (offsets[track_shards[counter.track.as_str()]] + local as u128).to_string();
    }
    let mut warnings = clock_layout.warnings;
    if mpi.omitted > 0 {
        warnings.push(format!(
            "{} MPI call summaries omitted by summary limits.",
            mpi.omitted
        ));
    }
    if mpi.released > 0 {
        warnings.push(format!(
            "{} MPI requests were released from tracking without confirming completion.",
            mpi.released
        ));
    }
    if mpi.limited {
        warnings.push("MPI tracking limits were encountered; some request/communicator details are incomplete.".into());
    }
    let mpi = mpi.finish();
    if io.omitted > 0 {
        warnings.push(format!("{} I/O events omitted from per-file summaries after reaching the file/metadata limit; category totals still cover the full capture.", io.omitted));
    }
    let io = io.finish(&lookup);
    if !summary.is_complete() {
        warnings.push(
            "Partial/recovered capture: missing data and loss counts may be incomplete.".into(),
        );
    }
    if totals[4] != 0 {
        warnings.push(format!("{} events were dropped by collectors; incomplete captures report a checkpointed lower bound.", totals[4]));
    }
    if domains.len() > 1 {
        warnings.push(
            "Clock domains are unaligned across hosts/boots; select one domain at a time.".into(),
        );
    }
    if data.legacy {
        warnings.push(
            "Legacy v1 capture: host/session identity and loss accounting are unavailable.".into(),
        );
    }
    if omitted > 0 {
        warnings.push(format!("{omitted} timeline events omitted by report retention limits; summary counts cover the full capture."));
    }
    let unfinished = spans.iter().filter(|s| s.incomplete).count();
    if unfinished > 0 {
        warnings.push(format!("{unfinished} retained activities have no confirmed completion; dashed extents are observation bounds."));
    }
    let correlations: HashSet<_> = spans
        .iter()
        .filter_map(|s| s.correlation.as_deref())
        .collect();
    let unresolved = dependencies
        .iter()
        .filter(|d| {
            !correlations.contains(d.from.as_str()) || !correlations.contains(d.to.as_str())
        })
        .count();
    if unresolved > 0 {
        warnings.push(format!(
            "{unresolved} dependency links have endpoints missing from the retained timeline."
        ));
    }
    let label = if synthetic {
        "Synthetic fixture / generated data"
    } else {
        "Measured capture"
    };
    let openmp = openmp.finish(&shards, &mut warnings);
    let cuda = cuda.finish(&shards, &mut warnings);
    let pmu = pmu.finish(&shards, &mut warnings);
    Ok(Report {
        source_bytes: data.source_bytes,
        version: 1,
        provenance: Provenance {
            kind: if synthetic { "synthetic" } else { "measured" }.into(),
            label: label.into(),
            source: path.display().to_string(),
        },
        summary: ReportSummary {
            format: if data.legacy {
                "legacy v1 JSON"
            } else {
                "rprof stream v2"
            }
            .into(),
            session_id: if data.legacy {
                "unknown (legacy)".into()
            } else {
                summary.header.session_id.to_string()
            },
            complete: summary.is_complete(),
            truncated_tail: summary.truncated_tail,
            recovered_inputs: summary.header.recovered_inputs.to_string(),
            tracks: totals[0].to_string(),
            spans: totals[1].to_string(),
            counters: totals[2].to_string(),
            dependencies: totals[3].to_string(),
            dropped: totals[4].to_string(),
            omitted_events: omitted.to_string(),
            warnings,
            categories: categories
                .into_iter()
                .map(|(name, (count, total_ns))| CategorySummary {
                    name,
                    count: count.to_string(),
                    total_ns: total_ns.to_string(),
                })
                .collect(),
        },
        domains,
        shards,
        tracks,
        spans,
        counters,
        dependencies,
        io,
        mpi,
        openmp,
        cuda,
        pmu,
    })
}
