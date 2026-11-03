//! Persistent, disposable SQLite sidecar for read-only windowed viewing of v2
//! JSONL captures (and the existing bounded legacy conversion). Capture files
//! remain authoritative and are never modified.
use crate::{
    Dependency, IndexedViewer, IoTimeBin, Report, ReportCounter, ReportSpan, ReportTrack,
    index_metrics::{self as metrics, Gauge, Io, integer, time},
};
use anyhow::{Context, Result, ensure};
use rprof_core::{
    Category, CounterValue,
    stream::{Attribute, Payload, Record},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const VERSION: u32 = 3;
const MAX_REPLY: usize = 16 * 1024 * 1024;
const MAX_RAW: usize = 20_000;
const MAX_METADATA: usize = 64 * 1024 * 1024;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Stamp {
    version: u32,
    length: u64,
    modified: u128,
    device: u64,
    inode: u64,
    changed: i128,
}
fn stamp(path: &Path) -> Result<Stamp> {
    let m = fs::metadata(path)?;
    ensure!(m.is_file(), "capture is not a regular file");
    let modified = m
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    #[cfg(unix)]
    let (device, inode, changed) = {
        use std::os::unix::fs::MetadataExt;
        (
            m.dev(),
            m.ino(),
            m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128,
        )
    };
    #[cfg(not(unix))]
    let (device, inode, changed) = (0, 0, modified as i128);
    Ok(Stamp {
        version: VERSION,
        length: m.len(),
        modified,
        device,
        inode,
        changed,
    })
}
fn text<'a>(attrs: &'a BTreeMap<String, Attribute>, key: &str) -> Option<&'a str> {
    if let Some(Attribute::Text(value)) = attrs.get(key) {
        Some(value)
    } else {
        None
    }
}
fn uint(attrs: &BTreeMap<String, Attribute>, key: &str) -> Option<u64> {
    if let Some(Attribute::Unsigned(value)) = attrs.get(key) {
        Some(*value)
    } else {
        None
    }
}
fn attributes(attrs: &BTreeMap<String, Attribute>) -> BTreeMap<String, String> {
    attrs
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                match value {
                    Attribute::Text(v) => v.clone(),
                    Attribute::Unsigned(v) => v.to_string(),
                    Attribute::Signed(v) => v.to_string(),
                    Attribute::Float(v) => v.to_string(),
                    Attribute::Bool(v) => v.to_string(),
                },
            )
        })
        .collect()
}
fn diagnostic(name: &str) -> bool {
    name.contains(".meta.")
        || name.starts_with("pytorch.import.")
        || name == "mpi.clock.uncertainty_ns"
        || [
            ".attempted_events",
            ".received_events",
            ".ingress_dropped_events",
            ".initialized_images",
            ".receiver_dropped",
            ".malformed",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

pub struct CaptureIndex {
    db: Connection,
    metadata: String,
    snapshot: String,
    domains: HashMap<String, u128>,
    filesystems: HashSet<(String, String)>,
    pub path: PathBuf,
    pub reused: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct WindowRequest {
    pub snapshot: String,
    pub domain: String,
    pub start_ns: String,
    pub end_ns: String,
    pub pixels: u32,
    #[serde(default)]
    pub tracks: Vec<String>,
    #[serde(default)]
    pub systems: Vec<String>,
    #[serde(default)]
    pub io_scopes: Vec<String>,
    #[serde(default = "all")]
    pub filesystem: String,
}
fn all() -> String {
    "all".into()
}
#[derive(Serialize)]
pub struct WindowResponse {
    pub snapshot: String,
    pub domain: String,
    pub start_ns: String,
    pub end_ns: String,
    pub spans: Vec<ReportSpan>,
    pub counters: Vec<ReportCounter>,
    pub io: BTreeMap<String, Vec<IoTimeBin>>,
    pub dependencies: Vec<Dependency>,
    pub limited: bool,
    pub warnings: Vec<String>,
}
#[derive(Deserialize)]
pub struct FilesRequest {
    pub snapshot: String,
    pub thread: String,
    pub offset: usize,
    pub limit: usize,
}
#[derive(Serialize)]
pub struct FilesResponse {
    pub snapshot: String,
    pub tracks: Vec<ReportTrack>,
    pub total: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
}

fn read_connection(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA cache_size=-16384;")?;
    Ok(db)
}
impl CaptureIndex {
    pub fn open(input: &Path, directory: &Path, rebuild: bool) -> Result<Self> {
        let input = fs::canonicalize(input)?;
        let source = stamp(&input)?;
        fs::create_dir_all(directory).context("creating viewer index cache")?;
        let identity = format!("{:x}", Sha256::digest(input.as_os_str().as_encoded_bytes()));
        let path = directory.join(format!("{identity}.sqlite"));
        if !rebuild && let Ok(db) = read_connection(&path) {
            let saved = db.query_row("SELECT value FROM metadata WHERE key='source'", [], |r| {
                r.get::<_, String>(0)
            });
            if saved
                .ok()
                .and_then(|v| serde_json::from_str::<Stamp>(&v).ok())
                .as_ref()
                == Some(&source)
                && let Ok(metadata) = db.query_row(
                    "SELECT value FROM metadata WHERE key='report' AND length(value)<=?1",
                    [MAX_METADATA],
                    |r| r.get::<_, String>(0),
                )
                && let Ok(report) = serde_json::from_str::<Report>(&metadata)
                && report.viewer.is_some()
            {
                return Self::loaded(db, metadata, report, path, true);
            }
        }
        eprintln!("Building indexed viewer data for {}…", input.display());
        let temporary = tempfile::NamedTempFile::new_in(directory)?;
        let db = Connection::open(temporary.path())?;
        db.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-16384; PRAGMA temp_store=FILE;
            CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE tracks(id TEXT PRIMARY KEY,n INTEGER UNIQUE NOT NULL,domain TEXT NOT NULL,parent TEXT,kind TEXT NOT NULL,data TEXT NOT NULL);
            CREATE INDEX track_children ON tracks(parent,kind,n);
            CREATE TABLE files(thread TEXT NOT NULL,track TEXT PRIMARY KEY,n INTEGER NOT NULL);
            CREATE INDEX files_thread ON files(thread,n);
            CREATE TABLE events(id INTEGER PRIMARY KEY,track INTEGER NOT NULL,kind INTEGER NOT NULL,start BLOB NOT NULL,end BLOB NOT NULL,gap INTEGER NOT NULL,data TEXT NOT NULL);
            CREATE VIRTUAL TABLE ranges USING rtree(id,track_min,track_max,start_ms,end_ms);
            CREATE TABLE pending(shard TEXT,track TEXT,correlation TEXT,data TEXT,PRIMARY KEY(shard,track,correlation));
            CREATE TABLE dependencies(id INTEGER PRIMARY KEY,a TEXT NOT NULL,b TEXT NOT NULL,relation TEXT NOT NULL);
            CREATE INDEX dependency_from ON dependencies(a);
            CREATE INDEX dependency_to ON dependencies(b);
            CREATE TABLE series(id INTEGER PRIMARY KEY,kind TEXT NOT NULL,domain TEXT NOT NULL,scope TEXT NOT NULL,filter TEXT NOT NULL,base BLOB NOT NULL,end BLOB NOT NULL,UNIQUE(kind,scope,filter));
            CREATE INDEX series_scope ON series(kind,scope);
            CREATE TABLE bins(series INTEGER NOT NULL,level INTEGER NOT NULL,bucket BLOB NOT NULL,data TEXT NOT NULL,PRIMARY KEY(series,level,bucket)) WITHOUT ROWID;
            BEGIN IMMEDIATE;")?;
        metrics::configure(&db)?;
        let mut report = crate::analyze::indexed_metadata(&input, source.length)?;
        let snapshot = format!("{:x}", Sha256::digest(serde_json::to_vec(&source)?));
        build(&db, &input, source.length, &mut report)?;
        metrics::pyramid(&db)?;
        ensure!(
            stamp(&input)? == source,
            "capture changed while indexing; retry when recording has stopped"
        );
        report.viewer = Some(IndexedViewer {
            endpoint: "/api/window".into(),
            files_endpoint: "/api/files".into(),
            snapshot: snapshot.clone(),
        });
        report.io.clear(); // Per-file aggregate browsing is a separate workflow; file lanes are paged below threads.
        report
            .tracks
            .retain(|t| !t.details.contains_key("viewer.paged_file"));
        let metadata = serde_json::to_string(&report)?;
        ensure!(
            metadata.len() <= MAX_METADATA,
            "indexed viewer metadata exceeds 64 MiB"
        );
        db.execute(
            "INSERT INTO metadata VALUES('source',?1)",
            [serde_json::to_string(&source)?],
        )?;
        db.execute("INSERT INTO metadata VALUES('report',?1)", [&metadata])?;
        db.execute_batch("COMMIT; PRAGMA optimize;")?;
        drop(db);
        temporary.as_file().sync_all()?;
        temporary
            .persist(&path)
            .map_err(|error| error.error)
            .context("publishing viewer index")?;
        let db = read_connection(&path)?;
        Self::loaded(db, metadata, report, path, false)
    }
    fn loaded(
        db: Connection,
        metadata: String,
        report: Report,
        path: PathBuf,
        reused: bool,
    ) -> Result<Self> {
        let domains = report
            .domains
            .iter()
            .map(|d| Ok((d.id.clone(), d.end_ns.parse()?)))
            .collect::<Result<_>>()?;
        let filesystems = report
            .io_timeline
            .as_ref()
            .map(|io| {
                io.filesystems
                    .iter()
                    .map(|f| (f.domain.clone(), f.id.clone()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            snapshot: report.viewer.context("missing index identity")?.snapshot,
            db,
            metadata,
            domains,
            filesystems,
            path,
            reused,
        })
    }
    pub fn report(&self) -> Result<Report> {
        Ok(serde_json::from_str(&self.metadata)?)
    }
    pub fn files(&self, query: FilesRequest) -> Result<FilesResponse> {
        ensure!(
            query.snapshot == self.snapshot,
            "viewer snapshot changed; reload the page"
        );
        ensure!(
            query.limit > 0
                && query.limit <= 100
                && query.offset <= 65_536
                && query.thread.len() <= 128,
            "invalid file page"
        );
        let kind: Option<String> = self
            .db
            .query_row(
                "SELECT kind FROM tracks WHERE id=?1",
                [&query.thread],
                |r| r.get(0),
            )
            .optional()?;
        ensure!(kind.as_deref() == Some("thread"), "unknown thread");
        let total: usize = self.db.query_row(
            "SELECT count(*) FROM files WHERE thread=?1",
            [&query.thread],
            |r| r.get(0),
        )?;
        let mut statement = self.db.prepare(
            "SELECT t.data FROM files f JOIN tracks t ON t.id=f.track WHERE f.thread=?1 ORDER BY f.n LIMIT ?2 OFFSET ?3",
        )?;
        let tracks = statement
            .query_map(params![query.thread, query.limit, query.offset], |r| {
                r.get::<_, String>(0)
            })?
            .map(|r| -> Result<_> { Ok(serde_json::from_str(&r?)?) })
            .collect::<Result<Vec<_>>>()?;
        let end = query.offset + tracks.len();
        Ok(FilesResponse {
            snapshot: self.snapshot.clone(),
            tracks,
            total,
            offset: query.offset,
            next_offset: (end < total).then_some(end),
        })
    }
    pub fn window(
        &self,
        query: WindowRequest,
        event_limit: usize,
        byte_limit: usize,
    ) -> Result<WindowResponse> {
        ensure!(
            query.snapshot == self.snapshot,
            "viewer snapshot changed; reload the page"
        );
        ensure!(
            query.tracks.len() <= 64
                && query.systems.len() <= 1024
                && query.io_scopes.len() <= 32
                && (1..=4096).contains(&query.pixels),
            "window request exceeds limits"
        );
        ensure!(
            query
                .tracks
                .iter()
                .chain(&query.systems)
                .chain(&query.io_scopes)
                .all(|s| s.len() <= 256)
                && query.filesystem.len() <= 128,
            "invalid scope identifier"
        );
        let start = query.start_ns.parse::<u128>().context("invalid start_ns")?;
        let end = query.end_ns.parse::<u128>().context("invalid end_ns")?;
        let extent = *self
            .domains
            .get(&query.domain)
            .context("unknown clock domain")?;
        ensure!(
            start < end && end <= extent,
            "window lies outside clock domain"
        );
        ensure!(
            query.filesystem == "all"
                || self
                    .filesystems
                    .contains(&(query.domain.clone(), query.filesystem.clone())),
            "unknown filesystem"
        );
        let mut response = WindowResponse {
            snapshot: self.snapshot.clone(),
            domain: query.domain.clone(),
            start_ns: start.to_string(),
            end_ns: end.to_string(),
            spans: vec![],
            counters: vec![],
            dependencies: vec![],
            io: BTreeMap::new(),
            limited: false,
            warnings: vec![],
        };
        let budget = byte_limit
            .min(MAX_REPLY)
            .saturating_sub(64 * 1024)
            .max(1024);
        let mut used = 512usize;
        let target = (query.pixels as usize * 2)
            .min(4096)
            .min(8192 / query.io_scopes.len().max(1))
            .max(1);
        for scope in query.io_scopes.iter().collect::<BTreeSet<_>>() {
            let series = self.db.query_row("SELECT id,base FROM series WHERE kind='io' AND scope=?1 AND filter=?2 AND domain=?3", params![scope, query.filesystem, query.domain], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))).optional()?;
            let Some((series, base)) = series else {
                response.io.insert(scope.clone(), vec![]);
                continue;
            };
            let (level, width) = resolution(integer(base)?, end - start, target);
            let mut statement = self.db.prepare("SELECT bucket,data FROM bins WHERE series=?1 AND level=?2 AND bucket>=?3 AND bucket<=?4 ORDER BY bucket")?;
            let mut rows = statement.query(params![
                series,
                level,
                time(start / width),
                time((end - 1) / width)
            ])?;
            let mut values = BTreeMap::new();
            while let Some(row) = rows.next()? {
                values.insert(
                    integer(row.get(0)?)?,
                    serde_json::from_str::<Io>(&row.get::<_, String>(1)?)?,
                );
            }
            let mut bins = Vec::new();
            for bucket in start / width..=(end - 1) / width {
                let at = bucket * width;
                let bin = values
                    .remove(&bucket)
                    .unwrap_or_default()
                    .bin(at, width.min(extent.saturating_sub(at)).max(1));
                let size = serde_json::to_vec(&bin)?.len();
                if used + size > budget {
                    response.limited = true;
                    break;
                }
                used += size;
                bins.push(bin);
            }
            response.io.insert(scope.clone(), bins);
        }
        let systems: BTreeSet<_> = query.systems.iter().collect();
        let per_system = (query.pixels as usize / 2)
            .min(2048)
            .min(2048 / systems.len().max(1))
            .max(1);
        let mut gauge_series = 0;
        for track in systems {
            if used >= budget || gauge_series >= 4096 {
                response.limited = true;
                break;
            }
            self.validate_track(track, &query.domain)?;
            let mut statement = self.db.prepare(
                "SELECT id,base FROM series WHERE kind='gauge' AND scope=?1 AND domain=?2 ORDER BY id LIMIT 257",
            )?;
            let mut rows = statement.query(params![track, query.domain])?;
            let mut per_track_series = 0;
            while let Some(row) = rows.next()? {
                if per_track_series >= 256 || gauge_series >= 4096 || used >= budget {
                    response.limited = true;
                    break;
                }
                per_track_series += 1;
                gauge_series += 1;
                let series: i64 = row.get(0)?;
                let (level, width) = resolution(integer(row.get(1)?)?, end - start, per_system);
                let mut bins = self.db.prepare("SELECT data FROM bins WHERE series=?1 AND level=?2 AND bucket>=?3 AND bucket<=?4 ORDER BY bucket")?;
                let mut gauges = bins.query(params![
                    series,
                    level,
                    time((start / width).saturating_sub(1)),
                    time(end / width + 1)
                ])?;
                let mut ids = BTreeSet::new();
                while let Some(row) = gauges.next()? {
                    let g: Gauge = serde_json::from_str(&row.get::<_, String>(0)?)?;
                    for point in [g.first, g.last, g.min, g.max] {
                        ids.insert((point.at, point.id));
                    }
                }
                let mut previous = None;
                for (_, id) in ids {
                    let (data, gap): (String, i64) = self.db.query_row(
                        "SELECT data,gap FROM events WHERE id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    let mut sample: ReportCounter = serde_json::from_str(&data)?;
                    sample.attributes.insert(
                        "system.report_gap_before".into(),
                        previous.is_some_and(|g| g != gap).to_string(),
                    );
                    sample
                        .attributes
                        .insert("system.report_interval_ns".into(), width.to_string());
                    sample
                        .attributes
                        .insert("viewer.resolution_ns".into(), width.to_string());
                    previous = Some(gap);
                    let size = serde_json::to_vec(&sample)?.len();
                    if used + size > budget {
                        response.limited = true;
                        break;
                    }
                    used += size;
                    response.counters.push(sample);
                }
            }
        }
        let maximum = event_limit.min(MAX_RAW);
        let mut count = 0usize;
        for track in query.tracks.iter().collect::<BTreeSet<_>>() {
            let n = self.validate_track(track, &query.domain)?;
            let mut statement = self.db.prepare(
                "SELECT e.kind,e.data FROM ranges r JOIN events e ON e.id=r.id
                WHERE r.track_min<=?1 AND r.track_max>=?1 AND r.start_ms<=?2 AND r.end_ms>=?3
                AND e.start<=?4 AND e.end>=?5 AND (e.kind=0 OR ?7=0) LIMIT ?6",
            )?;
            let mut rows = statement.query(params![
                n,
                end as f64 / 1e6,
                start as f64 / 1e6,
                time(end),
                time(start),
                maximum.saturating_sub(count) + 1,
                query.systems.contains(track)
            ])?;
            while let Some(row) = rows.next()? {
                let kind: i64 = row.get(0)?;
                let json: String = row.get(1)?;
                if count >= maximum || used + json.len() > budget {
                    response.limited = true;
                    break;
                }
                count += 1;
                used += json.len();
                if kind == 0 {
                    response.spans.push(serde_json::from_str(&json)?);
                } else {
                    response.counters.push(serde_json::from_str(&json)?);
                }
            }
        }
        let correlations: BTreeSet<_> = response
            .spans
            .iter()
            .filter_map(|s| s.correlation.as_deref())
            .collect();
        let mut seen = HashSet::new();
        for correlation in correlations {
            let mut statement = self.db.prepare_cached(
                "SELECT id,a,b,relation FROM dependencies WHERE a=?1 OR b=?1 LIMIT 5001",
            )?;
            let mut rows = statement.query([correlation])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                if !seen.insert(id) {
                    continue;
                }
                let edge = Dependency {
                    from: row.get(1)?,
                    to: row.get(2)?,
                    relation: row.get(3)?,
                };
                let size = serde_json::to_vec(&edge)?.len();
                if response.dependencies.len() >= 5000 || used + size > budget {
                    response.limited = true;
                    break;
                }
                used += size;
                response.dependencies.push(edge);
            }
            if response.dependencies.len() >= 5000 {
                break;
            }
        }
        if response.limited {
            response.warnings.push("Window response limit reached; zoom in or expand fewer tracks. The indexed capture remains available.".into());
        }
        Ok(response)
    }
    fn validate_track(&self, track: &str, domain: &str) -> Result<i64> {
        self.db
            .query_row(
                "SELECT n FROM tracks WHERE id=?1 AND domain=?2",
                params![track, domain],
                |r| r.get(0),
            )
            .optional()?
            .context("unknown track or wrong clock domain")
    }
}

fn resolution(mut width: u128, duration: u128, buckets: usize) -> (u32, u128) {
    let mut level = 0;
    while duration.div_ceil(width) > buckets as u128 {
        width *= metrics::FANOUT;
        level += 1;
    }
    (level, width)
}

struct Track {
    n: i64,
    parent: Option<String>,
    kind: String,
    rank: Option<u32>,
}
struct Shard {
    host: String,
    boot: String,
    domain: String,
    offset: u128,
    end: u128,
    observed_end: u128,
}
struct Builder<'a> {
    db: &'a Connection,
    tracks: HashMap<String, Track>,
    shards: HashMap<Uuid, Shard>,
    filesystems: HashMap<String, String>,
    series: HashMap<(String, String, String), (i64, u128)>,
    series_bytes: usize,
    next: i64,
    pending: metrics::Pending,
    gaps: HashMap<i64, (u128, Option<u64>, i64)>,
    non_io: HashSet<String>,
    health: BTreeMap<(String, String), ReportCounter>,
}
impl Builder<'_> {
    fn series(
        &mut self,
        kind: &str,
        domain: &str,
        scope: &str,
        filter: &str,
        base: u128,
        end: u128,
    ) -> Result<(i64, u128)> {
        let key = (kind.into(), scope.into(), filter.into());
        if let Some(value) = self.series.get(&key) {
            return Ok(*value);
        }
        let saved = self
            .db
            .prepare_cached("SELECT id,base FROM series WHERE kind=?1 AND scope=?2 AND filter=?3")?
            .query_row(params![kind, scope, filter], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .optional()?;
        let value = if let Some((id, bytes)) = saved {
            (id, integer(bytes)?)
        } else {
            self.db.prepare_cached("INSERT INTO series(kind,domain,scope,filter,base,end) VALUES(?1,?2,?3,?4,?5,?6)")?
                .execute(params![kind,domain,scope,filter,time(base),time(end)])?;
            (self.db.last_insert_rowid(), base)
        };
        let bytes = kind.len() + scope.len() + filter.len() + 64;
        if self.series.len() >= 8192 || self.series_bytes + bytes > 8 * 1024 * 1024 {
            self.series.clear();
            self.series_bytes = 0;
        }
        if bytes <= 8 * 1024 * 1024 {
            self.series_bytes += bytes;
            self.series.insert(key, value);
        }
        Ok(value)
    }
    fn event(
        &mut self,
        track: &str,
        kind: i64,
        start: u128,
        end: u128,
        gap: i64,
        data: &impl Serialize,
    ) -> Result<i64> {
        let id = self.next;
        self.next += 1;
        let n = self
            .tracks
            .get(track)
            .context("indexed event references unknown track")?
            .n;
        self.db
            .prepare_cached("INSERT INTO events VALUES(?1,?2,?3,?4,?5,?6,?7)")?
            .execute(params![
                id,
                n,
                kind,
                time(start),
                time(end),
                gap,
                serde_json::to_string(data)?
            ])?;
        self.db
            .prepare_cached("INSERT INTO ranges VALUES(?1,?2,?2,?3,?4)")?
            .execute(params![id, n, start as f64 / 1e6, end as f64 / 1e6])?;
        Ok(id)
    }
    fn record(&mut self, record: &Record) -> Result<()> {
        if let Record::Shard(shard) = record {
            self.shards
                .get_mut(&shard.shard_id)
                .context("shard metadata missing")?
                .boot = shard.boot_id.clone();
        }
        let Record::Chunk(chunk) = record else {
            return Ok(());
        };
        for (index, event) in chunk.events.iter().enumerate() {
            let shard = &self.shards[&chunk.shard_id];
            let domain = shard.domain.clone();
            let offset = shard.offset;
            let extent = shard.end;
            let observed_end = shard.observed_end;
            match &event.payload {
                Payload::Span(span) => {
                    let track = format!("{}:{}", chunk.shard_id, span.track_id);
                    let start = offset + span.start_ns as u128;
                    let end = start + span.duration_ns as u128;
                    let category = serde_json::to_value(&span.category)?
                        .as_str()
                        .context("invalid span category")?
                        .to_owned();
                    let item = ReportSpan {
                        id: self.next as usize,
                        source_id: format!("span:{}:{}:{index}", chunk.shard_id, chunk.sequence),
                        track: track.clone(),
                        name: span.name.clone(),
                        category: category.clone(),
                        start_ns: start.to_string(),
                        duration_ns: span.duration_ns.to_string(),
                        correlation: span
                            .correlation_id
                            .map(|c| format!("{}:{c}", chunk.shard_id)),
                        incomplete: text(&event.attributes, "mpi.request_state")
                            == Some("tracking_released")
                            || matches!(
                                event.attributes.get("mpi.abort_entry"),
                                Some(Attribute::Bool(true))
                            ),
                        attributes: attributes(&event.attributes),
                    };
                    if category != "io" && category != "process" {
                        self.non_io.insert(track.clone());
                    }
                    if let Some(correlation) = span.correlation_id {
                        self.db.prepare_cached("DELETE FROM pending WHERE shard=?1 AND track=?2 AND correlation=?3")?.execute(params![chunk.shard_id.to_string(),track,correlation.to_string()])?;
                    }
                    let fs_identity = crate::io_timeline::capture_identity(
                        &shard.host,
                        &shard.boot,
                        &event.attributes,
                    );
                    self.event(&track, 0, start, end, 0, &item)?;
                    if matches!(span.category, Category::Io)
                        && let Some(operation) = text(&event.attributes, "io.operation")
                    {
                        let failed = matches!(event.attributes.get("io.result"),Some(Attribute::Signed(n)) if *n < 0);
                        let bytes = if failed {
                            0
                        } else {
                            uint(&event.attributes, "io.completed_bytes").unwrap_or(0) as u128
                        };
                        let mut value = Io {
                            errors: u64::from(failed),
                            max: span.duration_ns,
                            ..Default::default()
                        };
                        match operation {
                            "read" | "pread" => {
                                value.rb = bytes;
                                value.ro = 1;
                                value.rl = span.duration_ns as u128;
                            }
                            "write" | "pwrite" => {
                                value.wb = bytes;
                                value.wo = 1;
                                value.wl = span.duration_ns as u128;
                            }
                            _ => {
                                value.other = 1;
                                value.ol = span.duration_ns as u128;
                            }
                        }
                        let fs = self
                            .filesystems
                            .get(&fs_identity)
                            .cloned()
                            .unwrap_or_else(|| "unlisted".into());
                        let mut scopes = vec![format!("io:{domain}")];
                        let thread = self.tracks[&track]
                            .parent
                            .as_ref()
                            .filter(|p| self.tracks.get(*p).is_some_and(|t| t.kind == "thread"))
                            .cloned()
                            .or_else(|| {
                                (self.tracks[&track].kind == "thread").then(|| track.clone())
                            });
                        if let Some(thread) = thread {
                            if let Some(rank) = self.tracks[&thread].rank {
                                scopes.push(format!(
                                    "io:rank:{domain}:{}",
                                    serde_json::to_string(&(chunk.shard_id.to_string(), rank))?
                                ));
                            }
                            if let (Some(pid), Some(instance)) = (
                                uint(&event.attributes, "pid"),
                                uint(&event.attributes, "process_instance"),
                            ) {
                                scopes.push(format!(
                                    "io:process:{domain}:{}",
                                    serde_json::to_string(&(
                                        chunk.shard_id.to_string(),
                                        pid.to_string(),
                                        instance.to_string()
                                    ))?
                                ));
                            }
                            scopes.push(format!("io:thread:{domain}:{thread}"));
                        }
                        for scope in scopes {
                            for filter in [
                                Some(fs.as_str()),
                                (text(&event.attributes, "io.filesystem.kind")
                                    != Some("non_filesystem"))
                                .then_some("all"),
                            ]
                            .into_iter()
                            .flatten()
                            {
                                let (series, base) =
                                    self.series("io", &domain, &scope, filter, 1_000_000, extent)?;
                                self.pending.io(
                                    series,
                                    end.min(extent.saturating_sub(1)) / base,
                                    value.clone(),
                                );
                            }
                        }
                    }
                }
                Payload::Counter(counter) => {
                    let track = format!("{}:{}", chunk.shard_id, counter.track_id);
                    let at = offset + counter.timestamp_ns as u128;
                    let (value, value_type) = match counter.value {
                        CounterValue::Integer(n) => (n.to_string(), "integer"),
                        CounterValue::Float(n) => (n.to_string(), "float"),
                    };
                    let item = ReportCounter {
                        track: track.clone(),
                        name: counter.name.clone(),
                        timestamp_ns: at.to_string(),
                        value,
                        value_type: value_type.into(),
                        unit: counter.unit.clone(),
                        attributes: attributes(&event.attributes),
                    };
                    if diagnostic(&counter.name) {
                        if self.health.len() < 16_384 && serde_json::to_vec(&item)?.len() <= 2048 {
                            self.health.insert((track, counter.name.clone()), item);
                        }
                        continue;
                    }
                    if self.tracks[&track].kind != "system" {
                        self.non_io.insert(track.clone());
                    }
                    let filter =
                        serde_json::to_string(&(&counter.name, &counter.unit, value_type))?;
                    let interval = uint(&event.attributes, "system.interval_ns")
                        .unwrap_or(1_000_000)
                        .max(1_000_000) as u128;
                    let (series, base) =
                        self.series("gauge", &domain, &track, &filter, interval, extent)?;
                    let sequence = uint(&event.attributes, "system.sample_index");
                    let is_system = self.tracks[&track].kind == "system";
                    let gap_key = self.tracks[&track].n;
                    let gap = if is_system && let Some((last, seq, epoch)) = self.gaps.get(&gap_key)
                    {
                        epoch
                            + i64::from(
                                at < *last
                                    || at.saturating_sub(*last) > interval * 2
                                    || seq
                                        .zip(sequence)
                                        .is_some_and(|(a, b)| b != a.saturating_add(1)),
                            )
                    } else {
                        0
                    };
                    if is_system {
                        self.gaps.insert(gap_key, (at, sequence, gap));
                    }
                    let id = self.event(&track, 1, at, at, gap, &item)?;
                    self.pending
                        .gauge(series, at / base, Gauge::new(id, at, &item)?);
                }
                Payload::ActivityStart {
                    track_id,
                    name,
                    correlation_id,
                    start_ns,
                } => {
                    let track = format!("{}:{track_id}", chunk.shard_id);
                    let start = offset + start_ns.unwrap_or(event.timestamp_ns) as u128;
                    let category = ["openmp", "mpi", "cuda", "python", "pytorch"]
                        .into_iter()
                        .find(|c| event.attributes.contains_key(&format!("{c}.role")))
                        .unwrap_or("activity");
                    let item = ReportSpan {
                        id: 0,
                        source_id: format!("open:{}:{}:{index}", chunk.shard_id, chunk.sequence),
                        track: track.clone(),
                        name: name.clone(),
                        category: category.into(),
                        start_ns: start.to_string(),
                        duration_ns: observed_end.saturating_sub(start).to_string(),
                        correlation: Some(format!("{}:{correlation_id}", chunk.shard_id)),
                        incomplete: true,
                        attributes: attributes(&event.attributes),
                    };
                    self.db
                        .prepare_cached("INSERT OR REPLACE INTO pending VALUES(?1,?2,?3,?4)")?
                        .execute(params![
                            chunk.shard_id.to_string(),
                            track,
                            correlation_id.to_string(),
                            serde_json::to_string(&item)?
                        ])?;
                }
                Payload::Dependency {
                    from_correlation_id,
                    to_correlation_id,
                    relation,
                } => {
                    self.db
                        .prepare_cached("INSERT INTO dependencies(a,b,relation) VALUES(?1,?2,?3)")?
                        .execute(params![
                            format!("{}:{from_correlation_id}", chunk.shard_id),
                            format!("{}:{to_correlation_id}", chunk.shard_id),
                            relation
                        ])?;
                }
                _ => (),
            }
            self.pending.maybe_flush(self.db)?;
        }
        Ok(())
    }
}
fn build(db: &Connection, input: &Path, length: u64, report: &mut Report) -> Result<()> {
    let mut tracks = HashMap::new();
    let shard_domains: HashMap<_, _> = report
        .shards
        .iter()
        .map(|s| (s.id.as_str(), s.domain.as_str()))
        .collect();
    for (n, track) in report.tracks.iter().enumerate() {
        let domain = shard_domains
            .get(track.shard.as_str())
            .context("track shard missing")?;
        db.prepare_cached("INSERT INTO tracks VALUES(?1,?2,?3,?4,?5,?6)")?
            .execute(params![
                track.id,
                n as i64,
                domain,
                track.parent,
                track.kind,
                serde_json::to_string(track)?
            ])?;
        tracks.insert(
            track.id.clone(),
            Track {
                n: n as i64,
                parent: track.parent.clone(),
                kind: track.kind.clone(),
                rank: track.rank,
            },
        );
    }
    let mut shards = HashMap::new();
    for shard in &report.shards {
        let id = shard.id.parse::<Uuid>()?;
        let end = report
            .domains
            .iter()
            .find(|d| d.id == shard.domain)
            .context("clock domain missing")?
            .end_ns
            .parse::<u128>()?
            .max(1);
        shards.insert(
            id,
            Shard {
                host: shard.hostname.clone(),
                boot: String::new(),
                domain: shard.domain.clone(),
                offset: shard.origin_ns.parse()?,
                end,
                observed_end: shard.end_ns.parse()?,
            },
        );
    }
    let filesystems = report
        .io_timeline
        .as_ref()
        .map(|io| {
            io.filesystems
                .iter()
                .map(|f| (f.capture_identity.clone(), f.id.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut builder = Builder {
        db,
        tracks,
        shards,
        filesystems,
        series: HashMap::new(),
        series_bytes: 0,
        next: 1,
        pending: Default::default(),
        gaps: HashMap::new(),
        non_io: HashSet::new(),
        health: BTreeMap::new(),
    };
    crate::analyze::scan(input, Some(length), |record| builder.record(&record))?;
    builder.pending.flush(db)?;
    let mut statement = db.prepare("SELECT data FROM pending")?;
    let mut rows = statement.query([])?;
    let mut unfinished = 0usize;
    while let Some(row) = rows.next()? {
        let mut span: ReportSpan = serde_json::from_str(&row.get::<_, String>(0)?)?;
        span.id = builder.next as usize;
        let start = span.start_ns.parse::<u128>()?;
        let end = start + span.duration_ns.parse::<u128>()?;
        builder.event(&span.track, 0, start, end, 0, &span)?;
        unfinished += 1;
    }
    if unfinished > 0 {
        report.summary.warnings.push(format!("{unfinished} indexed activities have no confirmed completion; displayed ends are observation bounds."));
    }
    db.execute("DELETE FROM pending", [])?;
    let mut file_counts = HashMap::<String, usize>::new();
    for track in &mut report.tracks {
        if track.kind == "io"
            && !builder.non_io.contains(&track.id)
            && let Some(parent) = &track.parent
            && builder
                .tracks
                .get(parent)
                .is_some_and(|p| p.kind == "thread")
        {
            *file_counts.entry(parent.clone()).or_default() += 1;
            track
                .details
                .insert("viewer.paged_file".into(), "true".into());
            db.prepare_cached("INSERT INTO files VALUES(?1,?2,?3)")?
                .execute(params![parent, track.id, builder.tracks[&track.id].n])?;
        }
    }
    let mut counter_series = BTreeMap::<String, Vec<String>>::new();
    let mut series_catalog =
        db.prepare("SELECT scope,filter FROM series WHERE kind='gauge' ORDER BY scope,id")?;
    let mut rows = series_catalog.query([])?;
    let mut catalog_bytes = 0;
    let mut catalog_omitted = 0;
    while let Some(row) = rows.next()? {
        let scope: String = row.get(0)?;
        let filter: String = row.get(1)?;
        let bytes = scope.len() + filter.len();
        if catalog_bytes + bytes > 8 * 1024 * 1024
            || counter_series.get(&scope).is_some_and(|s| s.len() >= 256)
        {
            catalog_omitted += 1;
            continue;
        }
        catalog_bytes += bytes;
        counter_series.entry(scope).or_default().push(filter);
    }
    if catalog_omitted > 0 {
        report.summary.warnings.push(format!("{catalog_omitted} counter-series declarations omitted from the bounded viewer catalog; original observations remain indexed."));
    }
    for track in &mut report.tracks {
        if let Some(count) = file_counts.get(&track.id) {
            track
                .details
                .insert("viewer.file_count".into(), count.to_string());
        }
        if builder.non_io.contains(&track.id) {
            track
                .details
                .insert("viewer.has_non_io".into(), "true".into());
        }
        if let Some(series) = counter_series.get(&track.id) {
            track.details.insert(
                "viewer.counter_series".into(),
                serde_json::to_string(series)?,
            );
        }
    }
    report.counters = builder.health.into_values().collect();
    Ok(())
}
