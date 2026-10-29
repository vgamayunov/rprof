//! Synthetic throughput/memory probe, not a hardware collector benchmark.
//! cargo run --release -p rprof-capture --example capture-load -- OUTPUT COUNT

use std::{io::BufReader, path::PathBuf, time::Instant};

use anyhow::{Context, Result, ensure};
use rprof_capture::{Recorder, RecorderConfig, summarize};
use rprof_core::{
    CounterSample, CounterValue, Track, TrackKind,
    stream::{Collector, End, EndReason, Event, Header, Loss, Payload, Shard},
};
use uuid::Uuid;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().context("usage: capture-load OUTPUT COUNT")?);
    let count: u64 = args.next().context("missing COUNT")?.parse()?;
    ensure!(args.next().is_none(), "unexpected argument");
    let id = Uuid::new_v4();
    let config = RecorderConfig::default();
    let shard = Shard {
        shard_id: id,
        parent_shard_id: None,
        hostname: "synthetic".into(),
        boot_id: "synthetic-clock".into(),
        monotonic_origin_ns: 0,
        started_unix_ns: 0,
        command: vec!["capture-load".into()],
        supervisor_pid: std::process::id(),
        collectors: vec![Collector {
            name: "synthetic-counters".into(),
            version: "1".into(),
        }],
        config: config.metadata(),
    };
    let start = Instant::now();
    let mut recorder = Recorder::create(&path, Header::new(Uuid::new_v4()), shard, config)?;
    recorder.reliable(Event::new(
        0,
        Payload::Track(Track {
            id: 0,
            parent_id: None,
            name: "synthetic process".into(),
            rank: None,
            kind: TrackKind::Process {
                pid: std::process::id(),
            },
        }),
    ))?;
    for timestamp_ns in 1..=count {
        recorder.emit(Event::new(
            timestamp_ns,
            Payload::Counter(CounterSample {
                track_id: 0,
                timestamp_ns,
                name: "cycles".into(),
                value: CounterValue::Integer(timestamp_ns),
                unit: "count".into(),
            }),
        ))?;
    }
    recorder.finish(End {
        shard_id: id,
        duration_ns: count,
        exit_code: Some(0),
        signal: None,
        interrupted_by: None,
        reason: EndReason::Completed,
        loss: Loss::default(),
    })?;
    let capture_time = start.elapsed();
    let summary = summarize(BufReader::new(std::fs::File::open(&path)?))?;
    let shard = &summary.shards[&id];
    ensure!(
        shard.counters + shard.loss.total() == count,
        "event accounting mismatch"
    );
    println!(
        "Attempted: {count}\nRecorded: {}\nDropped: {}\nCapture time: {:.3} s\nCapture rate: {:.0} events/s\nFile bytes: {}",
        shard.counters,
        shard.loss.total(),
        capture_time.as_secs_f64(),
        count as f64 / capture_time.as_secs_f64(),
        std::fs::metadata(path)?.len()
    );
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(peak) = status.lines().find(|line| line.starts_with("VmHWM:")) {
            println!(
                "Peak resident memory: {}",
                peak.trim_start_matches("VmHWM:").trim()
            );
        }
    }
    Ok(())
}
