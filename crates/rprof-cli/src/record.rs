use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use rprof_capture::{Recorder, RecorderConfig};
use rprof_core::{
    Category, Span, Track, TrackKind,
    stream::{Collector, End, EndReason, Event, Header, Loss, Payload, Shard},
};
use uuid::Uuid;

use crate::{RecordArgs, supervisor};

fn monotonic_ns() -> Result<u64> {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } == 0,
        "reading monotonic clock: {}",
        std::io::Error::last_os_error()
    );
    let seconds = u64::try_from(time.tv_sec)?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|s| s.checked_add(time.tv_nsec as u64))
        .context("monotonic clock overflow")
}

fn env_uuid(name: &str) -> Result<Option<Uuid>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(
            value.parse().with_context(|| format!("invalid {name}"))?,
        )),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {name}")),
    }
}

pub(crate) fn record(args: RecordArgs) -> Result<u8> {
    let config = RecorderConfig {
        chunk_events: args.chunk_events,
        queue_chunks: args.queue_chunks,
        flush_interval_ms: args.flush_interval_ms,
    };
    config.validate()?;
    let session_id = match args.session {
        Some(id) => id,
        None => env_uuid("RPROF_SESSION_ID")?.unwrap_or_else(Uuid::new_v4),
    };
    let shard_id = Uuid::new_v4();
    let output = if let Some(directory) = args.output_dir {
        fs::create_dir_all(&directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        directory.join(format!("{session_id}.{shard_id}.rprof.jsonl"))
    } else {
        args.output
            .unwrap_or_else(|| PathBuf::from("capture.rprof.jsonl"))
    };
    let origin = monotonic_ns()?;
    let elapsed = || -> Result<u64> {
        monotonic_ns()?
            .checked_sub(origin)
            .context("monotonic clock went backwards")
    };
    let shard = Shard {
        shard_id,
        parent_shard_id: env_uuid("RPROF_SHARD_ID")?,
        hostname: fs::read_to_string("/proc/sys/kernel/hostname")?
            .trim()
            .into(),
        boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .into(),
        monotonic_origin_ns: origin,
        started_unix_ns: SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos()
            .try_into()?,
        command: args
            .command
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect(),
        supervisor_pid: std::process::id(),
        collectors: vec![Collector {
            name: "process".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }],
        config: config.metadata(),
    };
    let mut recorder = Recorder::create(&output, Header::new(session_id), shard, config)?;
    eprintln!("Session: {session_id}\nCapture: {}", output.display());
    let mut started = false;
    let launch_start = elapsed()?;
    let result = supervisor::supervise(
        &args.command,
        supervisor::Config {
            grace: Duration::from_millis(args.grace_ms),
            heartbeat_interval: Duration::from_millis(args.flush_interval_ms),
            environment: vec![
                (
                    OsString::from("RPROF_SESSION_ID"),
                    session_id.to_string().into(),
                ),
                (
                    OsString::from("RPROF_SHARD_ID"),
                    shard_id.to_string().into(),
                ),
            ],
            ..Default::default()
        },
        |notification| {
            let now = elapsed()?;
            match notification {
                supervisor::Event::Started { pid } => {
                    started = true;
                    recorder.reliable(Event::new(
                        now,
                        Payload::Track(Track {
                            id: 0,
                            parent_id: None,
                            name: args.command[0].to_string_lossy().into_owned(),
                            rank: None,
                            kind: TrackKind::Process { pid },
                        }),
                    ))?;
                    recorder.reliable(Event::new(
                        now,
                        Payload::ActivityStart {
                            track_id: 0,
                            name: "supervised workload".into(),
                            correlation_id: 0,
                        },
                    ))?;
                }
                supervisor::Event::Heartbeat { .. } => {
                    recorder.emit(Event::new(now, Payload::Heartbeat))?;
                    recorder.flush()?;
                }
            }
            Ok(())
        },
    );
    let duration_ns = elapsed()?;
    match result {
        Ok(outcome) => {
            recorder.reliable(Event::new(
                duration_ns,
                Payload::Span(Span {
                    track_id: 0,
                    name: "supervised workload (launch, wait, descendant cleanup)".into(),
                    category: Category::Process,
                    start_ns: launch_start,
                    duration_ns: duration_ns - launch_start,
                    correlation_id: Some(0),
                }),
            ))?;
            let loss = recorder.loss().total();
            recorder.finish(End {
                shard_id,
                duration_ns,
                exit_code: outcome.exit_code,
                signal: outcome.signal,
                interrupted_by: outcome.interrupted_by,
                reason: if outcome.interrupted_by.is_some() {
                    EndReason::Interrupted
                } else {
                    EndReason::Completed
                },
                loss: Loss::default(),
            })?;
            eprintln!("Capture finalized ({loss} dropped events)");
            Ok(outcome
                .interrupted_by
                .map(|s| 128 + s)
                .or(outcome.exit_code)
                .or(outcome.signal.map(|s| 128 + s))
                .unwrap_or(1) as u8)
        }
        Err(error) => {
            let end = End {
                shard_id,
                duration_ns,
                exit_code: None,
                signal: None,
                interrupted_by: None,
                reason: if started {
                    EndReason::CollectorFailed
                } else {
                    EndReason::LaunchFailed
                },
                loss: Loss::default(),
            };
            if let Err(write_error) = recorder.finish(end) {
                eprintln!("Capture finalization failed; readable prefix retained: {write_error:#}");
            }
            Err(error)
        }
    }
}
