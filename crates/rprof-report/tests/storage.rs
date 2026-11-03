use rprof_capture::{Recorder, RecorderConfig};
use rprof_core::{
    CounterSample, CounterValue, Track, TrackKind,
    stream::{Attribute, Collector, End, EndReason, Event, Header, Loss, Payload, Shard},
};
use rprof_report::{
    Limits, analyze,
    indexed::{CaptureIndex, WindowRequest},
};
use std::collections::BTreeMap;
use uuid::Uuid;

#[test]
fn storage_series_keep_full_windows_exact_deltas_and_independent_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("storage.jsonl");
    let id = Uuid::new_v4();
    let config = RecorderConfig::default();
    let mut recorder = Recorder::create(
        &path,
        Header::new(Uuid::new_v4()),
        Shard {
            shard_id: id,
            parent_shard_id: None,
            hostname: "host".into(),
            boot_id: "boot".into(),
            monotonic_origin_ns: 0,
            started_unix_ns: 0,
            command: vec![],
            supervisor_pid: 1,
            collectors: vec![Collector {
                name: "storage".into(),
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
                id: 1,
                parent_id: None,
                name: "NFS".into(),
                rank: None,
                kind: TrackKind::Storage {
                    backend: "nfs".into(),
                    resource: "0:57:server:/data".into(),
                    host_id: "host/boot".into(),
                    filesystem: "nfs4 server:/data".into(),
                    mount_point: "/data".into(),
                },
            }),
        ))
        .unwrap();
    for i in 1..=20u64 {
        if i == 10 {
            continue;
        }
        for (name, unit, value) in [
            ("read.bytes_per_second", "B/s", 1048576.0),
            ("write.bytes_per_second", "B/s", 2097152.0),
            ("read.operations_per_second", "ops/s", 1.0),
            ("write.operations_per_second", "ops/s", 4.0),
            ("read.latency_ms", "ms", 10.0),
            ("write.latency_ms", "ms", 5.0),
        ] {
            let mut attributes = BTreeMap::from([
                (
                    "storage.interval_ns".into(),
                    Attribute::Unsigned(1_000_000_000),
                ),
                (
                    "storage.window_ns".into(),
                    Attribute::Unsigned(1_000_000_000),
                ),
                ("storage.sample_index".into(), Attribute::Unsigned(i)),
            ]);
            for (key, value) in [
                ("read_bytes", "1048576"),
                ("write_bytes", "2097152"),
                ("read_ops", "1"),
                ("write_ops", "4"),
                ("read_latency_ns", "10000000"),
                ("write_latency_ns", "20000000"),
            ] {
                attributes.insert(format!("storage.{key}"), Attribute::Text(value.into()));
            }
            recorder
                .reliable(Event {
                    timestamp_ns: i * 1_000_000_000,
                    attributes,
                    payload: Payload::Counter(CounterSample {
                        track_id: 1,
                        timestamp_ns: i * 1_000_000_000,
                        name: format!("storage.{name}"),
                        value: CounterValue::Float(value),
                        unit: unit.into(),
                    }),
                })
                .unwrap();
        }
    }
    recorder
        .finish(End {
            shard_id: id,
            duration_ns: 20_000_000_000,
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
    assert_eq!(report.counters.len(), 19 * 6);
    assert_eq!(
        report
            .counters
            .iter()
            .filter(|c| c.attributes["storage.report_gap_before"] == "true")
            .count(),
        6
    );
    assert_eq!(report.tracks[0].details["storage.backend"], "nfs");
    let index = CaptureIndex::open(&path, &dir.path().join("index"), false).unwrap();
    let response = index
        .window(
            WindowRequest {
                snapshot: index.report().unwrap().viewer.unwrap().snapshot,
                domain: "0".into(),
                start_ns: "0".into(),
                end_ns: "20000000000".into(),
                pixels: 1000,
                tracks: vec![],
                systems: vec![format!("{id}:1")],
                io_scopes: vec![],
                filesystem: "all".into(),
            },
            1000,
            16 * 1024 * 1024,
        )
        .unwrap();
    assert_eq!(response.counters.len(), 19 * 6);
    assert_eq!(
        response
            .counters
            .iter()
            .filter(|c| c.attributes["storage.report_gap_before"] == "true")
            .count(),
        6
    );
    assert!(
        response
            .counters
            .iter()
            .all(|c| c.attributes["storage.read_bytes"] == "1048576")
    );
}
