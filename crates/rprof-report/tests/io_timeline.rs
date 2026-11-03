use rprof_capture::{Recorder, RecorderConfig};
use rprof_core::{
    Category, Span, Track, TrackKind,
    stream::{Attribute, Collector, End, EndReason, Event, Header, Loss, Payload, Shard},
};
use rprof_report::{Limits, analyze};
use std::collections::BTreeMap;
use uuid::Uuid;

#[test]
fn completion_bins_preserve_bytes_errors_latencies_and_survive_raw_retention() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("io.jsonl");
    let config = RecorderConfig::default();
    let id = Uuid::new_v4();
    let mut recorder = Recorder::create(
        &path,
        Header::new(Uuid::new_v4()),
        Shard {
            shard_id: id,
            parent_shard_id: None,
            hostname: "host".into(),
            boot_id: "boot".into(),
            monotonic_origin_ns: 9007199254740993,
            started_unix_ns: 0,
            command: vec![],
            supervisor_pid: 1,
            collectors: vec![Collector {
                name: "io".into(),
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
                name: "IO".into(),
                rank: None,
                kind: TrackKind::Process { pid: 1 },
            }),
        ))
        .unwrap();
    for (track_id, parent, kind) in [
        (1, 0, TrackKind::Thread { tid: 9 }),
        (
            2,
            1,
            TrackKind::Io {
                resource: "/data/file".into(),
            },
        ),
    ] {
        recorder
            .reliable(Event::new(
                0,
                Payload::Track(Track {
                    id: track_id,
                    parent_id: Some(parent),
                    name: "detail".into(),
                    rank: Some(3),
                    kind,
                }),
            ))
            .unwrap();
    }
    for (op, end, duration, bytes, result, known) in [
        ("read", 1_500_000, 1_000_000, 40, 40, true),
        (
            "write",
            2_500_000,
            1_000_000,
            9007199254740995,
            9007199254740995,
            true,
        ),
        ("read", 2_700_000, 200_000, 0, -1, true),
        ("read", 4_000_000, 10, 7, 7, false),
        ("fsync", 5_500_000, 500_000, 0, 0, true),
    ] {
        let mut attributes = BTreeMap::from([
            ("io.operation".into(), Attribute::Text(op.into())),
            ("io.file".into(), Attribute::Text("/data/file".into())),
            ("io.completed_bytes".into(), Attribute::Unsigned(bytes)),
            ("io.result".into(), Attribute::Signed(result)),
            ("pid".into(), Attribute::Unsigned(1)),
            ("tid".into(), Attribute::Unsigned(9)),
            ("process_instance".into(), Attribute::Unsigned(111)),
        ]);
        if known {
            for (key, value) in [
                ("kind", "filesystem"),
                ("id", "mount:1"),
                ("type", "ext4"),
                ("mount_point", "/data"),
                ("device", "8:1"),
            ] {
                let value = if key == "mount_point" && end == 1_500_000 {
                    "unknown"
                } else {
                    value
                };
                attributes.insert(
                    format!("io.filesystem.{key}"),
                    Attribute::Text(value.into()),
                );
            }
        }
        recorder
            .emit(Event {
                timestamp_ns: end,
                attributes,
                payload: Payload::Span(Span {
                    track_id: 2,
                    name: op.into(),
                    category: Category::Io,
                    start_ns: end - duration,
                    duration_ns: duration,
                    correlation_id: None,
                }),
            })
            .unwrap();
    }
    recorder
        .finish(End {
            shard_id: id,
            duration_ns: 5_500_000,
            exit_code: Some(0),
            signal: None,
            interrupted_by: None,
            reason: EndReason::Completed,
            loss: Loss::default(),
        })
        .unwrap();
    let full = analyze(&path, Limits::default()).unwrap();
    let limited = analyze(
        &path,
        Limits {
            max_events: 0,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(limited.spans.is_empty());
    assert_eq!(
        serde_json::to_value(&full.io_timeline).unwrap(),
        serde_json::to_value(&limited.io_timeline).unwrap()
    );
    let timeline = full.io_timeline.unwrap();
    assert_eq!(timeline.filesystems.len(), 2);
    assert_eq!(timeline.omitted_events, "0");
    assert_eq!(timeline.scope_omitted_events, "0");
    assert_eq!(timeline.threads.len(), 2);
    for thread in &timeline.threads {
        assert_eq!(thread.rank, Some(3));
        assert_eq!(thread.pid, "1");
        assert_eq!(thread.tid, "9");
        assert_eq!(thread.process_instance, "111");
        assert_eq!(thread.thread_track, format!("{id}:1"));
        let filesystem = timeline
            .filesystems
            .iter()
            .find(|f| f.id == thread.filesystem_id)
            .unwrap();
        assert_eq!(
            serde_json::to_value(&filesystem.bins).unwrap(),
            serde_json::to_value(&thread.bins).unwrap()
        );
    }
    let fs = timeline
        .filesystems
        .iter()
        .find(|f| f.kind == "filesystem")
        .unwrap();
    assert_eq!(fs.filesystem_type, "ext4");
    assert_eq!(fs.mount_point, "/data");
    assert_eq!(fs.bin_width_ns, "1000000");
    assert_eq!(fs.bins.len(), 6);
    assert_eq!(fs.bins[0].read_ops, "0");
    assert_eq!(fs.bins[1].read_bytes, "40");
    assert_eq!(fs.bins[1].read_latency_ns, "1000000");
    assert_eq!(fs.bins[2].write_bytes, "9007199254740995");
    assert_eq!(fs.bins[2].write_ops, "1");
    assert_eq!(fs.bins[2].read_ops, "1");
    assert_eq!(fs.bins[2].errors, "1");
    assert_eq!(fs.bins[2].read_latency_ns, "200000");
    assert_eq!(fs.bins[5].duration_ns, "500000");
    assert_eq!(fs.bins[5].other_ops, "1");
    assert_eq!(fs.bins[5].other_latency_ns, "500000");
    let unknown = timeline
        .filesystems
        .iter()
        .find(|f| f.kind == "unknown")
        .unwrap();
    assert_eq!(unknown.filesystem_type, "unknown");
    assert_eq!(unknown.bins[4].read_bytes, "7");
}
