use rprof_core::{
    Category, CounterSample, CounterValue, Span, Track, TrackKind,
    stream::{
        Attribute, CaptureConfig, Chunk, Collector, End, EndReason, Event, Header, Loss, Payload,
        Record, Shard,
    },
};
use rprof_report::{
    Limits, analyze,
    indexed::{CaptureIndex, FilesRequest, WindowRequest},
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::Path,
};
use uuid::Uuid;

fn write(file: &mut File, record: Record) {
    serde_json::to_writer(&mut *file, &record).unwrap();
    file.write_all(b"\n").unwrap();
}
fn fixture(path: &Path) -> Uuid {
    let id = Uuid::new_v4();
    let mut file = File::create(path).unwrap();
    write(&mut file, Record::Header(Header::new(Uuid::new_v4())));
    write(
        &mut file,
        Record::Shard(Shard {
            shard_id: id,
            parent_shard_id: None,
            hostname: "host".into(),
            boot_id: "boot".into(),
            monotonic_origin_ns: 9007199254740993,
            started_unix_ns: 0,
            command: vec![],
            supervisor_pid: 1,
            collectors: vec![
                Collector {
                    name: "io".into(),
                    version: "test".into(),
                },
                Collector {
                    name: "system".into(),
                    version: "test".into(),
                },
            ],
            config: CaptureConfig {
                chunk_events: 256,
                queue_chunks: 8,
                flush_interval_ms: 100,
            },
        }),
    );
    let mut tracks = vec![
        Track {
            id: 0,
            parent_id: None,
            name: "process".into(),
            rank: Some(7),
            kind: TrackKind::Process { pid: 1 },
        },
        Track {
            id: 1,
            parent_id: Some(0),
            name: "thread".into(),
            rank: Some(7),
            kind: TrackKind::Thread { tid: 2 },
        },
        Track {
            id: 2,
            parent_id: Some(1),
            name: "/data/main".into(),
            rank: Some(7),
            kind: TrackKind::Io {
                resource: "/data/main".into(),
            },
        },
        Track {
            id: 3,
            parent_id: None,
            name: "CPU".into(),
            rank: None,
            kind: TrackKind::System {
                resource: "cpu".into(),
                host_id: "host/boot".into(),
            },
        },
    ];
    tracks.extend((4..154).map(|n| Track {
        id: n,
        parent_id: Some(1),
        name: format!("/data/file-{n}"),
        rank: Some(7),
        kind: TrackKind::Io {
            resource: format!("/data/file-{n}"),
        },
    }));
    write(
        &mut file,
        Record::Chunk(Chunk {
            shard_id: id,
            sequence: 0,
            loss: Loss::default(),
            events: tracks
                .into_iter()
                .map(|t| Event::new(0, Payload::Track(t)))
                .collect(),
        }),
    );
    for i in 1..=1000u64 {
        let at = i * 1_000_000_000;
        let attrs = BTreeMap::from([
            ("io.operation".into(), Attribute::Text("read".into())),
            ("io.file".into(), Attribute::Text("/data/main".into())),
            (
                "io.completed_bytes".into(),
                Attribute::Unsigned(90071992547409930),
            ),
            ("pid".into(), Attribute::Unsigned(1)),
            ("tid".into(), Attribute::Unsigned(2)),
            ("process_instance".into(), Attribute::Unsigned(99)),
        ]);
        let mut events = vec![Event {
            timestamp_ns: at,
            attributes: attrs,
            payload: Payload::Span(Span {
                track_id: 2,
                name: format!("read-{i}"),
                category: Category::Io,
                start_ns: at - 100,
                duration_ns: 100,
                correlation_id: Some(i + 1000),
            }),
        }];
        if i != 501 {
            // preserve a real sampling gap before the high CPU point
            let mut cpu = Event::new(
                at,
                Payload::Counter(CounterSample {
                    track_id: 3,
                    timestamp_ns: at,
                    name: "system.cpu.utilization".into(),
                    value: CounterValue::Float(if i == 502 { 100.0 } else { 20.0 }),
                    unit: "%".into(),
                }),
            );
            cpu.attributes.insert(
                "system.interval_ns".into(),
                Attribute::Unsigned(1_000_000_000),
            );
            cpu.attributes
                .insert("system.sample_index".into(), Attribute::Unsigned(i));
            events.push(cpu);
        }
        if i == 100 {
            events.push(Event::new(
                at,
                Payload::ActivityStart {
                    track_id: 1,
                    name: "unfinished".into(),
                    correlation_id: 77,
                    start_ns: None,
                },
            ));
        }
        if i == 901 {
            events.push(Event::new(
                at,
                Payload::Dependency {
                    from_correlation_id: 7000,
                    to_correlation_id: 1901,
                    relation: "test edge".into(),
                },
            ));
        }
        if i == 1000 {
            events.push(Event::new(
                at,
                Payload::Span(Span {
                    track_id: 0,
                    name: "long crossing span".into(),
                    category: Category::Process,
                    start_ns: 0,
                    duration_ns: at,
                    correlation_id: None,
                }),
            ));
        }
        write(
            &mut file,
            Record::Chunk(Chunk {
                shard_id: id,
                sequence: i,
                loss: Loss::default(),
                events,
            }),
        );
    }
    write(
        &mut file,
        Record::End(End {
            shard_id: id,
            duration_ns: 1_000_000_000_000,
            exit_code: Some(0),
            signal: None,
            interrupted_by: None,
            reason: EndReason::Completed,
            loss: Loss::default(),
        }),
    );
    id
}
fn query(index: &CaptureIndex, start: &str, end: &str) -> WindowRequest {
    WindowRequest {
        snapshot: index.report().unwrap().viewer.unwrap().snapshot,
        domain: "0".into(),
        start_ns: start.into(),
        end_ns: end.into(),
        pixels: 1000,
        tracks: vec![],
        systems: vec![],
        io_scopes: vec![],
        filesystem: "all".into(),
    }
}

