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
