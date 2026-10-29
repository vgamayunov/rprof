use std::{
    fs,
    io::{BufReader, Cursor},
    path::Path,
};

use rprof_capture::{MAX_FRAME_BYTES, Recorder, RecorderConfig, merge, summarize};
use rprof_core::{
    Category, CounterSample, CounterValue, Span, Track, TrackKind,
    stream::{Chunk, Collector, End, EndReason, Event, Header, Loss, Payload, Record, Shard},
};
use uuid::Uuid;

fn shard(id: Uuid) -> Shard {
    Shard {
        shard_id: id,
        parent_shard_id: None,
        hostname: "fixture-host".into(),
        boot_id: "fixture-boot".into(),
        monotonic_origin_ns: 1_000,
        started_unix_ns: 42,
        command: vec!["fixture".into()],
        supervisor_pid: 1,
        collectors: vec![Collector {
            name: "fixture".into(),
            version: "1".into(),
        }],
        config: RecorderConfig::default().metadata(),
    }
}

fn track() -> Event {
    Event::new(
        0,
        Payload::Track(Track {
            id: 0,
            parent_id: None,
            name: "main".into(),
            rank: None,
            kind: TrackKind::Process { pid: 10 },
        }),
    )
}

fn end(id: Uuid, duration: u64) -> End {
    End {
        shard_id: id,
        duration_ns: duration,
        exit_code: Some(0),
        signal: None,
        interrupted_by: None,
        reason: EndReason::Completed,
        loss: Loss::default(),
    }
}

fn records(session: Uuid, id: Uuid) -> Vec<Record> {
    vec![
        Record::Header(Header::new(session)),
        Record::Shard(shard(id)),
        Record::Chunk(Chunk {
            shard_id: id,
            sequence: 0,
            loss: Loss::default(),
            events: vec![track()],
        }),
        Record::Chunk(Chunk {
            shard_id: id,
            sequence: 1,
            loss: Loss::default(),
            events: vec![
                Event::new(
                    20,
                    Payload::Span(Span {
                        track_id: 0,
                        name: "inner".into(),
                        category: Category::Process,
                        start_ns: 10,
                        duration_ns: 10,
                        correlation_id: Some(0),
                    }),
                ),
                Event::new(
                    30,
                    Payload::Span(Span {
                        track_id: 0,
                        name: "outer".into(),
                        category: Category::Process,
                        start_ns: 0,
                        duration_ns: 30,
                        correlation_id: Some(1),
                    }),
                ),
                Event::new(
                    30,
                    Payload::Dependency {
                        from_correlation_id: 0,
                        to_correlation_id: 1,
                        relation: "nested".into(),
                    },
                ),
            ],
        }),
        Record::End(end(id, 30)),
    ]
}

fn bytes(records: &[Record]) -> Vec<u8> {
    records
        .iter()
        .flat_map(|r| {
            let mut v = serde_json::to_vec(r).unwrap();
            v.push(b'\n');
            v
        })
        .collect()
}

fn save(path: &Path, records: &[Record]) {
    fs::write(path, bytes(records)).unwrap();
}

#[test]
fn streaming_load_accounts_for_every_event() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("load");
    let id = Uuid::new_v4();
    let config = RecorderConfig {
        chunk_events: 16,
        queue_chunks: 1,
        ..Default::default()
    };
    let mut metadata = shard(id);
    metadata.config = config.metadata();
    let mut writer =
        Recorder::create(&path, Header::new(Uuid::new_v4()), metadata, config).unwrap();
    writer.reliable(track()).unwrap();
    const COUNT: u64 = 20_000;
    for timestamp_ns in 1..=COUNT {
        writer
            .emit(Event::new(
                timestamp_ns,
                Payload::Counter(CounterSample {
                    track_id: 0,
                    timestamp_ns,
                    name: "cycles".into(),
                    value: CounterValue::Integer(u64::MAX),
                    unit: "count".into(),
                }),
            ))
            .unwrap();
    }
    writer.finish(end(id, COUNT)).unwrap();
    let summary = summarize(BufReader::new(fs::File::open(path).unwrap())).unwrap();
    assert!(summary.is_complete());
    assert_eq!(summary.shards[&id].tracks, 1);
    assert_eq!(
        summary.shards[&id].counters + summary.shards[&id].loss.total(),
        COUNT
    );
}

