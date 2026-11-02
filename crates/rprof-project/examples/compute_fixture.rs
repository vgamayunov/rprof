//! Measured, single-core LCG workload for offline throughput-ratio validation.
//! cargo run --release -p rprof-project --example compute_fixture -- --iterations 120000000 --output capture.rprof.jsonl

use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use rprof_capture::{Recorder, RecorderConfig};
use rprof_core::{
    Category, Span, Track, TrackKind,
    stream::{Attribute, Collector, End, EndReason, Event, Header, Loss, Payload, Shard},
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn monotonic_ns() -> Result<u64> {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } == 0,
        "clock_gettime: {}",
        std::io::Error::last_os_error()
    );
    let seconds = u64::try_from(time.tv_sec)?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|n| n.checked_add(u64::try_from(time.tv_nsec).ok()?))
        .context("monotonic clock overflow")
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut iterations, mut output) = (None, None);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--iterations" => {
                ensure!(iterations.is_none(), "duplicate --iterations");
                iterations = Some(
                    args.next()
                        .context("missing --iterations value")?
                        .parse::<u64>()?,
                );
            }
            "--output" => {
                ensure!(output.is_none(), "duplicate --output");
                output = Some(PathBuf::from(
                    args.next().context("missing --output value")?,
                ));
            }
            _ => anyhow::bail!(
                "unknown argument {flag}; usage: compute_fixture --iterations N --output FILE"
            ),
        }
    }
    let iterations = iterations.context("--iterations is required")?;
    ensure!(
        (1..=2_000_000_000).contains(&iterations),
        "iterations must be 1..=2000000000"
    );
    let output = output.context("--output is required")?;
    let origin = monotonic_ns()?;
    let elapsed = || -> Result<u64> {
        monotonic_ns()?
            .checked_sub(origin)
            .context("monotonic clock went backwards")
    };
    let config = RecorderConfig::default();
    let shard_id = Uuid::new_v4();
    let shard = Shard {
        shard_id,
        parent_shard_id: None,
        hostname: std::fs::read_to_string("/proc/sys/kernel/hostname")?
            .trim()
            .into(),
        boot_id: std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .into(),
        monotonic_origin_ns: origin,
        started_unix_ns: SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .try_into()?,
        command: std::env::args().collect(),
        supervisor_pid: std::process::id(),
        collectors: vec![Collector {
            name: "benchmark-annotation".into(),
            version: "1".into(),
        }],
        config: config.metadata(),
    };
    let mut recorder = Recorder::create(&output, Header::new(Uuid::new_v4()), shard, config)?;
    recorder.reliable(Event::new(
        elapsed()?,
        Payload::Track(Track {
            id: 0,
            parent_id: None,
            name: "LCG benchmark process".into(),
            rank: None,
            kind: TrackKind::Process {
                pid: std::process::id(),
            },
        }),
    ))?;
    let start = elapsed()?;
    let mut checksum = 0x1234_5678_9abc_def0_u64;
    for _ in 0..iterations {
        checksum = std::hint::black_box(
            checksum
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407),
        );
    }
    let end = elapsed()?;
    let work_ns = end.checked_sub(start).context("clock went backwards")?;
    ensure!(work_ns > 0, "measured zero duration; increase iterations");
    let algorithm = format!("{:x}", Sha256::digest(include_bytes!("compute_fixture.rs")));
    let mut event = Event::new(
        end,
        Payload::Span(Span {
            track_id: 0,
            name: "lcg.work".into(),
            category: Category::Process,
            start_ns: start,
            duration_ns: work_ns,
            correlation_id: None,
        }),
    );
    event.attributes = BTreeMap::from([
        (
            "benchmark.iterations".into(),
            Attribute::Unsigned(iterations),
        ),
        (
            "benchmark.work_unit".into(),
            Attribute::Text("lcg iterations".into()),
        ),
        (
            "benchmark.algorithm".into(),
            Attribute::Text(algorithm.clone()),
        ),
        ("benchmark.checksum".into(), Attribute::Unsigned(checksum)),
    ]);
    recorder.reliable(event)?;
    recorder.finish(End {
        shard_id,
        duration_ns: elapsed()?,
        exit_code: Some(0),
        signal: None,
        interrupted_by: None,
        reason: EndReason::Completed,
        loss: Loss::default(),
    })?;
    println!("work_ns={work_ns} iterations={iterations} checksum={checksum} algorithm={algorithm}");
    Ok(())
}
