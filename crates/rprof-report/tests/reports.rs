use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use rprof_core::stream::{Payload, Record};
use rprof_report::{Limits, analyze, write_html, write_text, write_trace};
use serde_json::Value;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/multitrack.jsonl")
}

fn records() -> Vec<Record> {
    fs::read_to_string(fixture())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn save(path: &Path, records: &[Record]) {
    let mut file = fs::File::create_new(path).unwrap();
    for record in records {
        serde_json::to_writer(&mut file, record).unwrap();
        file.write_all(b"\n").unwrap();
    }
}

#[test]
fn shared_analysis_preserves_identities_clocks_and_exact_values() {
    let report = analyze(&fixture(), Limits::default()).unwrap();
    assert_eq!(report.provenance.kind, "synthetic");
    assert_eq!(report.summary.tracks, "18");
    assert_eq!(report.summary.spans, "21");
    assert_eq!(report.summary.counters, "18");
    assert_eq!(report.summary.dependencies, "9");
    assert_eq!(report.domains.len(), 2);
    assert_eq!(report.domains[0].origin_ns, "9007199254740993");
    assert_eq!(report.domains[0].end_ns, "40500000");
    assert_eq!(report.shards[1].origin_ns, "500000");
    assert_eq!(report.spans.iter().filter(|s| s.incomplete).count(), 3);
    assert!(
        report
            .counters
            .iter()
            .any(|c| c.value == "9007199254740993")
    );
    assert!(
        report
            .counters
            .iter()
            .any(|c| c.value == "0.25" && c.value_type == "float")
    );
    assert!(
        report
            .summary
            .warnings
            .iter()
            .any(|w| w.contains("3 dependency"))
    );
    let process_total = report
        .summary
        .categories
        .iter()
        .find(|c| c.name == "process")
        .unwrap();
    assert_eq!(process_total.total_ns, "120000000");
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(encoded.contains("\"value\":\"9007199254740993\""));
    assert!(encoded.contains("\"origin_ns\":\"9007199254740993\""));
}

#[test]
fn bounded_retention_keeps_full_summary_and_closes_omitted_completions() {
    let all = analyze(&fixture(), Limits::default()).unwrap();
    let limited = analyze(
        &fixture(),
        Limits {
            max_events: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(limited.summary.spans, all.summary.spans);
    assert_eq!(limited.summary.counters, all.summary.counters);
    assert_eq!(limited.tracks.len(), all.tracks.len());
    assert!(limited.summary.omitted_events.parse::<u64>().unwrap() > 0);
    assert!(
        limited.spans.is_empty(),
        "retained start was closed by omitted completion"
    );
    let tiny = analyze(
        &fixture(),
        Limits {
            max_bytes: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(tiny.spans.is_empty() && tiny.counters.is_empty());
    let text = analyze(
        &fixture(),
        Limits {
            retain_timeline: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(text.tracks.is_empty() && text.spans.is_empty() && text.counters.is_empty());
    assert_eq!(text.summary.omitted_events, "0");
    let mut out = Vec::new();
    write_text(&text, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("inclusive spans") && out.contains("unaligned"));
}

#[test]
fn html_data_is_inert_and_roundtrips_hostile_labels() {
    let mut report = analyze(&fixture(), Limits::default()).unwrap();
    let hostile = "</script><script>globalThis.rprofInjected=true</script>&<img src=x onerror=alert(1)>\u{2028}\u{2029}";
    report.spans[0].name = hostile.into();
    report.spans[0]
        .attributes
        .insert("exact".into(), u64::MAX.to_string());
    report.provenance.source = "{{SCRIPT}} {{DATA}} {{STYLE}}".into();
    let mut out = Vec::new();
    write_html(&report, &mut out).unwrap();
    let html = String::from_utf8(out).unwrap();
    assert!(!html.contains(hostile));
    assert_eq!(html.matches("</script>").count(), 2);
    let data = html
        .split("id=\"rprof-data\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    let value: Value = serde_json::from_str(data).unwrap();
    assert_eq!(value["spans"][0]["name"], hostile);
    assert_eq!(
        value["spans"][0]["attributes"]["exact"],
        u64::MAX.to_string()
    );
    assert_eq!(
        value["provenance"]["source"],
        "{{SCRIPT}} {{DATA}} {{STYLE}}"
    );
    assert!(!html.contains("<script src="));
}

#[test]
fn trace_requires_domain_and_exports_all_selected_events() {
    let mut out = Vec::new();
    assert!(
        write_trace(&fixture(), &mut out, None)
            .unwrap_err()
            .to_string()
            .contains("unaligned")
    );
    assert!(out.is_empty());
    write_trace(&fixture(), &mut out, Some("0")).unwrap();
    let trace: Value = serde_json::from_slice(&out).unwrap();
    let events = trace["traceEvents"].as_array().unwrap();
    assert_eq!(events.iter().filter(|e| e["ph"] == "X").count(), 16);
    assert_eq!(events.iter().filter(|e| e["ph"] == "C").count(), 12);
    assert!(events.iter().any(|e| e["ph"] == "s"));
    assert!(events.iter().any(|e| e["ph"] == "f"));
    assert!(
        events
            .iter()
            .any(|e| e["rprof"]["exact_value"] == "9007199254740993")
    );
    assert_eq!(trace["rprof"]["omitted_flow_endpoints_or_links"], "2");
    assert!(events.iter().all(|e| e["pid"].as_u64().unwrap_or(0) <= 2));
}

#[test]
fn trace_serializes_fractional_microseconds_without_rounding_through_f64() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.jsonl");
    let mut source = records();
    source.truncate(4); // first header/shard/chunk/footer
    if let Record::Shard(shard) = &mut source[1] {
        shard.monotonic_origin_ns = 0;
    }
    if let Record::Chunk(chunk) = &mut source[2] {
        chunk
            .events
            .retain(|e| matches!(e.payload, Payload::Track(_) | Payload::Span(_)));
        for event in &mut chunk.events {
            if let Payload::Span(span) = &mut event.payload {
                span.start_ns = u64::MAX - 10;
                span.duration_ns = 1;
                event.timestamp_ns = u64::MAX - 9;
            }
        }
        chunk.events.sort_by_key(|e| e.timestamp_ns);
    }
    if let Record::End(end) = &mut source[3] {
        end.duration_ns = u64::MAX;
    }
    save(&path, &source);
    let mut out = Vec::new();
    write_trace(&path, &mut out, None).unwrap();
    let json = String::from_utf8(out).unwrap();
    assert!(json.contains("\"ts\":18446744073709551.605"));
    assert!(json.contains("\"dur\":0.001"));
    assert!(json.contains("\"rprof_start_ns\":\"18446744073709551605\""));
}

#[test]
fn partial_and_empty_reports_preserve_missing_data_status() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.jsonl");
    let source = records();
    save(&path, &source[..source.len() - 1]);
    let report = analyze(&path, Limits::default()).unwrap();
    assert!(!report.summary.complete);
    assert!(
        report
            .shards
            .last()
            .unwrap()
            .end_reason
            .contains("missing footer")
    );
    let mut html = Vec::new();
    write_html(&report, &mut html).unwrap();
    let empty = dir.path().join("empty.jsonl");
    save(&empty, &source[..1]);
    let report = analyze(&empty, Limits::default()).unwrap();
    assert!(report.domains.is_empty() && report.spans.is_empty());
    let mut trace = Vec::new();
    write_trace(&empty, &mut trace, None).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&trace).unwrap()["traceEvents"],
        serde_json::json!([])
    );
}
