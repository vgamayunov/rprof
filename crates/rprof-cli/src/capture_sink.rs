use anyhow::{Context, Result, ensure};
use rprof_capture::Recorder;
use rprof_core::{
    Track, TrackKind,
    stream::{Event, Payload},
};

/// The supervisor and external native receiver serialize observations here. Raw
/// span timestamps remain original; observation time is monotonic at ingestion.
pub struct CaptureSink {
    pub recorder: Recorder,
    last_ns: u64,
    next_track: u64,
    ingress: std::collections::BTreeMap<&'static str, u64>,
    pub rank: Option<u32>,
    processes: std::collections::HashMap<(u32, u64), (u64, Option<u32>)>,
    threads: std::collections::HashMap<(u32, u64, u32), u64>,
    correlations: std::collections::HashMap<(&'static str, u64), u64>,
}

impl CaptureSink {
    pub fn new(recorder: Recorder) -> Self {
        Self {
            recorder,
            last_ns: 0,
            next_track: 1,
            ingress: std::collections::BTreeMap::new(),
            rank: None,
            processes: Default::default(),
            threads: Default::default(),
            correlations: Default::default(),
        }
    }
    pub fn set_ingress_loss(&mut self, collector: &'static str, count: u64) {
        let value = self.ingress.entry(collector).or_default();
        *value = (*value).max(count);
        self.recorder.set_ingress_loss(
            self.ingress
                .values()
                .fold(0u64, |sum, value| sum.saturating_add(*value)),
        );
    }
    pub fn reliable(&mut self, mut event: Event) -> Result<()> {
        event.timestamp_ns = event.timestamp_ns.max(self.last_ns);
        self.last_ns = event.timestamp_ns;
        self.recorder.reliable(event)
    }
    pub fn emit(&mut self, mut event: Event) -> Result<()> {
        event.timestamp_ns = event.timestamp_ns.max(self.last_ns);
        self.last_ns = event.timestamp_ns;
        self.recorder.emit(event)?;
        Ok(())
    }
    pub fn track(&mut self, at: u64, parent: u64, name: String, kind: TrackKind) -> Result<u64> {
        self.track_ranked(at, parent, name, kind, self.rank)
    }
    pub fn native_thread(
        &mut self,
        at: u64,
        pid: u32,
        instance: u64,
        tid: u32,
        rank: Option<u32>,
    ) -> Result<u64> {
        if let Some(track) = self.threads.get(&(pid, instance, tid)) {
            return Ok(*track);
        }
        let (process, rank) = if let Some(value) = self.processes.get(&(pid, instance)) {
            *value
        } else {
            ensure!(
                self.processes.len() < 4096,
                "native process-image limit exceeded"
            );
            let rank = rank.or(self.rank);
            let track = self.track_ranked(
                at,
                0,
                format!("process {pid} / image {instance:016x}"),
                TrackKind::Process { pid },
                rank,
            )?;
            self.processes.insert((pid, instance), (track, rank));
            (track, rank)
        };
        let track = self.track_ranked(
            at,
            process,
            format!("thread {tid}"),
            TrackKind::Thread { tid: tid as u64 },
            rank,
        )?;
        self.threads.insert((pid, instance, tid), track);
        Ok(track)
    }
    pub fn retire_native_thread(&mut self, pid: u32, instance: u64, tid: u32) {
        self.threads.remove(&(pid, instance, tid));
    }
    pub fn correlation(&mut self, collector: &'static str, instance: u64, id: u64) -> Result<u64> {
        ensure!(
            id < (1u64 << 48),
            "native per-image correlation space exhausted"
        );
        let key = (collector, instance);
        if !self.correlations.contains_key(&key) {
            ensure!(
                self.correlations.len() < 16384,
                "native correlation namespace limit exceeded"
            );
            self.correlations
                .insert(key, ((self.correlations.len() as u64) + 1) << 48);
        }
        Ok(self.correlations[&key] | id)
    }
    pub fn track_ranked(
        &mut self,
        at: u64,
        parent: u64,
        name: String,
        kind: TrackKind,
        rank: Option<u32>,
    ) -> Result<u64> {
        ensure!(
            self.next_track < rprof_capture::MAX_TRACKS as u64,
            "capture track limit exceeded; reduce I/O file/thread cardinality"
        );
        let id = self.next_track;
        self.next_track = self
            .next_track
            .checked_add(1)
            .context("track ID overflow")?;
        self.reliable(Event::new(
            at,
            Payload::Track(Track {
                id,
                parent_id: Some(parent),
                name,
                rank,
                kind,
            }),
        ))?;
        Ok(id)
    }
}
