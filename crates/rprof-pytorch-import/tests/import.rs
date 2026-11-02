use std::{fs, io::BufReader, path::Path};

use rprof_capture::Reader;
use rprof_core::{
    Category, CounterValue,
    stream::{Attribute, Payload, Record},
};
use rprof_pytorch_import::import;

fn records(path: &Path) -> Vec<Record> {
    let mut reader = Reader::new(BufReader::new(fs::File::open(path).unwrap()));
    let mut records = Vec::new();
    while let Some(record) = reader.next_record().unwrap() {
        records.push(record);
    }
    records
}

#[test]
fn legacy_kineto_flows_cross_pid_namespaces_and_api_copies_remain_on_cpu() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("trace.json");
    let output = dir.path().join("capture");
    fs::write(&input,r#"{"distributedInfo":{"rank":1,"backend":"gloo"},"traceEvents":[
      {"ph":"X","cat":"cuda_runtime","name":"cudaMemcpyAsync","pid":42,"tid":9,"ts":10,"dur":2},
      {"ph":"X","cat":"gpu_memcpy","name":"Memcpy HtoD","pid":0,"tid":7,"ts":11,"dur":1,"args":{"device":0,"stream":7,"context":1,"bytes":64}},
      {"ph":"X","cat":"gpu_user_annotation","name":"GPU range","pid":0,"tid":7,"ts":10,"dur":5},
      {"ph":"f","cat":"ac2g","name":"ac2g","id":123,"pid":0,"tid":7,"ts":11},
      {"ph":"s","cat":"ac2g","name":"ac2g","id":123,"pid":42,"tid":9,"ts":10}
    ]}"#).unwrap();
    let result = import(&input, &output, None).unwrap();
    assert_eq!(result.unresolved_flows, 0);
    let events: Vec<_> = records(&output)
        .into_iter()
        .filter_map(|r| {
            if let Record::Chunk(c) = r {
                Some(c.events)
            } else {
                None
            }
        })
        .flatten()
        .collect();
    let spans: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let Payload::Span(s) = &e.payload {
                Some((e, s))
            } else {
                None
            }
        })
        .collect();
    assert!(matches!(spans[0].1.category, Category::Pytorch));
    assert!(
        matches!(&spans[0].0.attributes["pytorch.role"],Attribute::Text(v) if v=="runtime_api")
    );
    assert!(matches!(spans[2].1.category, Category::Cuda));
    assert!(matches!(&spans[2].0.attributes["pytorch.role"],Attribute::Text(v) if v=="gpu_range"));
    assert!(events.iter().any(|e|matches!(&e.payload,Payload::Dependency{from_correlation_id,to_correlation_id,..} if Some(*from_correlation_id)==spans[0].1.correlation_id&&Some(*to_correlation_id)==spans[1].1.correlation_id)));
    assert!(
        events
            .iter()
            .filter_map(|e| if let Payload::Track(t) = &e.payload {
                Some(t)
            } else {
                None
            })
            .all(|t| t.rank == Some(1))
    );
}

#[test]
fn fixture_preserves_exact_time_gpu_and_explicit_flow() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.rprof");
    let summary = import(
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/kineto-mini.json"
        )),
        &output,
        None,
    )
    .unwrap();
    assert_eq!(summary.source_records, 7);
    assert_eq!(summary.spans, 2);
    assert_eq!(summary.unresolved_flows, 0);
    let records = records(&output);
    let events: Vec<_> = records
        .iter()
        .filter_map(|r| {
            if let Record::Chunk(c) = r {
                Some(c.events.as_slice())
            } else {
                None
            }
        })
        .flatten()
        .collect();
    let spans: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let Payload::Span(s) = &e.payload {
                Some((e, s))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(spans[0].1.start_ns, 0);
    assert_eq!(spans[0].1.duration_ns, 3250);
    assert!(matches!(spans[0].1.category, Category::Pytorch));
    assert_eq!(spans[1].1.start_ns, 1875);
    assert!(matches!(spans[1].1.category, Category::Cuda));
    assert!(
        matches!(&spans[0].0.attributes["pytorch.external_id"], Attribute::Text(s) if s == "12345678901234567890")
    );
    let deps: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let Payload::Dependency {
                from_correlation_id,
                to_correlation_id,
                ..
            } = &e.payload
            {
                Some((*from_correlation_id, *to_correlation_id))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        deps,
        vec![(
            spans[0].1.correlation_id.unwrap(),
            spans[1].1.correlation_id.unwrap()
        )]
    );
    assert!(events.iter().any(|e| matches!(&e.payload, Payload::Counter(c) if c.name == "memory.allocated" && matches!(c.value, CounterValue::Integer(2048)))));
    assert!(events.iter().any(|e| matches!(&e.payload, Payload::Counter(c) if c.name == "allocation" && c.unit == "marker")));
}

