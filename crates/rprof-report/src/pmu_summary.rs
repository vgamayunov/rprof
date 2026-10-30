use crate::model::{PmuEvent, PmuHost, PmuMetrics, PmuSummary, ReportShard};
use rprof_core::{
    CounterValue,
    stream::{Attribute, Payload, Record},
};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
pub(crate) struct PmuAnalysis {
    events: BTreeMap<(String, String), PmuEvent>,
    hosts: BTreeMap<String, BTreeMap<String, String>>,
    omitted: u64,
}

fn text(a: &BTreeMap<String, Attribute>, key: &str) -> String {
    match a.get(key) {
        Some(Attribute::Text(v)) => v.chars().take(2048).collect(),
        Some(Attribute::Unsigned(v)) => v.to_string(),
        _ => String::new(),
    }
}
fn number(a: &BTreeMap<String, Attribute>, key: &str) -> Option<u64> {
    if let Some(Attribute::Unsigned(v)) = a.get(key) {
        Some(*v)
    } else {
        None
    }
}
impl PmuAnalysis {
    pub fn observe(&mut self, record: &Record) {
        let Record::Chunk(chunk) = record else {
            return;
        };
        let shard = chunk.shard_id.to_string();
        for event in &chunk.events {
            let a = &event.attributes;
            let role = text(a, "pmu.role");
            if role.is_empty() {
                continue;
            }
            if role == "host" {
                if self.hosts.len() >= 4096 && !self.hosts.contains_key(&shard) {
                    self.omitted += 1;
                    continue;
                }
                self.hosts.insert(
                    shard.clone(),
                    a.iter()
                        .filter_map(|(k, v)| {
                            let name = k.strip_prefix("pmu.host.")?;
                            if let Attribute::Text(value) = v {
                                Some((name.into(), value.chars().take(2048).collect()))
                            } else {
                                None
                            }
                        })
                        .take(64)
                        .collect(),
                );
                continue;
            }
            let name = text(a, "pmu.event");
            let key = (shard.clone(), name.clone());
            if name.is_empty() {
                continue;
            }
            if self.events.len() >= 8192 && !self.events.contains_key(&key) {
                self.omitted += 1;
                continue;
            }
            let row = self.events.entry(key).or_insert_with(|| PmuEvent {
                shard: shard.clone(),
                domain: String::new(),
                name,
                pid: text(a, "pmu.pid"),
                scope: text(a, "pmu.scope"),
                privilege: text(a, "pmu.privilege"),
                event_type: text(a, "pmu.type"),
                config: text(a, "pmu.config"),
                unit: text(a, "pmu.unit"),
                status: String::new(),
                error: None,
                raw: None,
                time_enabled_ns: None,
                time_running_ns: None,
                scaled_estimate: None,
                running_percent: None,
                multiplexed: false,
                samples: "0".into(),
                final_read: false,
            });
            row.status = text(a, "pmu.status");
            let error = text(a, "pmu.error");
            if !error.is_empty() {
                row.error = Some(error);
                row.final_read = false;
            }
            if role == "count" {
                let Payload::Counter(counter) = &event.payload else {
                    continue;
                };
                let CounterValue::Integer(raw) = counter.value else {
                    continue;
                };
                let (Some(enabled_ns), Some(running_ns)) = (
                    number(a, "pmu.time_enabled_ns"),
                    number(a, "pmu.time_running_ns"),
                ) else {
                    continue;
                };
                row.raw = Some(raw.to_string());
                row.time_enabled_ns = Some(enabled_ns.to_string());
                row.time_running_ns = Some(running_ns.to_string());
                row.scaled_estimate = rprof_perf::Reading {
                    raw,
                    enabled_ns,
                    running_ns,
                }
                .scaled()
                .map(|n| n.to_string());
                row.running_percent = (enabled_ns > 0 && running_ns <= enabled_ns)
                    .then(|| format!("{:.3}", running_ns as f64 / enabled_ns as f64 * 100.0));
                row.multiplexed = running_ns > 0 && running_ns < enabled_ns;
                row.samples = (row.samples.parse::<u64>().unwrap_or(0) + 1).to_string();
                row.final_read = matches!(a.get("pmu.final"), Some(Attribute::Bool(true)));
                if running_ns > enabled_ns {
                    row.status = "invalid_timing".into();
                    row.final_read = false;
                }
            }
        }
    }
    pub fn finish(
        mut self,
        shards: &[ReportShard],
        warnings: &mut Vec<String>,
    ) -> Option<PmuSummary> {
        if self.hosts.is_empty() && self.events.is_empty() {
            return None;
        }
        let domains: HashMap<_, _> = shards
            .iter()
            .map(|s| (s.id.as_str(), s.domain.as_str()))
            .collect();
        if self.omitted > 0 {
            warnings.push(format!(
                "{} PMU summary observations omitted by metadata limits.",
                self.omitted
            ));
        }
        for row in self.events.values_mut() {
            row.domain = domains[row.shard.as_str()].into();
            if row.status != "measured" {
                warnings.push(format!(
                    "PMU {} / {}: {}{}",
                    row.shard,
                    row.name,
                    row.status,
                    row.error
                        .as_ref()
                        .map(|e| format!(": {e}"))
                        .unwrap_or_default()
                ));
            }
            if matches!(
                row.status.as_str(),
                "available" | "measured" | "not_counted"
            ) && !row.final_read
            {
                warnings.push(format!(
                    "PMU {} / {} has no final read; values are a checkpoint, not a workload total.",
                    row.shard, row.name
                ));
            }
            if row.multiplexed {
                warnings.push(format!(
                    "PMU {} / {} ran for {}% of enabled time; scaled count is an estimate.",
                    row.shard,
                    row.name,
                    row.running_percent.as_deref().unwrap_or("unknown")
                ));
            }
        }
        let metrics = shards
            .iter()
            .filter(|s| self.events.keys().any(|(shard, _)| shard == &s.id))
            .map(|s| {
                let ratio = |numerator: &str, denominator: &str, factor: f64| -> Option<String> {
                    let n = self.events.get(&(s.id.clone(), numerator.into()))?;
                    let d = self.events.get(&(s.id.clone(), denominator.into()))?;
                    if !n.final_read
                        || !d.final_read
                        || n.status != "measured"
                        || d.status != "measured"
                        || n.scope != d.scope
                        || n.privilege != d.privilege
                    {
                        return None;
                    }
                    let a = n.scaled_estimate.as_ref()?.parse::<f64>().ok()?;
                    let b = d.scaled_estimate.as_ref()?.parse::<f64>().ok()?;
                    (b > 0.0).then(|| format!("{:.6}", factor * a / b))
                };
                PmuMetrics {
                    shard: s.id.clone(),
                    domain: s.domain.clone(),
                    ipc_estimate: ratio("instructions", "cycles", 1.0),
                    cache_miss_percent_estimate: ratio("cache-misses", "cache-references", 100.0),
                }
            })
            .collect();
        let hosts = self
            .hosts
            .into_iter()
            .map(|(shard, metadata)| PmuHost {
                domain: domains[shard.as_str()].into(),
                shard,
                metadata,
            })
            .collect();
        Some(PmuSummary {
            hosts,
            events: self.events.into_values().collect(),
            metrics,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rprof_core::{
        CounterSample,
        stream::{Chunk, Event, Loss},
    };
    use uuid::Uuid;
    fn count(name: &str, raw: u64, enabled: u64, running: u64, final_read: bool) -> Event {
        Event {
            timestamp_ns: 100,
            attributes: BTreeMap::from([
                ("pmu.role".into(), Attribute::Text("count".into())),
                ("pmu.event".into(), Attribute::Text(name.into())),
                (
                    "pmu.status".into(),
                    Attribute::Text(
                        if running > 0 {
                            "measured"
                        } else {
                            "not_counted"
                        }
                        .into(),
                    ),
                ),
                ("pmu.scope".into(), Attribute::Text("workload-tree".into())),
                ("pmu.privilege".into(), Attribute::Text("user".into())),
                ("pmu.time_enabled_ns".into(), Attribute::Unsigned(enabled)),
                ("pmu.time_running_ns".into(), Attribute::Unsigned(running)),
                ("pmu.final".into(), Attribute::Bool(final_read)),
            ]),
            payload: Payload::Counter(CounterSample {
                track_id: 0,
                timestamp_ns: 100,
                name: format!("pmu.{name}.raw"),
                value: CounterValue::Integer(raw),
                unit: "count".into(),
            }),
        }
    }
    fn analyze(events: Vec<Event>) -> (PmuSummary, Vec<String>) {
        let id = Uuid::nil();
        let mut analysis = PmuAnalysis::default();
        analysis.observe(&Record::Chunk(Chunk {
            shard_id: id,
            sequence: 0,
            loss: Loss::default(),
            events,
        }));
        let shard = ReportShard {
            id: id.to_string(),
            domain: "domain".into(),
            name: "test".into(),
            hostname: "test".into(),
            command: vec![],
            collectors: vec!["pmu".into()],
            origin_ns: "0".into(),
            end_ns: "100".into(),
            complete: true,
            exit_code: Some(0),
            signal: None,
            interrupted_by: None,
            end_reason: "Completed".into(),
            dropped: "0".into(),
        };
        let mut warnings = vec![];
        let report = analysis.finish(&[shard], &mut warnings).unwrap();
        (report, warnings)
    }
    #[test]
    fn cumulative_snapshots_are_not_summed_and_scaled_values_remain_exact() {
        let (report, warnings) = analyze(vec![
            count("cycles", 10, 20, 10, false),
            count("cycles", 20, 40, 20, true),
            count("instructions", u64::MAX, u64::MAX, 1, true),
        ]);
        let cycles = &report.events[0];
        assert_eq!(cycles.raw.as_deref(), Some("20"));
        assert_eq!(cycles.scaled_estimate.as_deref(), Some("40"));
        assert_eq!(cycles.samples, "2");
        assert_eq!(cycles.domain, "domain");
        assert!(cycles.multiplexed);
        assert_eq!(
            report.events[1].scaled_estimate,
            Some((u64::MAX as u128 * u64::MAX as u128).to_string())
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("scaled count is an estimate"))
        );
    }
    #[test]
    fn ratios_require_final_usable_counts_and_unscheduled_is_not_zero() {
        let (report, warnings) = analyze(vec![
            count("cycles", 40, 10, 10, true),
            count("instructions", 80, 10, 10, false),
            count("cache-misses", 0, 10, 0, true),
        ]);
        assert!(report.metrics[0].ipc_estimate.is_none());
        assert!(report.events[0].scaled_estimate.is_none());
        assert_eq!(report.events[0].status, "not_counted");
        assert!(warnings.iter().any(|w| w.contains("checkpoint")));
        let (report, _) = analyze(vec![
            count("cycles", 40, 10, 10, true),
            count("instructions", 80, 10, 10, true),
        ]);
        assert_eq!(report.metrics[0].ipc_estimate.as_deref(), Some("2.000000"));
        let (report, _) = analyze(vec![
            count("cycles", 0, 10, 10, true),
            count("instructions", 80, 10, 10, true),
        ]);
        assert!(report.metrics[0].ipc_estimate.is_none());
    }
}