#[test]
fn late_windows_extrema_paging_and_cache_reuse_work_without_prefix_retention() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("training.jsonl");
    let id = fixture(&path);
    let offline = analyze(
        &path,
        Limits {
            max_events: 2,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!offline.spans.iter().any(|s| s.name == "read-901"));
    let index = CaptureIndex::open(&path, &dir.path().join("cache"), false).unwrap();
    assert!(!index.reused);
    let metadata = index.report().unwrap();
    assert!(metadata.spans.is_empty());
    assert!(metadata.tracks.iter().all(|t| t.kind != "io"));
    assert_eq!(metadata.summary.omitted_events, "0");
    assert_eq!(
        metadata
            .tracks
            .iter()
            .find(|t| t.id == format!("{id}:1"))
            .unwrap()
            .details["viewer.file_count"],
        "151"
    );
    let mut late = query(&index, "900250000000", "901250000000");
    late.tracks = vec![format!("{id}:2"), format!("{id}:0"), format!("{id}:1")];
    late.io_scopes = vec!["io:0".into()];
    let response = index.window(late.clone(), 20000, 16 * 1024 * 1024).unwrap();
    assert!(response.spans.iter().any(|s| s.name == "read-901"));
    assert!(
        response
            .spans
            .iter()
            .any(|s| s.name == "long crossing span")
    );
    assert!(
        response
            .spans
            .iter()
            .any(|s| s.name == "unfinished" && s.incomplete)
    );
    assert!(
        response
            .dependencies
            .iter()
            .any(|d| d.to == format!("{id}:1901"))
    );
    assert_eq!(
        response.io["io:0"]
            .iter()
            .map(|b| b.read_bytes.parse::<u128>().unwrap())
            .sum::<u128>(),
        90071992547409930
    );
    assert!(response.io["io:0"].len() >= 100);
    let mut all = query(&index, "0", "1000000000000");
    all.pixels = 64;
    all.systems = vec![format!("{id}:3")];
    all.io_scopes = vec!["io:0".into()];
    let overview = index.window(all.clone(), 20000, 16 * 1024 * 1024).unwrap();
    assert!(overview.counters.iter().any(|c| c.value == "100"));
    assert!(
        overview
            .counters
            .iter()
            .any(|c| c.attributes["system.report_gap_before"] == "true")
    );
    assert!(overview.counters.len() < 200);
    assert_eq!(
        overview.io["io:0"]
            .iter()
            .map(|b| b.read_ops.parse::<u64>().unwrap())
            .sum::<u64>(),
        1000
    );
    assert!(overview.io["io:0"].len() < response.io["io:0"].len());
    assert_eq!(
        overview.io["io:0"]
            .iter()
            .map(|b| b.read_bytes.parse::<u128>().unwrap())
            .sum::<u128>(),
        90_071_992_547_409_930_000u128
    );
    let snapshot = metadata.viewer.unwrap().snapshot;
    let page = index
        .files(FilesRequest {
            snapshot: snapshot.clone(),
            thread: format!("{id}:1"),
            offset: 0,
            limit: 100,
        })
        .unwrap();
    assert_eq!(page.total, 151);
    assert_eq!(page.tracks.len(), 100);
    assert_eq!(page.next_offset, Some(100));
    let rest = index
        .files(FilesRequest {
            snapshot,
            thread: format!("{id}:1"),
            offset: 100,
            limit: 100,
        })
        .unwrap();
    assert_eq!(rest.tracks.len(), 51);
    let reused = CaptureIndex::open(&path, &dir.path().join("cache"), false).unwrap();
    assert!(reused.reused);
    let again = reused.window(late, 20000, 16 * 1024 * 1024).unwrap();
    assert_eq!(
        response
            .spans
            .iter()
            .find(|s| s.name == "read-901")
            .unwrap()
            .source_id,
        again
            .spans
            .iter()
            .find(|s| s.name == "read-901")
            .unwrap()
            .source_id
    );
    let limited = index
        .window(
            WindowRequest {
                tracks: vec![format!("{id}:2")],
                ..all
            },
            1,
            1024 * 1024,
        )
        .unwrap();
    assert!(limited.limited);
    assert_eq!(limited.spans.len(), 1);
    let mut invalid = query(&index, "0", "100");
    invalid.domain = "other".into();
    assert!(index.window(invalid, 10, 1024).is_err());
    let original = fs::read(&path).unwrap();
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b" ").unwrap();
    drop(file);
    let rebuilt = CaptureIndex::open(&path, &dir.path().join("cache"), false).unwrap();
    assert!(!rebuilt.reused);
    assert!(rebuilt.report().unwrap().summary.truncated_tail);
    assert_eq!(
        &fs::read(&path).unwrap()[..original.len()],
        original.as_slice()
    );
}

