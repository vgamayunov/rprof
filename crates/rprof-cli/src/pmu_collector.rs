use crate::{PmuScope, RecordArgs, capture_sink::CaptureSink, record::monotonic_ns};
use anyhow::{Context, Result, ensure};
use rprof_core::{
    CounterSample, CounterValue,
    stream::{Attribute, Event, Payload},
};
use rprof_perf::{Counter, EventSpec, Prepared};
use std::{collections::BTreeMap, fs::File, io::Read};

pub fn prepare(args: &RecordArgs) -> Result<Prepared> {
    let names: Vec<_> = if args.pmu_events.is_empty() {
        vec![
            "cycles",
            "instructions",
            "cache-references",
            "cache-misses",
            "task-clock",
        ]
    } else {
        args.pmu_events.iter().map(String::as_str).collect()
    };
    ensure!(
        names.len() <= rprof_perf::MAX_EVENTS,
        "at most 32 PMU events may be requested"
    );
    let mut specs = Vec::new();
    for name in names {
        let spec = EventSpec::parse(name).map_err(anyhow::Error::msg)?;
        ensure!(!specs.contains(&spec), "duplicate PMU event {}", spec.name);
        specs.push(spec);
    }
    Ok(Prepared::new(
        specs,
        args.pmu_scope == PmuScope::WorkloadTree,
        args.pmu_kernel,
    )?)
}