#[test]
fn recover_every_truncated_footer_prefix() {
    let id = Uuid::new_v4();
    let records = records(Uuid::new_v4(), id);
    let complete = bytes(&records);
    let footer_offset = bytes(&records[..4]).len();
    for cutoff in footer_offset..complete.len() {
        let summary = summarize(Cursor::new(&complete[..cutoff])).unwrap();
        assert!(!summary.is_complete(), "cutoff {cutoff}");
        assert_eq!(summary.shards[&id].spans, 2);
        assert!(summary.shards[&id].end.is_none());
        assert_eq!(summary.truncated_tail, cutoff > footer_offset);
    }
    assert!(summarize(Cursor::new(complete)).unwrap().is_complete());
}

#[test]
fn truncated_chunk_drops_only_that_chunk_and_retains_loss_checkpoint() {
    let id = Uuid::new_v4();
    let mut records = records(Uuid::new_v4(), id);
    if let Record::Chunk(chunk) = &mut records[2] {
        chunk.loss.queue_full = 7;
    }
    let mut data = bytes(&records[..3]);
    data.extend_from_slice(b"{\"record\":\"chunk\",\"data\":");
    let summary = summarize(Cursor::new(data)).unwrap();
    assert!(summary.truncated_tail);
    assert_eq!(summary.shards[&id].loss.queue_full, 7);
    assert_eq!(summary.shards[&id].tracks, 1);
    assert_eq!(summary.shards[&id].spans, 0);
}

#[test]
fn corruption_and_unknown_versions_are_not_silently_recovered() {
    let mut records = records(Uuid::new_v4(), Uuid::new_v4());
    let mut data = bytes(&records[..2]);
    data.extend_from_slice(b"{broken}\n");
    assert!(
        summarize(Cursor::new(data))
            .unwrap_err()
            .to_string()
            .contains("invalid JSON")
    );
    if let Record::Header(header) = &mut records[0] {
        header.schema_version = 999;
    }
    let error = summarize(Cursor::new(bytes(&records))).unwrap_err();
    assert!(format!("{error:#}").contains("unsupported schema version"));
}

#[test]
fn frame_size_is_bounded_before_parsing() {
    let mut data = bytes(&records(Uuid::new_v4(), Uuid::new_v4())[..2]);
    data.extend(vec![b' '; MAX_FRAME_BYTES + 1]);
    assert!(
        summarize(Cursor::new(data))
            .unwrap_err()
            .to_string()
            .contains("frame exceeds")
    );
}

#[test]
fn validate_sequence_track_references_and_observation_order() {
    let original = records(Uuid::new_v4(), Uuid::new_v4());
    // Nested spans deliberately have descending starts but ascending observation times.
    assert!(summarize(Cursor::new(bytes(&original))).is_ok());
    for case in 0..5 {
        let mut mutated = original.clone();
        if let Record::Chunk(chunk) = &mut mutated[3] {
            match case {
                0 => chunk.sequence = 99,
                1 => chunk.events[1].timestamp_ns = 1,
                2 => {
                    if let Payload::Span(span) = &mut chunk.events[0].payload {
                        span.track_id = 99;
                    }
                }
                3 => {
                    if let Payload::Span(span) = &mut chunk.events[0].payload {
                        span.duration_ns = u64::MAX;
                    }
                }
                4 => chunk.events.push(track()),
                _ => unreachable!(),
            }
        }
        assert!(
            summarize(Cursor::new(bytes(&mutated))).is_err(),
            "case {case}"
        );
    }
}

