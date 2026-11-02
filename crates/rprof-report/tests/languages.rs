use rprof_capture::{Recorder, RecorderConfig};
use rprof_core::{
    Category, Span, Track, TrackKind,
    stream::{Attribute, Collector, End, EndReason, Event, Header, Loss, Payload, Shard},
};
use std::collections::BTreeMap;
use uuid::Uuid;

fn report(entries: &[(&str, u64, u64)]) -> rprof_report::Report {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let id = Uuid::new_v4();
    let config = RecorderConfig::default();
    let mut recorder = Recorder::create(
        &path,
        Header::new(Uuid::new_v4()),
        Shard {
            shard_id: id,
            parent_shard_id: None,
            hostname: "test".into(),
            boot_id: "imported-test".into(),
            monotonic_origin_ns: 0,
            started_unix_ns: 0,
            command: vec![],
            supervisor_pid: 0,
            collectors: vec![Collector {
                name: "pytorch-import".into(),
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
                name: "source lane".into(),
                rank: None,
                kind: TrackKind::Thread { tid: 1 },
            }),
        ))
        .unwrap();
    for (name, start, duration) in entries {
        recorder
            .reliable(Event {
                timestamp_ns: 100,
                attributes: BTreeMap::from([(
                    "pytorch.role".into(),
                    Attribute::Text("operator".into()),
                )]),
                payload: Payload::Span(Span {
                    track_id: 0,
                    name: (*name).into(),
                    category: Category::Pytorch,
                    start_ns: *start,
                    duration_ns: *duration,
                    correlation_id: None,
                }),
            })
            .unwrap();
    }
    recorder
        .finish(End {
            shard_id: id,
            duration_ns: 100,
            exit_code: None,
            signal: None,
            interrupted_by: None,
            reason: EndReason::Completed,
            loss: Loss::default(),
        })
        .unwrap();
    rprof_report::analyze(&path, Default::default()).unwrap()
}
#[test]
fn nested_cpu_self_times_are_exclusive_and_missing_bytes_remain_unknown() {
    let report = report(&[("outer", 0, 10), ("child", 2, 5), ("inner", 3, 2)]);
    assert_eq!(report.provenance.kind, "imported");
    let rows = &report.pytorch.unwrap().activities;
    for (name, expected) in [("outer", "5"), ("child", "3"), ("inner", "2")] {
        let row = rows.iter().find(|r| r.name == name).unwrap();
        assert_eq!(row.self_ns.as_deref(), Some(expected));
        assert!(row.bytes.is_none());
    }
}
#[test]
fn crossing_and_tied_cpu_intervals_do_not_fabricate_self_totals() {
    let rows = report(&[("a", 0, 10), ("b", 5, 10), ("clean", 15, 2)])
        .pytorch
        .unwrap()
        .activities;
    for name in ["a", "b"] {
        assert!(
            rows.iter()
                .find(|r| r.name == name)
                .unwrap()
                .self_ns
                .is_none()
        );
    }
    assert_eq!(
        rows.iter()
            .find(|r| r.name == "clean")
            .unwrap()
            .self_ns
            .as_deref(),
        Some("2")
    );
    let rows = report(&[("a", 0, 10), ("b", 0, 10)])
        .pytorch
        .unwrap()
        .activities;
    assert!(rows.iter().all(|r| r.self_ns.is_none()));
}