#[test]
fn nested_slices_incomplete_and_invalid_durations() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("trace.json");
    let out = dir.path().join("out");
    fs::write(&input, r#"[{"ph":"B","pid":"worker","tid":"loop","name":"outer","ts":-2},{"ph":"B","pid":"worker","tid":"loop","name":"inner","ts":-1},{"ph":"E","pid":"worker","tid":"loop","ts":1},{"ph":"E","pid":"worker","tid":"loop","ts":2},{"ph":"E","pid":"worker","tid":"loop","ts":3},{"ph":"B","pid":"worker","tid":"loop","name":"open","ts":4},{"ph":"X","pid":"worker","tid":"loop","name":"bad","ts":2,"dur":-3},{"ph":"Z","ts":0}]"#).unwrap();
    let s = import(&input, &out, None).unwrap();
    assert_eq!(s.spans, 2);
    assert_eq!(s.unmatched_endpoints, 2);
    assert_eq!(s.skipped_records, 2);
    let records = records(&out);
    let events: Vec<_> = records
        .iter()
        .filter_map(|r| {
            if let Record::Chunk(c) = r {
                Some(c.events.as_slice())
            } else {
                None
            }
        })
        .flatten()
        .collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(&e.payload, Payload::ActivityStart { name, .. } if name == "open"))
    );
    assert!(events.iter().any(|e| matches!(&e.payload, Payload::Track(t) if t.name.contains("source pid worker") && matches!(t.kind, rprof_core::TrackKind::Thread { tid: u64::MAX }))));
}

#[test]
fn invalid_json_never_has_success_footer() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("trace.json");
    let out = dir.path().join("out");
    fs::write(
        &input,
        "{\"traceEvents\":[{\"ph\":\"X\",\"ts\":1,\"dur\":2},",
    )
    .unwrap();
    assert!(import(&input, &out, None).is_err());
    assert!(!out.exists());
}

#[test]
fn out_of_order_flows_resolve_by_source_time_and_ambiguous_endpoints_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("trace.json");
    let out = dir.path().join("out");
    fs::write(&input, r#"{"traceEvents":[{"ph":"f","cat":"flow","name":"edge","id":"99","pid":1,"tid":2,"ts":12},{"ph":"X","cat":"kernel","name":"gpu","pid":1,"tid":2,"ts":11,"dur":2,"args":{"device":1,"stream":2}},{"ph":"s","cat":"flow","name":"edge","id":"99","pid":1,"tid":1,"ts":2},{"ph":"X","cat":"cpu_op","name":"cpu","pid":1,"tid":1,"ts":1,"dur":3}]}"#).unwrap();
    let s = import(&input, &out, None).unwrap();
    assert_eq!(s.unresolved_flows, 0);
    assert_eq!(s.spans, 2);
    let events: Vec<_> = records(&out)
        .into_iter()
        .filter_map(|r| {
            if let Record::Chunk(c) = r {
                Some(c.events)
            } else {
                None
            }
        })
        .flatten()
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.payload, Payload::Dependency { .. }))
            .count(),
        1
    );

    let ambiguous = dir.path().join("ambiguous.json");
    fs::write(&ambiguous, r#"[{"ph":"X","name":"a","pid":1,"tid":1,"ts":0,"dur":3},{"ph":"X","name":"b","pid":1,"tid":1,"ts":0,"dur":3},{"ph":"s","name":"flow","id":1,"pid":1,"tid":1,"ts":1}]"#).unwrap();
    let s = import(&ambiguous, &dir.path().join("other"), None).unwrap();
    assert_eq!(s.ambiguous_flows, 1);
    assert_eq!(s.unresolved_flows, 1);
}

#[test]
fn hostile_labels_are_bounded_and_offline_writer_reports_no_loss() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("trace.json");
    let output = dir.path().join("capture");
    let trace = serde_json::json!({"traceEvents":[{
        "ph":"X", "name":"\u{0001}".repeat(100_000), "cat":"cpu_op", "pid":"hostile", "tid":"lane",
        "ts":0, "dur":1, "args":{"Call stack":"z".repeat(100_000), "Input Dims":[1,2]}
    }], "deviceProperties":"y".repeat(100_000)});
    fs::write(&input, trace.to_string()).unwrap();
    let s = import(&input, &output, None).unwrap();
    assert_eq!(s.writer_loss, 0);
    assert_eq!(s.spans, 1);
    let events: Vec<_> = records(&output)
        .into_iter()
        .filter_map(|r| {
            if let Record::Chunk(c) = r {
                Some(c.events)
            } else {
                None
            }
        })
        .flatten()
        .collect();
    assert!(events.iter().any(|e| matches!(
        e.attributes.get("pytorch.args_truncated"),
        Some(Attribute::Bool(true))
    )));
    assert!(events.iter().any(|e| matches!(
        e.attributes.get("pytorch.source_loss_known"),
        Some(Attribute::Bool(false))
    )));
    assert!(events.iter().any(|e| matches!(&e.payload, Payload::Span(s) if s.name.len() <= 2048 && !s.name.contains('\u{0001}'))));
}

