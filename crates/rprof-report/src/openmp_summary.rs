use crate::model::*;
use rprof_core::stream::{Attribute, Payload, Record};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

type RuntimeKey = (Uuid, u64);
type RegionKey = (Uuid, u64, u64);
type ThreadKey = (Uuid, u64, u64, u64);
const CALLBACKS: [&str; 12] = [
    "ompt_callback_thread_begin",
    "ompt_callback_thread_end",
    "ompt_callback_parallel_begin",
    "ompt_callback_parallel_end",
    "ompt_callback_implicit_task",
    "ompt_callback_work",
    "ompt_callback_sync_region",
    "ompt_callback_sync_region_wait",
    "ompt_callback_task_create",
    "ompt_callback_task_schedule",
    "ompt_callback_task_dependence",
    "runtime.rejected",
];
#[derive(Default)]
struct Runtime {
    pid: u64,
    version: String,
    spec: u64,
    initialized: bool,
    finalized: bool,
    callbacks: BTreeMap<String, (String, Option<u64>, bool, String)>,
    backend: String,
    gaps: u64,
    unmatched: u64,
    abandoned: u64,
}
#[derive(Default)]
struct Region {
    pid: u64,
    requested: u64,
    duration: Option<u64>,
    members: HashSet<u64>,
}
#[derive(Default)]
struct Thread {
    pid: u64,
    index: Option<i64>,
    work: u128,
    wait: u128,
    sync: u128,
    scheduled: u128,
    segments: u64,
    work_events: u64,
}
#[derive(Default)]
pub(crate) struct OpenmpAnalysis {
    runtimes: BTreeMap<RuntimeKey, Runtime>,
    regions: BTreeMap<RegionKey, Region>,
    threads: BTreeMap<ThreadKey, Thread>,
    created: u64,
    completed: u64,
    segments: u64,
    pub omitted: u64,
    gomp_seen: bool,
    gomp_apis: BTreeMap<(Uuid, u64, String), Aggregate>,
    gomp_bodies: BTreeMap<(Uuid, u64, u64, String, String), Aggregate>,
    gomp_created: u64,
    gomp_body_spans: u64,
    gomp_lifetimes: u64,
}
#[derive(Default)]
struct Aggregate {
    count: u64,
    total: u128,
    max: u64,
}
fn callback_allowed(name: &str) -> bool {
    CALLBACKS.contains(&name) || (name.starts_with("gomp.") && name.len() <= 64)
}
fn text<'a>(a: &'a BTreeMap<String, Attribute>, key: &str) -> Option<&'a str> {
    if let Some(Attribute::Text(v)) = a.get(key) {
        Some(v)
    } else {
        None
    }
}
fn uint(a: &BTreeMap<String, Attribute>, key: &str) -> u64 {
    if let Some(Attribute::Unsigned(v)) = a.get(key) {
        *v
    } else {
        0
    }
}
fn signed(a: &BTreeMap<String, Attribute>, key: &str) -> Option<i64> {
    if let Some(Attribute::Signed(v)) = a.get(key) {
        Some(*v)
    } else {
        None
    }
}
fn flag(a: &BTreeMap<String, Attribute>, key: &str) -> bool {
    matches!(a.get(key), Some(Attribute::Bool(true)))
}

