//! Function cost channels are independent of retained/rendered timeline events.
use crate::{
    Report, ReportShard, ReportTrack,
    analyze::{key, scan},
};
use anyhow::{Context, Result, ensure};
use rprof_core::{
    Category,
    stream::{Attribute, Event, Payload, Record, StackTrace},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};
use uuid::Uuid;

const MAX_NODES: usize = 20_000;
const MAX_OFFLINE_FACTS: usize = 100_000;
const MAX_OFFLINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frame {
    pub key: String,
    pub name: String,
    pub module: String,
    pub language: String,
    pub address: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trace {
    pub frames: Vec<Frame>,
    pub status: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Cpu,
    IoTime,
    IoBytes,
    MpiTime,
    MpiBytes,
    CudaApi,
    GpuTime,
    GpuBytes,
}
impl Metric {
    fn category(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::IoTime | Self::IoBytes => "io",
            Self::MpiTime | Self::MpiBytes => "mpi",
            Self::CudaApi | Self::GpuTime | Self::GpuBytes => "cuda",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::IoTime => "io_time",
            Self::IoBytes => "io_bytes",
            Self::MpiTime => "mpi_time",
            Self::MpiBytes => "mpi_bytes",
            Self::CudaApi => "cuda_api",
            Self::GpuTime => "gpu_time",
            Self::GpuBytes => "gpu_bytes",
        }
    }
    pub fn is_duration(self) -> bool {
        matches!(
            self,
            Self::IoTime | Self::MpiTime | Self::CudaApi | Self::GpuTime
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Fact {
    pub domain: String,
    pub track: String,
    pub stack: Option<String>,
    pub correlation: Option<String>,
    pub name: String,
    pub metric: Metric,
    pub start_ns: String,
    pub end_ns: String,
    pub weight: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Link {
    pub from: String,
    pub to: String,
    pub priority: u8,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Data {
    pub traces: BTreeMap<String, Trace>,
    pub facts: Vec<Fact>,
    pub sources: BTreeMap<String, Vec<String>>,
    pub links: Vec<Link>,
    pub complete: bool,
    pub observations: String,
    pub warnings: Vec<String>,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Orientation {
    #[default]
    TopDown,
    BottomUp,
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    #[default]
    Combined,
    Rank,
    Thread,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Query {
    pub snapshot: String,
    pub domain: String,
    pub start_ns: String,
    pub end_ns: String,
    pub metric: Metric,
    #[serde(default)]
    pub orientation: Orientation,
    #[serde(default)]
    pub group: Group,
    #[serde(default)]
    pub filter: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: usize,
    pub parent: Option<usize>,
    pub frame: Frame,
    pub total: String,
    pub self_weight: String,
    pub observations: String,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Tree {
    pub snapshot: String,
    pub domain: String,
    pub metric: Metric,
    pub nodes: Vec<Node>,
    pub total: String,
    pub observations: String,
    pub unattributed: String,
    pub warnings: Vec<String>,
}

fn trace(value: &StackTrace) -> Trace {
    Trace {
        frames: value
            .frames
            .iter()
            .map(|f| Frame {
                key: f.key.clone(),
                name: f.name.clone(),
                module: f.module.clone(),
                language: match f.language {
                    rprof_core::stream::FrameLanguage::Native => "native",
                    rprof_core::stream::FrameLanguage::Python => "python",
                    rprof_core::stream::FrameLanguage::Unknown => "unknown",
                }
                .into(),
                address: format!("0x{:x}", f.address),
            })
            .collect(),
        status: match value.status {
            rprof_core::stream::StackStatus::Unknown => "unknown",
            rprof_core::stream::StackStatus::Truncated => "truncated",
            rprof_core::stream::StackStatus::Unavailable => "unavailable",
        }
        .into(),
    }
}
fn uint(event: &Event, name: &str) -> Option<u64> {
    if let Some(Attribute::Unsigned(n)) = event.attributes.get(name) {
        Some(*n)
    } else {
        None
    }
}
fn text<'a>(event: &'a Event, name: &str) -> Option<&'a str> {
    if let Some(Attribute::Text(s)) = event.attributes.get(name) {
        Some(s)
    } else {
        None
    }
}
fn correlation(event: &Event) -> Option<u64> {
    match &event.payload {
        Payload::Span(s) => s.correlation_id,
        Payload::ActivityStart { correlation_id, .. } => Some(*correlation_id),
        _ => None,
    }
}
fn link(shard: Uuid, event: &Event) -> Option<Link> {
    let Payload::Dependency {
        from_correlation_id,
        to_correlation_id,
        relation,
    } = &event.payload
    else {
        return None;
    };
    let priority = match relation.as_str() {
        "CUPTI runtime activity correlation" => 2,
        "CUPTI activity correlation" => 1,
        _ => return None,
    };
    Some(Link {
        from: key(shard, *from_correlation_id),
        to: key(shard, *to_correlation_id),
        priority,
    })
}
fn facts(shard: Uuid, domain: &str, offset: u128, event: &Event) -> Vec<Fact> {
    let mut output = Vec::new();
    let mut add = |track: u64,
                   stack: Option<u64>,
                   correlation: Option<u64>,
                   name: &str,
                   metric: Metric,
                   start: u64,
                   end: u64,
                   weight: u64| {
        output.push(Fact {
            domain: domain.into(),
            track: key(shard, track),
            stack: stack.map(|id| key(shard, id)),
            correlation: correlation.map(|id| key(shard, id)),
            name: name.into(),
            metric,
            start_ns: (offset + start as u128).to_string(),
            end_ns: (offset + end as u128).to_string(),
            weight: weight.to_string(),
        });
    };
    match &event.payload {
        Payload::StackSample(s) => add(
            s.track_id,
            s.stack_id,
            None,
            "CPU samples",
            Metric::Cpu,
            s.timestamp_ns,
            s.timestamp_ns,
            s.period_ns,
        ),
        Payload::Span(s) => {
            let id = uint(event, "stack.id");
            let end = s.start_ns + s.duration_ns;
            let time_metric = match s.category {
                Category::Io if text(event, "io.operation").is_some() => Some(Metric::IoTime),
                Category::Mpi if text(event, "mpi.record_type") == Some("call") => {
                    Some(Metric::MpiTime)
                }
                Category::Cuda if text(event, "cuda.role") == Some("api") => Some(Metric::CudaApi),
                Category::Cuda
                    if matches!(
                        text(event, "cuda.role"),
                        Some("kernel" | "memcpy" | "memset")
                    ) =>
                {
                    Some(Metric::GpuTime)
                }
                _ => None,
            };
            if let Some(metric) = time_metric {
                add(
                    s.track_id,
                    id,
                    s.correlation_id,
                    &s.name,
                    metric,
                    s.start_ns,
                    end,
                    s.duration_ns,
                );
                let bytes = match metric {
                    Metric::IoTime => {
                        uint(event, "io.completed_bytes").map(|n| (Metric::IoBytes, n))
                    }
                    Metric::MpiTime => uint(event, "mpi.bytes").map(|n| (Metric::MpiBytes, n)),
                    Metric::GpuTime
                        if matches!(text(event, "cuda.role"), Some("memcpy" | "memset")) =>
                    {
                        uint(event, "cuda.bytes").map(|n| (Metric::GpuBytes, n))
                    }
                    _ => None,
                };
                if let Some((metric, bytes)) = bytes {
                    add(
                        s.track_id,
                        id,
                        s.correlation_id,
                        &s.name,
                        metric,
                        end,
                        end,
                        bytes,
                    );
                }
            }
        }
        _ => (),
    }
    output
}

fn scope(shards: &[ReportShard]) -> Result<HashMap<Uuid, (String, u128)>> {
    shards
        .iter()
        .map(|s| Ok((s.id.parse()?, (s.domain.clone(), s.origin_ns.parse()?))))
        .collect()
}
pub(crate) fn collect(path: &Path, length: u64, shards: &[ReportShard]) -> Result<Option<Data>> {
    let scopes = scope(shards)?;
    let mut enabled = shards
        .iter()
        .any(|s| s.collectors.iter().any(|c| c == "stacks"));
    let mut data = Data {
        complete: true,
        ..Default::default()
    };
    let mut used = 0;
    let mut total = 0u64;
    scan(path, Some(length), |record| {
        let Record::Chunk(chunk) = record else {
            return Ok(());
        };
        let (domain, offset) = &scopes[&chunk.shard_id];
        for event in chunk.events {
            enabled |= matches!(
                event.payload,
                Payload::StackTrace(_) | Payload::StackSample(_)
            );
            let items = facts(chunk.shard_id, domain, *offset, &event);
            total += items.len() as u64;
            if !data.complete {
                continue;
            }
            let structural = matches!(event.payload, Payload::StackTrace(_));
            let source = correlation(&event).is_some() && uint(&event, "stack.id").is_some();
            let edge = link(chunk.shard_id, &event);
            if structural || source || edge.is_some() || !items.is_empty() {
                used += serde_json::to_vec(&event)?.len();
            }
            if let Payload::StackTrace(stack) = &event.payload {
                data.traces
                    .insert(key(chunk.shard_id, stack.id), trace(stack));
            }
            if let (Some(correlation), Some(stack)) =
                (correlation(&event), uint(&event, "stack.id"))
            {
                let source = data
                    .sources
                    .entry(key(chunk.shard_id, correlation))
                    .or_default();
                let stack = key(chunk.shard_id, stack);
                if !source.contains(&stack) {
                    source.push(stack);
                }
            }
            if let Some(link) = edge {
                data.links.push(link);
            }
            data.facts.extend(items);
            if data.facts.len() > MAX_OFFLINE_FACTS || used > MAX_OFFLINE_BYTES {
                data.complete = false;
                data.facts.clear();
                data.traces.clear();
                data.sources.clear();
                data.links.clear();
            }
        }
        Ok(())
    })?;
    if !enabled {
        return Ok(None);
    }
    data.observations = total.to_string();
    data.warnings.push("Frame-pointer stack coverage is not guaranteed; unknown/truncated callers and source loss are not extrapolated. CPU weights are estimates, not function-call counts.".into());
    if !data.complete {
        data.warnings.push("The complete stack filtering dataset exceeds the standalone report budget. Use rprof view for exact function queries; no retained prefix is presented as a complete profile.".into());
    }
    Ok(Some(data))
}

pub(crate) fn index(
    db: &Connection,
    path: &Path,
    length: u64,
    shards: &[ReportShard],
) -> Result<Option<Data>> {
    db.execute_batch("
        CREATE TABLE stack_traces(id TEXT PRIMARY KEY,data TEXT NOT NULL);
        CREATE TABLE stack_facts(id INTEGER PRIMARY KEY,domain TEXT NOT NULL,metric TEXT NOT NULL,start BLOB NOT NULL,end BLOB NOT NULL,data TEXT NOT NULL);
        CREATE INDEX stack_window ON stack_facts(domain,metric,end,start);
        CREATE TABLE stack_sources(correlation TEXT NOT NULL,stack TEXT NOT NULL,PRIMARY KEY(correlation,stack));
        CREATE TABLE stack_links(target TEXT NOT NULL,source TEXT NOT NULL,priority INTEGER NOT NULL);
        CREATE INDEX stack_link_target ON stack_links(target,priority);")?;
    let scopes = scope(shards)?;
    let mut enabled = shards
        .iter()
        .any(|s| s.collectors.iter().any(|c| c == "stacks"));
    let mut count = 0u64;
    scan(path, Some(length), |record| {
        let Record::Chunk(chunk) = record else {
            return Ok(());
        };
        let (domain, offset) = &scopes[&chunk.shard_id];
        for event in chunk.events {
            enabled |= matches!(
                event.payload,
                Payload::StackTrace(_) | Payload::StackSample(_)
            );
            if let Payload::StackTrace(stack) = &event.payload {
                db.prepare_cached("INSERT INTO stack_traces VALUES(?1,?2)")?
                    .execute(params![
                        key(chunk.shard_id, stack.id),
                        serde_json::to_string(&trace(stack))?
                    ])?;
            }
            if let (Some(correlation), Some(stack)) =
                (correlation(&event), uint(&event, "stack.id"))
            {
                db.prepare_cached("INSERT OR IGNORE INTO stack_sources VALUES(?1,?2)")?
                    .execute(params![
                        key(chunk.shard_id, correlation),
                        key(chunk.shard_id, stack)
                    ])?;
            }
            if let Some(link) = link(chunk.shard_id, &event) {
                db.prepare_cached("INSERT INTO stack_links VALUES(?1,?2,?3)")?
                    .execute(params![link.to, link.from, link.priority])?;
            }
            for fact in facts(chunk.shard_id, domain, *offset, &event) {
                db.prepare_cached(
                    "INSERT INTO stack_facts(domain,metric,start,end,data) VALUES(?1,?2,?3,?4,?5)",
                )?
                .execute(params![
                    domain,
                    fact.metric.name(),
                    fact.start_ns.parse::<u128>()?.to_be_bytes().as_slice(),
                    fact.end_ns.parse::<u128>()?.to_be_bytes().as_slice(),
                    serde_json::to_string(&fact)?
                ])?;
                count += 1;
            }
        }
        Ok(())
    })?;
    Ok(enabled.then(|| Data { complete: true, observations: count.to_string(),
        warnings: vec!["User-space frame-pointer stacks; missing callers and losses remain unknown. Cost channels are not additive wall-clock time.".into()],
        ..Default::default() }))
}

fn missing() -> Frame {
    Frame {
        key: "missing-stack".into(),
        name: "[stack not captured]".into(),
        module: String::new(),
        language: "unknown".into(),
        address: String::new(),
    }
}
pub(crate) fn filter_text(
    track: &ReportTrack,
    tracks: &HashMap<String, ReportTrack>,
    fact: &Fact,
) -> String {
    let mut value = format!(
        "{} {} {} ",
        fact.name,
        fact.metric.name(),
        fact.metric.category()
    );
    if let Some(rank) = track.rank {
        value.push_str(&format!("rank {rank} rank:{rank} "));
    }
    let mut parent = Some(track);
    for _ in 0..128 {
        let Some(track) = parent else {
            break;
        };
        value.push_str(&track.name);
        value.push(' ');
        parent = track.parent.as_ref().and_then(|id| tracks.get(id));
    }
    value.to_lowercase()
}
struct Accumulator {
    nodes: Vec<Node>,
    weights: Vec<(u128, u128, u128)>,
    lookup: HashMap<(Option<usize>, String), usize>,
    total: u128,
    count: u128,
    unattributed: u128,
}
impl Accumulator {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            weights: Vec::new(),
            lookup: HashMap::new(),
            total: 0,
            count: 0,
            unattributed: 0,
        }
    }
    fn add(
        &mut self,
        frames: &[Frame],
        scope: Option<Frame>,
        weight: u128,
        count: u128,
        orientation: Orientation,
    ) -> Result<()> {
        self.total = self
            .total
            .checked_add(weight)
            .context("function total overflow")?;
        self.count = self
            .count
            .checked_add(count)
            .context("function count overflow")?;
        if frames.is_empty() {
            self.unattributed = self
                .unattributed
                .checked_add(weight)
                .context("unattributed total overflow")?;
        }
        let unknown = [missing()];
        let frames = if frames.is_empty() {
            &unknown[..]
        } else {
            frames
        };
        let mut path: Vec<&Frame> = frames.iter().collect();
        if orientation == Orientation::BottomUp {
            path.reverse();
        }
        let self_index = if orientation == Orientation::BottomUp {
            0
        } else {
            path.len() - 1
        };
        let self_index = self_index + usize::from(scope.is_some());
        if let Some(scope) = &scope {
            path.insert(0, scope);
        }
        let mut parent = None;
        for (position, frame) in path.into_iter().enumerate() {
            let key = (parent, frame.key.clone());
            let id = if let Some(&id) = self.lookup.get(&key) {
                id
            } else {
                ensure!(
                    self.nodes.len() < MAX_NODES,
                    "function tree exceeds 20000 nodes; narrow the time/rank/thread scope"
                );
                let id = self.nodes.len();
                self.nodes.push(Node {
                    id,
                    parent,
                    frame: frame.clone(),
                    total: "0".into(),
                    self_weight: "0".into(),
                    observations: "0".into(),
                });
                self.weights.push((0, 0, 0));
                self.lookup.insert(key, id);
                id
            };
            let values = &mut self.weights[id];
            values.0 = values
                .0
                .checked_add(weight)
                .context("function inclusive total overflow")?;
            values.2 = values
                .2
                .checked_add(count)
                .context("function observation count overflow")?;
            if position == self_index {
                values.1 = values
                    .1
                    .checked_add(weight)
                    .context("function self total overflow")?;
            }
            parent = Some(id);
        }
        Ok(())
    }
    fn finish(mut self, query: &Query) -> Tree {
        for (node, (total, own, count)) in self.nodes.iter_mut().zip(self.weights) {
            node.total = total.to_string();
            node.self_weight = own.to_string();
            node.observations = count.to_string();
        }
        Tree {
            snapshot: query.snapshot.clone(),
            domain: query.domain.clone(),
            metric: query.metric,
            nodes: self.nodes,
            total: self.total.to_string(),
            observations: self.count.to_string(),
            unattributed: self.unattributed.to_string(),
            warnings: Vec::new(),
        }
    }
}
fn bounds(query: &Query) -> Result<(u128, u128)> {
    ensure!(
        query.filter.len() <= 1024,
        "function filter exceeds 1024 bytes"
    );
    let begin: u128 = query
        .start_ns
        .parse()
        .context("invalid function range start")?;
    let end: u128 = query.end_ns.parse().context("invalid function range end")?;
    ensure!(begin < end, "function range must have positive duration");
    Ok((begin, end))
}
fn amount(fact: &Fact, begin: u128, end: u128) -> Result<Option<(u128, u128)>> {
    let start: u128 = fact.start_ns.parse()?;
    let finish: u128 = fact.end_ns.parse()?;
    ensure!(
        finish >= start,
        "function observation ends before it starts"
    );
    if fact.metric.is_duration() {
        let overlap = finish.min(end).saturating_sub(start.max(begin));
        let count = u128::from(finish >= begin && finish < end);
        Ok((overlap > 0 || count > 0).then_some((overlap, count)))
    } else if start >= begin && start < end {
        Ok(Some((fact.weight.parse()?, 1)))
    } else {
        Ok(None)
    }
}
fn group_frame(
    group: Group,
    track: &ReportTrack,
    tracks: &HashMap<String, ReportTrack>,
) -> Option<Frame> {
    let mut track = track;
    if group == Group::Thread {
        for _ in 0..128 {
            if matches!(track.kind.as_str(), "thread" | "gpu_stream" | "process") {
                break;
            }
            let Some(parent) = track.parent.as_ref().and_then(|id| tracks.get(id)) else {
                break;
            };
            track = parent;
        }
    }
    let (key, name) = match group {
        Group::Combined => return None,
        Group::Rank => (
            format!(
                "rank:{}:{}",
                track.shard,
                track
                    .rank
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "null".into())
            ),
            format!(
                "{} / {}",
                track
                    .rank
                    .map(|n| format!("Rank {n}"))
                    .unwrap_or_else(|| "Unranked".into()),
                track.shard
            ),
        ),
        Group::Thread => (format!("track:{}", track.id), track.name.clone()),
    };
    Some(Frame {
        key,
        name,
        module: String::new(),
        language: "scope".into(),
        address: String::new(),
    })
}
fn resolved_stack(fact: &Fact, sources: &HashMap<String, Option<String>>) -> Option<String> {
    if fact.stack.is_some() {
        return fact.stack.clone();
    }
    if !matches!(fact.metric, Metric::GpuTime | Metric::GpuBytes) {
        return None;
    }
    sources.get(fact.correlation.as_ref()?).cloned().flatten()
}
fn attributions(data: &Data) -> HashMap<String, Option<String>> {
    let mut candidates = HashMap::<String, (u8, HashSet<String>)>::new();
    for link in &data.links {
        let entry = candidates.entry(link.to.clone()).or_default();
        if link.priority > entry.0 {
            entry.0 = link.priority;
            entry.1.clear();
        }
        if link.priority == entry.0 {
            if let Some(sources) = data.sources.get(&link.from) {
                entry.1.extend(sources.iter().cloned());
            }
        }
    }
    candidates
        .into_iter()
        .map(|(target, (_, values))| {
            (
                target,
                (values.len() == 1).then(|| values.into_iter().next().unwrap()),
            )
        })
        .collect()
}

pub fn aggregate(data: &Data, tracks: &[ReportTrack], query: &Query) -> Result<Tree> {
    ensure!(
        data.complete,
        "complete stack filtering data is unavailable; use rprof view"
    );
    let (begin, end) = bounds(query)?;
    let tracks: HashMap<_, _> = tracks.iter().cloned().map(|t| (t.id.clone(), t)).collect();
    let terms: Vec<_> = query
        .filter
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let mut result = Accumulator::new();
    let attribution = attributions(data);
    for fact in data
        .facts
        .iter()
        .filter(|f| f.domain == query.domain && f.metric == query.metric)
    {
        let Some((weight, count)) = amount(fact, begin, end)? else {
            continue;
        };
        let track = tracks
            .get(&fact.track)
            .context("function observation has no track")?;
        let text = filter_text(track, &tracks, fact);
        if !terms.iter().all(|term| text.contains(term)) {
            continue;
        }
        let stack = resolved_stack(fact, &attribution);
        let frames = stack
            .as_ref()
            .and_then(|id| data.traces.get(id))
            .map(|t| t.frames.as_slice())
            .unwrap_or(&[]);
        result.add(
            frames,
            group_frame(query.group, track, &tracks),
            weight,
            count,
            query.orientation,
        )?;
    }
    let mut tree = result.finish(query);
    tree.warnings = data.warnings.clone();
    Ok(tree)
}

pub(crate) fn query(db: &Connection, report: &Report, query: Query) -> Result<Tree> {
    let (begin, end) = bounds(&query)?;
    ensure!(
        report.domains.iter().any(|d| d.id == query.domain),
        "unknown function clock domain"
    );
    let mut tracks = HashMap::new();
    let mut rows = db.prepare("SELECT id,data FROM tracks")?;
    for row in rows.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (id, data) = row?;
        tracks.insert(id, serde_json::from_str::<ReportTrack>(&data)?);
    }
    let terms: Vec<_> = query
        .filter
        .split_whitespace()
        .map(str::to_lowercase)
        .collect();
    let mut traces = HashMap::<String, Trace>::new();
    let mut attribution = HashMap::<String, Option<String>>::new();
    let mut builder = Accumulator::new();
    let mut rows = db.prepare("SELECT data FROM stack_facts WHERE domain=?1 AND metric=?2 AND end>=?3 AND start<?4 ORDER BY id")?;
    for row in rows.query_map(
        params![
            query.domain,
            query.metric.name(),
            begin.to_be_bytes().as_slice(),
            end.to_be_bytes().as_slice()
        ],
        |r| r.get::<_, String>(0),
    )? {
        let fact: Fact = serde_json::from_str(&row?)?;
        let Some((weight, count)) = amount(&fact, begin, end)? else {
            continue;
        };
        let track = tracks
            .get(&fact.track)
            .context("function observation has no track")?;
        let text = filter_text(track, &tracks, &fact);
        if !terms.iter().all(|term| text.contains(term)) {
            continue;
        }
        let mut stack = fact.stack;
        if stack.is_none() && matches!(fact.metric, Metric::GpuTime | Metric::GpuBytes) {
            if let Some(correlation) = &fact.correlation {
                if let Some(saved) = attribution.get(correlation) {
                    stack = saved.clone();
                } else {
                    let mut links = db.prepare_cached("SELECT DISTINCT s.stack FROM stack_links l JOIN stack_sources s ON s.correlation=l.source WHERE l.target=?1 AND l.priority=(SELECT max(priority) FROM stack_links WHERE target=?1) LIMIT 2")?;
                    let ids = links
                        .query_map([correlation], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    stack = (ids.len() == 1).then(|| ids[0].clone());
                    if attribution.len() < 65_536 {
                        attribution.insert(correlation.clone(), stack.clone());
                    }
                }
            }
        }
        if let Some(id) = &stack {
            if !traces.contains_key(id) {
                let encoded: Option<String> = db
                    .query_row("SELECT data FROM stack_traces WHERE id=?1", [id], |r| {
                        r.get(0)
                    })
                    .optional()?;
                if let Some(encoded) = encoded {
                    traces.insert(id.clone(), serde_json::from_str(&encoded)?);
                }
            }
        }
        let frames = stack
            .as_ref()
            .and_then(|id| traces.get(id))
            .map(|t| t.frames.as_slice())
            .unwrap_or(&[]);
        builder.add(
            frames,
            group_frame(query.group, track, &tracks),
            weight,
            count,
            query.orientation,
        )?;
    }
    let mut result = builder.finish(&query);
    result.warnings = report
        .stacks
        .as_ref()
        .map(|s| s.warnings.clone())
        .unwrap_or_default();
    Ok(result)
}
