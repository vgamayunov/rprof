//! Explicit MPI offset estimates. Raw timestamps are never rewritten in captures.
use crate::model::{ClockCalibration, Domain};
use rprof_capture::Summary;
use rprof_core::stream::{Attribute, Record};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Sample {
    group: u64,
    offset: i64,
    uncertainty: u64,
}
#[derive(Default)]
struct Samples {
    stages: [Option<Sample>; 2],
    invalid: bool,
}
#[derive(Default)]
pub(crate) struct Clocks {
    samples: HashMap<Uuid, Samples>,
}
pub(crate) struct Layout {
    pub domains: Vec<Domain>,
    pub raw_domains: HashMap<(String, String), usize>,
    pub offsets: HashMap<Uuid, u128>,
    pub warnings: Vec<String>,
}

impl Clocks {
    pub fn observe(&mut self, record: &Record) {
        let Record::Chunk(chunk) = record else { return };
        for event in &chunk.events {
            let a = &event.attributes;
            if !matches!(a.get("mpi.record_type"),Some(Attribute::Text(kind)) if kind=="clock") {
                continue;
            }
            let entry = self.samples.entry(chunk.shard_id).or_default();
            let (
                Some(Attribute::Unsigned(group)),
                Some(Attribute::Signed(offset)),
                Some(Attribute::Unsigned(uncertainty)),
                Some(Attribute::Unsigned(stage)),
            ) = (
                a.get("mpi.clock.group"),
                a.get("mpi.clock.offset_ns"),
                a.get("mpi.clock.uncertainty_ns"),
                a.get("mpi.clock.stage"),
            )
            else {
                entry.invalid = true;
                continue;
            };
            if *stage > 1 || *group == 0 {
                entry.invalid = true;
                continue;
            }
            let sample = Sample {
                group: *group,
                offset: *offset,
                uncertainty: *uncertainty,
            };
            if entry.stages[*stage as usize].is_some_and(|old| old != sample) {
                entry.invalid = true;
            }
            entry.stages[*stage as usize] = Some(sample);
        }
    }

    pub fn finish(self, summary: &Summary) -> Layout {
        let mut raw = BTreeMap::<(String, String), Vec<Uuid>>::new();
        for (id, shard) in &summary.shards {
            raw.entry((shard.shard.hostname.clone(), shard.shard.boot_id.clone()))
                .or_default()
                .push(*id);
        }
        let mut assignments = HashMap::new();
        let mut group_origins = BTreeMap::<String, i128>::new();
        let mut calibration = HashMap::<String, ClockCalibration>::new();
        let mut warnings = Vec::new();
        for (host, ids) in &raw {
            let mut group = None;
            let aligned = ids.iter().all(|id| {
                let Some(samples) = self.samples.get(id) else {
                    return false;
                };
                let Some(first) = samples.stages[0] else {
                    return false;
                };
                if samples.invalid
                    || samples.stages[1].is_some_and(|last| last.group != first.group)
                {
                    return false;
                }
                if group.is_some_and(|previous| previous != first.group) {
                    return false;
                }
                group = Some(first.group);
                true
            });
            let key = if aligned {
                format!("mpi:{:016x}", group.unwrap())
            } else {
                format!("host:{}\0{}", host.0, host.1)
            };
            if !aligned && ids.iter().any(|id| self.samples.contains_key(id)) {
                warnings.push(format!("Incomplete/conflicting MPI clock metadata for {}; retaining its native clock domain.",host.0));
            }
            assignments.insert(host.clone(), key.clone());
            for id in ids {
                let offset = if aligned {
                    self.samples[id].stages[0].unwrap().offset as i128
                } else {
                    0
                };
                let effective = summary.shards[id].shard.monotonic_origin_ns as i128 + offset;
                group_origins
                    .entry(key.clone())
                    .and_modify(|origin| *origin = (*origin).min(effective))
                    .or_insert(effective);
                if aligned {
                    let samples = &self.samples[id];
                    let first = samples.stages[0].unwrap();
                    let drift = samples.stages[1].map_or(0, |last| {
                        (last.offset as i128 - first.offset as i128).unsigned_abs()
                    });
                    let uncertainty = first.uncertainty as u128
                        + samples.stages[1].map_or(0, |s| s.uncertainty as u128)
                        + drift;
                    let entry = calibration.entry(key.clone()).or_insert(ClockCalibration {
                        method: "min-RTT offset; observed drift included".into(),
                        uncertainty_ns: "0".into(),
                        max_drift_ns: "0".into(),
                        complete: true,
                    });
                    entry.uncertainty_ns = entry
                        .uncertainty_ns
                        .parse::<u128>()
                        .unwrap_or(0)
                        .max(uncertainty)
                        .to_string();
                    entry.max_drift_ns = entry
                        .max_drift_ns
                        .parse::<u128>()
                        .unwrap_or(0)
                        .max(drift)
                        .to_string();
                    entry.complete &= samples.stages[1].is_some();
                }
            }
        }
        let mut domains = Vec::new();
        let mut index = HashMap::new();
        for (key, origin) in group_origins {
            let info = calibration.remove(&key);
            if let Some(info) = &info {
                warnings.push(format!("Estimated MPI clock alignment {}: uncertainty budget {} ns, observed offset drift {} ns{}. Offset-only model; timings outside calibration samples are extrapolated.",key,info.uncertainty_ns,info.max_drift_ns,if info.complete{""}else{"; final calibration missing"}));
            }
            let id = domains.len();
            index.insert(key.clone(), id);
            let name = if info.is_some() {
                format!("MPI calibrated group {}", key.trim_start_matches("mpi:"))
            } else {
                key.trim_start_matches("host:").replace('\0', " / ")
            };
            domains.push(Domain {
                id: id.to_string(),
                name,
                origin_ns: origin.to_string(),
                end_ns: "0".into(),
                calibration: info,
            });
        }
        let mut raw_domains = HashMap::new();
        let mut offsets = HashMap::new();
        for (host, ids) in raw {
            let key = &assignments[&host];
            let domain = index[key];
            raw_domains.insert(host, domain);
            let base = domains[domain].origin_ns.parse::<i128>().unwrap();
            for id in ids {
                let offset = if key.starts_with("mpi:") {
                    self.samples[&id].stages[0].unwrap().offset as i128
                } else {
                    0
                };
                offsets.insert(
                    id,
                    (summary.shards[&id].shard.monotonic_origin_ns as i128 + offset - base) as u128,
                );
            }
        }
        Layout {
            domains,
            raw_domains,
            offsets,
            warnings,
        }
    }
}
