use crate::{
    capture_sink::CaptureSink,
    native_collector::{Mapping, Outcome, pump},
    record::monotonic_ns,
};
use anyhow::{Result, ensure};
use rprof_core::{
    Category, CounterSample, CounterValue, Span, TrackKind,
    stream::{Attribute, Event, Payload},
};
use rprof_ompt_protocol::*;
use std::{
    collections::{BTreeMap, HashMap},
    os::unix::net::UnixDatagram,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
};

type Image = (u32, u64);
type Thread = (u32, u64, u32);
type TaskKey = (u32, u64, u64);
type ScopeKey = (u32, u64, u16, u64, u32, i64);
const MAX_ACTIVE: usize = 65_536;

pub(crate) fn receive(
    socket: &UnixDatagram,
    mapping: &Mapping,
    stop: &AtomicBool,
    sink: &Mutex<CaptureSink>,
    origin: u64,
    capacity: usize,
) -> Result<Outcome> {
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let worker = thread::Builder::new()
            .name("rprof-openmp-ingress".into())
            .spawn_scoped(scope, || pump(socket, mapping, stop, sender))?;
        let result = consume(receiver, mapping, sink, origin);
        if result.is_err() {
            stop.store(true, Ordering::Release);
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("OpenMP ingress thread panicked"))??;
        result
    })
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.iter().position(|v| *v == 0).unwrap_or(bytes.len())])
        .into_owned()
}
fn callback(kind: u16) -> &'static str {
    match kind {
        THREAD_BEGIN => "ompt_callback_thread_begin",
        THREAD_END => "ompt_callback_thread_end",
        PARALLEL_BEGIN => "ompt_callback_parallel_begin",
        PARALLEL_END => "ompt_callback_parallel_end",
        IMPLICIT_BEGIN | IMPLICIT_END => "ompt_callback_implicit_task",
        WORK_BEGIN | WORK_END => "ompt_callback_work",
        SYNC_BEGIN | SYNC_END => "ompt_callback_sync_region",
        WAIT_BEGIN | WAIT_END => "ompt_callback_sync_region_wait",
        TASK_CREATE => "ompt_callback_task_create",
        TASK_SCHEDULE => "ompt_callback_task_schedule",
        DEPENDENCE => "ompt_callback_task_dependence",
        _ => "metadata",
    }
}
fn status(value: i64) -> &'static str {
    match value {
        1 => "complete",
        2 => "yield",
        3 => "cancel",
        4 => "detach",
        5 => "early_fulfill",
        6 => "late_fulfill",
        7 => "switch",
        8 => "taskwait_complete",
        _ => "unknown",
    }
}
fn support(value: i64) -> &'static str {
    match value {
        0 => "error",
        1 => "never",
        2 => "impossible",
        3 => "sometimes",
        4 => "sometimes_paired",
        5 => "always",
        _ => "unknown",
    }
}
fn attrs(raw: &OmpEvent, role: &str) -> BTreeMap<String, Attribute> {
    BTreeMap::from([
        ("openmp.role".into(), Attribute::Text(role.into())),
        (
            "openmp.callback".into(),
            Attribute::Text(callback(raw.kind).into()),
        ),
        ("openmp.thread".into(), Attribute::Unsigned(raw.thread_id)),
        (
            "openmp.parallel".into(),
            Attribute::Unsigned(raw.parallel_id),
        ),
        ("openmp.task".into(), Attribute::Unsigned(raw.task_id)),
        (
            "openmp.parent_task".into(),
            Attribute::Unsigned(raw.parent_task_id),
        ),
        ("openmp.flags".into(), Attribute::Unsigned(raw.flags)),
        (
            "openmp.codeptr".into(),
            Attribute::Text(format!("0x{:x}", raw.codeptr)),
        ),
        ("openmp.count".into(), Attribute::Unsigned(raw.count)),
        ("openmp.value".into(), Attribute::Signed(raw.value)),
        ("openmp.aux".into(), Attribute::Signed(raw.aux)),
        ("openmp.sequence".into(), Attribute::Unsigned(raw.sequence)),
        ("process_instance".into(), Attribute::Unsigned(raw.instance)),
        ("pid".into(), Attribute::Unsigned(raw.pid as u64)),
        ("tid".into(), Attribute::Unsigned(raw.tid as u64)),
    ])
}