#[test]
fn real_torch_cpu_export_imports_without_fabricated_gpu_events() {
    let dir = tempfile::tempdir().unwrap();
    let s = import(
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/torch-real-cpu.json"
        )),
        &dir.path().join("capture"),
        None,
    )
    .unwrap();
    assert!(s.source_records > 10);
    assert!(s.spans > 0);
    let events: Vec<_> = records(&dir.path().join("capture"))
        .into_iter()
        .filter_map(|r| {
            if let Record::Chunk(c) = r {
                Some(c.events)
            } else {
                None
            }
        })
        .flatten()
        .collect();
    assert!(
        !events.iter().any(
            |e| matches!(&e.payload, Payload::Span(s) if matches!(s.category, Category::Cuda))
        )
    );
}

#[test]
fn metadata_clock_zero_and_late_thread_name_do_not_shift_real_activity() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("source");
    let output = dir.path().join("out");
    fs::write(&input, r#"{"schemaVersion":2,"ERROR":"source warning","WARNING":"incomplete","profile_memory":true,"record_shapes":true,"with_stack":false,"with_flops":false,"rprof_custom":{"tag":"yes"},"traceEvents":[{"ph":"M","name":"process_name","pid":4,"tid":0,"ts":0,"args":{"name":"training"}},{"ph":"X","name":"step","cat":"cpu_op","pid":4,"tid":9,"ts":1700000000000000,"dur":1},{"ph":"M","name":"thread_name","pid":4,"tid":9,"ts":0,"args":{"name":"worker"}},{"ph":"Z","ts":-999999999999999}]}"#).unwrap();
    let s = import(&input, &output, None).unwrap();
    assert_eq!(s.spans, 1);
    let all = records(&output);
    assert!(matches!(&all[1], Record::Shard(shard) if shard.boot_id.starts_with("pytorch-trace:")));
    let events: Vec<_> = all
        .into_iter()
        .filter_map(|r| {
            if let Record::Chunk(c) = r {
                Some(c.events)
            } else {
                None
            }
        })
        .flatten()
        .collect();
    assert!(events.iter().any(
        |e| matches!(&e.payload, Payload::Span(s) if s.start_ns == 0 && s.duration_ns == 1000)
    ));
    assert!(events.iter().any(|e| matches!(&e.payload, Payload::Track(t) if t.name.contains("worker") && matches!(t.kind, rprof_core::TrackKind::Thread { tid: 9 }))));
    assert!(events.iter().any(|e| matches!(&e.payload, Payload::Track(t) if t.name.contains("training") && matches!(t.kind, rprof_core::TrackKind::Process { pid: 4 }))));
    let meta = events
        .iter()
        .find(|e| matches!(&e.payload, Payload::Counter(c) if c.name == "pytorch.import.metadata"))
        .unwrap();
    for key in [
        "schemaVersion",
        "ERROR",
        "WARNING",
        "profile_memory",
        "record_shapes",
        "with_stack",
        "with_flops",
        "rprof_custom",
    ] {
        assert!(
            meta.attributes.contains_key(&format!("pytorch.{key}")),
            "missing {key}"
        );
    }
    assert!(
        matches!(meta.attributes.get("pytorch.role"), Some(Attribute::Text(role)) if role == "runtime")
    );
}

