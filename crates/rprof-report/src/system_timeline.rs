//! Full-run gauge retention, independent of the raw span/event budget. Preserve
//! original sample values/times and propagate source gaps when reducing samples.
use crate::model::ReportCounter;
use rprof_core::{
    CounterValue, TrackKind,
    stream::{Attribute, Event, Payload, Record},
};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

const MAX_SERIES: usize = 16_384;
const MAX_POINTS: usize = 65_536;
const MAX_PER_SERIES: usize = 4096;
const MAX_SAMPLE_BYTES: usize = 1024;
type Key = (Uuid, u64);

#[derive(Clone, Copy)]
struct Kind {
    memory: bool,
    root: bool,
}
struct Metadata {
    count: u64,
    first: u64,
    last: u64,
    root: bool,
}
#[derive(Default)]
pub(crate) struct Discovery {
    tracks: HashMap<Key, Kind>,
    series: BTreeMap<Key, Metadata>,
    omitted: u64,
}
fn uint(attrs: &BTreeMap<String, Attribute>, key: &str) -> Option<u64> {
    match attrs.get(key) {
        Some(Attribute::Unsigned(n)) => Some(*n),
        _ => None,
    }
}
impl Discovery {
    pub fn is_sample(&self, shard: Uuid, event: &Event) -> bool {
        let Payload::Counter(counter) = &event.payload else {
            return false;
        };
        self.tracks
            .get(&(shard, counter.track_id))
            .is_some_and(|kind| {
                counter.name
                    == if kind.memory {
                        "system.memory.used"
                    } else {
                        "system.cpu.utilization"
                    }
            })
    }
    pub fn observe(&mut self, record: &Record) {
        let Record::Chunk(chunk) = record else { return };
        for event in &chunk.events {
            if let Payload::Track(track) = &event.payload
                && let TrackKind::System { resource, .. } = &track.kind
                && (resource == "cpu"
                    || resource.starts_with("cpu:")
                    || resource == "memory"
                    || resource.starts_with("node:"))
            {
                self.tracks.insert(
                    (chunk.shard_id, track.id),
                    Kind {
                        memory: resource == "memory" || resource.starts_with("node:"),
                        root: resource == "cpu" || resource == "memory",
                    },
                );
            }
            if !self.is_sample(chunk.shard_id, event) {
                continue;
            }
            let Payload::Counter(counter) = &event.payload else {
                unreachable!()
            };
            let key = (chunk.shard_id, counter.track_id);
            if (!self.series.contains_key(&key) && self.series.len() == MAX_SERIES)
                || serde_json::to_vec(event).map_or(true, |v| v.len() > MAX_SAMPLE_BYTES)
            {
                self.omitted += 1;
                continue;
            }
            let metadata = self.series.entry(key).or_insert(Metadata {
                count: 0,
                first: counter.timestamp_ns,
                last: counter.timestamp_ns,
                root: self.tracks[&key].root,
            });
            metadata.count += 1;
            metadata.first = metadata.first.min(counter.timestamp_ns);
            metadata.last = metadata.last.max(counter.timestamp_ns);
        }
    }
    pub fn start(self, offsets: &HashMap<Uuid, u128>) -> Analysis {
        // Every series gets endpoints. Give overview CPU/memory tracks priority,
        // then spend the remaining points on logical CPUs and NUMA nodes.
        let roots = self.series.values().filter(|s| s.root).count();
        let children = self.series.len() - roots;
        let remaining = MAX_POINTS.saturating_sub(self.series.len() * 2);
        let root_extra = (remaining / roots.max(1) / 2).min(MAX_PER_SERIES - 2);
        let child_extra = (remaining.saturating_sub(root_extra * roots) / children.max(1))
            .min(MAX_PER_SERIES - 2);
        let series = self
            .series
            .into_iter()
            .map(|(key, meta)| {
                let budget = 2 + if meta.root { root_extra } else { child_extra };
                let width = if meta.count <= budget as u64 {
                    0
                } else {
                    (meta.last as u128 - meta.first as u128 + 1)
                        .div_ceil(budget.saturating_sub(2).max(1) as u128)
                };
                (
                    key,
                    Series {
                        meta,
                        width,
                        budget,
                        bins: BTreeMap::new(),
                        first: None,
                        last: None,
                        previous: None,
                        gap_epoch: 0,
                    },
                )
            })
            .collect();
        Analysis {
            tracks: self.tracks,
            series,
            offsets: offsets.clone(),
            omitted: self.omitted,
        }
    }
}

