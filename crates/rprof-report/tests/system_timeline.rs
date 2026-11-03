use rprof_capture::{Recorder, RecorderConfig};
use rprof_core::{
    Category, CounterSample, CounterValue, Span, Track, TrackKind,
    stream::{Attribute, Collector, End, EndReason, Event, Header, Loss, Payload, Shard},
};
use rprof_report::{Limits, analyze};
use uuid::Uuid;

#[test]
fn host_cpu_memory_and_children_survive_a_saturated_raw_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("training.jsonl");
    let shard_id = Uuid::new_v4();
    let config = RecorderConfig::default();
    let mut recorder = Recorder::create(
        &path,
        Header::new(Uuid::new_v4()),
        Shard {
            shard_id,
            parent_shard_id: None,
            hostname: "host".into(),
            boot_id: "boot".into(),
            monotonic_origin_ns: 9007199254740993,
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
    recorder
        .reliable(Event::new(
            0,
            Payload::Track(Track {
                id: 0,
                parent_id: None,
                name: "training".into(),
                rank: None,
                kind: TrackKind::Process { pid: 1 },
            }),
        ))
        .unwrap();
    for (id, resource, parent) in [
        (1, "cpu", None),
        (2, "memory", None),
        (3, "cpu:0", Some(1)),
        (4, "node:0", Some(2)),
    ] {
        recorder
            .reliable(Event::new(
                0,
                Payload::Track(Track {
                    id,
                    parent_id: parent,
                    name: resource.into(),
                    rank: None,
                    kind: TrackKind::System {
                        resource: resource.into(),
                        host_id: "host/boot".into(),
                    },
                }),
            ))
            .unwrap();
    }
    for i in 1..=25u64 {
        let at = i * 1_000_000_000;
        recorder
            .emit(Event::new(
                at,
                Payload::Span(Span {
                    track_id: 0,
                    name: "busy I/O".into(),
                    category: Category::Io,
                    start_ns: at - 1000,
                    duration_ns: 1000,
                    correlation_id: None,
                }),
            ))
            .unwrap();
        for id in 1..=4 {
            let memory = id % 2 == 0;
            let mut event = Event::new(
                at,
                Payload::Counter(CounterSample {
                    track_id: id,
                    timestamp_ns: at,
                    name: if memory {
                        "system.memory.used"
                    } else {
                        "system.cpu.utilization"
                    }
                    .into(),
                    value: if memory {
                        CounterValue::Integer(9007199254740993 + i)
                    } else {
                        CounterValue::Float(i as f64)
                    },
                    unit: if memory { "bytes" } else { "%" }.into(),
                }),
            );
            event
                .attributes
                .insert("system.sample_index".into(), Attribute::Unsigned(i));
            event.attributes.insert(
                "system.interval_ns".into(),
                Attribute::Unsigned(1_000_000_000),
            );
            recorder.emit(event).unwrap();
        }
    }
    recorder
        .finish(End {
            shard_id,
            duration_ns: 25_000_000_000,
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
            max_events: 1,
            max_bytes: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(report.spans.is_empty());
    assert_eq!(report.counters.len(), 100);
    assert_eq!(report.summary.omitted_events, "25");
    for id in 1..=4 {
        let samples: Vec<_> = report
            .counters
            .iter()
            .filter(|c| c.track == format!("{shard_id}:{id}"))
            .collect();
        assert_eq!(samples.len(), 25);
        assert_eq!(samples.first().unwrap().timestamp_ns, "1000000000");
        assert_eq!(samples.last().unwrap().timestamp_ns, "25000000000");
        assert!(
            samples
                .iter()
                .all(|c| c.attributes["system.report_gap_before"] == "false")
        );
        if id % 2 == 0 {
            assert_eq!(samples.last().unwrap().value, "9007199254741018");
        }
    }
}
