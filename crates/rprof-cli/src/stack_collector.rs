use crate::{
    capture_sink::CaptureSink,
    record::monotonic_ns,
    stack_symbols::{Image, Mapping},
};
use anyhow::{Context, Result, ensure};
use rprof_core::{
    CounterSample, CounterValue, TrackKind,
    stream::{Attribute, Event, Payload, StackSample, StackStatus},
};
use rprof_perf::sampling::Record;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(crate) struct Running {
    ready: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<()>>>,
}
impl Running {
    pub fn start(
        prepared: rprof_perf::Prepared,
        sink: Arc<Mutex<CaptureSink>>,
        origin: u64,
        hz: u32,
    ) -> Result<Self> {
        let ready = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let started = Arc::clone(&ready);
        let stopping = Arc::clone(&stop);
        let worker = thread::Builder::new().name("rprof-stacks".into()).spawn(move || {
            let mut sampler = prepared.receive_stacks()
                .context("preparing user-space perf stack rings (check perf_event_paranoid/permissions and CPU affinity)")?;
            while !started.load(Ordering::Acquire) {
                if stopping.load(Ordering::Acquire) { return Ok(()); }
                thread::sleep(Duration::from_millis(1));
            }
            let mut state = State::default();
            let mut read = || -> Result<()> {
                sampler.read(|record| {
                    let mut out = sink.lock().map_err(|_| std::io::Error::other("capture writer lock poisoned"))?;
                    state.observe(record, &mut out, origin).map_err(std::io::Error::other)
                })?;
                Ok(())
            };
            let result = (|| -> Result<()> {
                while !stopping.load(Ordering::Acquire) {
                    read()?;
                    thread::sleep(Duration::from_millis(5));
                }
                read()?;
                thread::sleep(Duration::from_millis(1));
                read()
            })();
            let mut out = sink.lock().map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
            let at = monotonic_ns()?.saturating_sub(origin);
            for (name, value) in [
                ("samples", state.samples), ("lost", state.lost), ("throttled", state.throttled),
                ("unresolved_frames", out.stack_symbols.unresolved), ("symbol_errors", out.stack_symbols.failures),
                ("omitted_stacks", out.stack_omitted),
            ] {
                let mut event = Event::new(at, Payload::Counter(CounterSample {
                    track_id: 0, timestamp_ns: at, name: format!("stacks.meta.{name}"),
                    value: CounterValue::Integer(value), unit: "count".into(),
                }));
                event.attributes.insert("stacks.method".into(), Attribute::Text("user-space cpu-clock; frame-pointer chains; initial CPU affinity, inherited threads/children".into()));
                event.attributes.insert("stacks.requested_hz".into(), Attribute::Unsigned(hz as u64));
                event.attributes.insert("stacks.coverage".into(), Attribute::Text(
                    "Frame-pointer chains can omit callers; samples are not call counts or exact function durations. Kernel stacks and off-CPU time are not collected.".into()));
                if let Err(error) = &result {
                    event.attributes.insert("stacks.error".into(), Attribute::Text(format!("{error:#}")));
                }
                if !out.stack_symbols.last_error.is_empty() {
                    event.attributes.insert("stacks.symbol_error".into(), Attribute::Text(out.stack_symbols.last_error.clone()));
                }
                out.reliable(event)?;
            }
            result
        })?;
        Ok(Self {
            ready,
            stop,
            worker: Some(worker),
        })
    }
    pub fn activate(&self) {
        self.ready.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<()> {
        ensure!(
            !self.worker.as_ref().is_some_and(|w| w.is_finished()),
            "stack sampler stopped; inspect collection diagnostics"
        );
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow::anyhow!("stack sampler panicked"))?
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct State {
    processes: HashMap<(u32, u64), u64>,
    threads: HashMap<(u32, u64, u32), u64>,
    samples: u64,
    lost: u64,
    throttled: u64,
}
impl State {
    fn observe(&mut self, record: Record, out: &mut CaptureSink, origin: u64) -> Result<()> {
        let record_time = record.time();
        match record {
            Record::Comm {
                pid,
                name,
                exec,
                time,
            } => {
                ensure!(
                    out.stack_symbols.images.contains_key(&pid)
                        || out.stack_symbols.images.len() < 4096,
                    "stack process limit exceeded"
                );
                let image = out.stack_symbols.images.entry(pid).or_default();
                if exec && time != image.epoch {
                    *image = Image {
                        epoch: time,
                        name,
                        mappings: Vec::new(),
                    };
                } else {
                    image.name = name;
                }
            }
            Record::Mapping {
                pid,
                address,
                length,
                offset,
                path,
                device,
                inode,
                ..
            } => {
                let end = address
                    .checked_add(length)
                    .context("perf executable mapping overflow")?;
                out.stack_symbols.mapping(
                    pid,
                    Mapping {
                        start: address,
                        end,
                        offset,
                        path,
                        device,
                        inode,
                    },
                )?;
            }
            Record::Fork {
                pid, parent, time, ..
            } if pid != parent => {
                ensure!(
                    out.stack_symbols.images.contains_key(&pid)
                        || out.stack_symbols.images.len() < 4096,
                    "stack process limit exceeded"
                );
                let mut image = out
                    .stack_symbols
                    .images
                    .get(&parent)
                    .cloned()
                    .unwrap_or_default();
                image.epoch = time;
                out.stack_symbols.images.insert(pid, image);
            }
            Record::Exit { pid, tid, .. } => {
                if let Some(image) = out.stack_symbols.images.get(&pid) {
                    self.threads.remove(&(pid, image.epoch, tid));
                }
            }
            Record::Sample {
                pid,
                tid,
                time,
                period,
                ips,
            } => {
                out.stack_symbols.advance_python(time);
                ensure!(period > 0, "perf returned a zero sampling period");
                if time < origin {
                    return Ok(());
                }
                let at = monotonic_ns()?.saturating_sub(origin);
                ensure!(
                    time - origin <= at,
                    "perf sample is outside the monotonic clock domain"
                );
                let image = out
                    .stack_symbols
                    .images
                    .get(&pid)
                    .cloned()
                    .unwrap_or_default();
                let process_key = (pid, image.epoch);
                let process = if let Some(&id) = self.processes.get(&process_key) {
                    id
                } else {
                    ensure!(
                        self.processes.len() < 4096,
                        "stack process-image limit exceeded"
                    );
                    let id = out.track(
                        at,
                        0,
                        format!("CPU samples: {} (PID {pid})", image.name),
                        TrackKind::Process { pid },
                    )?;
                    self.processes.insert(process_key, id);
                    id
                };
                let key = (pid, image.epoch, tid);
                let track = if let Some(&id) = self.threads.get(&key) {
                    id
                } else {
                    let id = out.track(
                        at,
                        process,
                        format!("CPU samples: thread {tid}"),
                        TrackKind::Thread { tid: tid as u64 },
                    )?;
                    self.threads.insert(key, id);
                    id
                };
                let frames = out.stack_symbols.resolve(pid, image.epoch, &ips, false);
                let stack_id = out.stack(
                    at,
                    frames,
                    if ips.len() == 128 {
                        StackStatus::Truncated
                    } else {
                        StackStatus::Unknown
                    },
                )?;
                out.emit(Event::new(
                    at,
                    Payload::StackSample(StackSample {
                        track_id: track,
                        timestamp_ns: time - origin,
                        stack_id,
                        period_ns: period,
                    }),
                ))?;
                self.samples += 1;
            }
            Record::Lost { count, .. } => {
                self.lost = self.lost.saturating_add(count);
                out.set_ingress_loss("stacks", self.lost);
            }
            Record::Throttled { .. } => self.throttled += 1,
            _ => (),
        }
        out.stack_symbols.advance_python(record_time);
        Ok(())
    }
}
