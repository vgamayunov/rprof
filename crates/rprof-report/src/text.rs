use std::io::Write;

use anyhow::Result;

use crate::Report;

pub fn write_text(report: &Report, mut out: impl Write) -> Result<()> {
    let summary = &report.summary;
    writeln!(out, "Format:     {}", summary.format)?;
    writeln!(out, "Provenance: {}", report.provenance.label)?;
    writeln!(out, "Session:    {}", summary.session_id)?;
    writeln!(
        out,
        "Status:     {}",
        if summary.complete {
            "complete"
        } else {
            "partial/recovered"
        }
    )?;
    writeln!(out, "Shards:     {}", report.shards.len())?;
    writeln!(
        out,
        "Tail:       {}",
        if summary.truncated_tail {
            "truncated (ignored)"
        } else {
            "intact"
        }
    )?;
    writeln!(out, "Recovered inputs: {}", summary.recovered_inputs)?;
    writeln!(
        out,
        "Tracks:     {}\nSpans:      {}\nCounters:   {}\nDependencies: {}",
        summary.tracks, summary.spans, summary.counters, summary.dependencies
    )?;
    writeln!(
        out,
        "Dropped:    {}{}",
        summary.dropped,
        if summary.complete {
            ""
        } else {
            " (checkpointed lower bound)"
        }
    )?;
    writeln!(
        out,
        "Clock domains: {}{}",
        report.domains.len(),
        if report.domains.len() > 1 {
            " (unaligned across hosts/boots)"
        } else {
            ""
        }
    )?;
    for domain in &report.domains {
        writeln!(out, "  {}: {}", domain.id, domain.name)?;
    }
    for shard in &report.shards {
        writeln!(out, "\nShard:      {}", shard.id)?;
        writeln!(out, "Host:       {}", shard.hostname)?;
        writeln!(out, "Command:    {:?}", shard.command)?;
        writeln!(out, "Collectors: {}", shard.collectors.join(", "))?;
        let duration = shard.end_ns.parse::<u128>()? - shard.origin_ns.parse::<u128>()?;
        writeln!(
            out,
            "{} {:.6} s",
            if shard.complete {
                "Duration:  "
            } else {
                "Observed:  "
            },
            duration as f64 / 1e9
        )?;
        writeln!(out, "End reason: {}", shard.end_reason)?;
        writeln!(
            out,
            "Exit code:  {:?}\nSignal:     {:?}\nInterrupted by: {:?}",
            shard.exit_code, shard.signal, shard.interrupted_by
        )?;
        writeln!(
            out,
            "Dropped:    {}{}",
            shard.dropped,
            if shard.complete {
                ""
            } else {
                " (checkpointed lower bound)"
            }
        )?;
    }
    if !summary.categories.is_empty() {
        writeln!(
            out,
            "\nCategory totals (inclusive spans; overlap/nesting can double-count):"
        )?;
        for category in &summary.categories {
            writeln!(
                out,
                "  {:<10} {:>10} spans  {:>16} ns",
                category.name, category.count, category.total_ns
            )?;
        }
    }
    if let Some(python) = &report.python {
        writeln!(
            out,
            "\nPython instrumented execution segments (inclusive wall time; self excludes included child segments, not native/GIL waits):"
        )?;
        for r in &python.runtimes {
            writeln!(
                out,
                "  PID {} / interpreter {} / {}: CPython {} via {}; initialized={} finalized={}; source drops={} omitted={} unmatched={} abandoned={}",
                r.pid,
                r.interpreter,
                r.source,
                r.version,
                r.backend,
                r.initialized,
                r.finalized,
                r.source_dropped,
                r.omitted_callbacks,
                r.unmatched,
                r.abandoned
            )?;
        }
        for f in &python.functions {
            writeln!(
                out,
                "  {} {} ({}:{}): {} segments, inclusive {} ns, selected-self {} ns, yields={}, unwinds={}",
                f.kind,
                f.name,
                f.file,
                f.line,
                f.segments,
                f.total_ns,
                f.self_ns.as_deref().unwrap_or("not applicable"),
                f.yields,
                f.unwinds
            )?;
        }
        writeln!(
            out,
            "  Receiver drops={}, malformed={}; user ranges are elapsed lifetimes and can include suspension.",
            python.receiver_dropped, python.malformed
        )?;
    }
    if let Some(torch) = &report.pytorch {
        writeln!(
            out,
            "\nPyTorch/Kineto imported activities (source clocks are separate; source profiler loss is unknown):"
        )?;
        for a in &torch.activities {
            writeln!(
                out,
                "  {} {:<40} {} observations, total {} ns, CPU contained-self {} ns, bytes {}",
                a.role,
                a.name,
                a.count,
                a.total_ns,
                a.self_ns.as_deref().unwrap_or("unavailable/not applicable"),
                a.bytes.as_deref().unwrap_or("unavailable")
            )?;
        }
        for m in &torch.memory {
            writeln!(
                out,
                "  {} device {:?}: peak observed {} B, last {} B ({} samples)",
                m.name, m.device, m.peak_bytes, m.last_bytes, m.samples
            )?;
        }
    }
    if let Some(pmu) = &report.pmu {
        writeln!(
            out,
            "\nPMU counters (cumulative raw counts; independent events, scaled values are estimates):"
        )?;
        for event in &pmu.events {
            writeln!(
                out,
                "  {} / {}: {} [{}; {}]; raw={} {}; enabled={} ns running={} ns ({}%); scaled={}; final={}",
                event.shard,
                event.name,
                event.status,
                event.scope,
                event.privilege,
                event.raw.as_deref().unwrap_or("unavailable"),
                event.unit,
                event.time_enabled_ns.as_deref().unwrap_or("unavailable"),
                event.time_running_ns.as_deref().unwrap_or("unavailable"),
                event.running_percent.as_deref().unwrap_or("unknown"),
                event.scaled_estimate.as_deref().unwrap_or("unavailable"),
                event.final_read
            )?;
        }
        for metrics in &pmu.metrics {
            writeln!(
                out,
                "  {}: estimated IPC={}; cache-miss/reference percent={} (independently scheduled counters)",
                metrics.shard,
                metrics.ipc_estimate.as_deref().unwrap_or("unavailable"),
                metrics
                    .cache_miss_percent_estimate
                    .as_deref()
                    .unwrap_or("unavailable")
            )?;
        }
        for host in &pmu.hosts {
            writeln!(out, "  PMU source host {}:", host.shard)?;
            for (key, value) in &host.metadata {
                writeln!(out, "    {key}: {value}")?;
            }
        }
    }
    if let Some(cuda) = &report.cuda {
        writeln!(
            out,
            "\nCUDA devices (GPU intervals are distinct from host API duration):"
        )?;
        for device in &cuda.devices {
            writeln!(
                out,
                "  GPU {} / {}{}: {} kernels, {} copies, {} memsets; kernel sum {} ns; copied {} B",
                device.device,
                device.name,
                if device.kernels == "0" && device.copies == "0" && device.memsets == "0" {
                    " (metadata only)"
                } else {
                    ""
                },
                device.kernels,
                device.copies,
                device.memsets,
                device.kernel_ns,
                device.copy_bytes
            )?;
        }
        for runtime in &cuda.runtimes {
            writeln!(
                out,
                "  CUPTI {} / CUDA SDK {}: {}; GPU clock interpolation uncertainty unavailable",
                runtime.cupti_version, runtime.cuda_version, runtime.clock_method
            )?;
        }
        writeln!(out, "CUDA host APIs (runtime/driver intervals can nest):")?;
        for api in &cuda.apis {
            writeln!(
                out,
                "  {:<7} {:<40} {:>8} calls  total {} ns  max {} ns  errors {}",
                api.api_domain, api.name, api.calls, api.total_ns, api.max_ns, api.errors
            )?;
        }
        writeln!(out, "CUDA collection health:")?;
        for source in &cuda.sources {
            writeln!(
                out,
                "  {}: {} initialized images; finalized={}; native attempted={} received={} ingress drops={}; CUPTI drops={} source errors={}",
                source.shard,
                source.initialized_images,
                source.finalized,
                source.attempted,
                source.received,
                source.ingress_dropped,
                source.cupti_dropped,
                source.source_errors
            )?;
            writeln!(
                out,
                "    unusable timing={} unattributed={} unresolved correlations={} ambiguous correlations={}",
                source.invalid_timing,
                source.unattributed,
                source.unresolved_correlations,
                source.ambiguous_correlations
            )?;
        }
    }
    if let Some(omp) = &report.openmp {
        writeln!(
            out,
            "\nOpenMP runtime capabilities (registration is not proof every construct was observed):"
        )?;
        for runtime in &omp.runtimes {
            writeln!(
                out,
                "  {} / process {}: {} (OpenMP {}), initialized={}, finalized={}",
                runtime.shard,
                runtime.pid,
                runtime.version,
                runtime.omp_version,
                runtime.initialized,
                runtime.finalized
            )?;
            for callback in &runtime.callbacks {
                writeln!(
                    out,
                    "    {:<34} {:<18} received {}{}",
                    callback.name,
                    callback.status,
                    callback.observed.as_deref().unwrap_or("unknown"),
                    if callback.observed_complete {
                        ""
                    } else {
                        " (checkpoint/unknown)"
                    }
                )?;
            }
        }
        writeln!(
            out,
            "OpenMP recorded tasks: {} created, {} completed, {} scheduled execution segments",
            omp.tasks_created, omp.tasks_completed, omp.execution_segments
        )?;
        for region in &omp.regions {
            writeln!(
                out,
                "  Parallel {} / process {}: requested {}, observed {} threads; duration {} ns; recorded work {} ns; wait {} ns; work spread {} ns",
                region.parallel_id,
                region.pid,
                region.requested_threads,
                region.observed_threads,
                region.duration_ns.as_deref().unwrap_or("unknown"),
                region.work_ns,
                region.wait_ns,
                region.work_spread_ns.as_deref().unwrap_or("unknown")
            )?;
        }
        writeln!(
            out,
            "  Work/sync durations are inclusive observations; scheduled time is not CPU utilization."
        )?;
    }
    if !report.mpi.is_empty() {
        writeln!(
            out,
            "\nMPI API calls by rank (request lifetimes excluded; bytes are local logical extents):"
        )?;
        for call in &report.mpi {
            writeln!(
                out,
                "  rank {:>4} {:<22} {:>8} calls  total {} ns  max {} ns  errors {}  bytes {} ({} calls with byte information)",
                call.rank,
                call.name,
                call.calls,
                call.total_ns,
                call.max_ns,
                call.errors,
                call.logical_bytes,
                call.calls_with_bytes
            )?;
        }
    }
    if !report.io.is_empty() {
        writeln!(
            out,
            "\nPOSIX I/O by file (API bytes; rates use the observed file activity window):"
        )?;
        for file in &report.io {
            let bytes = file.read_bytes.parse::<u128>()? + file.write_bytes.parse::<u128>()?;
            let window = file.window_ns.parse::<u128>()?;
            let rate = if window > 0 {
                format!(
                    "{:.3} MiB/s",
                    bytes as f64 * 1e9 / window as f64 / 1_048_576.0
                )
            } else {
                "n/a".into()
            };
            writeln!(
                out,
                "  {} ({}; domain {})",
                file.file, file.host, file.domain
            )?;
            writeln!(
                out,
                "    {} calls, {} errors; read {} / requested {} B; wrote {} / requested {} B; {}",
                file.operations,
                file.errors,
                file.read_bytes,
                file.requested_read_bytes,
                file.write_bytes,
                file.requested_write_bytes,
                rate
            )?;
            for operation in &file.by_operation {
                let count = operation.count.parse::<u128>()?;
                let average = operation.total_ns.parse::<u128>()? / count.max(1);
                writeln!(
                    out,
                    "    {:<10} {:>8} calls  avg {} ns  max {} ns  errors {}",
                    operation.name, operation.count, average, operation.max_ns, operation.errors
                )?;
            }
        }
    }
    for warning in &summary.warnings {
        writeln!(out, "Note: {warning}")?;
    }
    Ok(())
}