impl OpenmpAnalysis {
    pub fn observe(&mut self, record: &Record) {
        let Record::Chunk(chunk) = record else {
            return;
        };
        for event in &chunk.events {
            let a = &event.attributes;
            let Some(role) = text(a, "openmp.role") else {
                continue;
            };
            let image = uint(a, "process_instance");
            let pid = uint(a, "pid");
            let tid = uint(a, "tid");
            let parallel = uint(a, "openmp.parallel");
            let rk = (chunk.shard_id, image);
            let pk = (chunk.shard_id, image, parallel);
            let tk = (chunk.shard_id, image, parallel, tid);
            if !self.runtimes.contains_key(&rk) && self.runtimes.len() >= 4096 {
                self.omitted += 1;
                continue;
            }
            let runtime = self.runtimes.entry(rk).or_default();
            runtime.pid = pid.max(runtime.pid);
            if let Some(backend) = text(a, "openmp.backend") {
                runtime.backend = backend.into();
            }
            self.gomp_seen |= runtime.backend == "gomp";
            match role {
                "runtime" => {
                    runtime.version = text(a, "openmp.runtime_version")
                        .unwrap_or("unknown")
                        .chars()
                        .take(512)
                        .collect();
                    runtime.spec = signed(a, "openmp.value").unwrap_or(0).max(0) as u64;
                    runtime.initialized = flag(a, "openmp.initialized");
                }
                "capability" => {
                    let name = text(a, "openmp.callback").unwrap_or("unknown");
                    if !callback_allowed(name) {
                        continue;
                    }
                    let status = text(a, "openmp.support").unwrap_or("unknown");
                    let status = if [
                        "error",
                        "never",
                        "impossible",
                        "sometimes",
                        "sometimes_paired",
                        "always",
                    ]
                    .contains(&status)
                    {
                        status
                    } else {
                        "unknown"
                    };
                    if runtime.callbacks.contains_key(name) || runtime.callbacks.len() < 64 {
                        let item = runtime.callbacks.entry(name.into()).or_default();
                        item.0 = status.into();
                        item.3 = text(a, "openmp.detail")
                            .unwrap_or("")
                            .chars()
                            .take(512)
                            .collect();
                    } else {
                        self.omitted += 1;
                    }
                }
                "observed" => {
                    let name = text(a, "openmp.callback").unwrap_or("unknown");
                    let count = uint(a, "openmp.received_callbacks");
                    match name {
                        "sequence_gaps" => runtime.gaps = runtime.gaps.max(count),
                        "unmatched_endpoints" => runtime.unmatched = runtime.unmatched.max(count),
                        "abandoned_intervals" => runtime.abandoned = runtime.abandoned.max(count),
                        name if callback_allowed(name)
                            && (runtime.callbacks.contains_key(name)
                                || runtime.callbacks.len() < 64) =>
                        {
                            let item = runtime.callbacks.entry(name.into()).or_default();
                            item.1 = Some(item.1.unwrap_or(0).max(count));
                            item.2 = flag(a, "openmp.observed_complete");
                        }
                        _ => (),
                    }
                }
                "finalize" => runtime.finalized = true,
                _ => (),
            }
            // These aggregates are independent of per-region detail retention.
            if role == "gomp_task_lifetime" {
                match event.payload {
                    Payload::ActivityStart { .. } => self.gomp_created += 1,
                    Payload::Span(_) => self.gomp_lifetimes += 1,
                    _ => (),
                }
            }
            if let Payload::Span(span) = &event.payload {
                if role == "gomp_api" {
                    let key = (chunk.shard_id, image, span.name.clone());
                    if self.gomp_apis.contains_key(&key) || self.gomp_apis.len() < 8192 {
                        let row = self.gomp_apis.entry(key).or_default();
                        row.count += 1;
                        row.total += span.duration_ns as u128;
                        row.max = row.max.max(span.duration_ns);
                    } else {
                        self.omitted += 1;
                    }
                }
                if matches!(role, "gomp_parallel_body" | "gomp_task_body") {
                    if role == "gomp_task_body" {
                        self.gomp_body_spans += 1;
                    }
                    let key = (
                        chunk.shard_id,
                        image,
                        tid,
                        role.trim_start_matches("gomp_").into(),
                        text(a, "openmp.codeptr").unwrap_or("unknown").into(),
                    );
                    if self.gomp_bodies.contains_key(&key) || self.gomp_bodies.len() < 16384 {
                        let row = self.gomp_bodies.entry(key).or_default();
                        row.count += 1;
                        row.total += span.duration_ns as u128;
                        row.max = row.max.max(span.duration_ns);
                    } else {
                        self.omitted += 1;
                    }
                }
            }
            if matches!(role, "parallel" | "implicit_task" | "gomp_parallel_body") && parallel != 0
            {
                if !self.regions.contains_key(&pk) && self.regions.len() >= 16384 {
                    self.omitted += 1;
                    continue;
                }
                let region = self.regions.entry(pk).or_default();
                region.pid = pid;
                if role == "parallel" {
                    region.requested = uint(a, "openmp.count");
                    if let Payload::Span(span) = &event.payload {
                        region.duration = Some(span.duration_ns);
                    }
                } else {
                    region.members.insert(tid);
                }
            }
            if matches!(
                role,
                "implicit_task"
                    | "work"
                    | "wait"
                    | "sync"
                    | "task_execution"
                    | "gomp_parallel_body"
            ) {
                if !self.threads.contains_key(&tk) && self.threads.len() >= 65536 {
                    self.omitted += 1;
                    continue;
                }
                let thread = self.threads.entry(tk).or_default();
                thread.pid = pid;
                if matches!(role, "implicit_task" | "gomp_parallel_body") {
                    thread.index = signed(a, "openmp.aux");
                }
                if let Payload::Span(span) = &event.payload {
                    let ns = span.duration_ns as u128;
                    match role {
                        "work" => {
                            thread.work += ns;
                            thread.work_events += 1;
                        }
                        "wait" => thread.wait += ns,
                        "sync" => thread.sync += ns,
                        "task_execution" => {
                            thread.scheduled += ns;
                            thread.segments += 1;
                            self.segments += 1;
                        }
                        _ => (),
                    }
                }
            }
            if role == "task_lifetime" {
                match event.payload {
                    Payload::ActivityStart { .. } => self.created += 1,
                    Payload::Span(_) => self.completed += 1,
                    _ => (),
                }
            }
        }
    }
    pub fn finish(
        self,
        shards: &[ReportShard],
        warnings: &mut Vec<String>,
    ) -> Option<OpenmpSummary> {
        if self.runtimes.is_empty() {
            return None;
        }
        let domains: HashMap<_, _> = shards
            .iter()
            .map(|s| (s.id.as_str(), s.domain.as_str()))
            .collect();
        if self.omitted > 0 {
            warnings.push(format!(
                "{} OpenMP summary observations omitted by metadata limits.",
                self.omitted
            ));
        }
        let runtimes=self.runtimes.into_iter().map(|((shard,image),r)|{
            if !r.initialized{warnings.push(format!("No {} OpenMP collector initialization observed for process {}; no timing is inferred.",if r.backend=="gomp"{"GNU ABI"}else{"OMPT"},r.pid));}
            else if !r.finalized{warnings.push(format!("OpenMP collector finalization was not observed for process {}.",r.pid));}
            if r.gaps+r.unmatched+r.abandoned>0{warnings.push(format!("OpenMP process {}: {} sequence gaps, {} unmatched endpoints, {} abandoned intervals; incomplete intervals are not treated as completed work.",r.pid,r.gaps,r.unmatched,r.abandoned));}
            let unsupported:Vec<_>=r.callbacks.iter().filter(|(_,v)|matches!(v.0.as_str(),"never"|"impossible"|"error")).map(|(name,_)|name.as_str()).collect();
            if !unsupported.is_empty(){warnings.push(format!("OpenMP unavailable/disabled observations for process {}: {}",r.pid,unsupported.join(", ")));}
            OpenmpRuntime{domain:domains[shard.to_string().as_str()].to_string(),shard:shard.to_string(),instance:image.to_string(),pid:r.pid.to_string(),backend:if r.backend.is_empty(){"ompt".into()}else{r.backend},version:r.version,omp_version:r.spec.to_string(),initialized:r.initialized,finalized:r.finalized,
                callbacks:r.callbacks.into_iter().map(|(name,(status,observed,complete,detail))|OpenmpCallback{name,status:if status.is_empty(){"unknown".into()}else{status},observed:observed.map(|n|n.to_string()),observed_complete:complete,detail}).collect(),
                sequence_gaps:r.gaps.to_string(),unmatched_endpoints:r.unmatched.to_string(),abandoned_intervals:r.abandoned.to_string(),
            }
        }).collect();
        let mut region_work = BTreeMap::<RegionKey, (u128, u128, Vec<u128>, u64)>::new();
        for ((shard, image, parallel, _), thread) in &self.threads {
            let entry = region_work.entry((*shard, *image, *parallel)).or_default();
            entry.0 += thread.work;
            entry.1 += thread.wait;
            entry.2.push(thread.work);
            entry.3 += thread.work_events;
        }
        let regions = self
            .regions
            .into_iter()
            .map(|((shard, image, parallel), r)| {
                let (work, wait, per_thread, events) = region_work
                    .remove(&(shard, image, parallel))
                    .unwrap_or_default();
                let min = (events > 0).then(|| per_thread.iter().min().copied().unwrap_or(0));
                let max = (events > 0).then(|| per_thread.iter().max().copied().unwrap_or(0));
                OpenmpRegion {
                    domain: domains[shard.to_string().as_str()].to_string(),
                    shard: shard.to_string(),
                    instance: image.to_string(),
                    pid: r.pid.to_string(),
                    parallel_id: parallel.to_string(),
                    requested_threads: r.requested.to_string(),
                    observed_threads: r.members.len().to_string(),
                    duration_ns: r.duration.map(|v| v.to_string()),
                    work_ns: work.to_string(),
                    wait_ns: wait.to_string(),
                    work_min_ns: min.map(|v| v.to_string()),
                    work_max_ns: max.map(|v| v.to_string()),
                    work_spread_ns: min.zip(max).map(|(a, b)| (b - a).to_string()),
                }
            })
            .collect();
        let threads = self
            .threads
            .into_iter()
            .map(|((shard, image, parallel, tid), t)| OpenmpThread {
                domain: domains[shard.to_string().as_str()].to_string(),
                shard: shard.to_string(),
                instance: image.to_string(),
                pid: t.pid.to_string(),
                tid: tid.to_string(),
                parallel_id: parallel.to_string(),
                team_index: t.index.map(|v| v.to_string()),
                work_ns: t.work.to_string(),
                wait_ns: t.wait.to_string(),
                sync_ns: t.sync.to_string(),
                scheduled_ns: t.scheduled.to_string(),
                execution_segments: t.segments.to_string(),
            })
            .collect();
        let gomp = if self.gomp_seen {
            Some(GompSummary {
                apis: self
                    .gomp_apis
                    .into_iter()
                    .map(|((shard, image, name), t)| GompApi {
                        domain: domains[shard.to_string().as_str()].into(),
                        shard: shard.to_string(),
                        instance: image.to_string(),
                        name,
                        calls: t.count.to_string(),
                        total_ns: t.total.to_string(),
                        max_ns: t.max.to_string(),
                    })
                    .collect(),
                bodies: self
                    .gomp_bodies
                    .into_iter()
                    .map(|((shard, image, tid, kind, codeptr), t)| GompBody {
                        domain: domains[shard.to_string().as_str()].into(),
                        shard: shard.to_string(),
                        instance: image.to_string(),
                        tid: tid.to_string(),
                        kind,
                        codeptr,
                        count: t.count.to_string(),
                        total_ns: t.total.to_string(),
                        max_ns: t.max.to_string(),
                    })
                    .collect(),
                task_creations: self.gomp_created.to_string(),
                task_body_spans: self.gomp_body_spans.to_string(),
                task_lifetimes_to_body_return: self.gomp_lifetimes.to_string(),
            })
        } else {
            None
        };
        Some(OpenmpSummary {
            runtimes,
            regions,
            threads,
            tasks_created: self.created.to_string(),
            tasks_completed: self.completed.to_string(),
            execution_segments: self.segments.to_string(),
            gomp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rprof_core::{
        Category, Span,
        stream::{Chunk, Event, Loss},
    };
    #[test]
    fn gomp_callsite_totals_continue_after_region_thread_budget_is_full() {
        let mut analysis = OpenmpAnalysis::default();
        let shard = Uuid::nil();
        for region in 0..65536 {
            analysis
                .threads
                .insert((shard, 1, region, 1), Thread::default());
        }
        let event = Event {
            timestamp_ns: 20,
            attributes: BTreeMap::from([
                (
                    "openmp.role".into(),
                    Attribute::Text("gomp_parallel_body".into()),
                ),
                ("openmp.backend".into(), Attribute::Text("gomp".into())),
                ("process_instance".into(), Attribute::Unsigned(1)),
                ("pid".into(), Attribute::Unsigned(1)),
                ("tid".into(), Attribute::Unsigned(1)),
                ("openmp.parallel".into(), Attribute::Unsigned(65537)),
                ("openmp.codeptr".into(), Attribute::Text("0x123".into())),
            ]),
            payload: Payload::Span(Span {
                track_id: 0,
                name: "parallel body".into(),
                category: Category::Openmp,
                start_ns: 10,
                duration_ns: 10,
                correlation_id: None,
            }),
        };
        analysis.observe(&Record::Chunk(Chunk {
            shard_id: shard,
            sequence: 0,
            loss: Loss::default(),
            events: vec![event],
        }));
        assert_eq!(analysis.gomp_bodies.values().next().unwrap().count, 1);
        assert_eq!(analysis.gomp_bodies.values().next().unwrap().total, 10);
        assert!(analysis.omitted > 0);
    }
}
