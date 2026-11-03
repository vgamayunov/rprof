//! Full-capture I/O rate summaries, independent of raw timeline retention.
//! Discover bounded filesystem identities on pass one, then bin completions on
//! pass two using the finalized clock layout and a fixed input snapshot.
use crate::model::{Domain, IoFilesystem, IoThreadSeries, IoTimeBin, IoTimeline};
use rprof_core::{
    Category, Span, TrackKind,
    stream::{Attribute, Payload, Record},
};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

const MAX_FILESYSTEMS: usize = 1024;
const MAX_THREAD_SERIES: usize = 4096;
const MAX_METADATA: usize = 4 * 1024 * 1024;
// Independent overview and detail budgets: many threads must not coarsen the
// main filesystem/aggregate graph into multi-second plateaus.
const MAX_TOTAL_BINS: usize = 65_536; // per layer
const MAX_BINS_PER_FILESYSTEM: usize = 2048;
const MAX_BINS_PER_SCOPE: usize = 512;
type Host = (String, String);
type Key = (Host, String, String);
type ScopeKey = (Uuid, u64, Key);
struct TrackInfo {
    parent: Option<u64>,
    rank: Option<u32>,
    thread: bool,
}
struct ScopeMetadata {
    rank: Option<u32>,
    pid: u64,
    tid: u64,
    instance: u64,
}

fn thread_track(tracks: &HashMap<(Uuid, u64), TrackInfo>, shard: Uuid, track: u64) -> Option<u64> {
    let info = tracks.get(&(shard, track))?;
    if info.thread {
        return Some(track);
    }
    let parent = info.parent?;
    tracks
        .get(&(shard, parent))
        .filter(|info| info.thread)
        .map(|_| parent)
}

fn text<'a>(attrs: &'a BTreeMap<String, Attribute>, key: &str) -> Option<&'a str> {
    match attrs.get(key) {
        Some(Attribute::Text(value)) => Some(value),
        _ => None,
    }
}
fn unsigned(attrs: &BTreeMap<String, Attribute>, key: &str) -> u64 {
    match attrs.get(key) {
        Some(Attribute::Unsigned(value)) => *value,
        _ => 0,
    }
}
fn identity(host: &Host, attrs: &BTreeMap<String, Attribute>) -> Key {
    let kind = match text(attrs, "io.filesystem.kind") {
        Some("filesystem") => "filesystem",
        Some("non_filesystem") => "non_filesystem",
        _ => "unknown",
    };
    (
        host.clone(),
        kind.into(),
        text(attrs, "io.filesystem.id").unwrap_or("unknown").into(),
    )
}