#[derive(Clone)]
struct Sample {
    counter: ReportCounter,
    time: u64,
    gap_epoch: u64,
}
struct Series {
    meta: Metadata,
    width: u128,
    budget: usize,
    bins: BTreeMap<u128, Sample>,
    first: Option<Sample>,
    last: Option<Sample>,
    previous: Option<(u64, Option<u64>)>,
    gap_epoch: u64,
}
pub(crate) struct Analysis {
    tracks: HashMap<Key, Kind>,
    series: BTreeMap<Key, Series>,
    offsets: HashMap<Uuid, u128>,
    omitted: u64,
}
impl Analysis {
    pub fn is_empty(&self) -> bool {
        self.series.is_empty()
    }
    pub fn observe(&mut self, record: &Record) {
        let Record::Chunk(chunk) = record else { return };
        for event in &chunk.events {
            let Payload::Counter(counter) = &event.payload else {
                continue;
            };
            let key = (chunk.shard_id, counter.track_id);
            let Some(kind) = self.tracks.get(&key) else {
                continue;
            };
            if counter.name
                != if kind.memory {
                    "system.memory.used"
                } else {
                    "system.cpu.utilization"
                }
            {
                continue;
            }
            let Some(series) = self.series.get_mut(&key) else {
                continue;
            };
            if serde_json::to_vec(event).map_or(true, |v| v.len() > MAX_SAMPLE_BYTES) {
                continue;
            }
            let sequence = uint(&event.attributes, "system.sample_index");
            let interval = uint(&event.attributes, "system.interval_ns").unwrap_or(1_000_000_000);
            if let Some((time, previous)) = series.previous
                && (counter.timestamp_ns < time
                    || counter.timestamp_ns.saturating_sub(time) > interval.saturating_mul(2)
                    || previous
                        .zip(sequence)
                        .is_some_and(|(a, b)| b != a.saturating_add(1)))
            {
                series.gap_epoch += 1;
            }
            series.previous = Some((counter.timestamp_ns, sequence));
            let (value, value_type) = match counter.value {
                CounterValue::Integer(n) => (n.to_string(), "integer"),
                CounterValue::Float(n) => (n.to_string(), "float"),
            };
            let attributes = event
                .attributes
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        match v {
                            Attribute::Text(s) => s.clone(),
                            Attribute::Unsigned(n) => n.to_string(),
                            Attribute::Signed(n) => n.to_string(),
                            Attribute::Float(n) => n.to_string(),
                            Attribute::Bool(b) => b.to_string(),
                        },
                    )
                })
                .collect();
            let sample = Sample {
                time: counter.timestamp_ns,
                gap_epoch: series.gap_epoch,
                counter: ReportCounter {
                    track: format!("{}:{}", chunk.shard_id, counter.track_id),
                    name: counter.name.clone(),
                    timestamp_ns: (self.offsets[&chunk.shard_id] + counter.timestamp_ns as u128)
                        .to_string(),
                    value,
                    value_type: value_type.into(),
                    unit: counter.unit.clone(),
                    attributes,
                },
            };
            if series.first.is_none() {
                series.first = Some(sample.clone());
            }
            series.last = Some(sample.clone());
            if series.width == 0 {
                series.bins.insert(counter.timestamp_ns as u128, sample);
            } else if series.budget > 2 {
                let bucket =
                    (counter.timestamp_ns.saturating_sub(series.meta.first) as u128) / series.width;
                series.bins.insert(bucket, sample);
            }
        }
    }
    pub fn finish(self, warnings: &mut Vec<String>) -> Vec<ReportCounter> {
        let mut counters = Vec::new();
        let mut reduced = 0u64;
        for (_, series) in self.series {
            let mut selected = BTreeMap::new();
            for sample in series
                .first
                .into_iter()
                .chain(series.bins.into_values())
                .chain(series.last)
            {
                selected.insert(sample.time, sample);
            }
            let retained = selected.len();
            reduced += series.meta.count.saturating_sub(retained as u64);
            let mut previous_gap = None;
            for (_, mut sample) in selected {
                let gap = previous_gap.is_some_and(|epoch| sample.gap_epoch != epoch);
                previous_gap = Some(sample.gap_epoch);
                sample
                    .counter
                    .attributes
                    .insert("system.report_gap_before".into(), gap.to_string());
                sample.counter.attributes.insert(
                    "system.report_samples".into(),
                    series.meta.count.to_string(),
                );
                sample.counter.attributes.insert(
                    "system.report_retained_samples".into(),
                    retained.to_string(),
                );
                sample
                    .counter
                    .attributes
                    .insert("system.report_interval_ns".into(), series.width.to_string());
                counters.push(sample.counter);
            }
        }
        if reduced > 0 {
            warnings.push(format!("System timeline downsampled {reduced} gauge observations across the full run using a separate {MAX_POINTS}-point budget; endpoints and source gaps are preserved."));
        }
        if self.omitted > 0 {
            warnings.push(format!("{} system gauge observations omitted by series/sample-size limits; missing measurements remain unknown.", self.omitted));
        }
        counters
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rprof_core::{
        CounterSample, Track,
        stream::{Chunk, Loss},
    };
    #[test]
    fn bounded_sampling_keeps_both_endpoints_and_real_gaps_across_the_full_run() {
        let shard = Uuid::new_v4();
        let mut events = vec![Event::new(
            0,
            Payload::Track(Track {
                id: 1,
                parent_id: None,
                name: "CPU".into(),
                rank: None,
                kind: TrackKind::System {
                    resource: "cpu".into(),
                    host_id: "host/boot".into(),
                },
            }),
        )];
        for i in 1..=10000u64 {
            if i == 5000 {
                continue;
            } // A genuinely absent source observation.
            let mut event = Event::new(
                i * 1_000_000_000,
                Payload::Counter(CounterSample {
                    track_id: 1,
                    timestamp_ns: i * 1_000_000_000,
                    name: "system.cpu.utilization".into(),
                    value: CounterValue::Float((i % 100) as f64),
                    unit: "%".into(),
                }),
            );
            event
                .attributes
                .insert("system.sample_index".into(), Attribute::Unsigned(i));
            event.attributes.insert(
                "system.interval_ns".into(),
                Attribute::Unsigned(1_000_000_000),
            );
            events.push(event);
        }
        let record = Record::Chunk(Chunk {
            shard_id: shard,
            sequence: 0,
            events,
            loss: Loss::default(),
        });
        let mut discovery = Discovery::default();
        discovery.observe(&record);
        let mut analysis = discovery.start(&HashMap::from([(shard, 37)]));
        analysis.observe(&record);
        let mut warnings = Vec::new();
        let samples = analysis.finish(&mut warnings);
        assert!(samples.len() <= MAX_PER_SERIES);
        assert_eq!(samples.first().unwrap().timestamp_ns, "1000000037");
        assert_eq!(samples.last().unwrap().timestamp_ns, "10000000000037");
        assert_eq!(samples.last().unwrap().value, "0");
        assert_eq!(samples[0].attributes["system.report_samples"], "9999");
        assert_eq!(
            samples
                .iter()
                .filter(|s| s.attributes["system.report_gap_before"] == "true")
                .count(),
            1
        );
        assert!(warnings.iter().any(|w| w.contains("downsampled")));
    }
}