fn bounded_file(path: &str, limit: u64) -> Option<String> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(limit)
        .read_to_string(&mut text)
        .ok()?;
    Some(text.trim().to_owned())
}
fn host_metadata() -> BTreeMap<String, Attribute> {
    let mut a = BTreeMap::new();
    for (name, path) in [
        ("kernel", "/proc/sys/kernel/osrelease"),
        (
            "perf_event_paranoid",
            "/proc/sys/kernel/perf_event_paranoid",
        ),
        ("online_cpus", "/sys/devices/system/cpu/online"),
        ("numa_nodes", "/sys/devices/system/node/online"),
        (
            "cpu0_package",
            "/sys/devices/system/cpu/cpu0/topology/physical_package_id",
        ),
        ("cpu0_core", "/sys/devices/system/cpu/cpu0/topology/core_id"),
        (
            "cpu0_thread_siblings",
            "/sys/devices/system/cpu/cpu0/topology/thread_siblings_list",
        ),
        (
            "cpu0_governor",
            "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
        ),
        (
            "cpu0_min_khz",
            "/sys/devices/system/cpu/cpu0/cpufreq/scaling_min_freq",
        ),
        (
            "cpu0_max_khz",
            "/sys/devices/system/cpu/cpu0/cpufreq/scaling_max_freq",
        ),
    ] {
        if let Some(value) = bounded_file(path, 2048) {
            a.insert(format!("pmu.host.{name}"), Attribute::Text(value));
        }
    }
    if let Some(cpu) = bounded_file("/proc/cpuinfo", 16384) {
        for key in [
            "vendor_id",
            "model name",
            "cpu family",
            "model",
            "stepping",
            "cpu MHz",
        ] {
            if let Some(value) = cpu
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(k, _)| k.trim() == key)
                .map(|(_, v)| v.trim())
            {
                a.insert(
                    format!("pmu.host.{}", key.replace(' ', "_")),
                    Attribute::Text(value.into()),
                );
            }
        }
    }
    if let Some(status) = bounded_file("/proc/self/status", 16384) {
        for key in ["Cpus_allowed_list", "Mems_allowed_list"] {
            if let Some(value) = status
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{key}:")))
            {
                a.insert(
                    format!("pmu.host.{key}"),
                    Attribute::Text(value.trim().into()),
                );
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir("/sys/bus/event_source/devices") {
        let mut names: Vec<_> = entries
            .flatten()
            .take(256)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        a.insert(
            "pmu.host.event_sources".into(),
            Attribute::Text(names.join(", ").chars().take(2048).collect()),
        );
    }
    for index in 0..8 {
        let base = format!("/sys/devices/system/cpu/cpu0/cache/index{index}");
        if let Some(size) = bounded_file(&format!("{base}/size"), 128) {
            let level = bounded_file(&format!("{base}/level"), 128).unwrap_or_default();
            let kind = bounded_file(&format!("{base}/type"), 128).unwrap_or_default();
            let cpus = bounded_file(&format!("{base}/shared_cpu_list"), 1024).unwrap_or_default();
            a.insert(
                format!("pmu.host.cpu0_cache{index}"),
                Attribute::Text(format!("L{level} {kind}: {size}; CPUs {cpus}")),
            );
        }
    }
    a
}
struct State {
    counter: Counter,
    failed: bool,
    measured: bool,
}
#[derive(Clone, Copy)]
pub struct Settings {
    pub scope: PmuScope,
    pub kernel: bool,
    pub interval_ms: u64,
}
pub struct Running {
    states: Vec<State>,
    scope: PmuScope,
    kernel: bool,
    pid: u32,
    origin: u64,
    interval_ns: u64,
    next_read: u64,
}
impl Running {
    pub fn start(
        prepared: Prepared,
        settings: Settings,
        pid: u32,
        out: &mut CaptureSink,
        origin: u64,
        at: u64,
    ) -> Result<Self> {
        let counters = prepared
            .receive()
            .context("receiving pre-exec perf counters")?;
        let mut host = host_metadata();
        host.insert("pmu.role".into(), Attribute::Text("host".into()));
        Self::write(out, at, "pmu.meta.host", 0, "metadata", host, true)?;
        let mut this = Self {
            states: counters
                .into_iter()
                .map(|counter| State {
                    counter,
                    failed: false,
                    measured: false,
                })
                .collect(),
            scope: settings.scope,
            kernel: settings.kernel,
            pid,
            origin,
            interval_ns: settings.interval_ms * 1_000_000,
            next_read: at,
        };
        for state in &this.states {
            let mut a = this.attrs(&state.counter.spec);
            a.insert("pmu.role".into(), Attribute::Text("event".into()));
            let status = match &state.counter.fd {
                Ok(_) => "available",
                Err(errno) => {
                    a.insert("pmu.errno".into(), Attribute::Signed(*errno as i64));
                    a.insert(
                        "pmu.error".into(),
                        Attribute::Text(std::io::Error::from_raw_os_error(*errno).to_string()),
                    );
                    eprintln!(
                        "PMU {}: {} ({})",
                        state.counter.spec.name,
                        rprof_perf::error_status(*errno),
                        std::io::Error::from_raw_os_error(*errno)
                    );
                    rprof_perf::error_status(*errno)
                }
            };
            a.insert("pmu.status".into(), Attribute::Text(status.into()));
            Self::write(out, at, "pmu.meta.event", 0, "metadata", a, true)?;
        }
        this.sample(out, at, false)?;
        Ok(this)
    }
    fn attrs(&self, spec: &EventSpec) -> BTreeMap<String, Attribute> {
        BTreeMap::from([
            ("pmu.event".into(), Attribute::Text(spec.name.clone())),
            ("pmu.type".into(), Attribute::Unsigned(spec.kind as u64)),
            ("pmu.config".into(), Attribute::Unsigned(spec.config)),
            (
                "pmu.scope".into(),
                Attribute::Text(self.scope.name().into()),
            ),
            (
                "pmu.privilege".into(),
                Attribute::Text(if self.kernel { "user+kernel" } else { "user" }.into()),
            ),
            ("pmu.pid".into(), Attribute::Unsigned(self.pid as u64)),
            ("pmu.unit".into(), Attribute::Text(spec.unit().into())),
        ])
    }
    fn write(
        out: &mut CaptureSink,
        at: u64,
        name: &str,
        value: u64,
        unit: &str,
        attributes: BTreeMap<String, Attribute>,
        reliable: bool,
    ) -> Result<()> {
        let event = Event {
            timestamp_ns: at,
            attributes,
            payload: Payload::Counter(CounterSample {
                track_id: 0,
                timestamp_ns: at,
                name: name.into(),
                value: CounterValue::Integer(value),
                unit: unit.into(),
            }),
        };
        if reliable {
            out.reliable(event)
        } else {
            out.emit(event)
        }
    }
    pub fn sample(&mut self, out: &mut CaptureSink, at: u64, final_read: bool) -> Result<()> {
        if !final_read && at < self.next_read {
            return Ok(());
        }
        self.next_read = at.saturating_add(self.interval_ns);
        for index in 0..self.states.len() {
            if self.states[index].counter.fd.is_err() {
                continue;
            }
            let mut a = self.attrs(&self.states[index].counter.spec);
            let state = &mut self.states[index];
            // Freeze the entire inherited counter before the final read.
            let disabled = if final_read {
                state.counter.disable()
            } else {
                Ok(())
            };
            if state.failed {
                continue;
            }
            let begin = monotonic_ns()?.saturating_sub(self.origin);
            let reading = disabled.and_then(|_| state.counter.read());
            let end = monotonic_ns()?.saturating_sub(self.origin);
            match reading {
                Ok(value) if value.running_ns <= value.enabled_ns => {
                    state.measured |= value.running_ns > 0;
                    a.insert("pmu.role".into(), Attribute::Text("count".into()));
                    a.insert(
                        "pmu.status".into(),
                        Attribute::Text(
                            if value.running_ns > 0 {
                                "measured"
                            } else {
                                "not_counted"
                            }
                            .into(),
                        ),
                    );
                    a.insert(
                        "pmu.time_enabled_ns".into(),
                        Attribute::Unsigned(value.enabled_ns),
                    );
                    a.insert(
                        "pmu.time_running_ns".into(),
                        Attribute::Unsigned(value.running_ns),
                    );
                    a.insert("pmu.read_start_ns".into(), Attribute::Unsigned(begin));
                    a.insert("pmu.read_end_ns".into(), Attribute::Unsigned(end));
                    a.insert("pmu.final".into(), Attribute::Bool(final_read));
                    if let Some(scaled) = value.scaled() {
                        a.insert(
                            "pmu.scaled_estimate".into(),
                            Attribute::Text(scaled.to_string()),
                        );
                    }
                    Self::write(
                        out,
                        end,
                        &format!("pmu.{}.raw", state.counter.spec.name),
                        value.raw,
                        state.counter.spec.unit(),
                        a,
                        final_read,
                    )?;
                }
                result => {
                    state.failed = true;
                    let detail = match result {
                        Err(e) => e.to_string(),
                        Ok(_) => "time_running exceeded time_enabled".into(),
                    };
                    a.insert("pmu.role".into(), Attribute::Text("event".into()));
                    a.insert("pmu.status".into(), Attribute::Text("read_error".into()));
                    a.insert("pmu.error".into(), Attribute::Text(detail));
                    Self::write(out, end, "pmu.meta.event", 0, "metadata", a, true)?;
                }
            }
        }
        Ok(())
    }
    pub fn finish(mut self, out: &mut CaptureSink, at: u64) -> Result<()> {
        self.sample(out, at, true)?;
        let measured = self
            .states
            .iter()
            .filter(|s| s.measured && !s.failed)
            .count();
        eprintln!(
            "PMU: {measured}/{} requested events measured; scope={}; privilege={}",
            self.states.len(),
            self.scope.name(),
            if self.kernel { "user+kernel" } else { "user" }
        );
        ensure!(
            measured > 0,
            "No usable perf counters; inspect PMU event diagnostics, perf_event_paranoid, permissions, and hardware PMU availability"
        );
        ensure!(
            !self.states.iter().any(|s| s.failed),
            "Perf counter read failed; saved PMU values may be partial"
        );
        Ok(())
    }
}
