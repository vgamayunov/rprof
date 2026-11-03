//! Periodic Linux host metrics. procfs/sysfs views may include other workloads
//! and the profiler itself, even when the application runs inside a container.
use crate::capture_sink::CaptureSink;
use anyhow::{Context, Result, ensure};
use rprof_core::{
    CounterSample, CounterValue, TrackKind,
    stream::{Attribute, Event, Payload},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAX_CPUS: usize = 8192;
const MAX_NODES: usize = 1024;
const MAX_FILE: u64 = 2 * 1024 * 1024;

// Never unlink an active lease: a new inode would allow two simultaneous owners.
// Closed/stale zero-byte files are harmless; flock releases on exit/crash.
fn lease(directory: &Path, session: Uuid, host_id: &str) -> Result<Option<fs::File>> {
    let uid = unsafe { libc::geteuid() };
    let hash = format!("{:x}", Sha256::digest(host_id.as_bytes()));
    let path = directory.join(format!("rprof-system-{uid}-{session}-{hash}.lock"));
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .with_context(|| format!("opening host sampling lease {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == uid,
        "invalid host sampling lease owner/type"
    );
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Ok(None);
        }
        return Err(error.into());
    }
    Ok(Some(file))
}

fn read(path: &Path) -> Result<String> {
    let mut text = String::new();
    fs::File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= MAX_FILE,
        "system metrics file exceeds limit"
    );
    Ok(text)
}

#[derive(Clone, Debug)]
struct Cpu([u64; 8]);

fn cpus(text: &str) -> Result<BTreeMap<String, Cpu>> {
    let mut result = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        if name != "cpu"
            && !name
                .strip_prefix("cpu")
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        ensure!(result.len() < MAX_CPUS + 1, "logical CPU limit exceeded");
        let mut ticks = [0; 8];
        // guest/guest_nice are already included in user/nice. Never add them twice.
        for tick in &mut ticks {
            *tick = fields.next().context("missing CPU tick field")?.parse()?;
        }
        ensure!(
            result.insert(name.into(), Cpu(ticks)).is_none(),
            "duplicate CPU"
        );
    }
    ensure!(result.contains_key("cpu"), "missing aggregate CPU ticks");
    Ok(result)
}

fn utilization(previous: &Cpu, current: &Cpu) -> Result<Option<f64>> {
    let mut delta = [0u64; 8];
    for (i, value) in delta.iter_mut().enumerate() {
        *value = current.0[i]
            .checked_sub(previous.0[i])
            .context("CPU ticks decreased; baseline reset")?;
    }
    let total: u128 = delta.iter().map(|n| *n as u128).sum();
    if total == 0 {
        return Ok(None);
    }
    let busy = total - delta[3] as u128 - delta[4] as u128;
    Ok(Some(100.0 * busy as f64 / total as f64))
}

#[derive(Debug, PartialEq)]
struct Memory {
    total: u64,
    free: u64,
    available: Option<u64>,
}
fn memory(text: &str) -> Result<Memory> {
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let tokens: Vec<_> = line.split_whitespace().collect();
        // sysfs node meminfo prefixes each line with "Node N".
        let start = if tokens.first() == Some(&"Node") {
            2
        } else {
            0
        };
        let Some(&name) = tokens.get(start) else {
            continue;
        };
        if !["MemTotal:", "MemFree:", "MemAvailable:"].contains(&name) {
            continue;
        }
        ensure!(
            tokens.get(start + 2) == Some(&"kB"),
            "unexpected memory unit"
        );
        let value = tokens
            .get(start + 1)
            .context("missing memory value")?
            .parse::<u64>()?
            .checked_mul(1024)
            .context("memory byte overflow")?;
        ensure!(
            fields.insert(name, value).is_none(),
            "duplicate memory field"
        );
    }
    let total = *fields.get("MemTotal:").context("missing MemTotal")?;
    let free = *fields.get("MemFree:").context("missing MemFree")?;
    let available = fields.get("MemAvailable:").copied();
    ensure!(
        free <= total && available.is_none_or(|n| n <= total),
        "inconsistent memory fields"
    );
    Ok(Memory {
        total,
        free,
        available,
    })
}

