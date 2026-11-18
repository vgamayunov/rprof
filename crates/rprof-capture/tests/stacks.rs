use rprof_capture::{Recorder, RecorderConfig};
use rprof_core::{
    Track, TrackKind,
    stream::{
        Collector, End, EndReason, Event, FrameLanguage, Header, Loss, Payload, Shard, StackFrame,
        StackSample, StackStatus, StackTrace,
    },
};
use uuid::Uuid;

fn trace(id: u64) -> Event {
    Event::new(
        1,
        Payload::StackTrace(StackTrace {
            id,
            status: StackStatus::Unknown,
            frames: vec![StackFrame {
                key: "module:function".into(),
                name: "function".into(),
                module: "module".into(),
                language: FrameLanguage::Native,
                address: u64::MAX - 4096,
            }],
        }),
    )
}
fn capture(events: Vec<Event>, version: u32) -> (tempfile::TempDir, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("capture");
    let config = RecorderConfig::default();
    let id = Uuid::new_v4();
    let mut header = Header::new(Uuid::new_v4());
    header.schema_version = version;
    let mut recorder = Recorder::create(
        &path,
        header,
        Shard {
            shard_id: id,
            parent_shard_id: None,
            hostname: "host".into(),
            boot_id: "boot".into(),
            monotonic_origin_ns: 1,
            started_unix_ns: 0,
            command: vec![],
            supervisor_pid: 1,
            collectors: vec![Collector {
                name: "stacks".into(),
                version: "test".into(),
            }],
            config: config.metadata(),
        },
        config,
    )
    .unwrap();
    recorder
        .reliable(Event::new(
            0,
            Payload::Track(Track {
                id: 0,
                parent_id: None,
                name: "thread".into(),
                rank: None,
                kind: TrackKind::Thread { tid: 1 },
            }),
        ))
        .unwrap();
    assert!(recorder.emit(trace(99)).is_err());
    for event in events {
        recorder.reliable(event).unwrap();
    }
    recorder
        .finish(End {
            shard_id: id,
            duration_ns: 10,
            exit_code: Some(0),
            signal: None,
            interrupted_by: None,
            reason: EndReason::Completed,
            loss: Loss::default(),
        })
        .unwrap();
    (directory, path)
}
fn read(path: &std::path::Path) -> anyhow::Result<rprof_capture::Summary> {
    rprof_capture::summarize(std::io::BufReader::new(std::fs::File::open(path)?))
}
fn sample(id: Option<u64>, period: u64) -> Event {
    Event::new(
        2,
        Payload::StackSample(StackSample {
            track_id: 0,
            timestamp_ns: 2,
            stack_id: id,
            period_ns: period,
        }),
    )
}
#[test]
fn stacks_are_versioned_and_sample_references_and_periods_are_validated() {
    let (_directory, path) = capture(vec![trace(1), sample(Some(1), 7), sample(None, 9)], 3);
    assert!(read(&path).unwrap().is_complete());
    for events in [
        vec![sample(Some(1), 7)],
        vec![trace(1), trace(1)],
        vec![trace(1), sample(Some(1), 0)],
    ] {
        let (_directory, path) = capture(events, 3);
        assert!(read(&path).is_err());
    }
    let (_directory, path) = capture(vec![trace(1)], 2);
    assert!(
        read(&path)
            .unwrap_err()
            .to_string()
            .contains("invalid frame")
    );
    let (_directory, path) = capture(vec![], 2);
    assert!(read(&path).unwrap().is_complete());
}