struct Metadata {
    filesystem_type: String,
    mount_point: String,
    device: String,
}
#[derive(Default)]
pub(crate) struct Discovery {
    hosts: HashMap<Uuid, Host>,
    groups: BTreeMap<Key, Metadata>,
    bytes: usize,
    omitted: u64,
    seen: bool,
    tracks: HashMap<(Uuid, u64), TrackInfo>,
    thread_groups: BTreeMap<ScopeKey, ScopeMetadata>,
    scope_omitted: u64,
}
impl Discovery {
    fn discover_thread(
        &mut self,
        shard: Uuid,
        track: u64,
        attrs: &BTreeMap<String, Attribute>,
        filesystem: Key,
    ) {
        let Some(thread) = thread_track(&self.tracks, shard, track) else {
            self.scope_omitted += 1;
            return;
        };
        let key = (shard, thread, filesystem);
        if self.thread_groups.contains_key(&key) {
            return;
        }
        let (
            Some(Attribute::Unsigned(pid)),
            Some(Attribute::Unsigned(tid)),
            Some(Attribute::Unsigned(instance)),
        ) = (
            attrs.get("pid"),
            attrs.get("tid"),
            attrs.get("process_instance"),
        )
        else {
            self.scope_omitted += 1;
            return;
        };
        let bytes = key.2.0.0.len() + key.2.0.1.len() + key.2.1.len() + key.2.2.len() + 64;
        if self.thread_groups.len() >= MAX_THREAD_SERIES
            || bytes > MAX_METADATA.saturating_sub(self.bytes)
        {
            self.scope_omitted += 1;
            return;
        }
        self.bytes += bytes;
        self.thread_groups.insert(
            key,
            ScopeMetadata {
                rank: self.tracks[&(shard, thread)].rank,
                pid: *pid,
                tid: *tid,
                instance: *instance,
            },
        );
    }
    pub fn observe(&mut self, record: &Record) {
        match record {
            Record::Shard(shard) => {
                self.seen |= shard.collectors.iter().any(|c| c.name == "io");
                self.hosts.insert(
                    shard.shard_id,
                    (shard.hostname.clone(), shard.boot_id.clone()),
                );
            }
            Record::Chunk(chunk) => {
                let Some(host) = self.hosts.get(&chunk.shard_id).cloned() else {
                    return;
                };
                for event in &chunk.events {
                    if let Payload::Track(track) = &event.payload {
                        self.tracks.insert(
                            (chunk.shard_id, track.id),
                            TrackInfo {
                                parent: track.parent_id,
                                rank: track.rank,
                                thread: matches!(track.kind, TrackKind::Thread { .. }),
                            },
                        );
                        continue;
                    }
                    let Payload::Span(span) = &event.payload else {
                        continue;
                    };
                    if !matches!(span.category, Category::Io) {
                        continue;
                    }
                    if text(&event.attributes, "io.operation").is_none() {
                        continue;
                    }
                    self.seen = true;
                    let key = identity(&host, &event.attributes);
                    if let Some(previous) = self.groups.get_mut(&key) {
                        if previous.mount_point == "unknown"
                            && let Some(point) =
                                text(&event.attributes, "io.filesystem.mount_point")
                                    .filter(|p| *p != "unknown")
                        {
                            let filesystem_type = text(&event.attributes, "io.filesystem.type")
                                .unwrap_or(&previous.filesystem_type);
                            let old = previous.mount_point.len() + previous.filesystem_type.len();
                            let new = point.len() + filesystem_type.len();
                            if new <= MAX_METADATA.saturating_sub(self.bytes.saturating_sub(old)) {
                                self.bytes = self.bytes - old + new;
                                previous.mount_point = point.into();
                                previous.filesystem_type = filesystem_type.into();
                            }
                        }
                        self.discover_thread(chunk.shard_id, span.track_id, &event.attributes, key);
                        continue;
                    }
                    let mut metadata = Metadata {
                        filesystem_type: text(&event.attributes, "io.filesystem.type")
                            .unwrap_or("unknown")
                            .into(),
                        mount_point: text(&event.attributes, "io.filesystem.mount_point")
                            .unwrap_or("unknown")
                            .into(),
                        device: text(&event.attributes, "io.filesystem.device")
                            .unwrap_or("unknown")
                            .into(),
                    };
                    // Unknown and non-file buckets may contain unrelated FDs;
                    // do not label the whole bucket with its first FD's device.
                    if key.1 != "filesystem" {
                        metadata.filesystem_type = "unknown".into();
                        metadata.mount_point = "unknown".into();
                        metadata.device = "unknown".into();
                    }
                    let bytes = key.0.0.len()
                        + key.0.1.len()
                        + key.1.len()
                        + key.2.len()
                        + metadata.filesystem_type.len()
                        + metadata.mount_point.len()
                        + metadata.device.len();
                    if self.groups.len() >= MAX_FILESYSTEMS
                        || bytes > MAX_METADATA.saturating_sub(self.bytes)
                    {
                        self.omitted += 1;
                        continue;
                    }
                    self.bytes += bytes;
                    self.groups.insert(key.clone(), metadata);
                    self.discover_thread(chunk.shard_id, span.track_id, &event.attributes, key);
                }
            }
            _ => (),
        }
    }
    pub fn start(
        self,
        domains: &[Domain],
        lookup: &HashMap<Host, usize>,
        offsets: &HashMap<Uuid, u128>,
    ) -> Option<Analysis> {
        if !self.seen {
            return None;
        }
        let target = (MAX_TOTAL_BINS / self.groups.len().max(1)).min(MAX_BINS_PER_FILESYSTEM);
        let scope_target =
            (MAX_TOTAL_BINS / self.thread_groups.len().max(1)).min(MAX_BINS_PER_SCOPE);
        let groups: BTreeMap<_, _> = self
            .groups
            .into_iter()
            .enumerate()
            .map(|(id, (key, metadata))| {
                let domain = lookup[&key.0];
                let end = domains[domain].end_ns.parse::<u128>().unwrap_or(0).max(1);
                let width = end.div_ceil(target as u128).max(1_000_000);
                let bins = vec![Bin::default(); end.div_ceil(width) as usize];
                (
                    key,
                    Group {
                        id: format!("filesystem:{id}"),
                        domain,
                        metadata,
                        end,
                        width,
                        bins,
                    },
                )
            })
            .collect();
        let thread_groups = self
            .thread_groups
            .into_iter()
            .map(|(key, metadata)| {
                let group = &groups[&key.2];
                let width = group.end.div_ceil(scope_target as u128).max(1_000_000);
                let bins = vec![Bin::default(); group.end.div_ceil(width) as usize];
                (
                    key,
                    ScopeGroup {
                        metadata,
                        bins,
                        width,
                        end: group.end,
                    },
                )
            })
            .collect();
        Some(Analysis {
            hosts: self.hosts,
            offsets: offsets.clone(),
            groups,
            omitted: self.omitted,
            tracks: self.tracks,
            thread_groups,
            scope_omitted: self.scope_omitted,
        })
    }
}