#[derive(Clone)]
struct Open {
    raw: OmpEvent,
    track: u64,
    correlation: u64,
    role: &'static str,
    name: String,
}
struct Task {
    raw: OmpEvent,
    life: Option<Open>,
    initial: bool,
}
#[derive(Default)]
struct ImageCounts {
    track: u64,
    callbacks: BTreeMap<&'static str, u64>,
    gaps: u64,
    unmatched: u64,
    abandoned: u64,
}
#[derive(Default)]
struct Decode {
    scopes: HashMap<ScopeKey, Vec<Open>>,
    tasks: HashMap<TaskKey, Task>,
    running: HashMap<Thread, Open>,
    implicit: HashMap<Thread, Vec<u64>>,
    scope_tracks: HashMap<u64, u64>,
    task_tracks: HashMap<u64, u64>,
    last: HashMap<Thread, (u64, u64)>,
    counts: HashMap<Image, ImageCounts>,
    next_scope: u64,
    active: usize,
    seen: u64,
}

impl Decode {
    fn new_scope(&mut self, out: &mut CaptureSink, raw: &OmpEvent) -> Result<u64> {
        self.next_scope += 1;
        out.correlation("openmp.scope", raw.instance, self.next_scope)
    }
    fn task_corr(out: &mut CaptureSink, raw: &OmpEvent, id: u64) -> Result<u64> {
        out.correlation("openmp.task", raw.instance, id)
    }
    fn lane(
        &mut self,
        out: &mut CaptureSink,
        raw: &OmpEvent,
        thread: u64,
        at: u64,
        tasks: bool,
    ) -> Result<u64> {
        let tracks = if tasks {
            &mut self.task_tracks
        } else {
            &mut self.scope_tracks
        };
        if let Some(id) = tracks.get(&thread) {
            return Ok(*id);
        }
        let id = out.track(
            at,
            thread,
            if tasks {
                "OpenMP task lifetimes"
            } else {
                "OpenMP regions & implicit tasks"
            }
            .into(),
            TrackKind::Thread {
                tid: raw.tid as u64,
            },
        )?;
        tracks.insert(thread, id);
        Ok(id)
    }
    fn begin(open: &Open, out: &mut CaptureSink, origin: u64, at: u64) -> Result<()> {
        out.emit(Event {
            timestamp_ns: at,
            attributes: attrs(&open.raw, open.role),
            payload: Payload::ActivityStart {
                track_id: open.track,
                name: open.name.clone(),
                correlation_id: open.correlation,
                start_ns: Some(open.raw.timestamp_ns - origin),
            },
        })
    }
    fn end(
        open: Open,
        raw: &OmpEvent,
        out: &mut CaptureSink,
        origin: u64,
        at: u64,
        reason: &str,
    ) -> Result<()> {
        ensure!(
            raw.timestamp_ns >= open.raw.timestamp_ns,
            "OpenMP interval ends before its start"
        );
        let mut attributes = attrs(&open.raw, open.role);
        attributes.insert("openmp.end_tid".into(), Attribute::Unsigned(raw.tid as u64));
        attributes.insert("openmp.end_reason".into(), Attribute::Text(reason.into()));
        out.emit(Event {
            timestamp_ns: at,
            attributes,
            payload: Payload::Span(Span {
                track_id: open.track,
                name: open.name,
                category: Category::Openmp,
                start_ns: open.raw.timestamp_ns - origin,
                duration_ns: raw.timestamp_ns - open.raw.timestamp_ns,
                correlation_id: Some(open.correlation),
            }),
        })
    }
    fn edge(out: &mut CaptureSink, at: u64, from: u64, to: u64, relation: &str) -> Result<()> {
        out.emit(Event::new(
            at,
            Payload::Dependency {
                from_correlation_id: from,
                to_correlation_id: to,
                relation: relation.into(),
            },
        ))
    }
    fn key(raw: &OmpEvent, group: u16) -> ScopeKey {
        (
            raw.pid,
            raw.instance,
            group,
            if group == PARALLEL_BEGIN {
                raw.parallel_id
            } else {
                raw.task_id
            },
            if group == PARALLEL_BEGIN || group == IMPLICIT_BEGIN {
                0
            } else {
                raw.tid
            },
            if group >= WORK_BEGIN { raw.value } else { 0 },
        )
    }
    fn scope_begin(
        &mut self,
        raw: OmpEvent,
        out: &mut CaptureSink,
        origin: u64,
        at: u64,
        thread: u64,
    ) -> Result<()> {
        ensure!(
            self.active < MAX_ACTIVE,
            "OpenMP active-scope limit exceeded"
        );
        let (role, name, track, corr) = match raw.kind {
            PARALLEL_BEGIN => (
                "parallel",
                format!("parallel region {}", raw.parallel_id),
                self.lane(out, &raw, thread, at, false)?,
                out.correlation("openmp.parallel", raw.instance, raw.parallel_id)?,
            ),
            IMPLICIT_BEGIN => (
                "implicit_task",
                format!("implicit task {} / team index {}", raw.task_id, raw.aux),
                self.lane(out, &raw, thread, at, false)?,
                Self::task_corr(out, &raw, raw.task_id)?,
            ),
            WORK_BEGIN => ("work", text(&raw.name), thread, self.new_scope(out, &raw)?),
            SYNC_BEGIN => ("sync", text(&raw.name), thread, self.new_scope(out, &raw)?),
            WAIT_BEGIN => ("wait", text(&raw.name), thread, self.new_scope(out, &raw)?),
            _ => unreachable!(),
        };
        let key = Self::key(&raw, raw.kind);
        let open = Open {
            raw,
            track,
            correlation: corr,
            role,
            name,
        };
        Self::begin(&open, out, origin, at)?;
        let stack = self.scopes.entry(key).or_default();
        ensure!(stack.len() < 128, "OpenMP nesting limit exceeded");
        stack.push(open);
        self.active += 1;
        if raw.kind == IMPLICIT_BEGIN && raw.parallel_id != 0 {
            let parent = out.correlation("openmp.parallel", raw.instance, raw.parallel_id)?;
            Self::edge(out, at, parent, corr, "parallel team task")?;
        }
        Ok(())
    }
    fn scope_end(
        &mut self,
        raw: &OmpEvent,
        group: u16,
        out: &mut CaptureSink,
        origin: u64,
        at: u64,
    ) -> Result<()> {
        let mut key = Self::key(raw, group);
        if !self.scopes.contains_key(&key) {
            // Some runtimes omit task/parallel data at an implicit-barrier end.
            // Match only if there is exactly one compatible pending scope.
            let unknown = if group == PARALLEL_BEGIN {
                raw.parallel_id == 0
            } else {
                raw.task_id == 0
            };
            if unknown {
                let mut candidates = self.scopes.iter().filter(|(k, s)| {
                    k.0 == raw.pid
                        && k.1 == raw.instance
                        && k.2 == group
                        && k.5 == key.5
                        && s.last().is_some_and(|o| o.raw.tid == raw.tid)
                });
                if let Some((candidate, _)) = candidates.next() {
                    if candidates.next().is_none() {
                        key = *candidate;
                    }
                }
            }
        }
        let Some(stack) = self.scopes.get_mut(&key) else {
            self.counts
                .entry((raw.pid, raw.instance))
                .or_default()
                .unmatched += 1;
            return Ok(());
        };
        let open = stack.pop().unwrap();
        if stack.is_empty() {
            self.scopes.remove(&key);
        }
        self.active -= 1;
        Self::end(open, raw, out, origin, at, "scope_end")
    }
    fn execution_start(
        &mut self,
        raw: &OmpEvent,
        task_id: u64,
        thread: u64,
        out: &mut CaptureSink,
        origin: u64,
        at: u64,
    ) -> Result<()> {
        if task_id == 0 {
            return Ok(());
        }
        let Some(task) = self.tasks.get(&(raw.pid, raw.instance, task_id)) else {
            return Ok(());
        };
        if task.initial {
            return Ok(());
        }
        let mut context = *raw;
        context.task_id = task_id;
        context.parallel_id = task.raw.parallel_id;
        context.parent_task_id = task.raw.parent_task_id;
        context.flags = task.raw.flags;
        context.codeptr = task.raw.codeptr;
        let open = Open {
            raw: context,
            track: thread,
            correlation: Self::task_corr(out, raw, task_id)?,
            role: "task_execution",
            name: format!("task {task_id} scheduled execution"),
        };
        Self::begin(&open, out, origin, at)?;
        self.running.insert((raw.pid, raw.instance, raw.tid), open);
        Ok(())
    }
    fn execution_end(
        &mut self,
        raw: &OmpEvent,
        out: &mut CaptureSink,
        origin: u64,
        at: u64,
        reason: &str,
    ) -> Result<()> {
        if let Some(open) = self.running.remove(&(raw.pid, raw.instance, raw.tid)) {
            Self::end(open, raw, out, origin, at, reason)?;
        }
        Ok(())
    }
    fn invalidate_thread(&mut self, key: Thread) {
        let mut abandoned = u64::from(self.running.remove(&key).is_some());
        self.implicit.remove(&key);
        self.scopes.retain(|_, stack| {
            let old = stack.len();
            stack.retain(|s| (s.raw.pid, s.raw.instance, s.raw.tid) != key);
            self.active -= old - stack.len();
            abandoned += (old - stack.len()) as u64;
            !stack.is_empty()
        });
        self.counts.entry((key.0, key.1)).or_default().abandoned += abandoned;
        // Emitted activity starts remain open in the capture; no fabricated end.
    }
    fn event(&mut self, raw: OmpEvent, out: &mut CaptureSink, origin: u64, at: u64) -> Result<()> {
        let image = (raw.pid, raw.instance);
        let key = (raw.pid, raw.instance, raw.tid);
        if let Some((previous, thread_id)) = self.last.get(&key).copied() {
            if raw.kind == THREAD_BEGIN && thread_id != 0 && thread_id != raw.thread_id {
                self.invalidate_thread(key);
                out.retire_native_thread(raw.pid, raw.instance, raw.tid);
            } else if raw.sequence != previous.saturating_add(1) {
                self.counts.entry(image).or_default().gaps += raw
                    .sequence
                    .saturating_sub(previous.saturating_add(1))
                    .max(1);
                self.invalidate_thread(key);
            }
        } else if raw.sequence > 1 {
            self.counts.entry(image).or_default().gaps += raw.sequence - 1;
        }
        self.last.insert(key, (raw.sequence, raw.thread_id));
        let thread = out.native_thread(at, raw.pid, raw.instance, raw.tid, None)?;
        let counts = self.counts.entry(image).or_default();
        counts.track = thread;
        if callback(raw.kind) != "metadata" {
            *counts.callbacks.entry(callback(raw.kind)).or_default() += 1;
        }
        if raw.kind == CAPABILITY {
            let name = text(&raw.name);
            for kind in [
                THREAD_BEGIN,
                THREAD_END,
                PARALLEL_BEGIN,
                PARALLEL_END,
                IMPLICIT_BEGIN,
                WORK_BEGIN,
                SYNC_BEGIN,
                WAIT_BEGIN,
                TASK_CREATE,
                TASK_SCHEDULE,
                DEPENDENCE,
            ] {
                if name == callback(kind) {
                    counts.callbacks.entry(callback(kind)).or_default();
                }
            }
        }
        self.seen += 1;
        match raw.kind {
            RUNTIME | CAPABILITY | THREAD_BEGIN | THREAD_END | FINALIZE => {
                let (role, name, value) = match raw.kind {
                    RUNTIME => (
                        "runtime",
                        "openmp.meta.runtime".to_string(),
                        raw.value.max(0) as u64,
                    ),
                    CAPABILITY => (
                        "capability",
                        format!("openmp.meta.capability.{}", text(&raw.name)),
                        raw.value.max(0) as u64,
                    ),
                    FINALIZE => ("finalize", "openmp.meta.finalize".into(), 1),
                    _ => (
                        "thread",
                        "openmp.meta.thread".into(),
                        u64::from(raw.kind == THREAD_BEGIN),
                    ),
                };
                let mut attributes = attrs(&raw, role);
                if raw.kind == RUNTIME {
                    attributes.insert(
                        "openmp.runtime_version".into(),
                        Attribute::Text(text(&raw.detail)),
                    );
                    attributes.insert("openmp.initialized".into(), Attribute::Bool(true));
                }
                if raw.kind == CAPABILITY {
                    attributes.insert("openmp.callback".into(), Attribute::Text(text(&raw.name)));
                    attributes.insert(
                        "openmp.support".into(),
                        Attribute::Text(support(raw.value).into()),
                    );
                }
                out.reliable(Event {
                    timestamp_ns: at,
                    attributes,
                    payload: Payload::Counter(CounterSample {
                        track_id: thread,
                        timestamp_ns: raw.timestamp_ns - origin,
                        name,
                        value: CounterValue::Integer(value),
                        unit: "metadata".into(),
                    }),
                })?;
                if raw.kind == THREAD_END {
                    self.invalidate_thread(key);
                }
            }
            PARALLEL_BEGIN | WORK_BEGIN | SYNC_BEGIN | WAIT_BEGIN => {
                self.scope_begin(raw, out, origin, at, thread)?
            }
            PARALLEL_END => self.scope_end(&raw, PARALLEL_BEGIN, out, origin, at)?,
            WORK_END => self.scope_end(&raw, WORK_BEGIN, out, origin, at)?,
            SYNC_END => self.scope_end(&raw, SYNC_BEGIN, out, origin, at)?,
            WAIT_END => self.scope_end(&raw, WAIT_BEGIN, out, origin, at)?,
            IMPLICIT_BEGIN => {
                ensure!(
                    self.tasks.len() < MAX_ACTIVE,
                    "OpenMP active-task limit exceeded"
                );
                let initial = raw.flags & 1 != 0;
                self.tasks.insert(
                    (raw.pid, raw.instance, raw.task_id),
                    Task {
                        raw,
                        life: None,
                        initial,
                    },
                );
                if !initial {
                    self.scope_begin(raw, out, origin, at, thread)?;
                    self.execution_end(&raw, out, origin, at, "nested_implicit_begin")?;
                    let stack = self.implicit.entry(key).or_default();
                    ensure!(stack.len() < 128, "OpenMP implicit nesting limit exceeded");
                    stack.push(raw.task_id);
                    self.execution_start(&raw, raw.task_id, thread, out, origin, at)?;
                }
            }
            IMPLICIT_END => {
                if let Some(task) = self.tasks.remove(&(raw.pid, raw.instance, raw.task_id)) {
                    if !task.initial {
                        self.scope_end(&raw, IMPLICIT_BEGIN, out, origin, at)?;
                        if self
                            .running
                            .get(&key)
                            .is_some_and(|e| e.raw.task_id == raw.task_id)
                        {
                            self.execution_end(&raw, out, origin, at, "implicit_end")?;
                        }
                        if let Some(stack) = self.implicit.get_mut(&key) {
                            if stack.last() == Some(&raw.task_id) {
                                stack.pop();
                            } else {
                                stack.clear();
                            }
                            let next = stack.last().copied();
                            if stack.is_empty() {
                                self.implicit.remove(&key);
                            }
                            if let Some(next) = next {
                                self.execution_start(&raw, next, thread, out, origin, at)?;
                            }
                        }
                    }
                } else {
                    self.counts.entry(image).or_default().unmatched += 1;
                }
            }
            TASK_CREATE => {
                ensure!(
                    self.tasks.len() < MAX_ACTIVE,
                    "OpenMP active-task limit exceeded"
                );
                let mut raw = raw;
                if raw.parallel_id == 0 {
                    if let Some(parent) =
                        self.tasks.get(&(raw.pid, raw.instance, raw.parent_task_id))
                    {
                        raw.parallel_id = parent.raw.parallel_id;
                    }
                }
                let track = self.lane(out, &raw, thread, at, true)?;
                let correlation = Self::task_corr(out, &raw, raw.task_id)?;
                let life = Open {
                    raw,
                    track,
                    correlation,
                    role: "task_lifetime",
                    name: format!("task {} lifetime", raw.task_id),
                };
                Self::begin(&life, out, origin, at)?;
                if self
                    .tasks
                    .get(&(raw.pid, raw.instance, raw.parent_task_id))
                    .is_some_and(|t| !t.initial)
                {
                    let parent = Self::task_corr(out, &raw, raw.parent_task_id)?;
                    Self::edge(out, at, parent, correlation, "creates task")?;
                }
                self.tasks.insert(
                    (raw.pid, raw.instance, raw.task_id),
                    Task {
                        raw,
                        life: Some(life),
                        initial: false,
                    },
                );
            }
            TASK_SCHEDULE => {
                let notification = matches!(raw.value, 5 | 6 | 8);
                if !notification {
                    if self
                        .running
                        .get(&key)
                        .is_some_and(|o| o.raw.task_id == raw.task_id)
                    {
                        self.execution_end(&raw, out, origin, at, status(raw.value))?;
                    } else {
                        self.running.remove(&key);
                    }
                }
                if matches!(raw.value, 1 | 3 | 6 | 8) {
                    if let Some(task) = self.tasks.remove(&(raw.pid, raw.instance, raw.task_id)) {
                        if let Some(life) = task.life {
                            Self::end(life, &raw, out, origin, at, status(raw.value))?;
                        }
                    }
                }
                if !notification {
                    self.execution_start(&raw, raw.next_task_id, thread, out, origin, at)?;
                }
            }
            DEPENDENCE if raw.parent_task_id != 0 && raw.task_id != 0 => {
                let from = Self::task_corr(out, &raw, raw.parent_task_id)?;
                let to = Self::task_corr(out, &raw, raw.task_id)?;
                Self::edge(out, at, from, to, "OpenMP task dependence")?;
            }
            _ => (),
        }
        if self.seen % 1024 == 0 {
            self.checkpoint(out, at, false)?;
        }
        Ok(())
    }
    fn checkpoint(&self, out: &mut CaptureSink, at: u64, complete: bool) -> Result<()> {
        let mut images: Vec<_> = self.counts.iter().collect();
        images.sort_by_key(|(key, _)| **key);
        for ((pid, instance), counts) in images {
            let pending = self
                .scopes
                .values()
                .flat_map(|v| v.iter())
                .filter(|o| o.raw.pid == *pid && o.raw.instance == *instance)
                .count()
                + self
                    .running
                    .values()
                    .filter(|o| o.raw.pid == *pid && o.raw.instance == *instance)
                    .count()
                + self
                    .tasks
                    .values()
                    .filter(|t| {
                        t.raw.pid == *pid && t.raw.instance == *instance && t.life.is_some()
                    })
                    .count();
            for (name, value) in counts.callbacks.iter().map(|(n, v)| (*n, *v)).chain([
                ("sequence_gaps", counts.gaps),
                ("unmatched_endpoints", counts.unmatched),
                (
                    "abandoned_intervals",
                    counts.abandoned + if complete { pending as u64 } else { 0 },
                ),
            ]) {
                let mut attributes = BTreeMap::from([
                    ("openmp.role".into(), Attribute::Text("observed".into())),
                    ("openmp.callback".into(), Attribute::Text(name.into())),
                    ("openmp.observed_complete".into(), Attribute::Bool(complete)),
                    ("process_instance".into(), Attribute::Unsigned(*instance)),
                    ("pid".into(), Attribute::Unsigned(*pid as u64)),
                ]);
                attributes.insert(
                    "openmp.received_callbacks".into(),
                    Attribute::Unsigned(value),
                );
                out.reliable(Event {
                    timestamp_ns: at,
                    attributes,
                    payload: Payload::Counter(CounterSample {
                        track_id: counts.track,
                        timestamp_ns: at,
                        name: format!("openmp.meta.observed.{name}"),
                        value: CounterValue::Integer(value),
                        unit: "callbacks".into(),
                    }),
                })?;
            }
        }
        Ok(())
    }
}