pub(crate) struct Running {
    proc_root: PathBuf,
    node_root: PathBuf,
    interval_ns: u64,
    last: Option<u64>,
    previous: BTreeMap<String, Cpu>,
    cpu_track: u64,
    memory_track: u64,
    cpu_tracks: BTreeMap<u32, u64>,
    node_tracks: BTreeMap<u32, u64>,
    errors: u64,
    last_error: String,
    sample_index: u64,
    host_id: String,
    _lease: fs::File,
}

impl Running {
    pub fn start(
        out: &mut CaptureSink,
        now: u64,
        interval_ms: u64,
        session: Uuid,
    ) -> Result<Option<Self>> {
        let host_id = format!(
            "{}/{}",
            read(Path::new("/proc/sys/kernel/hostname"))?.trim(),
            read(Path::new("/proc/sys/kernel/random/boot_id"))?.trim()
        );
        let Some(lease) = lease(&std::env::temp_dir(), session, &host_id)? else {
            let mut event = Event::new(
                now,
                Payload::Counter(CounterSample {
                    track_id: 0,
                    timestamp_ns: now,
                    name: "system.meta.delegated".into(),
                    value: CounterValue::Integer(1),
                    unit: "status".into(),
                }),
            );
            event
                .attributes
                .insert("system.host_id".into(), Attribute::Text(host_id));
            event.attributes.insert("system.detail".into(), Attribute::Text("another concurrent rank in this session owns host sampling; retain its capture".into()));
            out.reliable(event)?;
            return Ok(None);
        };
        let cpu_track = Self::track(
            out,
            now,
            None,
            "CPU utilization · host-wide",
            "cpu".into(),
            &host_id,
        )?;
        let memory_track = Self::track(
            out,
            now,
            None,
            "Memory used incl. cache · host-wide",
            "memory".into(),
            &host_id,
        )?;
        let mut running = Self {
            proc_root: "/proc".into(),
            node_root: "/sys/devices/system/node".into(),
            interval_ns: interval_ms * 1_000_000,
            last: None,
            previous: BTreeMap::new(),
            cpu_track,
            memory_track,
            cpu_tracks: BTreeMap::new(),
            node_tracks: BTreeMap::new(),
            errors: 0,
            last_error: String::new(),
            sample_index: 0,
            host_id,
            _lease: lease,
        };
        running.sample(out, now, true)?;
        Ok(Some(running))
    }
    fn track(
        out: &mut CaptureSink,
        now: u64,
        parent: Option<u64>,
        name: &str,
        resource: String,
        host_id: &str,
    ) -> Result<u64> {
        out.track_with_parent(
            now,
            parent,
            name.into(),
            TrackKind::System {
                resource,
                host_id: host_id.into(),
            },
            None,
        )
    }
    fn error(&mut self, source: &str, error: impl std::fmt::Display) {
        self.errors = self.errors.saturating_add(1);
        self.last_error = format!("{source}: {error}").chars().take(512).collect();
    }
    fn event(&self, now: u64, track: u64, name: &str, value: CounterValue, unit: &str) -> Event {
        let mut event = Event::new(
            now,
            Payload::Counter(CounterSample {
                track_id: track,
                timestamp_ns: now,
                name: name.into(),
                value,
                unit: unit.into(),
            }),
        );
        event.attributes.insert("system.scope".into(), Attribute::Text("host-wide procfs/sysfs view; includes other workloads and profiler; not process/cgroup usage".into()));
        event.attributes.insert(
            "system.interval_ns".into(),
            Attribute::Unsigned(self.interval_ns),
        );
        event.attributes.insert(
            "system.sample_index".into(),
            Attribute::Unsigned(self.sample_index),
        );
        event
    }
    fn emit_memory(
        &self,
        out: &mut CaptureSink,
        now: u64,
        track: u64,
        value: Memory,
    ) -> Result<()> {
        let mut event = self.event(
            now,
            track,
            "system.memory.used",
            CounterValue::Integer(value.total - value.free),
            "bytes",
        );
        event.attributes.insert(
            "system.definition".into(),
            Attribute::Text(
                "MemTotal - MemFree; includes cache/reclaimable memory, not application RSS".into(),
            ),
        );
        event.attributes.insert(
            "system.memory.total_bytes".into(),
            Attribute::Unsigned(value.total),
        );
        event.attributes.insert(
            "system.memory.free_bytes".into(),
            Attribute::Unsigned(value.free),
        );
        if let Some(available) = value.available {
            event.attributes.insert(
                "system.memory.available_bytes".into(),
                Attribute::Unsigned(available),
            );
        }
        out.emit(event)
    }
    fn nodes(&self) -> Result<Vec<(u32, PathBuf)>> {
        let mut nodes = Vec::new();
        for entry in fs::read_dir(&self.node_root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|s| s.strip_prefix("node"))
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            ensure!(nodes.len() < MAX_NODES, "NUMA node limit exceeded");
            nodes.push((id, entry.path().join("meminfo")));
        }
        ensure!(!nodes.is_empty(), "NUMA node meminfo unavailable");
        nodes.sort_by_key(|(id, _)| *id);
        Ok(nodes)
    }
    pub fn sample(&mut self, out: &mut CaptureSink, now: u64, force: bool) -> Result<()> {
        if !force
            && self
                .last
                .is_some_and(|last| now.saturating_sub(last) < self.interval_ns)
        {
            return Ok(());
        }
        let window_start = self.last;
        self.last = Some(now);
        self.sample_index += 1;
        match read(&self.proc_root.join("stat")).and_then(|s| cpus(&s)) {
            Ok(current) => {
                // Aggregate counters are not comparable when the online CPU set changes.
                let same_cpus = current.keys().eq(self.previous.keys());
                for (name, value) in &current {
                    let track = if name == "cpu" {
                        self.cpu_track
                    } else {
                        let id: u32 = name[3..].parse()?;
                        if !self.cpu_tracks.contains_key(&id) {
                            ensure!(
                                self.cpu_tracks.len() < MAX_CPUS,
                                "observed logical CPU limit exceeded"
                            );
                            let track = Self::track(
                                out,
                                now,
                                Some(self.cpu_track),
                                &format!("CPU {id}"),
                                format!("cpu:{id}"),
                                &self.host_id,
                            )?;
                            self.cpu_tracks.insert(id, track);
                        }
                        self.cpu_tracks[&id]
                    };
                    if name == "cpu" && !same_cpus {
                        continue;
                    }
                    if let Some(previous) = self.previous.get(name) {
                        match utilization(previous, value) {
                            Ok(Some(percent)) => {
                                let mut event = self.event(
                                    now,
                                    track,
                                    "system.cpu.utilization",
                                    CounterValue::Float(percent),
                                    "%",
                                );
                                event.attributes.insert("system.definition".into(), Attribute::Text("100 * delta(total-idle-iowait) / delta(total); includes steal; guest ticks not double counted".into()));
                                event.attributes.insert(
                                    "system.window_ns".into(),
                                    Attribute::Unsigned(
                                        now.saturating_sub(window_start.unwrap_or(now)),
                                    ),
                                );
                                out.emit(event)?;
                            }
                            Ok(None) => (), // No clock ticks elapsed: not a measured zero.
                            Err(error) => self.error(name, error),
                        }
                    }
                }
                self.previous = current;
            }
            Err(error) => {
                self.previous.clear();
                self.error("/proc/stat", error);
            }
        }
        match read(&self.proc_root.join("meminfo")).and_then(|s| memory(&s)) {
            Ok(value) => self.emit_memory(out, now, self.memory_track, value)?,
            Err(error) => self.error("/proc/meminfo", error),
        }
        match self.nodes() {
            Ok(nodes) => {
                for (id, path) in nodes {
                    if !self.node_tracks.contains_key(&id) {
                        ensure!(
                            self.node_tracks.len() < MAX_NODES,
                            "observed NUMA node limit exceeded"
                        );
                        let track = Self::track(
                            out,
                            now,
                            Some(self.memory_track),
                            &format!("NUMA node {id} · used incl. cache"),
                            format!("node:{id}"),
                            &self.host_id,
                        )?;
                        self.node_tracks.insert(id, track);
                    }
                    match read(&path).and_then(|s| memory(&s)) {
                        Ok(value) => self.emit_memory(out, now, self.node_tracks[&id], value)?,
                        Err(error) => self.error(&format!("NUMA node {id}"), error),
                    }
                }
            }
            Err(error) => self.error("NUMA", error),
        }
        // Reliable health checkpoints remain visible after timeline retention fills.
        let mut event = self.event(
            now,
            self.cpu_track,
            "system.meta.read_errors",
            CounterValue::Integer(self.errors),
            "count",
        );
        event.attributes.insert(
            "system.last_error".into(),
            Attribute::Text(self.last_error.clone()),
        );
        out.reliable(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_reads_leave_gaps_and_sparse_topology_is_preserved() {
        use rprof_capture::{Recorder, RecorderConfig};
        use rprof_core::stream::{Collector, End, EndReason, Header, Loss, Shard};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.jsonl");
        let config = RecorderConfig::default();
        let shard_id = Uuid::new_v4();
        let recorder = Recorder::create(
            &path,
            Header::new(Uuid::new_v4()),
            Shard {
                shard_id,
                parent_shard_id: None,
                hostname: "host".into(),
                boot_id: "boot".into(),
                monotonic_origin_ns: 0,
                started_unix_ns: 0,
                command: vec![],
                supervisor_pid: 1,
                collectors: vec![Collector {
                    name: "system".into(),
                    version: "test".into(),
                }],
                config: config.metadata(),
            },
            config,
        )
        .unwrap();
        let mut out = CaptureSink::new(recorder);
        let cpu_track =
            Running::track(&mut out, 0, None, "CPU", "cpu".into(), "host/boot").unwrap();
        let memory_track =
            Running::track(&mut out, 0, None, "Memory", "memory".into(), "host/boot").unwrap();
        let nodes = directory.path().join("nodes");
        fs::create_dir_all(nodes.join("node7")).unwrap();
        let mut running = Running {
            proc_root: directory.path().into(),
            node_root: nodes.clone(),
            interval_ns: 100,
            last: None,
            previous: BTreeMap::new(),
            cpu_track,
            memory_track,
            cpu_tracks: BTreeMap::new(),
            node_tracks: BTreeMap::new(),
            errors: 0,
            last_error: String::new(),
            sample_index: 0,
            host_id: "host/boot".into(),
            _lease: tempfile::tempfile().unwrap(),
        };
        fs::write(
            directory.path().join("meminfo"),
            "MemTotal: 2048 kB\nMemFree: 512 kB",
        )
        .unwrap();
        fs::write(
            nodes.join("node7/meminfo"),
            "Node 7 MemTotal: 2048 kB\nNode 7 MemFree: 512 kB",
        )
        .unwrap();
        for round in 0..5 {
            let stat = if round == 2 {
                "bad CPU data".into()
            } else {
                format!(
                    "cpu {} 0 0 {} 0 0 0 0\ncpu7 {} 0 0 {} 0 0 0 0",
                    round * 10,
                    round * 10,
                    round * 10,
                    round * 10
                )
            };
            fs::write(directory.path().join("stat"), stat).unwrap();
            if round == 2 {
                fs::remove_file(nodes.join("node7/meminfo")).unwrap();
            }
            running.sample(&mut out, round * 100, true).unwrap();
        }
        out.recorder
            .finish(End {
                shard_id,
                duration_ns: 400,
                exit_code: Some(0),
                signal: None,
                interrupted_by: None,
                reason: EndReason::Completed,
                loss: Loss::default(),
            })
            .unwrap();
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        let cpu_samples: Vec<_> = report
            .counters
            .iter()
            .filter(|c| {
                c.name == "system.cpu.utilization" && c.track.ends_with(&format!(":{cpu_track}"))
            })
            .collect();
        assert_eq!(
            cpu_samples
                .iter()
                .map(|c| c.timestamp_ns.as_str())
                .collect::<Vec<_>>(),
            ["100", "400"]
        );
        assert!(
            cpu_samples
                .iter()
                .all(|c| c.value.parse::<f64>().unwrap() == 50.0)
        );
        assert!(
            report
                .tracks
                .iter()
                .any(|t| t.details.get("resource").is_some_and(|r| r == "node:7"))
        );
        let node = report
            .tracks
            .iter()
            .find(|t| t.details.get("resource").is_some_and(|r| r == "node:7"))
            .unwrap();
        assert_eq!(
            report
                .counters
                .iter()
                .filter(|c| c.track == node.id)
                .count(),
            2
        );
        let summary_only = rprof_report::analyze(
            &path,
            rprof_report::Limits {
                retain_timeline: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            summary_only
                .summary
                .warnings
                .iter()
                .any(|w| w.contains("4 read/parse/topology errors"))
        );
    }
    #[test]
    fn host_leases_deduplicate_ranks_but_not_hosts_or_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let session = Uuid::new_v4();
        let first = lease(directory.path(), session, "host-a/boot-a")
            .unwrap()
            .unwrap();
        assert!(
            lease(directory.path(), session, "host-a/boot-a")
                .unwrap()
                .is_none()
        );
        assert!(
            lease(directory.path(), session, "host-b/boot-b")
                .unwrap()
                .is_some()
        );
        assert!(
            lease(directory.path(), Uuid::new_v4(), "host-a/boot-a")
                .unwrap()
                .is_some()
        );
        drop(first);
        assert!(
            lease(directory.path(), session, "host-a/boot-a")
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn cpu_deltas_exclude_idle_iowait_and_do_not_double_count_guest() {
        let before =
            cpus("cpu 100 10 20 200 30 2 3 5 90 8\ncpu7 50 5 10 100 15 1 1 2 40 4").unwrap();
        let after =
            cpus("cpu 120 10 30 240 50 4 6 10 110 8\ncpu7 50 5 10 100 15 1 1 2 40 4").unwrap();
        assert_eq!(
            utilization(&before["cpu"], &after["cpu"]).unwrap(),
            Some(40.0)
        );
        assert_eq!(utilization(&before["cpu7"], &after["cpu7"]).unwrap(), None);
        assert!(utilization(&after["cpu"], &before["cpu"]).is_err());
        assert!(cpus("cpu 1 invalid").is_err());
        assert_eq!(before.len(), 2);
    }
    #[test]
    fn host_and_sparse_numa_memory_preserve_exact_bytes_and_absence() {
        assert_eq!(
            memory("MemTotal: 9007199254741 kB\nMemFree: 100 kB\nMemAvailable: 200 kB").unwrap(),
            Memory {
                total: 9007199254741 * 1024,
                free: 102400,
                available: Some(204800),
            }
        );
        assert_eq!(
            memory("Node 7 MemTotal: 2048 kB\nNode 7 MemFree: 512 kB").unwrap(),
            Memory {
                total: 2097152,
                free: 524288,
                available: None,
            }
        );
        for bad in [
            "MemTotal: 2 kB",
            "MemTotal: 2 kB\nMemFree: 3 kB",
            "MemTotal: 2 MB\nMemFree: 1 kB",
            "MemTotal: 18446744073709551615 kB\nMemFree: 0 kB",
        ] {
            assert!(memory(bad).is_err());
        }
    }
}