#[test]
fn merge_is_deterministic_and_preserves_namespaces_and_clocks() {
    let dir = tempfile::tempdir().unwrap();
    let session = Uuid::new_v4();
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    let first = dir.path().join("a");
    let second = dir.path().join("b");
    save(&first, &records(session, a));
    let mut other = records(session, b);
    if let Record::Shard(shard) = &mut other[1] {
        shard.hostname = "other-host".into();
        shard.monotonic_origin_ns = 999_999;
    }
    save(&second, &other);
    let merged = dir.path().join("merged");
    let reversed = dir.path().join("reversed");
    let summary = merge(&[first.clone(), second.clone()], &merged).unwrap();
    merge(&[second, first], &reversed).unwrap();
    assert_eq!(fs::read(merged).unwrap(), fs::read(reversed).unwrap());
    assert_eq!(summary.shards.len(), 2);
    assert_eq!(summary.shards[&a].tracks, 1);
    assert_eq!(summary.shards[&b].tracks, 1);
    assert_eq!(summary.shards[&b].shard.monotonic_origin_ns, 999_999);
    assert_eq!(summary.shards[&b].observed_ns, 30);
    assert!(summary.is_complete());
}

#[test]
fn merge_rejects_duplicate_shards_and_unrelated_sessions_without_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    let other = dir.path().join("other");
    let output = dir.path().join("output");
    save(&input, &records(Uuid::new_v4(), Uuid::new_v4()));
    save(&other, &records(Uuid::new_v4(), Uuid::new_v4()));
    assert!(merge(&[input.clone(), input.clone()], &output).is_err());
    assert!(!output.exists());
    assert!(merge(&[input.clone(), other], &output).is_err());
    assert!(!output.exists());
    fs::write(&output, "existing user data").unwrap();
    assert!(merge(&[input], &output).is_err());
    assert_eq!(fs::read_to_string(output).unwrap(), "existing user data");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
}

#[test]
fn merge_retains_recovery_provenance_without_inventing_footers() {
    let dir = tempfile::tempdir().unwrap();
    let id = Uuid::new_v4();
    let input = dir.path().join("input");
    let output = dir.path().join("output");
    let mut data = bytes(&records(Uuid::new_v4(), id)[..4]);
    data.extend_from_slice(b"{\"record\":");
    fs::write(&input, data).unwrap();
    let summary = merge(&[input], &output).unwrap();
    assert!(!summary.is_complete());
    assert!(!summary.truncated_tail);
    assert_eq!(summary.header.recovered_inputs, 1);
    assert!(summary.shards[&id].end.is_none());
    let again = merge(&[output], &dir.path().join("again")).unwrap();
    assert_eq!(again.header.recovered_inputs, 1);
}

#[test]
fn oversized_data_is_counted_and_nonfinite_counters_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("output");
    let id = Uuid::new_v4();
    let mut writer = Recorder::create(
        &path,
        Header::new(Uuid::new_v4()),
        shard(id),
        RecorderConfig::default(),
    )
    .unwrap();
    writer.reliable(track()).unwrap();
    let counter = |name, value| {
        Event::new(
            1,
            Payload::Counter(CounterSample {
                track_id: 0,
                timestamp_ns: 1,
                name,
                value,
                unit: "count".into(),
            }),
        )
    };
    assert!(
        !writer
            .emit(counter("x".repeat(100_000), CounterValue::Integer(1)))
            .unwrap()
    );
    assert!(
        writer
            .emit(counter("bad".into(), CounterValue::Float(f64::NAN)))
            .is_err()
    );
    writer.finish(end(id, 1)).unwrap();
    let summary = summarize(BufReader::new(fs::File::open(path).unwrap())).unwrap();
    assert_eq!(summary.shards[&id].loss.oversized, 1);
    assert!(summary.is_complete());
}