#[test]
fn memory_instants_preserve_exact_signed_delta_and_only_present_totals() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("memory.json");
    let out = dir.path().join("out");
    fs::write(&input, r#"[{"ph":"i","cat":"cpu_instant_event","name":"[memory]","pid":5,"tid":6,"ts":0,"args":{"Bytes":512,"Device Type":0,"Device Id":-1,"Total Allocated":512,"Total Reserved":1024}},{"ph":"i","cat":"cpu_instant_event","name":"[memory]","pid":5,"tid":6,"ts":1,"args":{"Bytes":-18446744073709551616,"Device Type":0,"Device Id":-1,"Total Allocated":0}},{"ph":"C","cat":"power","name":"sensors","pid":5,"tid":6,"ts":2,"args":{"watts":23}}]"#).unwrap();
    import(&input, &out, None).unwrap();
    let events: Vec<_> = records(&out)
        .into_iter()
        .filter_map(|r| {
            if let Record::Chunk(c) = r {
                Some(c.events)
            } else {
                None
            }
        })
        .flatten()
        .collect();
    let allocations: Vec<_> = events.iter().filter(|e| matches!(&e.payload, Payload::Counter(c) if c.name == "pytorch.memory.allocated" && c.unit == "bytes")).collect();
    assert_eq!(allocations.len(), 2);
    assert!(
        matches!(&allocations[0].payload, Payload::Counter(c) if matches!(c.value, CounterValue::Integer(512)))
    );
    let reserved: Vec<_> = events.iter().filter(|e| matches!(&e.payload, Payload::Counter(c) if c.name == "pytorch.memory.reserved" && c.unit == "bytes")).collect();
    assert_eq!(reserved.len(), 1);
    assert!(
        matches!(reserved[0].attributes.get("pytorch.counter_key"), Some(Attribute::Text(v)) if v == "Total Reserved")
    );
    assert!(events.iter().any(|e| matches!(&e.payload, Payload::Counter(c) if c.name == "[memory]" && c.unit == "marker") && matches!(e.attributes.get("pytorch.delta_bytes"), Some(Attribute::Text(v)) if v == "-18446744073709551616") && matches!(e.attributes.get("pytorch.device_type"), Some(Attribute::Text(v)) if v == "0") && matches!(e.attributes.get("pytorch.role"), Some(Attribute::Text(v)) if v == "memory")));
    assert!(events.iter().any(|e| matches!(&e.payload, Payload::Counter(c) if c.name == "sensors.watts") && matches!(e.attributes.get("pytorch.role"), Some(Attribute::Text(v)) if v == "counter")));
}

#[test]
fn large_sequential_lane_indexes_only_overlapping_flow_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("trace.json");
    let out = dir.path().join("out");
    let mut trace = String::from("[\n");
    for n in 0..4100 {
        trace.push_str(&format!(
            "{{\"ph\":\"X\",\"name\":\"unit\",\"pid\":1,\"tid\":1,\"ts\":{n},\"dur\":1}},"
        ));
    }
    trace.push_str(r#"{"ph":"s","name":"edge","id":1,"pid":1,"tid":1,"ts":4099.5},{"ph":"X","name":"target","pid":1,"tid":2,"ts":4100,"dur":2},{"ph":"f","name":"edge","id":1,"pid":1,"tid":2,"ts":4101}]"#);
    fs::write(&input, trace).unwrap();
    let s = import(&input, &out, None).unwrap();
    assert_eq!(s.spans, 4101);
    assert_eq!(s.unresolved_flows, 0);
    let events: Vec<_> = records(&out)
        .into_iter()
        .filter_map(|r| {
            if let Record::Chunk(c) = r {
                Some(c.events)
            } else {
                None
            }
        })
        .flatten()
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.payload, Payload::Dependency { .. }))
            .count(),
        1
    );
}

#[test]
fn flow_at_adjacent_slice_boundary_attaches_to_starting_slice() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("boundary");
    let output = dir.path().join("out");
    fs::write(&input, r#"[{"ph":"X","name":"before","pid":1,"tid":1,"ts":0,"dur":1},{"ph":"X","name":"after","pid":1,"tid":1,"ts":1,"dur":2},{"ph":"X","name":"destination","pid":1,"tid":2,"ts":2,"dur":2},{"ph":"s","name":"edge","id":1,"pid":1,"tid":1,"ts":1},{"ph":"f","name":"edge","id":1,"pid":1,"tid":2,"ts":3}]"#).unwrap();
    let s = import(&input, &output, None).unwrap();
    assert_eq!(s.unresolved_flows, 0);
    assert_eq!(s.ambiguous_flows, 0);
    let events: Vec<_> = records(&output)
        .into_iter()
        .filter_map(|r| {
            if let Record::Chunk(c) = r {
                Some(c.events)
            } else {
                None
            }
        })
        .flatten()
        .collect();
    let after_id = events
        .iter()
        .find_map(|e| {
            if let Payload::Span(s) = &e.payload {
                (s.name == "after").then_some(s.correlation_id.unwrap())
            } else {
                None
            }
        })
        .unwrap();
    assert!(events.iter().any(|e| matches!(&e.payload, Payload::Dependency { from_correlation_id, .. } if *from_correlation_id == after_id)));
}

#[test]
fn optional_real_cpu_probe() {
    let Ok(input) = std::env::var("RPROF_PYTORCH_PROBE") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let summary = import(Path::new(&input), &dir.path().join("capture"), None).unwrap();
    assert!(summary.source_records > 100, "{summary:?}");
    assert!(summary.spans > 0, "{summary:?}");
    assert_eq!(summary.writer_loss, 0, "{summary:?}");
    eprintln!("real CPU probe: {summary:?}");
}
