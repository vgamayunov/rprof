use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::BufRead,
};

use anyhow::{Context, Result, ensure};
use rprof_core::{
    CounterValue,
    stream::{Attribute, End, Event, FORMAT, Header, Loss, Payload, Record, Shard, VERSION},
};
use uuid::Uuid;

use crate::{MAX_FRAME_BYTES, MAX_SHARDS, MAX_TRACKS};

#[derive(Debug, Clone)]
pub struct ShardSummary {
    pub shard: Shard,
    pub tracks: u64,
    pub spans: u64,
    pub counters: u64,
    pub dependencies: u64,
    pub observed_ns: u64,
    pub loss: Loss,
    pub end: Option<End>,
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub header: Header,
    pub shards: BTreeMap<Uuid, ShardSummary>,
    pub truncated_tail: bool,
}

impl Summary {
    pub fn is_complete(&self) -> bool {
        !self.truncated_tail
            && self.header.recovered_inputs == 0
            && !self.shards.is_empty()
            && self.shards.values().all(|s| s.end.is_some())
    }
}

struct State {
    tracks: HashSet<u64>,
    next_sequence: u64,
}

/// Streaming validation has explicit limits on frames, shards, and track IDs.
/// Event history is not retained. Incomplete final frames are ignored; malformed
/// newline-terminated frames are errors, never silently treated as truncation.
pub struct Reader<R> {
    input: R,
    header: Option<Header>,
    summaries: BTreeMap<Uuid, ShardSummary>,
    states: HashMap<Uuid, State>,
    total_tracks: usize,
    metadata_bytes: usize,
    line: u64,
    truncated: bool,
    eof: bool,
}

impl<R: BufRead> Reader<R> {
    pub fn new(input: R) -> Self {
        Self {
            input,
            header: None,
            summaries: BTreeMap::new(),
            states: HashMap::new(),
            total_tracks: 0,
            metadata_bytes: 0,
            line: 0,
            truncated: false,
            eof: false,
        }
    }

    pub fn next_record(&mut self) -> Result<Option<Record>> {
        if self.eof {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        loop {
            let available = self.input.fill_buf()?;
            if available.is_empty() {
                self.eof = true;
                self.truncated = !bytes.is_empty();
                ensure!(self.header.is_some(), "missing complete stream header");
                return Ok(None);
            }
            let count = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |i| i + 1);
            ensure!(
                bytes.len() + count <= MAX_FRAME_BYTES,
                "frame exceeds {} bytes",
                MAX_FRAME_BYTES
            );
            let finished = available[count - 1] == b'\n';
            bytes.extend_from_slice(&available[..count]);
            self.input.consume(count);
            if finished {
                break;
            }
        }
        self.line += 1;
        let record: Record = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid JSON frame at line {}", self.line))?;
        self.validate(&record, bytes.len())
            .with_context(|| format!("invalid frame at line {}", self.line))?;
        Ok(Some(record))
    }

    fn validate(&mut self, record: &Record, frame_bytes: usize) -> Result<()> {
        if let Record::Header(header) = record {
            ensure!(
                self.header.is_none() && self.line == 1,
                "duplicate or misplaced header"
            );
            ensure!(
                header.format == FORMAT,
                "unsupported capture format {}",
                header.format
            );
            ensure!(
                header.schema_version == VERSION,
                "unsupported schema version {} (expected {})",
                header.schema_version,
                VERSION
            );
            self.header = Some(header.clone());
            return Ok(());
        }
        ensure!(self.header.is_some(), "first frame must be a header");
        match record {
            Record::Shard(shard) => {
                ensure!(
                    !self.states.contains_key(&shard.shard_id),
                    "duplicate shard {}",
                    shard.shard_id
                );
                ensure!(
                    self.states.len() < MAX_SHARDS,
                    "shard limit exceeded ({MAX_SHARDS})"
                );
                self.metadata_bytes += frame_bytes;
                ensure!(
                    self.metadata_bytes <= 16 * 1024 * 1024,
                    "shard metadata exceeds 16 MiB"
                );
                ensure!(
                    !shard.hostname.is_empty() && !shard.boot_id.is_empty(),
                    "missing clock domain identity"
                );
                self.states.insert(
                    shard.shard_id,
                    State {
                        tracks: HashSet::new(),
                        next_sequence: 0,
                    },
                );
                self.summaries.insert(
                    shard.shard_id,
                    ShardSummary {
                        shard: shard.clone(),
                        tracks: 0,
                        spans: 0,
                        counters: 0,
                        dependencies: 0,
                        observed_ns: 0,
                        loss: Loss::default(),
                        end: None,
                    },
                );
            }
            Record::Chunk(chunk) => {
                let state = self
                    .states
                    .get_mut(&chunk.shard_id)
                    .context("chunk references unknown shard")?;
                let summary = self.summaries.get_mut(&chunk.shard_id).unwrap();
                ensure!(summary.end.is_none(), "chunk after shard footer");
                ensure!(
                    chunk.sequence == state.next_sequence,
                    "out-of-order chunk: expected {}, got {}",
                    state.next_sequence,
                    chunk.sequence
                );
                state.next_sequence = state
                    .next_sequence
                    .checked_add(1)
                    .context("sequence overflow")?;
                validate_loss(&summary.loss, &chunk.loss)?;
                summary.loss = chunk.loss.clone();
                for event in &chunk.events {
                    validate_event(event, state, summary, &mut self.total_tracks)?;
                }
            }
            Record::End(end) => {
                let summary = self
                    .summaries
                    .get_mut(&end.shard_id)
                    .context("footer references unknown shard")?;
                ensure!(summary.end.is_none(), "duplicate shard footer");
                ensure!(
                    end.duration_ns >= summary.observed_ns,
                    "footer precedes last event"
                );
                ensure!(
                    end.exit_code.is_none() || end.signal.is_none(),
                    "footer has both exit code and signal"
                );
                validate_loss(&summary.loss, &end.loss)?;
                summary.loss = end.loss.clone();
                summary.end = Some(end.clone());
            }
            Record::Header(_) => unreachable!(),
        }
        Ok(())
    }

