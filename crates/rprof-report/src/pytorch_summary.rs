use crate::model::*;
use rprof_core::{
    CounterValue,
    stream::{Attribute, Payload, Record},
};
use std::collections::{BTreeMap, HashMap};
type Key = (String, String, String, Option<String>);
#[derive(Default)]
struct Total {
    count: u64,
    total: u128,
    self_ns: u128,
    max: u64,
    bytes: u128,
    known_bytes: u64,
    ambiguous: bool,
}
struct Interval {
    start: u64,
    end: u64,
    row: Key,
    children: u64,
    child_end: u64,
    ambiguous: bool,
}
#[derive(Default)]
pub(crate) struct PytorchAnalysis {
    sources: BTreeMap<String, PytorchSource>,
    rows: BTreeMap<Key, Total>,
    memory: BTreeMap<(String, u64, String, Option<String>), PytorchMemory>,
    intervals: HashMap<(String, u64), Vec<Interval>>,
    self_count: usize,
    self_overflow: bool,
    omitted: u64,
}
fn value(a: &BTreeMap<String, Attribute>, key: &str) -> Option<String> {
    match a.get(key) {
        Some(Attribute::Text(v)) => Some(v.chars().take(8192).collect()),
        Some(Attribute::Unsigned(v)) => Some(v.to_string()),
        Some(Attribute::Signed(v)) => Some(v.to_string()),
        Some(Attribute::Bool(v)) => Some(v.to_string()),
        _ => None,
    }
}
impl PytorchAnalysis {
    pub fn observe(&mut self, record: &Record) {
        let Record::Chunk(chunk) = record else {
            return;
        };
        let shard = chunk.shard_id.to_string();
        for event in &chunk.events {
            let a = &event.attributes;
            let Some(role) = value(a, "pytorch.role") else {
                continue;
            };
            if !self.sources.contains_key(&shard) && self.sources.len() >= 1024 {
                self.omitted += 1;
                continue;
            }
            let source = self
                .sources
                .entry(shard.clone())
                .or_insert_with(|| PytorchSource {
                    shard: shard.clone(),
                    ..Default::default()
                });
            if role == "runtime" {
                for k in a.keys().filter(|k| k.starts_with("pytorch.")).take(64) {
                    if source.metadata.len() < 128 {
                        if let Some(v) = value(a, k) {
                            if source.metadata.values().map(String::len).sum::<usize>() + v.len()
                                <= 65536
                            {
                                source.metadata.insert(k.clone(), v);
                            } else {
                                self.omitted += 1;
                            }
                        }
                    }
                }
            }
            if role == "diagnostic" {
                if let Payload::Counter(c) = &event.payload {
                    if let CounterValue::Integer(v) = c.value {
                        source.diagnostics.insert(c.name.clone(), v.to_string());
                    }
                }
            }
            if let Payload::Counter(c) = &event.payload {
                if role == "memory" && c.unit == "bytes" {
                    if let CounterValue::Integer(v) = c.value {
                        let device = value(a, "pytorch.device");
                        let key = (shard.clone(), c.track_id, c.name.clone(), device.clone());
                        if !self.memory.contains_key(&key) && self.memory.len() >= 8192 {
                            self.omitted += 1;
                            continue;
                        }
                        let row = self.memory.entry(key).or_insert_with(|| PytorchMemory {
                            shard: shard.clone(),
                            domain: String::new(),
                            track: c.track_id.to_string(),
                            name: c.name.clone(),
                            device,
                            samples: "0".into(),
                            peak_bytes: "0".into(),
                            last_bytes: "0".into(),
                        });
                        row.samples = (row.samples.parse::<u64>().unwrap() + 1).to_string();
                        row.peak_bytes = row.peak_bytes.parse::<u64>().unwrap().max(v).to_string();
                        row.last_bytes = v.to_string();
                    }
                }
                continue;
            }
            let Payload::Span(span) = &event.payload else {
                continue;
            };
            let key = (
                shard.clone(),
                role.clone(),
                span.name.clone(),
                value(a, "pytorch.device"),
            );
            if !self.rows.contains_key(&key) && self.rows.len() >= 8192 {
                self.omitted += 1;
                continue;
            }
            let total = self.rows.entry(key.clone()).or_default();
            total.count += 1;
            total.total += span.duration_ns as u128;
            total.max = total.max.max(span.duration_ns);
            if let Some(bytes) = value(a, "pytorch.bytes").and_then(|v| v.parse::<u64>().ok()) {
                total.bytes += bytes as u128;
                total.known_bytes += 1;
            }
            if matches!(
                role.as_str(),
                "operator" | "user_range" | "runtime_api" | "python"
            ) && !self.self_overflow
            {
                if self.self_count >= 200000 {
                    self.intervals.clear();
                    self.self_overflow = true;
                    continue;
                }
                self.self_count += 1;
                self.intervals
                    .entry((shard.clone(), span.track_id))
                    .or_default()
                    .push(Interval {
                        start: span.start_ns,
                        end: span.start_ns.saturating_add(span.duration_ns),
                        row: key,
                        children: 0,
                        child_end: 0,
                        ambiguous: false,
                    });
            }
        }
    }
    pub fn finish(
        mut self,
        shards: &[ReportShard],
        warnings: &mut Vec<String>,
    ) -> Option<PytorchSummary> {
        if self.sources.is_empty() {
            return None;
        }
        let mut crossings = 0;
        let finish = |item: Interval, rows: &mut BTreeMap<Key, Total>| {
            let row = rows.get_mut(&item.row).unwrap();
            row.self_ns += (item.end - item.start).saturating_sub(item.children) as u128;
            row.ambiguous |= item.ambiguous;
        };
        for (_, mut spans) in self.intervals {
            spans.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
            let mut stack: Vec<Interval> = Vec::new();
            for mut span in spans {
                while stack.last().is_some_and(|p| p.end <= span.start) {
                    finish(stack.pop().unwrap(), &mut self.rows);
                }
                while stack.last().is_some_and(|p| p.end < span.end) {
                    let mut prior = stack.pop().unwrap();
                    prior.ambiguous = true;
                    span.ambiguous = true;
                    crossings += 1;
                    finish(prior, &mut self.rows);
                }
                if let Some(parent) = stack.last_mut() {
                    if parent.start == span.start && parent.end == span.end {
                        parent.ambiguous = true;
                        span.ambiguous = true;
                        crossings += 1;
                    }
                    parent.children += span.end.saturating_sub(span.start.max(parent.child_end));
                    parent.child_end = parent.child_end.max(span.end);
                }
                stack.push(span);
            }
            while let Some(span) = stack.pop() {
                finish(span, &mut self.rows);
            }
        }
        let domains: HashMap<_, _> = shards
            .iter()
            .map(|s| (s.id.as_str(), s.domain.as_str()))
            .collect();
        let sources=self.sources.into_values().map(|mut source|{source.domain=domains[source.shard.as_str()].into();
            warnings.push(format!("PyTorch import {}: source profiler loss/coverage is unknown; imported clock is not aligned with native captures.",source.shard));
            for (name,count) in &source.diagnostics{if count!="0"&&matches!(name.as_str(),"pytorch.import.skipped_records"|"pytorch.import.unmatched_endpoints"|"pytorch.import.unresolved_flows"|"pytorch.import.ambiguous_flows"){warnings.push(format!("PyTorch {}: {name}={count}",source.shard));}}
            if let Some(error)=source.metadata.get("pytorch.ERROR"){warnings.push(format!("PyTorch source reported: {error}"));}source
        }).collect();
        let activities = self
            .rows
            .into_iter()
            .map(|((shard, role, name, device), t)| {
                let cpu = matches!(
                    role.as_str(),
                    "operator" | "user_range" | "runtime_api" | "python"
                );
                PytorchActivity {
                    domain: domains[shard.as_str()].into(),
                    shard,
                    self_ns: (cpu && !self.self_overflow && !t.ambiguous)
                        .then(|| t.self_ns.to_string()),
                    role,
                    name,
                    device,
                    count: t.count.to_string(),
                    total_ns: t.total.to_string(),
                    max_ns: t.max.to_string(),
                    bytes: (t.known_bytes == t.count).then(|| t.bytes.to_string()),
                }
            })
            .collect();
        let memory = self
            .memory
            .into_values()
            .map(|mut row| {
                row.domain = domains[row.shard.as_str()].into();
                row.track = format!("{}:{}", row.shard, row.track);
                row
            })
            .collect();
        if crossings > 0 {
            warnings.push(format!("PyTorch CPU self time unavailable for rows affected by {crossings} crossing/tied intervals."));
        }
        if self.self_overflow {
            warnings.push("PyTorch CPU self-time index exceeded 200000 intervals; self totals are unavailable.".into());
        }
        if self.omitted > 0 {
            warnings.push(format!(
                "{} PyTorch summary observations omitted by metadata limits.",
                self.omitted
            ));
        }
        Some(PytorchSummary {
            sources,
            activities,
            memory,
        })
    }
}