#[derive(Clone, Default)]
struct Bin {
    read_bytes: u128,
    write_bytes: u128,
    read_ops: u64,
    write_ops: u64,
    other_ops: u64,
    errors: u64,
    read_latency: u128,
    write_latency: u128,
    other_latency: u128,
    max_latency: u64,
}
impl Bin {
    fn observe(&mut self, span: &Span, attrs: &BTreeMap<String, Attribute>) {
        let latency = span.duration_ns as u128;
        let failed =
            matches!(attrs.get("io.result"), Some(Attribute::Signed(result)) if *result < 0);
        let bytes = if failed {
            0
        } else {
            unsigned(attrs, "io.completed_bytes") as u128
        };
        match text(attrs, "io.operation").unwrap_or(&span.name) {
            "read" | "pread" => {
                self.read_ops += 1;
                self.read_bytes += bytes;
                self.read_latency += latency;
            }
            "write" | "pwrite" => {
                self.write_ops += 1;
                self.write_bytes += bytes;
                self.write_latency += latency;
            }
            _ => {
                self.other_ops += 1;
                self.other_latency += latency;
            }
        }
        self.errors += u64::from(failed);
        self.max_latency = self.max_latency.max(span.duration_ns);
    }
}
fn export_bins(bins: Vec<Bin>, width: u128, end: u128) -> Vec<IoTimeBin> {
    bins.into_iter()
        .enumerate()
        .map(|(i, bin)| {
            let start = i as u128 * width;
            IoTimeBin {
                start_ns: start.to_string(),
                duration_ns: width.min(end - start).to_string(),
                read_bytes: bin.read_bytes.to_string(),
                write_bytes: bin.write_bytes.to_string(),
                read_ops: bin.read_ops.to_string(),
                write_ops: bin.write_ops.to_string(),
                other_ops: bin.other_ops.to_string(),
                errors: bin.errors.to_string(),
                read_latency_ns: bin.read_latency.to_string(),
                write_latency_ns: bin.write_latency.to_string(),
                other_latency_ns: bin.other_latency.to_string(),
                max_latency_ns: bin.max_latency.to_string(),
            }
        })
        .collect()
}
struct Group {
    id: String,
    domain: usize,
    metadata: Metadata,
    end: u128,
    width: u128,
    bins: Vec<Bin>,
}
struct ScopeGroup {
    metadata: ScopeMetadata,
    bins: Vec<Bin>,
    width: u128,
    end: u128,
}
pub(crate) struct Analysis {
    hosts: HashMap<Uuid, Host>,
    offsets: HashMap<Uuid, u128>,
    groups: BTreeMap<Key, Group>,
    omitted: u64,
    tracks: HashMap<(Uuid, u64), TrackInfo>,
    thread_groups: BTreeMap<ScopeKey, ScopeGroup>,
    scope_omitted: u64,
}
impl Analysis {
    pub fn observe(&mut self, record: &Record) {
        let Record::Chunk(chunk) = record else { return };
        let Some(host) = self.hosts.get(&chunk.shard_id) else {
            return;
        };
        for event in &chunk.events {
            let Payload::Span(span) = &event.payload else {
                continue;
            };
            if !matches!(span.category, Category::Io) {
                continue;
            }
            if text(&event.attributes, "io.operation").is_none() {
                continue;
            }
            let key = identity(host, &event.attributes);
            let Some(group) = self.groups.get_mut(&key) else {
                continue;
            };
            let end =
                self.offsets[&chunk.shard_id] + span.start_ns as u128 + span.duration_ns as u128;
            // The final endpoint belongs to the last (right-closed) bin.
            let index = (end.min(group.end - 1) / group.width) as usize;
            group.bins[index].observe(span, &event.attributes);
            if let Some(thread) = thread_track(&self.tracks, chunk.shard_id, span.track_id)
                && let Some(scope) = self.thread_groups.get_mut(&(chunk.shard_id, thread, key))
            {
                let index = (end.min(scope.end - 1) / scope.width) as usize;
                scope.bins[index].observe(span, &event.attributes);
            }
        }
    }
    pub fn finish(self) -> IoTimeline {
        let threads = self
            .thread_groups
            .into_iter()
            .map(|((shard, thread, key), scope)| {
                let group = &self.groups[&key];
                IoThreadSeries {
                    filesystem_id: group.id.clone(),
                    domain: group.domain.to_string(),
                    shard: shard.to_string(),
                    rank: scope.metadata.rank,
                    process_instance: scope.metadata.instance.to_string(),
                    pid: scope.metadata.pid.to_string(),
                    tid: scope.metadata.tid.to_string(),
                    thread_track: format!("{shard}:{thread}"),
                    bin_width_ns: scope.width.to_string(),
                    bins: export_bins(scope.bins, scope.width, scope.end),
                }
            })
            .collect();
        IoTimeline {
            omitted_events: self.omitted.to_string(),
            scope_omitted_events: self.scope_omitted.to_string(),
            threads,
            filesystems: self
                .groups
                .into_iter()
                .map(|(key, group)| IoFilesystem {
                    id: group.id,
                    domain: group.domain.to_string(),
                    host: key.0.0,
                    filesystem_type: group.metadata.filesystem_type,
                    mount_point: group.metadata.mount_point,
                    device: group.metadata.device,
                    kind: key.1,
                    bin_width_ns: group.width.to_string(),
                    bins: export_bins(group.bins, group.width, group.end),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rprof_core::{
        Span, Track,
        stream::{Chunk, Event, Loss},
    };
    #[test]
    fn training_length_overview_stays_subsecond_with_hundreds_of_thread_series() {
        let shard = Uuid::new_v4();
        let host = ("training-host".into(), "boot".into());
        let mut discovery = Discovery::default();
        discovery.hosts.insert(shard, host.clone());
        let mut events = Vec::new();
        for id in 0..960u64 {
            events.push(Event::new(
                0,
                Payload::Track(Track {
                    id,
                    parent_id: None,
                    name: "thread".into(),
                    rank: Some(0),
                    kind: TrackKind::Thread { tid: id },
                }),
            ));
            events.push(Event {
                timestamp_ns: 1,
                attributes: BTreeMap::from([
                    ("io.operation".into(), Attribute::Text("read".into())),
                    (
                        "io.filesystem.id".into(),
                        Attribute::Text(format!("mount:{}", id % 8)),
                    ),
                    (
                        "io.filesystem.kind".into(),
                        Attribute::Text("filesystem".into()),
                    ),
                    ("io.completed_bytes".into(), Attribute::Unsigned(4096)),
                    ("pid".into(), Attribute::Unsigned(1)),
                    ("tid".into(), Attribute::Unsigned(id)),
                    ("process_instance".into(), Attribute::Unsigned(1)),
                ]),
                payload: Payload::Span(Span {
                    track_id: id,
                    name: "read".into(),
                    category: Category::Io,
                    start_ns: 0,
                    duration_ns: 1,
                    correlation_id: None,
                }),
            });
        }
        let record = Record::Chunk(Chunk {
            shard_id: shard,
            sequence: 0,
            loss: Loss::default(),
            events,
        });
        discovery.observe(&record);
        let domains = vec![Domain {
            id: "0".into(),
            name: "training-host".into(),
            origin_ns: "0".into(),
            end_ns: "988185000000".into(),
            calibration: None,
        }];
        let mut analysis = discovery
            .start(
                &domains,
                &HashMap::from([(host, 0)]),
                &HashMap::from([(shard, 0)]),
            )
            .unwrap();
        analysis.observe(&record);
        let result = analysis.finish();
        assert_eq!(result.filesystems.len(), 8);
        assert_eq!(result.threads.len(), 960);
        assert!(result.filesystems.iter().all(
            |f| f.bins.len() == 2048 && f.bin_width_ns.parse::<u64>().unwrap() < 1_000_000_000
        ));
        assert!(
            result
                .threads
                .iter()
                .all(|t| t.bin_width_ns.parse::<u64>().unwrap() > 10_000_000_000)
        );
        let total = |bins: &[IoTimeBin]| {
            bins.iter()
                .map(|b| b.read_bytes.parse::<u64>().unwrap())
                .sum::<u64>()
        };
        assert_eq!(
            result
                .filesystems
                .iter()
                .map(|f| total(&f.bins))
                .sum::<u64>(),
            960 * 4096
        );
        assert_eq!(
            result.threads.iter().map(|t| total(&t.bins)).sum::<u64>(),
            960 * 4096
        );
    }
    #[test]
    fn scope_budget_does_not_stop_parent_totals_and_identity_keeps_reused_tids_separate() {
        let shard = Uuid::new_v4();
        let host = ("host".into(), "boot".into());
        let mut discovery = Discovery::default();
        discovery.hosts.insert(shard, host.clone());
        let mut events = Vec::new();
        for id in 0..MAX_THREAD_SERIES + 1 {
            events.push(Event::new(
                0,
                Payload::Track(Track {
                    id: id as u64,
                    parent_id: None,
                    name: "thread".into(),
                    rank: Some(0),
                    kind: TrackKind::Thread { tid: 7 },
                }),
            ));
            events.push(Event {
                timestamp_ns: 1,
                attributes: BTreeMap::from([
                    ("io.operation".into(), Attribute::Text("write".into())),
                    ("io.completed_bytes".into(), Attribute::Unsigned(1)),
                    ("pid".into(), Attribute::Unsigned(12)),
                    ("tid".into(), Attribute::Unsigned(7)),
                    (
                        "process_instance".into(),
                        Attribute::Unsigned(id as u64 + 100),
                    ),
                ]),
                payload: Payload::Span(Span {
                    track_id: id as u64,
                    name: "write".into(),
                    category: Category::Io,
                    start_ns: 0,
                    duration_ns: 1,
                    correlation_id: None,
                }),
            });
        }
        let record = Record::Chunk(Chunk {
            shard_id: shard,
            sequence: 0,
            loss: Loss::default(),
            events,
        });
        discovery.observe(&record);
        let domains = vec![Domain {
            id: "0".into(),
            name: "host".into(),
            origin_ns: "0".into(),
            end_ns: "1000000000".into(),
            calibration: None,
        }];
        let mut analysis = discovery
            .start(
                &domains,
                &HashMap::from([(host, 0)]),
                &HashMap::from([(shard, 0)]),
            )
            .unwrap();
        analysis.observe(&record);
        let result = analysis.finish();
        assert_eq!(result.omitted_events, "0");
        assert_eq!(result.scope_omitted_events, "1");
        assert_eq!(result.threads.len(), MAX_THREAD_SERIES);
        assert_eq!(
            result.filesystems[0].bins[0].write_ops,
            (MAX_THREAD_SERIES + 1).to_string()
        );
        assert_ne!(
            result.threads[0].process_instance,
            result.threads[1].process_instance
        );
        assert_ne!(
            result.threads[0].thread_track,
            result.threads[1].thread_track
        );
        let bins = result
            .filesystems
            .iter()
            .map(|f| f.bins.len())
            .sum::<usize>()
            + result.threads.iter().map(|t| t.bins.len()).sum::<usize>();
        assert!(bins <= 2 * MAX_TOTAL_BINS);
        assert!(
            result.filesystems[0].bins.len() > result.threads[0].bins.len(),
            "overview resolution must not shrink with thread count"
        );
    }
    #[test]
    fn filesystem_cardinality_and_bins_are_bounded_without_truncating_existing_groups() {
        let shard = Uuid::new_v4();
        let host = ("host".into(), "boot".into());
        let mut discovery = Discovery::default();
        discovery.hosts.insert(shard, host.clone());
        let events = (0..MAX_FILESYSTEMS + 1)
            .map(|id| Event {
                timestamp_ns: 1,
                attributes: BTreeMap::from([
                    ("io.operation".into(), Attribute::Text("write".into())),
                    (
                        "io.filesystem.id".into(),
                        Attribute::Text(format!("mount:{id}")),
                    ),
                    (
                        "io.filesystem.kind".into(),
                        Attribute::Text("filesystem".into()),
                    ),
                    ("io.completed_bytes".into(), Attribute::Unsigned(17)),
                ]),
                payload: Payload::Span(Span {
                    track_id: 0,
                    name: "write".into(),
                    category: Category::Io,
                    start_ns: 0,
                    duration_ns: 1,
                    correlation_id: None,
                }),
            })
            .collect();
        let record = Record::Chunk(Chunk {
            shard_id: shard,
            sequence: 0,
            loss: Loss::default(),
            events,
        });
        discovery.observe(&record);
        assert_eq!(discovery.omitted, 1);
        let domains = vec![Domain {
            id: "0".into(),
            name: "estimated".into(),
            origin_ns: "0".into(),
            end_ns: "1000000000".into(),
            calibration: None,
        }];
        let mut analysis = discovery
            .start(
                &domains,
                &HashMap::from([(host, 0)]),
                &HashMap::from([(shard, 500_000_000)]),
            )
            .unwrap();
        analysis.observe(&record);
        let result = analysis.finish();
        assert_eq!(result.filesystems.len(), MAX_FILESYSTEMS);
        assert_eq!(
            result
                .filesystems
                .iter()
                .map(|f| f.bins.len())
                .sum::<usize>(),
            MAX_TOTAL_BINS
        );
        // Applying the domain offset moves completions to the correct half of
        // the timeline; no averaging or raw-observation retention is involved.
        for fs in result.filesystems {
            assert_eq!(fs.bins[0].write_bytes, "0");
            assert_eq!(fs.bins[32].write_bytes, "17");
        }
    }
}