#[test]
fn interval_candidates_keep_nanosecond_precision_and_non_file_io_roots() {
    use rprof_core::{Capture, Metadata, SCHEMA_VERSION};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.json");
    let base = 9_007_199_254_740_993u64;
    let capture = Capture {
        schema_version: SCHEMA_VERSION,
        metadata: Metadata {
            command: vec![],
            started_unix_ns: 0,
            duration_ns: base + 20,
            exit_code: Some(0),
            signal: None,
            collectors: vec!["process".into()],
        },
        tracks: vec![
            Track {
                id: 0,
                parent_id: None,
                name: "import diagnostics".into(),
                rank: None,
                kind: TrackKind::Io {
                    resource: "pytorch-import".into(),
                },
            },
            Track {
                id: 1,
                parent_id: Some(0),
                name: "process".into(),
                rank: None,
                kind: TrackKind::Process { pid: 1 },
            },
        ],
        spans: [0, 4, 8]
            .into_iter()
            .map(|offset| Span {
                track_id: 1,
                name: format!("at-{offset}"),
                category: Category::Process,
                start_ns: base + offset,
                duration_ns: 1,
                correlation_id: None,
            })
            .collect(),
        counters: vec![],
    };
    fs::write(&path, serde_json::to_vec(&capture).unwrap()).unwrap();
    let index = CaptureIndex::open(&path, &dir.path().join("cache"), false).unwrap();
    assert_eq!(
        index.report().unwrap().tracks.len(),
        2,
        "non-file I/O roots must remain in metadata"
    );
    let mut request = query(&index, &(base + 3).to_string(), &(base + 6).to_string());
    request.tracks = vec![format!("{}:1", Uuid::nil())];
    let result = index.window(request, 100, 1024 * 1024).unwrap();
    assert_eq!(result.spans.len(), 1);
    assert_eq!(result.spans[0].name, "at-4");
    assert_eq!(result.spans[0].start_ns, (base + 4).to_string());
}
