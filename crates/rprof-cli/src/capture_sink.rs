use anyhow::{Context, Result, ensure};
use rprof_capture::Recorder;
use rprof_core::{
    CounterSample, CounterValue, Track, TrackKind,
    stream::{Attribute, Event, Payload},
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
    process_names: std::collections::HashMap<(u32, u64), std::result::Result<String, i32>>,
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
            process_names: Default::default(),
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
        let process = self.native_process(at, pid, instance, rank)?;
        let rank = self.processes[&(pid, instance)].1;
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
    pub fn native_process(
        &mut self,
        at: u64,
        pid: u32,
        instance: u64,
        rank: Option<u32>,
    ) -> Result<u64> {
        if let Some((track, _)) = self.processes.get(&(pid, instance)) {
            return Ok(*track);
        }
        ensure!(
            self.processes.len() < 4096,
            "native process-image limit exceeded"
        );
        let rank = rank.or(self.rank);
        let name = self.process_names.get(&(pid, instance)).cloned();
        let label = name
            .as_ref()
            .and_then(|name| name.as_ref().ok())
            .map_or_else(
                || format!("process {pid} / image {instance:016x}"),
                |name| format!("{name} (PID {pid}) / image {instance:016x}"),
            );
        let track = self.track_ranked(at, 0, label, TrackKind::Process { pid }, rank)?;
        self.processes.insert((pid, instance), (track, rank));
        if let Some(name) = name {
            self.emit_process_name(at, track, pid, instance, &name)?;
        }
        Ok(track)
    }
    pub fn native_process_name(
        &mut self,
        at: u64,
        pid: u32,
        instance: u64,
        name: std::result::Result<String, i32>,
    ) -> Result<()> {
        let key = (pid, instance);
        if self
            .process_names
            .get(&key)
            .is_some_and(|previous| previous.is_ok() || *previous == name)
        {
            return Ok(());
        }
        ensure!(
            self.process_names.contains_key(&key) || self.process_names.len() < 4096,
            "native process-name limit exceeded"
        );
        if let Some(&(track, _)) = self.processes.get(&key) {
            self.emit_process_name(at, track, pid, instance, &name)?;
        }
        // Defer track creation until an ordinary observation supplies its MPI rank.
        self.process_names.insert(key, name);
        Ok(())
    }
    fn emit_process_name(
        &mut self,
        at: u64,
        track: u64,
        pid: u32,
        instance: u64,
        name: &std::result::Result<String, i32>,
    ) -> Result<()> {
        let mut event = Event::new(
            at,
            Payload::Counter(CounterSample {
                track_id: track,
                timestamp_ns: at,
                name: "process.meta.executable".into(),
                value: CounterValue::Integer(u64::from(name.is_ok())),
                unit: "status".into(),
            }),
        );
        event.attributes.insert(
            "process.name_source".into(),
            Attribute::Text("native /proc/self/exe basename; no command-line arguments".into()),
        );
        event
            .attributes
            .insert("process.instance".into(), Attribute::Unsigned(instance));
        match name {
            Ok(name) => {
                event
                    .attributes
                    .insert("process.executable".into(), Attribute::Text(name.clone()));
                event.attributes.insert(
                    "process.label".into(),
                    Attribute::Text(format!("{name} (PID {pid}) / image {instance:016x}")),
                );
            }
            Err(error) => {
                event.attributes.insert(
                    "process.name_error".into(),
                    Attribute::Text(std::io::Error::from_raw_os_error(*error).to_string()),
                );
            }
        }
        self.reliable(event)
    }
    pub fn retire_native_thread(&mut self, pid: u32, instance: u64, tid: u32) {
        self.threads.remove(&(pid, instance, tid));
    }
    pub fn finish_process_names(&mut self, at: u64) -> Result<()> {
        let pending: Vec<_> = self
            .process_names
            .keys()
            .filter(|key| !self.processes.contains_key(key))
            .copied()
            .collect();
        for (pid, instance) in pending {
            self.native_process(at, pid, instance, None)?;
        }
        Ok(())
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
        self.track_with_parent(at, Some(parent), name, kind, rank)
    }
    pub fn track_with_parent(
        &mut self,
        at: u64,
        parent: Option<u64>,
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
                parent_id: parent,
                name,
                rank,
                kind,
            }),
        ))?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rprof_capture::RecorderConfig;
    use rprof_core::stream::{End, EndReason, Header, Loss, Shard};
    use rprof_report::{Limits, analyze, indexed::CaptureIndex};
    use uuid::Uuid;

    #[test]
    fn executable_names_preserve_rank_and_exec_identity_even_when_metadata_arrives_late() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("names.jsonl");
        let shard_id = Uuid::new_v4();
        let config = RecorderConfig::default();
        let recorder = Recorder::create(
            &path,
            Header::new(Uuid::new_v4()),
            Shard {
                shard_id,
                parent_shard_id: None,
                hostname: "node".into(),
                boot_id: "boot".into(),
                monotonic_origin_ns: 0,
                started_unix_ns: 0,
                command: vec!["launcher".into()],
                supervisor_pid: 1,
                collectors: vec![],
                config: config.metadata(),
            },
            config,
        )
        .unwrap();
        let mut sink = CaptureSink::new(recorder);
        sink.reliable(Event::new(
            0,
            Payload::Track(Track {
                id: 0,
                parent_id: None,
                name: "launcher".into(),
                rank: None,
                kind: TrackKind::Process { pid: 1 },
            }),
        ))
        .unwrap();
        sink.native_process_name(1, 42, 1, Ok("gemm".into()))
            .unwrap();
        let early = sink.native_process(2, 42, 1, Some(7)).unwrap();
        let late = sink.native_process(3, 42, 2, Some(8)).unwrap();
        sink.native_process_name(4, 42, 2, Ok("second-image".into()))
            .unwrap();
        sink.native_process_name(5, 42, 3, Err(libc::EACCES))
            .unwrap();
        let missing = sink.native_process(6, 42, 3, None).unwrap();
        let legacy = sink.native_process(7, 42, 4, None).unwrap();
        sink.native_process_name(7, 43, 5, Ok("metadata-only".into()))
            .unwrap();
        sink.finish_process_names(8).unwrap();
        sink.recorder
            .finish(End {
                shard_id,
                duration_ns: 8,
                exit_code: Some(0),
                signal: None,
                interrupted_by: None,
                reason: EndReason::Completed,
                loss: Loss::default(),
            })
            .unwrap();
        let report = analyze(
            &path,
            Limits {
                max_events: 0,
                max_bytes: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let get = |id| {
            report
                .tracks
                .iter()
                .find(|track| track.id == format!("{shard_id}:{id}"))
                .unwrap()
        };
        assert_eq!(get(early).name, "gemm (PID 42) / image 0000000000000001");
        assert_eq!(get(early).rank, Some(7));
        assert_eq!(
            get(late).name,
            "second-image (PID 42) / image 0000000000000002"
        );
        assert_eq!(get(late).rank, Some(8));
        assert_eq!(get(late).details["process.executable"], "second-image");
        assert!(get(missing).details.contains_key("process.name_error"));
        assert_eq!(get(legacy).name, "process 42 / image 0000000000000004");
        assert!(
            report
                .summary
                .warnings
                .iter()
                .any(|warning| warning.contains("names unavailable for 1 process"))
        );
        assert!(report.counters.is_empty());
        assert!(report.tracks.iter().any(|track| {
            track
                .details
                .get("process.executable")
                .is_some_and(|name| name == "metadata-only")
        }));
        let index = CaptureIndex::open(&path, &directory.path().join("index"), false).unwrap();
        let indexed = index.report().unwrap();
        assert_eq!(
            indexed
                .tracks
                .iter()
                .find(|track| track.id == get(late).id)
                .unwrap()
                .name,
            get(late).name
        );
    }
}
