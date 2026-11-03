//! Read-only, host-scoped block and filesystem-client polling. These are not
//! process I/O counters and must not be added to POSIX call byte totals.
use crate::capture_sink::CaptureSink;
use anyhow::{Context, Result, ensure};
use rprof_core::{
    CounterSample, CounterValue, TrackKind,
    stream::{Attribute, Event, Payload},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAX_FILE: u64 = 2 * 1024 * 1024;
const MAX_RESOURCES: usize = 4096;
const MAX_DISKS: usize = 8192;
const MAX_PATH: usize = 4096;
type Counters = BTreeMap<&'static str, u128>;
type Components = BTreeMap<String, Counters>;
type LustreGroup = (Counters, Vec<String>, usize, Components);

fn read(path: &Path) -> Result<String> {
    let mut s = String::new();
    fs::File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_string(&mut s)?;
    ensure!(s.len() as u64 <= MAX_FILE, "oversized storage source");
    Ok(s)
}
fn label(s: &str) -> String {
    s.chars().take(MAX_PATH).collect()
}
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
            && b[i + 1] <= b'3'
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + b[i + 3] - b'0');
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
#[derive(Clone, Debug)]
struct Mount {
    dev: String,
    point: String,
    fstype: String,
    source: String,
}
fn mountinfo(s: &str) -> Result<Vec<Mount>> {
    let mut mounts = Vec::new();
    for line in s.lines() {
        let (left, right) = line
            .split_once(" - ")
            .context("invalid mountinfo separator")?;
        let l: Vec<_> = left.split_whitespace().collect();
        let r: Vec<_> = right.split_whitespace().collect();
        ensure!(l.len() >= 5 && r.len() >= 2, "short mountinfo row");
        ensure!(mounts.len() < MAX_RESOURCES, "mountinfo limit exceeded");
        mounts.push(Mount {
            dev: label(l[2]),
            point: label(&unescape(l[4])),
            fstype: label(r[0]),
            source: label(&unescape(r[1])),
        });
    }
    Ok(mounts)
}
#[derive(Clone, Debug)]
struct Snapshot {
    backend: &'static str,
    resource: String,
    filesystem: String,
    mount_point: String,
    generation: String,
    values: Counters,
    components: Components,
}
fn super_deltas(
    previous: &Snapshot,
    current: &Snapshot,
) -> Result<Option<BTreeMap<&'static str, u128>>> {
    if previous.generation != current.generation
        || previous.filesystem != current.filesystem
        || previous.mount_point != current.mount_point
    {
        return Ok(None);
    }
    let mut result = BTreeMap::new();
    for (name, values) in &current.components {
        if let Some(old) = previous.components.get(name) {
            for (key, value) in values {
                ensure!(
                    old.get(key).is_none_or(|n| value >= n),
                    "{name}/{key} decreased"
                );
            }
        }
    }
    for (&key, &value) in &current.values {
        if ["in_flight", "osc_targets", "mdc_targets"].contains(&key) {
            continue;
        }
        if let Some(old) = previous.values.get(key) {
            result.insert(
                key,
                value
                    .checked_sub(*old)
                    .with_context(|| format!("{key} decreased"))?,
            );
        }
    }
    Ok(Some(result))
}
fn diskstats(s: &str) -> Result<Vec<(String, String, Counters)>> {
    let mut result = Vec::new();
    for line in s.lines() {
        ensure!(result.len() < MAX_DISKS, "diskstats row limit exceeded");
        let f: Vec<_> = line.split_whitespace().collect();
        ensure!(f.len() >= 14, "short diskstats row");
        let n = |i: usize| -> Result<u128> { Ok(f[i].parse()?) };
        let mut v = BTreeMap::new();
        for (k, i) in [
            ("read_ops", 3),
            ("write_ops", 7),
            ("read_latency_ns", 6),
            ("write_latency_ns", 10),
            ("in_flight", 11),
            ("busy_ns", 12),
            ("weighted_io_ns", 13),
        ] {
            v.insert(
                k,
                if k.ends_with("_ns") {
                    n(i)?.checked_mul(1_000_000).context("disk time overflow")?
                } else {
                    n(i)?
                },
            );
        }
        // Linux diskstats sector is always 512 bytes, independent of logical block size.
        v.insert(
            "read_bytes",
            n(5)?.checked_mul(512).context("sector overflow")?,
        );
        v.insert(
            "write_bytes",
            n(9)?.checked_mul(512).context("sector overflow")?,
        );
        result.push((format!("{}:{}", f[0], f[1]), label(f[2]), v));
    }
    Ok(result)
}
fn excluded(name: &str) -> bool {
    ["loop", "ram", "zram", "dm-", "md", "sr", "fd"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}
// Resolve a mounted device to its actual leaf backing devices; never attribute
// a partition or device-mapper layer a second time alongside its backing disk.
fn backing(
    root: &Path,
    dev: &str,
    depth: usize,
    seen: &mut BTreeSet<String>,
    out: &mut BTreeSet<String>,
) -> Result<()> {
    ensure!(
        depth < 16 && seen.len() < MAX_RESOURCES,
        "block device graph limit exceeded"
    );
    if !seen.insert(dev.to_owned()) {
        return Ok(());
    }
    let path = root.join("dev/block").join(dev);
    let real = fs::canonicalize(&path)?;
    let slaves = real.join("slaves");
    if slaves.exists() {
        for entry in fs::read_dir(slaves)? {
            let entry = entry?;
            let slave = read(&entry.path().join("dev"))?;
            backing(root, slave.trim(), depth + 1, seen, out)?;
        }
    }
    if real.join("partition").exists() {
        let parent = read(
            &real
                .parent()
                .context("partition without parent")?
                .join("dev"),
        )?;
        backing(root, parent.trim(), depth + 1, seen, out)?;
    } else if real
        .join("slaves")
        .read_dir()
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
        && !real.to_string_lossy().contains("/virtual/block/")
        && !excluded(real.file_name().and_then(|n| n.to_str()).unwrap_or(""))
    {
        out.insert(dev.to_owned());
    }
    Ok(())
}
fn block(proc: &Path, sys: &Path, mounts: &[Mount]) -> Result<Vec<Snapshot>> {
    let mut labels: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for m in mounts {
        let mut leaves = BTreeSet::new();
        // Some pseudo filesystems cannot be resolved through /sys/dev/block.
        if backing(sys, &m.dev, 0, &mut BTreeSet::new(), &mut leaves).is_ok() {
            for d in leaves {
                labels
                    .entry(d)
                    .or_default()
                    .insert((format!("{} {}", m.fstype, m.source), m.point.clone()));
            }
        }
    }
    let mut result = Vec::new();
    for (dev, name, values) in diskstats(&read(&proc.join("diskstats"))?)? {
        if excluded(&name) {
            continue;
        }
        let mut leaves = BTreeSet::new();
        backing(sys, &dev, 0, &mut BTreeSet::new(), &mut leaves)
            .with_context(|| format!("resolving disk {dev}:{name}"))?;
        if !leaves.contains(&dev) {
            continue;
        }
        let mapping = labels.get(&dev);
        let names: BTreeSet<_> = mapping
            .into_iter()
            .flatten()
            .map(|(f, _)| f.as_str())
            .collect();
        let filesystem = if names.is_empty() {
            "unmapped block device".into()
        } else if names.len() == 1 {
            label(names.iter().next().unwrap())
        } else {
            label(&format!(
                "shared device: {}",
                names.into_iter().collect::<Vec<_>>().join(", ")
            ))
        };
        let mount_point = mapping
            .map(|m| {
                label(
                    &m.iter()
                        .map(|(_, p)| p.as_str())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            })
            .unwrap_or_else(|| "unknown".into());
        ensure!(
            result.len() < MAX_RESOURCES,
            "block resource limit exceeded"
        );
        result.push(Snapshot {
            backend: "block",
            resource: format!("{dev}:{name}"),
            filesystem,
            mount_point,
            generation: format!(
                "{dev}:{}",
                read(&sys.join("dev/block").join(&dev).join("diskseq"))
                    .unwrap_or_default()
                    .trim()
            ),
            values,
            components: BTreeMap::new(),
        });
    }
    Ok(result)
}
#[derive(Default)]
struct NfsEntry {
    point: String,
    source: String,
    fstype: String,
    age: String,
    values: BTreeMap<&'static str, u128>,
}
fn is_nfs(fstype: &str) -> bool {
    matches!(fstype, "nfs" | "nfs4")
}
fn nfs_mountstats(s: &str) -> Result<Vec<NfsEntry>> {
    let mut result = Vec::new();
    let mut current: Option<NfsEntry> = None;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("device ") {
            if let Some(e) = current.take() {
                result.push(e);
            }
            ensure!(result.len() < MAX_RESOURCES, "NFS resource limit exceeded");
            if let Some((source, tail)) = rest.split_once(" mounted on ") {
                if let Some((point, ty)) = tail.split_once(" with fstype ") {
                    if is_nfs(ty.split_whitespace().next().unwrap_or("")) {
                        current = Some(NfsEntry {
                            point: unescape(point),
                            source: unescape(source),
                            fstype: ty.split_whitespace().next().unwrap_or("nfs").into(),
                            ..Default::default()
                        });
                    }
                }
            }
            continue;
        }
        let Some(e) = current.as_mut() else { continue };
        let t = line.trim();
        if let Some(age) = t.strip_prefix("age:") {
            e.age = age.trim().into();
        }
        if let Some(bytes) = t.strip_prefix("bytes:") {
            let b: Vec<_> = bytes.split_whitespace().collect();
            ensure!(b.len() >= 6, "short NFS bytes row");
            e.values.insert("read_bytes", b[4].parse()?);
            e.values.insert("write_bytes", b[5].parse()?);
        }
        if let Some((name, data)) = t.split_once(':') {
            if matches!(name, "READ" | "READ_PLUS" | "WRITE" | "WRITE_SAME") {
                let prefix = if name.starts_with("READ") {
                    "read"
                } else {
                    "write"
                };
                let f: Vec<_> = data.split_whitespace().collect();
                ensure!(f.len() >= 8, "short NFS per-op row");
                for (key, i, mult) in [
                    ("read_ops", 0, 1),
                    ("read_transmissions", 1, 1),
                    ("read_timeouts", 2, 1),
                    ("read_queue_ns", 5, 1_000_000),
                    ("read_rtt_ns", 6, 1_000_000),
                    ("read_latency_ns", 7, 1_000_000),
                    ("write_ops", 0, 1),
                    ("write_transmissions", 1, 1),
                    ("write_timeouts", 2, 1),
                    ("write_queue_ns", 5, 1_000_000),
                    ("write_rtt_ns", 6, 1_000_000),
                    ("write_latency_ns", 7, 1_000_000),
                ] {
                    if key.starts_with(prefix) {
                        let value = f[i]
                            .parse::<u128>()?
                            .checked_mul(mult)
                            .context("NFS time overflow")?;
                        let total = e.values.entry(key).or_default();
                        *total = total.checked_add(value).context("NFS per-op overflow")?;
                    }
                }
                if f.len() > 8 {
                    let key = if prefix == "read" {
                        "read_errors"
                    } else {
                        "write_errors"
                    };
                    let total = e.values.entry(key).or_default();
                    *total = total
                        .checked_add(f[8].parse()?)
                        .context("NFS error count overflow")?;
                }
            }
        }
    }
    if let Some(e) = current {
        result.push(e);
    }
    Ok(result)
}
fn nfs(proc: &Path, mounts: &[Mount]) -> Result<Vec<Snapshot>> {
    let rows = nfs_mountstats(&read(&proc.join("self/mountstats"))?)?;
    let mut result = BTreeMap::new();
    for e in rows {
        let Some(m) = mounts
            .iter()
            .find(|m| m.point == e.point && is_nfs(&m.fstype) && m.source == e.source)
        else {
            continue;
        };
        let key = format!("{}:{}", m.dev, m.source);
        let points = mounts
            .iter()
            .filter(|x| x.dev == m.dev && x.source == m.source && is_nfs(&x.fstype))
            .map(|x| x.point.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        let mut values = e.values;
        ensure!(
            values.contains_key("read_bytes") && values.contains_key("write_bytes"),
            "NFS server-payload byte counters unavailable for {}",
            e.point
        );
        if let Ok(age) = e.age.parse() {
            values.insert("mount_age_seconds", age);
        }
        result.entry(key.clone()).or_insert_with(|| Snapshot {
            backend: "nfs",
            resource: key,
            filesystem: format!("{} {}", e.fstype, e.source),
            mount_point: label(&points),
            generation: format!("{}:{}:{}", m.dev, m.source, e.fstype),
            values,
            components: BTreeMap::new(),
        });
    }
    Ok(result.into_values().collect())
}
// lprocfs stats: counter samples [unit] min max sum [sumsquare]. Zero
// counters are omitted; header is needed to distinguish this from bad input.
fn lproc_counter(f: &[&str], expected: &[&str]) -> Result<(u128, u128)> {
    ensure!(f.len() >= 6, "short Lustre counter");
    let count = f[1].parse()?;
    // Modern lprocfs uses `N samples [unit] min max sum`; old exports
    // occasionally omit `samples` and brackets.
    let (unit, sum) = if f[2] == "samples" {
        ensure!(f.len() >= 7, "short Lustre samples row");
        (f[3].trim_start_matches('[').trim_end_matches(']'), f[6])
    } else {
        (f[2], f[5])
    };
    ensure!(expected.contains(&unit), "unexpected Lustre counter unit");
    Ok((count, sum.parse()?))
}
fn lustre_stats(s: &str) -> Result<(String, BTreeMap<&'static str, u128>)> {
    let mut values = BTreeMap::new();
    let mut start = None;
    let mut header = false;
    for line in s.lines() {
        let f: Vec<_> = line.split_whitespace().collect();
        if f.is_empty() {
            continue;
        }
        if f[0] == "snapshot_time" {
            header = true;
            continue;
        }
        if f[0] == "start_time" {
            start = Some(f.get(1).context("missing start_time")?.to_string());
            continue;
        }
        if f[0] == "req_waittime" {
            let (count, sum) = lproc_counter(&f, &["usecs", "usec"])?;
            values.insert("rpc_wait_ops", count);
            values.insert(
                "rpc_wait_ns",
                sum.checked_mul(1000).context("Lustre wait time overflow")?,
            );
            continue;
        }
        let key = match f[0] {
            "read_bytes" => Some("read_bytes"),
            "write_bytes" => Some("write_bytes"),
            "ost_read" => Some("read_ops"),
            "ost_write" => Some("write_ops"),
            _ => None,
        };
        if let Some(key) = key {
            let (count, cumulative) = lproc_counter(
                &f,
                if key.ends_with("bytes") {
                    &["bytes"]
                } else {
                    &["usecs", "usec"]
                },
            )?;
            let sum = if key.ends_with("bytes") {
                cumulative
            } else {
                count
            };
            ensure!(
                values.insert(key, sum).is_none(),
                "duplicate Lustre counter"
            );
            if key.ends_with("ops") {
                let latency = if key == "read_ops" {
                    "read_latency_ns"
                } else {
                    "write_latency_ns"
                };
                values.insert(
                    latency,
                    cumulative
                        .checked_mul(1000)
                        .context("Lustre time overflow")?,
                );
            }
        }
    }
    ensure!(header, "missing Lustre stats header");
    for key in [
        "read_bytes",
        "write_bytes",
        "read_ops",
        "write_ops",
        "read_latency_ns",
        "write_latency_ns",
    ] {
        values.entry(key).or_insert(0);
    }
    Ok((start.unwrap_or_default(), values))
}
fn mdc_stats(s: &str) -> Result<(String, u128, u128)> {
    let mut header = false;
    let mut start = String::new();
    let (mut count, mut time) = (0u128, 0u128);
    for line in s.lines() {
        let f: Vec<_> = line.split_whitespace().collect();
        if f.is_empty() {
            continue;
        }
        if f[0] == "snapshot_time" {
            header = true;
        }
        if f[0] == "start_time" {
            start = f.get(1).context("missing MDC start_time")?.to_string();
        }
        // req_waittime is recorded once per completed client RPC. Summing
        // mds_reint and its version-dependent extra counters can double count
        // operations, and some extra counters use [reqs], not timing units.
        if f[0] == "req_waittime" {
            let (samples, sum) = lproc_counter(&f, &["usecs", "usec"])?;
            count = count.checked_add(samples).context("MDC RPC overflow")?;
            time = time
                .checked_add(sum.checked_mul(1000).context("MDC time overflow")?)
                .context("MDC aggregate overflow")?;
        }
    }
    ensure!(header, "missing MDC stats header");
    Ok((start, count, time))
}
fn lustre_fs(name: &str) -> String {
    name.split("-OST")
        .next()
        .unwrap_or(name)
        .split("-MDT")
        .next()
        .unwrap_or(name)
        .into()
}
fn lustre(proc: &Path, sys: &Path, mounts: &[Mount]) -> Result<Vec<Snapshot>> {
    let mut groups: BTreeMap<String, LustreGroup> = BTreeMap::new();
    for kind in ["osc", "mdc"] {
        // Choose one representation per type: procfs, debugfs, or sysfs.
        let candidates = [
            proc.join("fs/lustre").join(kind),
            sys.join("fs/lustre").join(kind),
            sys.join("kernel/debug/lustre").join(kind),
        ];
        let mut base = None;
        for candidate in candidates {
            if !candidate.try_exists()? {
                continue;
            }
            // Empty compatibility procfs directories can coexist with modern
            // debugfs stats. Choose the first root that actually has stats.
            for (i, entry) in fs::read_dir(&candidate)?.enumerate() {
                ensure!(i < MAX_RESOURCES, "Lustre target limit exceeded");
                if entry?.path().join("stats").try_exists()? {
                    base = Some(candidate.clone());
                    break;
                }
            }
            if base.is_some() {
                break;
            }
        }
        let Some(base) = base else { continue };
        let mut count = 0;
        for entry in fs::read_dir(base)? {
            let entry = entry?;
            count += 1;
            ensure!(count <= MAX_RESOURCES, "Lustre target limit exceeded");
            let name = label(&entry.file_name().to_string_lossy());
            let stats = read(&entry.path().join("stats"))?;
            let group = groups.entry(lustre_fs(&name)).or_default();
            let start = if kind == "mdc" {
                let (start, count, ns) = mdc_stats(&stats)?;
                let values =
                    BTreeMap::from([("metadata_rpc_ops", count), ("metadata_wait_ns", ns)]);
                group.3.insert(format!("mdc/{name}"), values.clone());
                for (key, value) in values {
                    *group.0.entry(key).or_default() = group
                        .0
                        .get(key)
                        .copied()
                        .unwrap_or(0)
                        .checked_add(value)
                        .context("MDC aggregate overflow")?;
                }
                start
            } else {
                let (start, values) = lustre_stats(&stats)?;
                group.3.insert(format!("osc/{name}"), values.clone());
                for (key, value) in values {
                    *group.0.entry(key).or_default() = group
                        .0
                        .get(key)
                        .copied()
                        .unwrap_or(0)
                        .checked_add(value)
                        .context("Lustre aggregate overflow")?;
                }
                group.2 += 1;
                start
            };
            group.1.push(format!("{name}@{start}"));
        }
    }
    let mut result = Vec::new();
    for (fsname, (mut values, mut targets, count, components)) in groups {
        targets.sort();
        values.insert("osc_targets", count as u128);
        values.insert("mdc_targets", (targets.len() - count) as u128);
        let matches: Vec<_> = mounts
            .iter()
            .filter(|m| {
                m.fstype == "lustre"
                    && (m.source.ends_with(&format!("/{fsname}"))
                        || m.source.ends_with(&format!(":{fsname}")))
            })
            .collect();
        let filesystem = if matches.is_empty() {
            format!("lustre {fsname} (unmapped client)")
        } else {
            format!("lustre {fsname}")
        };
        let point = if matches.is_empty() {
            "unknown".into()
        } else {
            label(
                &matches
                    .iter()
                    .map(|m| m.point.as_str())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        };
        result.push(Snapshot {
            backend: "lustre",
            resource: fsname,
            filesystem,
            mount_point: point,
            generation: targets.join("|"),
            values,
            components,
        });
    }
    Ok(result)
}

pub(crate) struct Running {
    proc: PathBuf,
    sys: PathBuf,
    host_id: String,
    interval_ns: u64,
    last: Option<u64>,
    index: u64,
    previous: BTreeMap<String, (u64, Snapshot)>,
    tracks: BTreeMap<String, u64>,
    errors: u64,
    last_error: String,
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
        let namespace = fs::read_link("/proc/self/ns/mnt")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "unknown-mnt-namespace".into());
        let Some(lease) = crate::system_collector::lease(
            &std::env::temp_dir(),
            session,
            &format!("{host_id}/{namespace}"),
            "storage",
        )?
        else {
            let mut event = Event::new(
                now,
                Payload::Counter(CounterSample {
                    track_id: 0,
                    timestamp_ns: now,
                    name: "storage.meta.delegated".into(),
                    value: CounterValue::Integer(1),
                    unit: "status".into(),
                }),
            );
            event.attributes.insert(
                "storage.detail".into(),
                Attribute::Text(
                    "another concurrent rank in this mount namespace owns storage sampling".into(),
                ),
            );
            out.reliable(event)?;
            return Ok(None);
        };
        let mut running = Self {
            proc: "/proc".into(),
            sys: "/sys".into(),
            host_id,
            interval_ns: interval_ms.saturating_mul(1_000_000),
            last: None,
            index: 0,
            previous: BTreeMap::new(),
            tracks: BTreeMap::new(),
            errors: 0,
            last_error: String::new(),
            _lease: lease,
        };
        running.sample(out, now, true)?;
        Ok(Some(running))
    }
    fn error(&mut self, source: &str, error: impl std::fmt::Display) {
        self.errors = self.errors.saturating_add(1);
        self.last_error = format!("{source}: {error}").chars().take(512).collect();
    }
    fn event(
        &self,
        now: u64,
        track: u64,
        name: &str,
        value: CounterValue,
        unit: &str,
        source: (&str, u64),
    ) -> Event {
        let (backend, window) = source;
        let mut e = Event::new(
            now,
            Payload::Counter(CounterSample {
                track_id: track,
                timestamp_ns: now,
                name: name.into(),
                value,
                unit: unit.into(),
            }),
        );
        for (k, v) in [
            ("storage.backend", backend),
            (
                "storage.scope",
                match backend {
                    "block" => "host block-device counters; all workloads; not physical media",
                    "nfs" => "NFS client server-payload/RPC counters; not server disk",
                    "lustre" => "Lustre client bulk-transfer/RPC counters; not server disk",
                    _ => "storage-monitoring diagnostics",
                },
            ),
        ] {
            e.attributes.insert(k.into(), Attribute::Text(v.into()));
        }
        e.attributes.insert(
            "storage.interval_ns".into(),
            Attribute::Unsigned(self.interval_ns),
        );
        e.attributes
            .insert("storage.window_ns".into(), Attribute::Unsigned(window));
        e.attributes.insert(
            "storage.sample_index".into(),
            Attribute::Unsigned(self.index),
        );
        e
    }
    fn status(
        &self,
        out: &mut CaptureSink,
        now: u64,
        backend: &str,
        supported: bool,
        detail: &str,
    ) -> Result<()> {
        let mut e = self.event(
            now,
            0,
            &format!("storage.meta.status.{backend}"),
            CounterValue::Integer(u64::from(supported)),
            "status",
            (backend, 0),
        );
        e.attributes
            .insert("storage.detail".into(), Attribute::Text(label(detail)));
        out.reliable(e)
    }
    pub fn sample(&mut self, out: &mut CaptureSink, now: u64, force: bool) -> Result<()> {
        if !force
            && self
                .last
                .is_some_and(|last| now.saturating_sub(last) < self.interval_ns)
        {
            return Ok(());
        }
        self.last = Some(now);
        self.index = self.index.saturating_add(1);
        let mounts = read(&self.proc.join("self/mountinfo")).and_then(|s| mountinfo(&s));
        let has_nfs = mounts
            .as_ref()
            .is_ok_and(|m| m.iter().any(|x| is_nfs(&x.fstype)));
        let has_lustre = mounts
            .as_ref()
            .is_ok_and(|m| m.iter().any(|x| x.fstype == "lustre"));
        for backend in ["block", "nfs", "lustre"] {
            let current = match (&mounts, backend) {
                (Ok(m), "block") => block(&self.proc, &self.sys, m),
                (Ok(m), "nfs") if has_nfs => nfs(&self.proc, m),
                (Ok(m), "lustre")
                    if has_lustre
                        || self.proc.join("fs/lustre/osc").exists()
                        || self.sys.join("fs/lustre/osc").exists() =>
                {
                    lustre(&self.proc, &self.sys, m)
                }
                (Ok(_), _) => Ok(Vec::new()),
                (Err(e), _) => Err(anyhow::anyhow!("mountinfo: {e}")),
            };
            match current {
                Ok(snapshots) => {
                    let applicable = if backend == "nfs" {
                        has_nfs
                    } else if backend == "lustre" {
                        has_lustre || !snapshots.is_empty()
                    } else {
                        true
                    };
                    self.status(
                        out,
                        now,
                        backend,
                        !snapshots.is_empty() || !applicable,
                        if !applicable {
                            "not applicable: no matching mount"
                        } else if snapshots.is_empty() {
                            "no usable resources; source counters unavailable"
                        } else {
                            "sampling (first observation establishes baseline)"
                        },
                    )?;
                    let mut keys = BTreeSet::new();
                    for snapshot in snapshots {
                        let key = format!("{}:{}", backend, snapshot.resource);
                        if !keys.insert(key.clone()) {
                            self.error(backend, "duplicate resource");
                            continue;
                        }
                        let old = self.previous.get(&key).cloned();
                        let mut deltas = BTreeMap::new();
                        if let Some((_, p)) = &old {
                            match super_deltas(p, &snapshot) {
                                Ok(Some(values)) => deltas = values,
                                Ok(None) => {
                                    self.error(&key, "topology/remount changed; rebaselining")
                                }
                                Err(e) => self.error(&key, format!("{e}; rebaselining")),
                            }
                        }
                        if backend == "nfs" {
                            for direction in ["read", "write"] {
                                let transmissions = format!("{direction}_transmissions");
                                let operations = format!("{direction}_ops");
                                if let (Some(t), Some(o)) = (
                                    deltas.remove(transmissions.as_str()),
                                    deltas.get(operations.as_str()),
                                ) {
                                    if let Some(retrans) = t.checked_sub(*o) {
                                        deltas.insert(
                                            if direction == "read" {
                                                "read_retransmissions"
                                            } else {
                                                "write_retransmissions"
                                            },
                                            retrans,
                                        );
                                    } else {
                                        self.error(
                                            &key,
                                            "NFS transmissions below operations; rebaselining",
                                        );
                                        deltas.clear();
                                    }
                                }
                            }
                            // Age is a generation/reset check, not an I/O metric.
                            deltas.remove("mount_age_seconds");
                        }
                        let window = old
                            .as_ref()
                            .map(|(at, _)| now.saturating_sub(*at))
                            .unwrap_or(0);
                        if window > 0 && !deltas.is_empty() {
                            // A counter reset/reconnect is a gap on the same
                            // resource, not a second independently summable device.
                            let mapping =
                                format!("{}\0{}", snapshot.filesystem, snapshot.mount_point);
                            let track_key =
                                format!("{key}:{:x}", Sha256::digest(mapping.as_bytes()));
                            if !self.tracks.contains_key(&track_key) {
                                if self.tracks.len() >= MAX_RESOURCES {
                                    self.error(&key, "track limit reached");
                                    continue;
                                }
                                let track = out.track_with_parent(
                                    now,
                                    None,
                                    format!("{} · {}", snapshot.backend, snapshot.resource),
                                    TrackKind::Storage {
                                        backend: backend.into(),
                                        resource: snapshot.resource.clone(),
                                        host_id: self.host_id.clone(),
                                        filesystem: snapshot.filesystem.clone(),
                                        mount_point: snapshot.mount_point.clone(),
                                    },
                                    None,
                                )?;
                                self.tracks.insert(track_key.clone(), track);
                            }
                            let track = self.tracks[&track_key];
                            for direction in ["read", "write"] {
                                for (field, suffix, unit) in [
                                    ("bytes", "bytes_per_second", "B/s"),
                                    ("ops", "operations_per_second", "ops/s"),
                                ] {
                                    let name = format!("{direction}_{field}");
                                    if let Some(value) = deltas.get(name.as_str()) {
                                        let mut e = self.event(
                                            now,
                                            track,
                                            &format!("storage.{direction}.{suffix}"),
                                            CounterValue::Float(
                                                *value as f64 * 1e9 / window as f64,
                                            ),
                                            unit,
                                            (backend, window),
                                        );
                                        for (&k, &v) in &deltas {
                                            e.attributes.insert(
                                                format!("storage.{k}"),
                                                Attribute::Text(v.to_string()),
                                            );
                                        }
                                        if let Some(gauge) = snapshot.values.get("in_flight") {
                                            e.attributes.insert(
                                                "storage.in_flight".into(),
                                                Attribute::Text(gauge.to_string()),
                                            );
                                        }
                                        if let Some(targets) = snapshot.values.get("osc_targets") {
                                            e.attributes.insert(
                                                "storage.osc_targets".into(),
                                                Attribute::Text(targets.to_string()),
                                            );
                                        }
                                        if let Some(targets) = snapshot.values.get("mdc_targets") {
                                            e.attributes.insert(
                                                "storage.mdc_targets".into(),
                                                Attribute::Text(targets.to_string()),
                                            );
                                        }
                                        e.attributes.insert("storage.definition".into(),Attribute::Text(match (backend,field) { ("block","bytes") => "delta Linux diskstats sectors * 512 / elapsed seconds; host device, includes all workloads",("nfs","bytes") => "delta NFS mountstats serverread/serverwrite payload bytes / elapsed seconds",("lustre","bytes") => "delta OSC read_bytes/write_bytes bulk-transfer SUM / elapsed seconds",_ => "delta completed device operations or client RPC calls / elapsed seconds" }.into()));
                                        out.emit(e)?;
                                    }
                                }
                                let ops = format!("{direction}_ops");
                                let latency = format!("{direction}_latency_ns");
                                if let (Some(&ops), Some(&sum)) =
                                    (deltas.get(ops.as_str()), deltas.get(latency.as_str()))
                                {
                                    if ops > 0 {
                                        let mut e = self.event(
                                            now,
                                            track,
                                            &format!("storage.{direction}.latency_ms"),
                                            CounterValue::Float(sum as f64 / ops as f64 / 1e6),
                                            "ms",
                                            (backend, window),
                                        );
                                        for (&k, &v) in &deltas {
                                            e.attributes.insert(
                                                format!("storage.{k}"),
                                                Attribute::Text(v.to_string()),
                                            );
                                        }
                                        e.attributes.insert("storage.definition".into(),Attribute::Text(match backend {"block"=>"delta diskstats cumulative request milliseconds / completed requests", "nfs"=>"delta NFS RPC execute milliseconds / completed RPCs",_=>"delta OSC ost_read/ost_write request usecs SUM / RPC calls"}.into()));
                                        out.emit(e)?;
                                    }
                                }
                            }
                        }
                        self.previous.insert(key, (now, snapshot));
                    }
                    self.previous.retain(|key, _| {
                        !key.starts_with(&format!("{backend}:")) || keys.contains(key)
                    });
                }
                Err(e) => {
                    self.previous
                        .retain(|key, _| !key.starts_with(&format!("{backend}:")));
                    self.error(backend, &e);
                    self.status(out, now, backend, false, &format!("unavailable: {e}"))?;
                }
            }
        }
        let mut e = self.event(
            now,
            0,
            "storage.meta.read_errors",
            CounterValue::Integer(self.errors),
            "count",
            ("storage", 0),
        );
        e.attributes.insert(
            "storage.last_error".into(),
            Attribute::Text(self.last_error.clone()),
        );
        out.reliable(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sectors_are_always_512_and_partitions_are_not_second_tracks() {
        let rows =
            diskstats("8 0 sda 10 0 3 12 20 0 5 18 2 30 40 0 0 0\n8 1 sda1 1 0 1 2 1 0 2 3 0 1 1")
                .unwrap();
        assert_eq!(rows[0].2["read_bytes"], 1536);
        assert_eq!(rows[0].2["write_bytes"], 2560);
        assert_eq!(rows[0].2["read_latency_ns"], 12_000_000);
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("devices/pci/block/sda");
        fs::create_dir_all(base.join("sda1")).unwrap();
        fs::create_dir_all(tmp.path().join("dev/block")).unwrap();
        fs::write(base.join("dev"), "8:0\n").unwrap();
        fs::write(base.join("sda1/dev"), "8:1\n").unwrap();
        fs::write(base.join("sda1/partition"), "1\n").unwrap();
        std::os::unix::fs::symlink(&base, tmp.path().join("dev/block/8:0")).unwrap();
        std::os::unix::fs::symlink(base.join("sda1"), tmp.path().join("dev/block/8:1")).unwrap();
        let mut leaves = BTreeSet::new();
        backing(tmp.path(), "8:1", 0, &mut BTreeSet::new(), &mut leaves).unwrap();
        assert_eq!(leaves, BTreeSet::from(["8:0".into()]));
        fs::write(
            tmp.path().join("diskstats"),
            "8 0 sda 10 0 3 12 20 0 5 18 2 30 40\n8 1 sda1 1 0 1 2 1 0 2 3 0 1 1",
        )
        .unwrap();
        let mounts = mountinfo("1 0 8:1 / /data\\040one rw - ext4 /dev/sda1 rw").unwrap();
        let snapshots = block(tmp.path(), tmp.path(), &mounts).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].filesystem, "ext4 /dev/sda1");
        assert_eq!(snapshots[0].mount_point, "/data one");
    }

    #[test]
    fn nfs_uses_server_payload_and_per_op_execute_time() {
        let mounts =
            mountinfo("1 0 0:57 / /escaped\\040name rw - nfs4 server:/export\\040space rw")
                .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("self")).unwrap();
        fs::write(tmp.path().join("self/mountstats"),"device server:/export space mounted on /escaped name with fstype nfs4 statvers=1.1\nage:\t23\n bytes:\t10000 20000 1 2 123 456 0 0\n per-op statistics\n READ: 10 13 1 3 4 20 30 40 2\n READ_PLUS: 2 2 0 3 4 1 2 3 0\n WRITE: 4 4 0 3 4 10 12 14\n").unwrap();
        let n = nfs(tmp.path(), &mounts).unwrap();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].values["read_bytes"], 123);
        assert_eq!(n[0].values["write_bytes"], 456);
        assert_eq!(n[0].values["read_latency_ns"], 43_000_000);
        assert_eq!(n[0].values["mount_age_seconds"], 23);
        assert_eq!(n[0].values["read_ops"], 12);
        assert_eq!(
            n[0].values["read_transmissions"] - n[0].values["read_ops"],
            3
        );
        assert_eq!(n[0].values["read_errors"], 2);
        assert!(!n[0].values.contains_key("write_errors"));
        assert_eq!(n[0].mount_point, "/escaped name");
        assert_eq!(n[0].generation, "0:57:server:/export space:nfs4");
    }

    #[test]
    fn lustre_uses_byte_sum_not_samples_or_min_and_omitted_zeros() {
        let (start,c)=lustre_stats("snapshot_time 1.0\nstart_time 2.0\nread_bytes 3 samples [bytes] 10 90 200 999\nost_read 4 samples [usecs] 1 20 50 999\n").unwrap();
        assert_eq!(start, "2.0");
        assert_eq!(c["read_bytes"], 200);
        assert_eq!(c["read_ops"], 4);
        assert_eq!(c["read_latency_ns"], 50_000);
        assert_eq!(c["write_bytes"], 0);
        assert_eq!(c["write_ops"], 0);
        assert!(lustre_stats("read_bytes 3 bytes 10 90 200").is_err());
        assert!(lustre_stats("snapshot_time 1\nread_bytes 3 usecs 10 90 200").is_err());
        let (_,rpc,ns)=mdc_stats("snapshot_time 1\nreq_waittime 5 samples [usecs] 1 4 12\nmds_reint 2 samples [usecs] 1 4 5\nmds_reint_setattr 2 samples [reqs] 1 4 5\nmds_getattr 3 samples [usec] 1 4 7\n").unwrap();
        assert_eq!((rpc, ns), (5, 12_000));
    }

    #[test]
    fn lustre_groups_targets_per_filesystem_and_resets_on_target_change() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("fs/lustre/osc");
        for (target, bytes) in [("alpha-OST0000-osc-a", 100), ("alpha-OST0001-osc-b", 200)] {
            let dir = base.join(target);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("stats"), format!("snapshot_time 1\nstart_time 2\nread_bytes 1 samples [bytes] {bytes} {bytes} {bytes}\nost_read 1 samples [usecs] 5 5 5\n")).unwrap();
        }
        let mounts = mountinfo("1 0 0:34 / /mnt/alpha rw - lustre server@tcp:/alpha rw").unwrap();
        let first = lustre(tmp.path(), tmp.path(), &mounts).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].values["read_bytes"], 300);
        assert_eq!(first[0].values["read_ops"], 2);
        assert_eq!(first[0].values["osc_targets"], 2);
        assert_eq!(first[0].mount_point, "/mnt/alpha");
        // A reset on one target must not be hidden by growth on another.
        for (target, bytes) in [("alpha-OST0000-osc-a", 50), ("alpha-OST0001-osc-b", 500)] {
            fs::write(base.join(target).join("stats"), format!("snapshot_time 2\nstart_time 2\nread_bytes 2 samples [bytes] 1 {bytes} {bytes}\nost_read 2 samples [usecs] 5 5 10\n")).unwrap();
        }
        let masked = lustre(tmp.path(), tmp.path(), &mounts).unwrap();
        assert!(masked[0].values["read_bytes"] > first[0].values["read_bytes"]);
        assert!(super_deltas(&first[0], &masked[0]).is_err());
        fs::remove_dir_all(base.join("alpha-OST0001-osc-b")).unwrap();
        let second = lustre(tmp.path(), tmp.path(), &mounts).unwrap();
        assert!(super_deltas(&first[0], &second[0]).unwrap().is_none());
    }

    #[test]
    fn absent_roots_and_unmounted_remote_are_not_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(lustre(tmp.path(), tmp.path(), &[]).unwrap().is_empty());
        assert_eq!(mountinfo("").unwrap().len(), 0);
        assert!(nfs_mountstats("").unwrap().is_empty());
        assert!(diskstats("bad").is_err());
        assert_eq!(unescape(r"a\777b"), r"a\777b");
        fs::create_dir_all(tmp.path().join("fs/lustre/osc")).unwrap();
        let modern = tmp
            .path()
            .join("kernel/debug/lustre/osc/alpha-OST0000-osc-a");
        fs::create_dir_all(&modern).unwrap();
        fs::write(modern.join("stats"), "snapshot_time 1\nstart_time 0\n").unwrap();
        assert_eq!(
            lustre(tmp.path(), tmp.path(), &[]).unwrap().len(),
            1,
            "empty procfs must not mask debugfs stats"
        );
    }
    #[test]
    fn reset_topology_and_unknown_latency_do_not_become_zero_samples() {
        let mut before = Snapshot {
            backend: "block",
            resource: "8:0:sda".into(),
            filesystem: "ext4".into(),
            mount_point: "/data".into(),
            generation: "8:0".into(),
            values: BTreeMap::from([("read_ops", 10), ("read_latency_ns", 30), ("in_flight", 9)]),
            components: BTreeMap::new(),
        };
        let mut after = before.clone();
        after.values.insert("read_ops", 10);
        after.values.insert("read_latency_ns", 30);
        after.values.insert("in_flight", 0);
        let d = super_deltas(&before, &after).unwrap().unwrap();
        assert_eq!(d["read_ops"], 0); // zero operations: no latency division.
        assert!(!d.contains_key("in_flight")); // gauge may decrease without reset.
        after.values.remove("read_latency_ns");
        assert!(
            !super_deltas(&before, &after)
                .unwrap()
                .unwrap()
                .contains_key("read_latency_ns")
        );
        after.generation = "8:1".into();
        assert!(super_deltas(&before, &after).unwrap().is_none());
        before.values.insert("read_ops", 11);
        after.generation = "8:0".into();
        assert!(super_deltas(&before, &after).is_err());
    }
}
