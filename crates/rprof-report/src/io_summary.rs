use std::collections::{BTreeMap, HashMap};

use rprof_core::{
    Category,
    stream::{Attribute, Record},
};
use uuid::Uuid;

use crate::model::{IoOperation, IoSummary};

const MAX_FILES: usize = 8192;

#[derive(Default)]
struct Operation {
    count: u64,
    errors: u64,
    total: u128,
    max: u64,
}
struct Totals {
    host: String,
    boot: String,
    file: String,
    start: u128,
    end: u128,
    read_bytes: u128,
    write_bytes: u128,
    requested_read: u128,
    requested_write: u128,
    operations: BTreeMap<String, Operation>,
}

#[derive(Default)]
pub(crate) struct IoAnalysis {
    origins: HashMap<Uuid, (String, String, u64)>,
    files: BTreeMap<(String, String, String), Totals>,
    pub omitted: u64,
    metadata_bytes: usize,
}

fn text<'a>(attrs: &'a BTreeMap<String, Attribute>, key: &str) -> Option<&'a str> {
    if let Some(Attribute::Text(value)) = attrs.get(key) {
        Some(value)
    } else {
        None
    }
}
fn unsigned(attrs: &BTreeMap<String, Attribute>, key: &str) -> u64 {
    if let Some(Attribute::Unsigned(value)) = attrs.get(key) {
        *value
    } else {
        0
    }
}
fn flag(attrs: &BTreeMap<String, Attribute>, key: &str) -> bool {
    matches!(attrs.get(key), Some(Attribute::Bool(true)))
}

impl IoAnalysis {
    pub fn observe(&mut self, record: &Record) {
        match record {
            Record::Shard(shard) => {
                self.origins.insert(
                    shard.shard_id,
                    (
                        shard.hostname.clone(),
                        shard.boot_id.clone(),
                        shard.monotonic_origin_ns,
                    ),
                );
            }
            Record::Chunk(chunk) => {
                let Some((host, boot, origin)) = self.origins.get(&chunk.shard_id) else {
                    return;
                };
                for event in &chunk.events {
                    let rprof_core::stream::Payload::Span(span) = &event.payload else {
                        continue;
                    };
                    if !matches!(span.category, Category::Io) {
                        continue;
                    }
                    let attrs = &event.attributes;
                    let (Some(operation), Some(path)) =
                        (text(attrs, "io.operation"), text(attrs, "io.file"))
                    else {
                        continue;
                    };
                    // Bound operation-key cardinality as well as file cardinality.
                    if !matches!(
                        operation,
                        "open"
                            | "openat"
                            | "creat"
                            | "close"
                            | "read"
                            | "write"
                            | "pread"
                            | "pwrite"
                            | "fsync"
                            | "fdatasync"
                            | "dup"
                            | "dup2"
                            | "dup3"
                            | "lseek"
                    ) {
                        continue;
                    }
                    let mut file = path.to_owned();
                    if flag(attrs, "io.path_truncated") {
                        file.push_str(&format!(
                            " [truncated; image {}; resource {}]",
                            unsigned(attrs, "process_instance"),
                            unsigned(attrs, "io.resource")
                        ));
                    }
                    if flag(attrs, "io.path_from_argument") {
                        file.push_str(&format!(
                            " [argument; image {}]",
                            unsigned(attrs, "process_instance")
                        ));
                    }
                    let identity =
                        format!("{file}\0{}", text(attrs, "io.path_raw_hex").unwrap_or(""));
                    let key = (host.clone(), boot.clone(), identity);
                    if !self.files.contains_key(&key) {
                        let bytes = key.0.len() + key.1.len() + key.2.len() + file.len();
                        if self.files.len() >= MAX_FILES
                            || bytes > (16 * 1024 * 1024usize).saturating_sub(self.metadata_bytes)
                        {
                            self.omitted += 1;
                            continue;
                        }
                        self.metadata_bytes += bytes;
                    }
                    let start = *origin as u128 + span.start_ns as u128;
                    let end = start + span.duration_ns as u128;
                    let totals = self.files.entry(key).or_insert_with(|| Totals {
                        host: host.clone(),
                        boot: boot.clone(),
                        file,
                        start,
                        end,
                        read_bytes: 0,
                        write_bytes: 0,
                        requested_read: 0,
                        requested_write: 0,
                        operations: BTreeMap::new(),
                    });
                    totals.start = totals.start.min(start);
                    totals.end = totals.end.max(end);
                    let stats = totals.operations.entry(operation.into()).or_default();
                    stats.count += 1;
                    stats.total += span.duration_ns as u128;
                    stats.max = stats.max.max(span.duration_ns);
                    if matches!(attrs.get("io.result"), Some(Attribute::Signed(result)) if *result < 0)
                    {
                        stats.errors += 1;
                    }
                    match operation {
                        "read" | "pread" => {
                            totals.read_bytes += unsigned(attrs, "io.completed_bytes") as u128;
                            totals.requested_read += unsigned(attrs, "io.requested_bytes") as u128;
                        }
                        "write" | "pwrite" => {
                            totals.write_bytes += unsigned(attrs, "io.completed_bytes") as u128;
                            totals.requested_write += unsigned(attrs, "io.requested_bytes") as u128;
                        }
                        _ => (),
                    }
                }
            }
            _ => (),
        }
    }

    pub fn finish(self, domains: &HashMap<(String, String), usize>) -> Vec<IoSummary> {
        self.files
            .into_values()
            .map(|file| {
                let count: u64 = file.operations.values().map(|s| s.count).sum();
                let errors: u64 = file.operations.values().map(|s| s.errors).sum();
                let total: u128 = file.operations.values().map(|s| s.total).sum();
                IoSummary {
                    domain: domains[&(file.host.clone(), file.boot)].to_string(),
                    host: file.host,
                    file: file.file,
                    operations: count.to_string(),
                    errors: errors.to_string(),
                    read_bytes: file.read_bytes.to_string(),
                    write_bytes: file.write_bytes.to_string(),
                    requested_read_bytes: file.requested_read.to_string(),
                    requested_write_bytes: file.requested_write.to_string(),
                    total_ns: total.to_string(),
                    window_ns: file.end.saturating_sub(file.start).to_string(),
                    by_operation: file
                        .operations
                        .into_iter()
                        .map(|(name, stats)| IoOperation {
                            name,
                            count: stats.count.to_string(),
                            errors: stats.errors.to_string(),
                            total_ns: stats.total.to_string(),
                            max_ns: stats.max.to_string(),
                        })
                        .collect(),
                }
            })
            .collect()
    }
}
