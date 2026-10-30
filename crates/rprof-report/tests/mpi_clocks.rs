use rprof_core::{
    CounterSample, CounterValue,
    stream::{Attribute, Event, Payload, Record},
};
use rprof_report::{Limits, analyze, write_trace};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

fn capture(path: &Path, peer_calibration: bool) {
    let fixture = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/multitrack.jsonl"),
    )
    .unwrap();
    let source: Vec<Record> = fixture
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let mut records = vec![source[0].clone()];
    for (index, offset) in [(1, 0i64), (7, -10_000_000)] {
        let mut shard = source[index].clone();
        let mut chunk = source[index + 1].clone();
        let end = source[index + 2].clone();
        let origin = if index == 1 { 10_000_000 } else { 20_000_000 };
        if let Record::Shard(s) = &mut shard {
            s.monotonic_origin_ns = origin;
        }
        if index == 1 || peer_calibration {
            if let Record::Chunk(c) = &mut chunk {
                for stage in 0..2 {
                    c.events.push(Event {
                        timestamp_ns: 40_000_000,
                        attributes: BTreeMap::from([
                            ("mpi.record_type".into(), Attribute::Text("clock".into())),
                            ("mpi.clock.group".into(), Attribute::Unsigned(123)),
                            (
                                "mpi.clock.offset_ns".into(),
                                Attribute::Signed(
                                    offset + if index == 7 && stage == 1 { 2 } else { 0 },
                                ),
                            ),
                            ("mpi.clock.stage".into(), Attribute::Unsigned(stage)),
                            (
                                "mpi.clock.local_ns".into(),
                                Attribute::Unsigned(origin + 30_000_000 + stage),
                            ),
                            (
                                "mpi.clock.uncertainty_ns".into(),
                                Attribute::Unsigned(if stage == 0 { 10 } else { 20 }),
                            ),
                        ]),
                        payload: Payload::Counter(CounterSample {
                            track_id: 0,
                            timestamp_ns: 40_000_000,
                            name: "mpi.clock.uncertainty_ns".into(),
                            value: CounterValue::Integer(10),
                            unit: "ns".into(),
                        }),
                    });
                }
            }
        }
        records.extend([shard, chunk, end]);
    }
    let mut out = fs::File::create_new(path).unwrap();
    for record in records {
        serde_json::to_writer(&mut out, &record).unwrap();
        out.write_all(b"\n").unwrap();
    }
}

#[test]
fn explicit_calibration_aligns_hosts_and_reports_offset_drift() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("aligned");
    capture(&path, true);
    let report = analyze(&path, Limits::default()).unwrap();
    assert_eq!(report.domains.len(), 1);
    let calibration = report.domains[0].calibration.as_ref().unwrap();
    assert_eq!(calibration.max_drift_ns, "2");
    assert_eq!(calibration.uncertainty_ns, "32");
    assert!(calibration.complete);
    assert!(report.shards.iter().all(|s| s.origin_ns == "0"));
    let mut trace = Vec::new();
    write_trace(&path, &mut trace, None).unwrap();
    assert!(serde_json::from_slice::<serde_json::Value>(&trace).unwrap()["traceEvents"].is_array());
}

#[test]
fn missing_calibration_never_implies_cross_host_synchronization() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unaligned");
    capture(&path, false);
    let report = analyze(&path, Limits::default()).unwrap();
    assert_eq!(report.domains.len(), 2);
    assert!(write_trace(&path, Vec::new(), None).is_err());
}