    pub fn summary(&self) -> Result<Summary> {
        ensure!(self.eof, "read to EOF before requesting summary");
        Ok(Summary {
            header: self.header.clone().context("missing header")?,
            shards: self.summaries.clone(),
            truncated_tail: self.truncated,
        })
    }
}

pub fn summarize(input: impl BufRead) -> Result<Summary> {
    let mut reader = Reader::new(input);
    while reader.next_record()?.is_some() {}
    reader.summary()
}

fn validate_loss(old: &Loss, new: &Loss) -> Result<()> {
    ensure!(
        new.queue_full >= old.queue_full
            && new.oversized >= old.oversized
            && new.ingress >= old.ingress,
        "loss counters decreased"
    );
    Ok(())
}

fn validate_event(
    event: &Event,
    state: &mut State,
    summary: &mut ShardSummary,
    total_tracks: &mut usize,
) -> Result<()> {
    ensure!(
        event.timestamp_ns >= summary.observed_ns,
        "observation timestamps went backwards"
    );
    for value in event.attributes.values() {
        if let Attribute::Float(value) = value {
            ensure!(value.is_finite(), "non-finite attribute");
        }
    }
    match &event.payload {
        Payload::Track(track) => {
            ensure!(
                !state.tracks.contains(&track.id),
                "duplicate track {}",
                track.id
            );
            if let Some(parent) = track.parent_id {
                ensure!(
                    state.tracks.contains(&parent),
                    "unknown parent track {parent}"
                );
            }
            ensure!(
                *total_tracks < MAX_TRACKS,
                "track limit exceeded ({MAX_TRACKS})"
            );
            state.tracks.insert(track.id);
            *total_tracks += 1;
            summary.tracks += 1;
        }
        Payload::Span(span) => {
            ensure!(
                state.tracks.contains(&span.track_id),
                "span references unknown track {}",
                span.track_id
            );
            let end = span
                .start_ns
                .checked_add(span.duration_ns)
                .context("span timestamp overflow")?;
            ensure!(end <= event.timestamp_ns, "span ends after observation");
            summary.spans += 1;
        }
        Payload::Counter(counter) => {
            ensure!(
                state.tracks.contains(&counter.track_id),
                "counter references unknown track"
            );
            ensure!(
                counter.timestamp_ns <= event.timestamp_ns,
                "counter follows observation"
            );
            if let CounterValue::Float(value) = counter.value {
                ensure!(value.is_finite(), "non-finite counter");
            }
            summary.counters += 1;
        }
        Payload::ActivityStart {
            track_id, start_ns, ..
        } => {
            ensure!(
                start_ns.is_none_or(|start| start <= event.timestamp_ns),
                "activity starts after observation"
            );
            ensure!(
                state.tracks.contains(track_id),
                "activity references unknown track"
            );
        }
        Payload::Dependency { .. } => summary.dependencies += 1,
        Payload::Heartbeat => (),
    }
    summary.observed_ns = event.timestamp_ns;
    Ok(())
}
