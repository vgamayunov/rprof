use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use rprof_core::{Capture, SCHEMA_VERSION};

pub(crate) fn report(path: &Path) -> Result<()> {
    let mut reader =
        BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?);
    // Bootstrap v1 captures are pretty-printed JSON objects, v2 begins with a
    // compact header frame. Probe a bounded line, then reopen for actual reading.
    let mut first = String::new();
    (&mut reader)
        .take(rprof_capture::MAX_FRAME_BYTES as u64)
        .read_line(&mut first)?;
    let stream = serde_json::from_str::<serde_json::Value>(&first)
        .ok()
        .is_some_and(|v| v.get("record").is_some());
    let file = File::open(path)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if !stream {
        ensure!(
            file.metadata()?.len() <= 16 * 1024 * 1024,
            "legacy v1 capture exceeds 16 MiB; use streaming v2 for large captures"
        );
        let capture: Capture =
            serde_json::from_reader(BufReader::new(file.take(16 * 1024 * 1024 + 1)))
                .context("reading legacy capture (or invalid stream header)")?;
        ensure!(
            capture.schema_version == SCHEMA_VERSION,
            "unsupported legacy schema version {}",
            capture.schema_version
        );
        writeln!(out, "Format:     legacy v1 JSON")?;
        writeln!(out, "Command:    {:?}", capture.metadata.command)?;
        writeln!(
            out,
            "Collectors: {}",
            capture.metadata.collectors.join(", ")
        )?;
        writeln!(
            out,
            "Wall time:  {:.6} s",
            capture.metadata.duration_ns as f64 / 1e9
        )?;
        writeln!(out, "Exit code:  {:?}", capture.metadata.exit_code)?;
        writeln!(out, "Signal:     {:?}", capture.metadata.signal)?;
        writeln!(out, "Tracks:     {}", capture.tracks.len())?;
        writeln!(out, "Spans:      {}", capture.spans.len())?;
        writeln!(out, "Counters:   {}", capture.counters.len())?;
        return Ok(());
    }
    let summary = rprof_capture::summarize(BufReader::new(file))?;
    writeln!(
        out,
        "Format:     rprof stream v{}",
        summary.header.schema_version
    )?;
    writeln!(out, "Session:    {}", summary.header.session_id)?;
    writeln!(
        out,
        "Status:     {}",
        if summary.is_complete() {
            "complete"
        } else {
            "partial/recovered"
        }
    )?;
    writeln!(out, "Shards:     {}", summary.shards.len())?;
    writeln!(
        out,
        "Tail:       {}",
        if summary.truncated_tail {
            "truncated (ignored)"
        } else {
            "intact"
        }
    )?;
    writeln!(out, "Recovered inputs: {}", summary.header.recovered_inputs)?;
    let domains: BTreeSet<_> = summary
        .shards
        .values()
        .map(|s| (&s.shard.hostname, &s.shard.boot_id))
        .collect();
    writeln!(
        out,
        "Clock domains: {}{}",
        domains.len(),
        if domains.len() > 1 {
            " (unaligned across hosts/boots)"
        } else {
            " (same-domain monotonic origins recorded)"
        }
    )?;
    for (id, shard) in &summary.shards {
        writeln!(out, "\nShard:      {id}")?;
        writeln!(out, "Host:       {}", shard.shard.hostname)?;
        writeln!(out, "Command:    {:?}", shard.shard.command)?;
        writeln!(
            out,
            "Collectors: {}",
            shard
                .shard
                .collectors
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )?;
        let duration = shard
            .end
            .as_ref()
            .map_or(shard.observed_ns, |e| e.duration_ns);
        writeln!(
            out,
            "{} {:.6} s",
            if shard.end.is_some() {
                "Duration:  "
            } else {
                "Observed:  "
            },
            duration as f64 / 1e9
        )?;
        if let Some(end) = &shard.end {
            writeln!(out, "End reason: {:?}", end.reason)?;
            writeln!(out, "Exit code:  {:?}", end.exit_code)?;
            writeln!(out, "Signal:     {:?}", end.signal)?;
            writeln!(out, "Interrupted by: {:?}", end.interrupted_by)?;
        } else {
            writeln!(out, "End reason: unknown (missing footer)")?;
        }
        writeln!(
            out,
            "Tracks:     {}\nSpans:      {}\nCounters:   {}\nDependencies: {}",
            shard.tracks, shard.spans, shard.counters, shard.dependencies
        )?;
        writeln!(
            out,
            "Dropped:    {} (queue-full: {}, oversized: {}){}",
            shard.loss.total(),
            shard.loss.queue_full,
            shard.loss.oversized,
            if shard.end.is_none() {
                " — checkpointed lower bound"
            } else {
                ""
            }
        )?;
    }
    Ok(())
}
