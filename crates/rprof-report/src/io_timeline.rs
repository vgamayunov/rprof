//! Full-capture I/O rate summaries, independent of raw timeline retention.
//! Discover bounded filesystem identities on pass one, then bin completions on
//! pass two using the finalized clock layout and a fixed input snapshot.
use crate::model::{Domain, IoFilesystem, IoTimeBin, IoTimeline};
use rprof_core::{
    Category,
    stream::{Attribute, Payload, Record},
};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

const MAX_FILESYSTEMS: usize = 1024;
const MAX_METADATA: usize = 4 * 1024 * 1024;
const MAX_TOTAL_BINS: usize = 65_536;
const MAX_BINS_PER_FILESYSTEM: usize = 512;
type Host = (String, String);
type Key = (Host, String, String);

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
}
impl Discovery {
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
                    self.seen = true;
                    let key = identity(host, &event.attributes);
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
                    self.groups.insert(key, metadata);
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
        let groups = self
            .groups
            .into_iter()
            .map(|(key, metadata)| {
                let domain = lookup[&key.0];
                let end = domains[domain].end_ns.parse::<u128>().unwrap_or(0).max(1);
                let width = end.div_ceil(target as u128).max(1_000_000);
                let bins = vec![Bin::default(); end.div_ceil(width) as usize];
                (
                    key,
                    Group {
                        domain,
                        metadata,
                        end,
                        width,
                        bins,
                    },
                )
            })
            .collect();
        Some(Analysis {
            hosts: self.hosts,
            offsets: offsets.clone(),
            groups,
            omitted: self.omitted,
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
struct Group {
    domain: usize,
    metadata: Metadata,
    end: u128,
    width: u128,
    bins: Vec<Bin>,
}
pub(crate) struct Analysis {
    hosts: HashMap<Uuid, Host>,
    offsets: HashMap<Uuid, u128>,
    groups: BTreeMap<Key, Group>,
    omitted: u64,
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
            let Some(group) = self.groups.get_mut(&identity(host, &event.attributes)) else {
                continue;
            };
            let end =
                self.offsets[&chunk.shard_id] + span.start_ns as u128 + span.duration_ns as u128;
            // The final endpoint belongs to the last (right-closed) bin.
            let index = (end.min(group.end - 1) / group.width) as usize;
            let bin = &mut group.bins[index];
            let latency = span.duration_ns as u128;
            let failed = matches!(event.attributes.get("io.result"), Some(Attribute::Signed(result)) if *result < 0);
            let bytes = if failed {
                0
            } else {
                unsigned(&event.attributes, "io.completed_bytes") as u128
            };
            match text(&event.attributes, "io.operation").unwrap_or(&span.name) {
                "read" | "pread" => {
                    bin.read_ops += 1;
                    bin.read_bytes += bytes;
                    bin.read_latency += latency;
                }
                "write" | "pwrite" => {
                    bin.write_ops += 1;
                    bin.write_bytes += bytes;
                    bin.write_latency += latency;
                }
                _ => {
                    bin.other_ops += 1;
                    bin.other_latency += latency;
                }
            }
            bin.errors += u64::from(failed);
            bin.max_latency = bin.max_latency.max(span.duration_ns);
        }
    }
    pub fn finish(self) -> IoTimeline {
        IoTimeline {
            omitted_events: self.omitted.to_string(),
            filesystems: self
                .groups
                .into_iter()
                .enumerate()
                .map(|(id, (key, group))| IoFilesystem {
                    id: format!("filesystem:{id}"),
                    domain: group.domain.to_string(),
                    host: key.0.0,
                    filesystem_type: group.metadata.filesystem_type,
                    mount_point: group.metadata.mount_point,
                    device: group.metadata.device,
                    kind: key.1,
                    bin_width_ns: group.width.to_string(),
                    bins: group
                        .bins
                        .into_iter()
                        .enumerate()
                        .map(|(i, bin)| {
                            let start = i as u128 * group.width;
                            IoTimeBin {
                                start_ns: start.to_string(),
                                duration_ns: group.width.min(group.end - start).to_string(),
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
                        .collect(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rprof_core::{
        Span,
        stream::{Chunk, Event, Loss},
    };
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
