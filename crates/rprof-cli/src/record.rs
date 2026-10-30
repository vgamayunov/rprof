use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use rprof_capture::{Recorder, RecorderConfig};
use rprof_core::{
    Category, CounterSample, CounterValue, Span, Track, TrackKind,
    stream::{Attribute, Collector, End, EndReason, Event, Header, Loss, Payload, Shard},
};
use uuid::Uuid;

use crate::{CollectorKind, RecordArgs, capture_sink::CaptureSink, native_collector, supervisor};

pub(crate) fn monotonic_ns() -> Result<u64> {
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
    let io_enabled = args.collectors.contains(&CollectorKind::Io);
    let mpi_enabled = args.collectors.contains(&CollectorKind::Mpi);
    let openmp_enabled = args.collectors.contains(&CollectorKind::Openmp);
    let cuda_enabled = args.collectors.contains(&CollectorKind::Cuda);
    let pmu_enabled = args.collectors.contains(&CollectorKind::Pmu);
    let pmu_settings = crate::pmu_collector::Settings {
        scope: args.pmu_scope,
        kernel: args.pmu_kernel,
        interval_ms: args.pmu_interval_ms,
    };
    ensure!(
        pmu_enabled
            || (args.pmu_events.is_empty()
                && !args.pmu_kernel
                && args.pmu_scope == crate::PmuScope::WorkloadTree
                && args.pmu_interval_ms == 100),
        "PMU options require --collect pmu"
    );
    let mut pmu_prepared = if pmu_enabled {
        Some(crate::pmu_collector::prepare(&args)?)
    } else {
        None
    };
    ensure!(
        cuda_enabled || args.cuda_library.is_none(),
        "--cuda-library requires --collect cuda"
    );
    ensure!(
        openmp_enabled || args.openmp_library.is_none(),
        "--openmp-library requires --collect openmp"
    );
    ensure!(
        mpi_enabled || (args.mpi_library.is_none() && !args.mpi_clock_sync),
        "MPI options require --collect mpi"
    );
    ensure!(
        io_enabled || args.io_library.is_none(),
        "--io-library requires --collect io"
    );
    let mut io_prepared = if io_enabled {
        Some(native_collector::Prepared::new(
            args.io_library.as_deref(),
            args.io_buffer_events as usize,
        )?)
    } else {
        None
    };
    let mut mpi_prepared = if mpi_enabled {
        Some(native_collector::Prepared::mpi(
            args.mpi_library.as_deref(),
            args.io_buffer_events as usize,
        )?)
    } else {
        None
    };
    let mut openmp_prepared = if openmp_enabled {
        Some(native_collector::Prepared::openmp(
            args.openmp_library.as_deref(),
            args.io_buffer_events as usize,
        )?)
    } else {
        None
    };
    if openmp_enabled {
        ensure!(
            std::env::var_os("OMP_TOOL_LIBRARIES").is_none_or(|value| value.is_empty()),
            "OMP_TOOL_LIBRARIES is already set; unset it before selecting rprof's OMPT collector (only one OMPT tool can own callbacks)"
        );
    }
    let mut cuda_prepared = if cuda_enabled {
        Some(native_collector::Prepared::cuda(
            args.cuda_library.as_deref(),
            args.io_buffer_events as usize,
        )?)
    } else {
        None
    };
    if cuda_enabled {
        ensure!(
            std::env::var_os("CUDA_INJECTION64_PATH").is_none_or(|value| value.is_empty()),
            "CUDA_INJECTION64_PATH is already set; select one CUDA injection tool before enabling rprof"
        );
    }
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
        collectors: [
            Some("process"),
            io_enabled.then_some("io"),
            mpi_enabled.then_some("mpi"),
            openmp_enabled.then_some("openmp"),
            cuda_enabled.then_some("cuda"),
            pmu_enabled.then_some("pmu"),
        ]
        .into_iter()
        .flatten()
        .map(|name| Collector {
            name: name.into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })
        .collect(),
        config: config.metadata(),
    };
    let recorder = Recorder::create(&output, Header::new(session_id), shard, config)?;
    let rank = launcher_rank();
    let mut initial_sink = CaptureSink::new(recorder);
    initial_sink.rank = rank;
    let sink = Arc::new(Mutex::new(initial_sink));
    eprintln!("Session: {session_id}\nCapture: {}", output.display());
    let mut started = false;
    let mut io_running = None;
    let mut mpi_running = None;
    let mut openmp_running = None;
    let mut cuda_running = None;
    let mut pmu_running = None;
    let mut environment = vec![
        (
            OsString::from("RPROF_SESSION_ID"),
            session_id.to_string().into(),
        ),
        (
            OsString::from("RPROF_SHARD_ID"),
            shard_id.to_string().into(),
        ),
    ];
    if let Some(io) = &io_prepared {
        environment.extend(io.environment());
    }
    if let Some(mpi) = &mpi_prepared {
        environment.extend(mpi.environment());
    }
    let mut preload = OsString::new();
    for collector in [&io_prepared, &mpi_prepared].into_iter().flatten() {
        if !preload.is_empty() {
            preload.push(" ");
        }
        preload.push(collector.library());
    }
    if !preload.is_empty() {
        if let Some(existing) = std::env::var_os("LD_PRELOAD").filter(|s| !s.is_empty()) {
            preload.push(" ");
            preload.push(existing);
        }
        environment.push(("LD_PRELOAD".into(), preload));
    }
    if mpi_enabled {
        environment.push((
            "RPROF_MPI_CLOCK_SYNC".into(),
            if args.mpi_clock_sync { "1" } else { "0" }.into(),
        ));
    }
    if let Some(openmp) = &openmp_prepared {
        environment.extend(openmp.environment());
        environment.push(("OMP_TOOL".into(), "enabled".into()));
        environment.push((
            "OMP_TOOL_LIBRARIES".into(),
            openmp.library().as_os_str().to_owned(),
        ));
    }
    if let Some(cuda) = &cuda_prepared {
        environment.extend(cuda.environment());
        environment.push((
            "CUDA_INJECTION64_PATH".into(),
            cuda.library().as_os_str().to_owned(),
        ));
    }
    let launch_start = elapsed()?;
    let result = supervisor::supervise(
        &args.command,
        supervisor::Config {
            grace: Duration::from_millis(args.grace_ms),
            heartbeat_interval: Duration::from_millis(if pmu_enabled {
                args.flush_interval_ms.min(args.pmu_interval_ms)
            } else {
                args.flush_interval_ms
            }),
            perf: pmu_prepared.as_ref().map(|p| p.child.clone()),
            environment,
            ..Default::default()
        },
        |notification| {
            let now = elapsed()?;
            let mut out = sink
                .lock()
                .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
            match notification {
                supervisor::Event::Started { pid } => {
                    started = true;
                    out.reliable(Event::new(
                        now,
                        Payload::Track(Track {
                            id: 0,
                            parent_id: None,
                            name: args.command[0].to_string_lossy().into_owned(),
                            rank,
                            kind: TrackKind::Process { pid },
                        }),
                    ))?;
                    out.reliable(Event::new(
                        now,
                        Payload::ActivityStart {
                            track_id: 0,
                            name: "supervised workload".into(),
                            correlation_id: 0,
                            start_ns: None,
                        },
                    ))?;
                    if let Some(prepared) = pmu_prepared.take() {
                        pmu_running = Some(crate::pmu_collector::Running::start(
                            prepared,
                            pmu_settings,
                            pid,
                            &mut out,
                            origin,
                            now,
                        )?);
                    }
                    drop(out);
                    if let Some(prepared) = io_prepared.take() {
                        io_running = Some(prepared.start(Arc::clone(&sink), origin)?);
                    }
                    if let Some(prepared) = mpi_prepared.take() {
                        mpi_running = Some(prepared.start(Arc::clone(&sink), origin)?);
                    }
                    if let Some(prepared) = openmp_prepared.take() {
                        openmp_running = Some(prepared.start(Arc::clone(&sink), origin)?);
                    }
                    if let Some(prepared) = cuda_prepared.take() {
                        cuda_running = Some(prepared.start(Arc::clone(&sink), origin)?);
                    }
                }
                supervisor::Event::Heartbeat { .. } => {
                    if let Some(pmu) = &mut pmu_running {
                        pmu.sample(&mut out, now, false)?;
                    }
                    out.emit(Event::new(now, Payload::Heartbeat))?;
                    out.recorder.flush()?;
                    if let Some(io) = &io_running {
                        io.check()?;
                    }
                    if let Some(mpi) = &mpi_running {
                        mpi.check()?;
                    }
                    if let Some(openmp) = &openmp_running {
                        openmp.check()?;
                    }
                    if let Some(cuda) = &cuda_running {
                        cuda.check()?;
                    }
                }
            }
            Ok(())
        },
    );
    let workload_end_ns = elapsed()?;
    let workload_status = result.as_ref().ok().copied();
    let io_result = io_running
        .map(native_collector::Running::finish)
        .transpose();
    let mpi_result = mpi_running
        .map(native_collector::Running::finish)
        .transpose();
    let openmp_result = openmp_running
        .map(native_collector::Running::finish)
        .transpose();
    let cuda_result = cuda_running
        .map(native_collector::Running::finish)
        .transpose();
    let pmu_result = pmu_running
        .map(|pmu| {
            let mut out = sink
                .lock()
                .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
            pmu.finish(&mut out, elapsed()?)
        })
        .transpose();
    let result = result.and_then(|outcome| {
        pmu_result?;
        if let Some(io) = io_result? {
            eprintln!("I/O: {} attempted, {} received, {} ingress drops across {} process images", io.attempted, io.received, io.dropped, io.initialized);
            ensure!(io.initialized > 0, "I/O preload collector did not initialize; check dynamic linking, LD_PRELOAD support, and the library path");
            let mut out = sink.lock().map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
            let at = elapsed()?;
            for (name, value) in [("io.attempted_events", io.attempted), ("io.received_events", io.received), ("io.ingress_dropped_events", io.dropped), ("io.initialized_images", io.initialized)] {
                out.reliable(Event::new(at, Payload::Counter(CounterSample { track_id: 0, timestamp_ns: at, name: name.into(), value: CounterValue::Integer(value), unit: "count".into() })))?;
            }
        }
        if let Some(mpi) = mpi_result? {
            eprintln!("MPI: {} attempted, {} received, {} ingress drops", mpi.attempted, mpi.received, mpi.dropped);
            ensure!(mpi.initialized > 0, "MPI preload collector did not initialize; use a compatible target-built PMPI library");
            let mut out = sink.lock().map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
            let at = elapsed()?;
            for (name, value) in [("mpi.attempted_events",mpi.attempted),("mpi.received_events",mpi.received),("mpi.ingress_dropped_events",mpi.dropped)] {
                out.reliable(Event::new(at, Payload::Counter(CounterSample { track_id: 0, timestamp_ns: at, name: name.into(), value: CounterValue::Integer(value), unit:"count".into() })))?;
            }
        }
        if let Some(openmp)=openmp_result? {
            eprintln!("OpenMP: {} attempted, {} received, {} ingress drops; {} OMPT initializations",openmp.attempted,openmp.received,openmp.dropped,openmp.initialized);
            let mut out=sink.lock().map_err(|_|anyhow::anyhow!("capture writer lock poisoned"))?;
            let at=elapsed()?;
            for (name,value) in [("openmp.attempted_events",openmp.attempted),("openmp.received_events",openmp.received),("openmp.ingress_dropped_events",openmp.dropped),("openmp.initialized_images",openmp.initialized)]{
                out.reliable(Event::new(at,Payload::Counter(CounterSample{track_id:0,timestamp_ns:at,name:name.into(),value:CounterValue::Integer(value),unit:"count".into()})))?;
            }
            if openmp.initialized==0 {
                let mut event=Event::new(at,Payload::Counter(CounterSample{track_id:0,timestamp_ns:at,name:"openmp.meta.runtime".into(),value:CounterValue::Integer(0),unit:"version".into()}));
                event.attributes.insert("openmp.role".into(),Attribute::Text("runtime".into()));
                event.attributes.insert("openmp.runtime_version".into(),Attribute::Text("not observed".into()));
                event.attributes.insert("openmp.initialized".into(),Attribute::Bool(false));
                out.reliable(event)?;
                anyhow::bail!("No OMPT initialization observed: the runtime may lack OMPT (including current GCC libgomp), or the workload did not initialize OpenMP. See docs/openmp-profiling.md");
            }
        }
        if let Some(cuda)=cuda_result?{
            eprintln!("CUDA: {} native records attempted, {} received, {} ingress drops; {} CUPTI drops; finalized={}",cuda.attempted,cuda.received,cuda.dropped,cuda.source_dropped,cuda.finalized>=cuda.initialized.max(1));
            let mut out=sink.lock().map_err(|_|anyhow::anyhow!("capture writer lock poisoned"))?;
            let at=elapsed()?;
            for (name,value) in [("cuda.attempted_events",cuda.attempted),("cuda.received_events",cuda.received),("cuda.ingress_dropped_events",cuda.dropped),("cuda.cupti_dropped_records",cuda.source_dropped),("cuda.source_errors",cuda.source_errors),("cuda.finalized",cuda.finalized),("cuda.initialized_images",cuda.initialized)]{
                let mut event=Event::new(at,Payload::Counter(CounterSample{track_id:0,timestamp_ns:at,name:name.into(),value:CounterValue::Integer(value),unit:"count".into()}));
                event.attributes.insert("cuda.role".into(),Attribute::Text("diagnostic".into()));
                out.reliable(event)?;
            }
            ensure!(cuda.initialized>0,"No CUDA/CUPTI initialization observed; check CUDA activity, the target-built injection library, and CUPTI availability");
            ensure!(cuda.source_errors==0,"CUPTI reported {} collector errors; inspect CUDA diagnostics in the saved capture",cuda.source_errors);
        }
        Ok(outcome)
    });
    let mut sink = Arc::try_unwrap(sink)
        .map_err(|_| anyhow::anyhow!("I/O receiver still owns capture"))?
        .into_inner()
        .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
    let duration_ns = elapsed()?;
    if workload_status.is_some() {
        sink.reliable(Event::new(
            duration_ns,
            Payload::Span(Span {
                track_id: 0,
                name: "supervised workload (launch, wait, descendant cleanup)".into(),
                category: Category::Process,
                start_ns: launch_start,
                duration_ns: workload_end_ns - launch_start,
                correlation_id: Some(0),
            }),
        ))?;
    }
    match result {
        Ok(outcome) => {
            let loss = sink.recorder.loss().total();
            sink.recorder.finish(End {
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
                exit_code: workload_status.and_then(|status| status.exit_code),
                signal: workload_status.and_then(|status| status.signal),
                interrupted_by: workload_status.and_then(|status| status.interrupted_by),
                reason: if started {
                    EndReason::CollectorFailed
                } else {
                    EndReason::LaunchFailed
                },
                loss: Loss::default(),
            };
            if let Err(write_error) = sink.recorder.finish(end) {
                eprintln!("Capture finalization failed; readable prefix retained: {write_error:#}");
            }
            Err(error)
        }
    }
}

fn launcher_rank() -> Option<u32> {
    [
        "OMPI_COMM_WORLD_RANK",
        "PMI_RANK",
        "PMIX_RANK",
        "SLURM_PROCID",
    ]
    .into_iter()
    .find_map(|key| std::env::var(key).ok().and_then(|value| value.parse().ok()))
}