fn consume(
    receiver: Receiver<OmpEvent>,
    mapping: &Mapping,
    sink: &Mutex<CaptureSink>,
    origin: u64,
) -> Result<Outcome> {
    let mut decode = Decode::default();
    let mut received = 0;
    for raw in receiver {
        if raw.magic != MAGIC
            || raw.version != VERSION
            || !(RUNTIME..=FINALIZE).contains(&raw.kind)
            || raw.timestamp_ns < origin
        {
            continue;
        }
        let now = monotonic_ns()?;
        if raw.timestamp_ns > now {
            continue;
        }
        received += 1;
        let mut out = sink
            .lock()
            .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
        out.set_ingress_loss("openmp", mapping.stats().dropped.load(Ordering::Relaxed));
        decode.event(raw, &mut out, origin, now - origin)?;
    }
    let stats = mapping.stats();
    let attempted = stats.attempted.load(Ordering::Acquire);
    let dropped = attempted.saturating_sub(received);
    let mut out = sink
        .lock()
        .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
    decode.checkpoint(&mut out, monotonic_ns()?.saturating_sub(origin), true)?;
    out.set_ingress_loss("openmp", dropped);
    Ok(Outcome {
        attempted,
        received,
        dropped,
        initialized: stats.initialized.load(Ordering::Acquire),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rprof_capture::{Recorder, RecorderConfig};
    use rprof_core::{
        Track,
        stream::{Collector, End, EndReason, Header, Loss, Shard},
    };
    use uuid::Uuid;
    fn setup() -> (tempfile::TempDir, std::path::PathBuf, Uuid, CaptureSink) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omp.jsonl");
        let id = Uuid::new_v4();
        let config = RecorderConfig::default();
        let recorder = Recorder::create(
            &path,
            Header::new(Uuid::new_v4()),
            Shard {
                shard_id: id,
                parent_shard_id: None,
                hostname: "host".into(),
                boot_id: "boot".into(),
                monotonic_origin_ns: 0,
                started_unix_ns: 0,
                command: vec![],
                supervisor_pid: 1,
                collectors: vec![Collector {
                    name: "openmp".into(),
                    version: "test".into(),
                }],
                config: config.metadata(),
            },
            config,
        )
        .unwrap();
        let mut out = CaptureSink::new(recorder);
        out.rank = Some(0);
        out.reliable(Event::new(
            0,
            Payload::Track(Track {
                id: 0,
                parent_id: None,
                name: "root".into(),
                rank: Some(0),
                kind: TrackKind::Process { pid: 1 },
            }),
        ))
        .unwrap();
        (dir, path, id, out)
    }
    fn raw(kind: u16, time: u64, tid: u32, task: u64, parallel: u64) -> OmpEvent {
        let mut r: OmpEvent = unsafe { std::mem::zeroed() };
        r.kind = kind;
        r.pid = 1;
        r.instance = 123;
        r.tid = tid;
        r.thread_id = tid as u64;
        r.timestamp_ns = time;
        r.task_id = task;
        r.parallel_id = parallel;
        r.flags = 2;
        r.count = 2;
        r
    }
    fn finish(decode: Decode, mut sink: CaptureSink, id: Uuid) {
        decode.checkpoint(&mut sink, 1000, true).unwrap();
        sink.recorder
            .finish(End {
                shard_id: id,
                duration_ns: 1000,
                exit_code: Some(0),
                signal: None,
                interrupted_by: None,
                reason: EndReason::Completed,
                loss: Loss::default(),
            })
            .unwrap();
    }
    #[test]
    fn migrated_tasks_and_detached_fulfillment_preserve_executing_thread() {
        let (_dir, path, id, mut sink) = setup();
        let mut decode = Decode::default();
        let mut sequences = HashMap::<u32, u64>::new();
        let mut feed = |mut r: OmpEvent| {
            let sequence = sequences.entry(r.tid).or_default();
            *sequence += 1;
            r.sequence = *sequence;
            decode.event(r, &mut sink, 0, 1000).unwrap();
        };
        let mut meta = raw(RUNTIME, 1, 1, 0, 0);
        meta.value = 202111;
        meta.detail[..4].copy_from_slice(b"test");
        feed(meta);
        feed(raw(PARALLEL_BEGIN, 10, 1, 0, 10));
        feed(raw(IMPLICIT_BEGIN, 20, 1, 20, 10));
        feed(raw(IMPLICIT_BEGIN, 21, 2, 21, 10));
        let mut create = raw(TASK_CREATE, 30, 1, 30, 0);
        create.parent_task_id = 20;
        create.flags = 4;
        feed(create);
        let mut s = raw(TASK_SCHEDULE, 40, 1, 20, 0);
        s.value = 7;
        s.next_task_id = 30;
        feed(s);
        s = raw(TASK_SCHEDULE, 60, 1, 30, 0);
        s.value = 2;
        s.next_task_id = 20;
        feed(s);
        s = raw(TASK_SCHEDULE, 70, 2, 21, 0);
        s.value = 7;
        s.next_task_id = 30;
        feed(s);
        s = raw(TASK_SCHEDULE, 100, 2, 30, 0);
        s.value = 1;
        s.next_task_id = 21;
        feed(s);
        create = raw(TASK_CREATE, 110, 1, 31, 0);
        create.parent_task_id = 20;
        create.flags = 4;
        feed(create);
        s = raw(TASK_SCHEDULE, 120, 1, 20, 0);
        s.value = 7;
        s.next_task_id = 31;
        feed(s);
        s = raw(TASK_SCHEDULE, 130, 1, 31, 0);
        s.value = 4;
        s.next_task_id = 20;
        feed(s);
        s = raw(TASK_SCHEDULE, 140, 2, 31, 0);
        s.value = 6;
        feed(s);
        feed(raw(IMPLICIT_END, 150, 1, 20, 0));
        feed(raw(IMPLICIT_END, 160, 2, 21, 0));
        feed(raw(PARALLEL_END, 170, 1, 0, 10));
        feed(raw(FINALIZE, 180, 1, 0, 0));
        finish(decode, sink, id);
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        let omp = report.openmp.as_ref().unwrap();
        assert_eq!(omp.tasks_created, "2");
        assert_eq!(omp.tasks_completed, "2");
        let executions: Vec<_> = report
            .spans
            .iter()
            .filter(|s| {
                s.attributes
                    .get("openmp.role")
                    .is_some_and(|v| v == "task_execution")
                    && s.attributes["openmp.task"] == "30"
            })
            .collect();
        assert_eq!(executions.len(), 2);
        assert_ne!(executions[0].track, executions[1].track);
        assert_eq!(executions[0].duration_ns, "20");
        assert_eq!(executions[1].duration_ns, "30");
        let other = report
            .spans
            .iter()
            .find(|s| {
                s.attributes.get("openmp.task").is_some_and(|v| v == "21")
                    && s.attributes
                        .get("openmp.role")
                        .is_some_and(|v| v == "task_execution")
                    && s.start_ns == "100"
            })
            .unwrap();
        assert_eq!(
            other.duration_ns, "60",
            "late fulfillment must not stop another task's execution"
        );
        assert!(report.spans.iter().all(|s| !s.incomplete));
    }
    #[test]
    fn a_sequence_gap_does_not_fabricate_a_matched_interval() {
        let (_dir, path, id, mut sink) = setup();
        let mut decode = Decode::default();
        let mut begin = raw(WORK_BEGIN, 10, 1, 20, 10);
        begin.sequence = 1;
        begin.value = 1;
        begin.name[..9].copy_from_slice(b"work.loop");
        decode.event(begin, &mut sink, 0, 100).unwrap();
        let mut end = begin;
        end.kind = WORK_END;
        end.sequence = 3;
        end.timestamp_ns = 30;
        decode.event(end, &mut sink, 0, 100).unwrap();
        finish(decode, sink, id);
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        assert_eq!(report.spans.len(), 1);
        assert!(report.spans[0].incomplete);
        assert_eq!(report.spans[0].category, "openmp");
        assert_eq!(
            report.openmp.as_ref().unwrap().runtimes[0].sequence_gaps,
            "1"
        );
        assert_eq!(
            report.openmp.as_ref().unwrap().runtimes[0].unmatched_endpoints,
            "1"
        );
    }
    #[test]
    fn native_collectors_share_threads_but_not_correlation_namespaces() {
        let (_dir, _path, _id, mut sink) = setup();
        let thread = sink.native_thread(1, 1, 123, 1, Some(0)).unwrap();
        assert_eq!(thread, sink.native_thread(1, 1, 123, 1, None).unwrap());
        assert_ne!(thread, sink.native_thread(1, 1, 124, 1, Some(0)).unwrap());
        assert_ne!(
            sink.correlation("mpi", 123, 1).unwrap(),
            sink.correlation("openmp.task", 123, 1).unwrap()
        );
        let first = sink.correlation("mpi", 123, 1).unwrap();
        for id in 1..100_000 {
            sink.correlation("mpi", 123, id).unwrap();
        }
        assert_eq!(first, sink.correlation("mpi", 123, 1).unwrap());
    }
}
