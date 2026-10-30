use crate::model::MpiCallSummary;
use rprof_core::stream::{Attribute, Payload, Record};
use std::collections::BTreeMap;

#[derive(Default)]
struct Totals {
    calls: u64,
    errors: u64,
    total: u128,
    max: u64,
    bytes: u128,
    known: u64,
}
#[derive(Default)]
pub(crate) struct MpiAnalysis {
    calls: BTreeMap<(String, i32, String), Totals>,
    pub omitted: u64,
    pub released: u64,
    pub limited: bool,
}
impl MpiAnalysis {
    pub fn observe(&mut self, record: &Record) {
        let Record::Chunk(chunk) = record else { return };
        for event in &chunk.events {
            let Payload::Span(span) = &event.payload else {
                continue;
            };
            let a = &event.attributes;
            self.limited |= matches!(a.get("mpi.tracking_limit"), Some(Attribute::Bool(true)));
            if matches!(a.get("mpi.request_state"),Some(Attribute::Text(state)) if state=="tracking_released")
            {
                self.released += 1;
            }
            if !matches!(a.get("mpi.record_type"),Some(Attribute::Text(kind)) if kind=="call") {
                continue;
            }
            let rank = match a.get("mpi.rank") {
                Some(Attribute::Signed(v)) => i32::try_from(*v).unwrap_or(-1),
                _ => -1,
            };
            let key = (chunk.shard_id.to_string(), rank, span.name.clone());
            if span.name.len() > 128 || (!self.calls.contains_key(&key) && self.calls.len() >= 8192)
            {
                self.omitted += 1;
                continue;
            }
            let total = self.calls.entry(key).or_default();
            total.calls += 1;
            total.total += span.duration_ns as u128;
            total.max = total.max.max(span.duration_ns);
            if matches!(a.get("mpi.result"),Some(Attribute::Signed(result)) if *result!=0) {
                total.errors += 1;
            }
            if let Some(Attribute::Unsigned(bytes)) = a.get("mpi.bytes") {
                total.bytes += *bytes as u128;
                total.known += 1;
            }
        }
    }
    pub fn finish(self) -> Vec<MpiCallSummary> {
        self.calls
            .into_iter()
            .map(|((shard, rank, name), s)| MpiCallSummary {
                shard,
                rank,
                name,
                calls: s.calls.to_string(),
                errors: s.errors.to_string(),
                total_ns: s.total.to_string(),
                max_ns: s.max.to_string(),
                logical_bytes: s.bytes.to_string(),
                calls_with_bytes: s.known.to_string(),
            })
            .collect()
    }
}
