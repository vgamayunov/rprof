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
    for warning in &summary.warnings {
        writeln!(out, "Note: {warning}")?;
    }
    Ok(())
}
