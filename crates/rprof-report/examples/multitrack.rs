//! Generate an illustrative CPU/MPI/GPU/I/O fixture without hardware dependencies.
//! cargo run -p rprof-report --example multitrack -- OUTPUT [--single-domain]

use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};
use rprof_core::{
    Category, CounterSample, CounterValue, Span, Track, TrackKind,
    stream::{
        Attribute, CaptureConfig, Chunk, Collector, End, EndReason, Event, Header, Loss, Payload,
        Record, Shard,
    },
};
use uuid::Uuid;

fn write_record(out: &mut impl Write, record: Record) -> Result<()> {
    serde_json::to_writer(&mut *out, &record)?;
    out.write_all(b"\n")?;
    Ok(())
}

fn span(
    track: u64,
    name: &str,
    category: Category,
    start_ms: u64,
    duration_ms: u64,
    correlation: u64,
) -> Event {
    Event::new(
        (start_ms + duration_ms) * 1_000_000,
        Payload::Span(Span {
            track_id: track,
            name: name.into(),
            category,
            start_ns: start_ms * 1_000_000,
            duration_ns: duration_ms * 1_000_000,
            correlation_id: Some(correlation),
        }),
    )
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .context("usage: multitrack OUTPUT [--single-domain]")?,
    );
    let option = args.next();
    ensure!(
        option.as_deref().is_none_or(|v| v == "--single-domain") && args.next().is_none(),
        "unknown fixture option"
    );
    let single_domain = option.is_some();
    let mut out = BufWriter::new(File::create_new(path)?);
    write_record(&mut out, Record::Header(Header::new(Uuid::from_u128(42))))?;
    for rank in 0..if single_domain { 2 } else { 3 } {
        let shard_id = Uuid::from_u128(rank as u128 + 1);
        write_record(
            &mut out,
            Record::Shard(Shard {
                shard_id,
                parent_shard_id: None,
                hostname: if rank < 2 { "compute-a" } else { "compute-b" }.into(),
                boot_id: format!("synthetic-boot-{}", if rank < 2 { "a" } else { "b" }),
                monotonic_origin_ns: 9_007_199_254_740_993 + rank as u64 * 500_000,
                started_unix_ns: 1_800_000_000_000_000_001,
                command: vec!["synthetic-solver".into(), format!("--rank={rank}")],
                supervisor_pid: 1000 + rank,
                collectors: vec![Collector {
                    name: "synthetic-multitrack".into(),
                    version: "1".into(),
                }],
                config: CaptureConfig {
                    chunk_events: 64,
                    queue_chunks: 8,
                    flush_interval_ms: 100,
                },
            }),
        )?;
        let mut events = Vec::new();
        for (id, parent, name, kind) in [
            (
                0,
                None,
                "solver process",
                TrackKind::Process { pid: 2000 + rank },
            ),
            (
                1,
                Some(0),
                "main thread",
                TrackKind::Thread {
                    tid: 2000 + rank as u64,
                },
            ),
            (
                2,
                Some(0),
                "OpenMP worker",
                TrackKind::Thread {
                    tid: 3000 + rank as u64,
                },
            ),
            (3, Some(0), "GPU 0", TrackKind::GpuDevice { device: 0 }),
            (
                4,
                Some(3),
                "GPU stream 7",
                TrackKind::GpuStream { stream: 7 },
            ),
            (
                5,
                Some(0),
                "checkpoint I/O",
                TrackKind::Io {
                    resource: "checkpoint.bin".into(),
                },
            ),
        ] {
            events.push(Event::new(
                0,
                Payload::Track(Track {
                    id,
                    parent_id: parent,
                    name: name.into(),
                    rank: Some(rank),
                    kind,
                }),
            ));
        }
        events.push(Event::new(
            0,
            Payload::ActivityStart {
                track_id: 0,
                name: "solver step".into(),
                correlation_id: 99,
            },
        ));
        events.extend([
            span(0, "solver step", Category::Process, 0, 40, 99),
            span(1, "parallel region", Category::Openmp, 1, 15, 10),
            span(2, "compute tile", Category::Openmp, 2, 3, 11),
            span(1, "MPI exchange", Category::Mpi, 18, 2, 12),
            span(1, "cudaLaunchKernel", Category::Cuda, 5, 1, 20),
            span(4, "stencil kernel", Category::Cuda, 6, 8, 20),
        ]);
        let mut io = span(5, "checkpoint write", Category::Io, 22, 10, 30);
        io.attributes = BTreeMap::from([
            ("bytes".into(), Attribute::Unsigned(4_194_304)),
            (
                "path".into(),
                Attribute::Text("checkpoint & results.bin".into()),
            ),
        ]);
        events.push(io);
        for i in 0..4 {
            let timestamp_ns = (2 + i) * 1_000_000;
            events.push(Event::new(
                timestamp_ns,
                Payload::Counter(CounterSample {
                    track_id: 1,
                    timestamp_ns,
                    name: "cycles".into(),
                    value: CounterValue::Integer(9_007_199_254_740_993 + i),
                    unit: "count".into(),
                }),
            ));
        }
        for (ms, value) in [(7, 0.25), (12, 0.75)] {
            events.push(Event::new(
                ms * 1_000_000,
                Payload::Counter(CounterSample {
                    track_id: 4,
                    timestamp_ns: ms * 1_000_000,
                    name: "GPU busy".into(),
                    value: CounterValue::Float(value),
                    unit: "fraction".into(),
                }),
            ));
        }
        for (from, to, relation) in [
            (11, 20, "launch after compute"),
            (20, 30, "checkpoint after kernel"),
            (999, 12, "unresolved example"),
        ] {
            events.push(Event::new(
                33_000_000,
                Payload::Dependency {
                    from_correlation_id: from,
                    to_correlation_id: to,
                    relation: relation.into(),
                },
            ));
        }
        events.push(Event::new(
            39_000_000,
            Payload::ActivityStart {
                track_id: 5,
                name: "unfinished flush".into(),
                correlation_id: 777,
            },
        ));
        events.sort_by_key(|e| e.timestamp_ns);
        write_record(
            &mut out,
            Record::Chunk(Chunk {
                shard_id,
                sequence: 0,
                loss: Loss::default(),
                events,
            }),
        )?;
        write_record(
            &mut out,
            Record::End(End {
                shard_id,
                duration_ns: 40_000_000,
                exit_code: Some(0),
                signal: None,
                interrupted_by: None,
                reason: EndReason::Completed,
                loss: Loss::default(),
            }),
        )?;
    }
    out.flush()?;
    Ok(())
}
