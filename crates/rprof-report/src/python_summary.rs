use crate::model::*;
use rprof_core::{
    CounterValue,
    stream::{Attribute, Payload, Record},
};
use std::collections::{BTreeMap, HashMap};

fn text(a: &BTreeMap<String, Attribute>, key: &str) -> String {
    match a.get(key) {
        Some(Attribute::Text(v)) => v.chars().take(4096).collect(),
        Some(Attribute::Unsigned(v)) => v.to_string(),
        _ => String::new(),
    }
}
fn number(a: &BTreeMap<String, Attribute>, key: &str) -> u64 {
    if let Some(Attribute::Unsigned(v)) = a.get(key) {
        *v
    } else {
        0
    }
}
fn flag(a: &BTreeMap<String, Attribute>, key: &str) -> bool {
    matches!(a.get(key), Some(Attribute::Bool(true)))
}
#[derive(Default)]
struct Total {
    count: u64,
    total: u128,
    self_ns: u128,
    max: u64,
    yields: u64,
    unwinds: u64,
}
type FunctionKey = (String, String, String, u64, String, String, u64, String);
#[derive(Default)]
pub(crate) struct PythonAnalysis {
    runtimes: BTreeMap<(String, String), PythonRuntime>,
    functions: BTreeMap<FunctionKey, Total>,
    receiver_dropped: BTreeMap<String, u64>,
    malformed: BTreeMap<String, u64>,
    omitted: u64,
}
impl PythonAnalysis {
    pub fn observe(&mut self, record: &Record) {
        let Record::Chunk(chunk) = record else {
            return;
        };
        let shard = chunk.shard_id.to_string();
        for event in &chunk.events {
            let a = &event.attributes;
            let role = text(a, "python.role");
            if role.is_empty() {
                continue;
            }
            if role == "diagnostic" {
                if let Payload::Counter(counter) = &event.payload {
                    if let CounterValue::Integer(value) = counter.value {
                        match counter.name.as_str() {
                            "python.receiver_dropped" => {
                                self.receiver_dropped.insert(shard.clone(), value);
                            }
                            "python.malformed" => {
                                self.malformed.insert(shard.clone(), value);
                            }
                            _ => (),
                        }
                    }
                }
                continue;
            }
            let source = text(a, "python.source");
            let key = (shard.clone(), source.clone());
            if !self.runtimes.contains_key(&key) && self.runtimes.len() >= 4096 {
                self.omitted += 1;
                continue;
            }
            let runtime = self.runtimes.entry(key).or_insert_with(|| PythonRuntime {
                shard: shard.clone(),
                source: source.clone(),
                pid: text(a, "pid"),
                instance: text(a, "process_instance"),
                interpreter: text(a, "python.interpreter"),
                attempted: "0".into(),
                received: "0".into(),
                source_dropped: "0".into(),
                omitted_callbacks: "0".into(),
                sequence_gaps: "0".into(),
                unmatched: "0".into(),
                abandoned: "0".into(),
                inflight: "0".into(),
                source_errors: "0".into(),
                ..Default::default()
            });
            for (key, value) in [
                ("python.attempted", &mut runtime.attempted),
                ("python.received", &mut runtime.received),
                ("python.source_dropped", &mut runtime.source_dropped),
                ("python.omitted_callbacks", &mut runtime.omitted_callbacks),
                ("python.sequence_gaps", &mut runtime.sequence_gaps),
                ("python.unmatched", &mut runtime.unmatched),
                ("python.abandoned", &mut runtime.abandoned),
                ("python.inflight", &mut runtime.inflight),
                ("python.errors", &mut runtime.source_errors),
            ] {
                *value = value
                    .parse::<u64>()
                    .unwrap_or(0)
                    .max(number(a, key))
                    .to_string();
            }
            if role == "runtime" {
                runtime.initialized |= flag(a, "python.initialized");
                runtime.backend = text(a, "python.backend");
                runtime.version = text(a, "python.version");
                runtime.clock = text(a, "python.clock");
                runtime.detail = text(a, "python.detail");
            }
            if role == "source" {
                runtime.initialized |= flag(a, "python.initialized");
                runtime.finalized |= flag(a, "python.finalized");
            }
            if role == "error" {
                if runtime.errors.len() < 32 {
                    runtime.errors.push(text(a, "python.detail"));
                } else {
                    self.omitted += 1;
                }
            }
            if !matches!(role.as_str(), "function" | "range") {
                continue;
            }
            let Payload::Span(span) = &event.payload else {
                continue;
            };
            let key = (
                shard.clone(),
                source,
                role,
                number(a, "python.symbol"),
                span.name.clone(),
                text(a, "python.file"),
                number(a, "python.line"),
                text(a, "python.execution_kind"),
            );
            if !self.functions.contains_key(&key) && self.functions.len() >= 8192 {
                self.omitted += 1;
                continue;
            }
            let total = self.functions.entry(key).or_default();
            total.count += 1;
            total.total += span.duration_ns as u128;
            total.max = total.max.max(span.duration_ns);
            total.self_ns += number(a, "python.self_ns") as u128;
            match text(a, "python.boundary").as_str() {
                "yield" => total.yields += 1,
                "unwind" => total.unwinds += 1,
                _ => (),
            }
        }
    }
    pub fn finish(
        self,
        shards: &[ReportShard],
        warnings: &mut Vec<String>,
    ) -> Option<PythonSummary> {
        if self.runtimes.is_empty() && self.receiver_dropped.is_empty() && self.malformed.is_empty()
        {
            return None;
        }
        if self.runtimes.is_empty() {
            warnings.push("No Python interpreter initialization was observed.".into());
        }
        let domains: HashMap<_, _> = shards
            .iter()
            .map(|s| (s.id.as_str(), s.domain.as_str()))
            .collect();
        let runtimes=self.runtimes.into_values().map(|mut r|{
            r.domain=domains[r.shard.as_str()].into();
            if !r.initialized{warnings.push(format!("Python source {} has no confirmed interpreter initialization.",r.source));}
            if !r.finalized{warnings.push(format!("Python source {} has no confirmed final checkpoint; counts are lower bounds.",r.source));}
            for (value,label) in [(&r.source_dropped,"source packets dropped"),(&r.omitted_callbacks,"source observations omitted"),(&r.unmatched,"unmatched execution endpoints"),(&r.abandoned,"open frames/ranges at stop"),(&r.inflight,"callbacks in flight at stop"),(&r.source_errors,"instrumentation errors")]{if value!="0"{warnings.push(format!("Python source {}: {value} {label}.",r.source));}}
            for error in &r.errors{warnings.push(format!("Python source {}: {error}",r.source));}r
        }).collect();
        let functions = self
            .functions
            .into_iter()
            .map(
                |((shard, source, kind, symbol, name, file, line, execution_kind), t)| {
                    PythonFunction {
                        domain: domains[shard.as_str()].into(),
                        shard,
                        source,
                        self_ns: (kind == "function").then(|| t.self_ns.to_string()),
                        kind,
                        symbol: symbol.to_string(),
                        name,
                        file,
                        line: line.to_string(),
                        execution_kind,
                        segments: t.count.to_string(),
                        total_ns: t.total.to_string(),
                        max_ns: t.max.to_string(),
                        yields: t.yields.to_string(),
                        unwinds: t.unwinds.to_string(),
                    }
                },
            )
            .collect();
        if self.omitted > 0 {
            warnings.push(format!(
                "{} Python summary observations omitted by metadata limits.",
                self.omitted
            ));
        }
        let receiver_dropped = self
            .receiver_dropped
            .values()
            .map(|v| *v as u128)
            .sum::<u128>()
            .to_string();
        let malformed = self
            .malformed
            .values()
            .map(|v| *v as u128)
            .sum::<u128>()
            .to_string();
        if malformed != "0" {
            warnings.push(format!(
                "Python collector rejected {malformed} malformed/out-of-order observations."
            ));
        }
        Some(PythonSummary {
            runtimes,
            functions,
            receiver_dropped,
            malformed,
        })
    }
}
