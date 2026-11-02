use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use rprof_project::*;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Json,
    Text,
    Html,
}
#[derive(Clone, Copy, ValueEnum)]
enum Scale {
    Fixed,
    Strong,
    Weak,
}
impl From<Scale> for Scaling {
    fn from(s: Scale) -> Self {
        match s {
            Scale::Fixed => Self::Fixed,
            Scale::Strong => Self::Strong,
            Scale::Weak => Self::Weak,
        }
    }
}
#[derive(Args)]
pub struct ProjectArgs {
    input: PathBuf,
    /// Emit bounded evidence inventory and stable span IDs for authoring a model.
    #[arg(long)]
    inspect: bool,
    #[arg(long)]
    source_hardware: Option<PathBuf>,
    /// Repeat to compare multiple offline target profiles.
    #[arg(long)]
    target_hardware: Vec<PathBuf>,
    #[arg(long)]
    workload_model: Option<PathBuf>,
    /// Replay an exported editor request, reloading evidence from INPUT.
    #[arg(long)]
    request: Option<PathBuf>,
    /// Explicit clock domain; required for a multi-domain inspection.
    #[arg(long)]
    domain: Option<String>,
    /// Node-count sweep; defaults to each target profile's node count.
    #[arg(long,value_delimiter=',',value_parser=clap::value_parser!(u32).range(1..=1_000_000))]
    nodes: Vec<u32>,
    #[arg(long, value_enum)]
    scaling: Option<Scale>,
    /// JSON array of held-out observations; never used to fit parameters.
    #[arg(long)]
    validation: Option<PathBuf>,
    #[arg(long,value_enum,default_value_t=Format::Json)]
    format: Format,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long,default_value_t=10000,value_parser=clap::value_parser!(u32).range(1..=10000))]
    max_events: u32,
}
fn load<T: serde::de::DeserializeOwned>(path: &Path, limit: usize) -> Result<T> {
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "{} exceeds input size limit",
        path.display()
    );
    serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))
}
fn evidence(path: &Path, domain: Option<&str>, max_events: u32) -> Result<Baseline> {
    // Hash and analyze the same frozen byte sequence, including incomplete tails.
    // Disk usage is bounded by the input's initial length; RAM is bounded separately.
    let source = File::open(path)?;
    let length = source.metadata()?.len();
    let mut input = source.take(length);
    let mut snapshot = tempfile::NamedTempFile::new()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        snapshot.write_all(&buffer[..n])?;
    }
    snapshot.flush()?;
    ensure!(
        snapshot.as_file().metadata()?.len() == length,
        "capture shrank while taking snapshot"
    );
    let capture_sha256 = format!("{:x}", hasher.finalize());
    let report = rprof_report::analyze(
        snapshot.path(),
        rprof_report::Limits {
            retain_timeline: true,
            max_events: max_events as usize,
            max_bytes: 32 * 1024 * 1024,
        },
    )?;
    let selected = if let Some(id) = domain {
        report
            .domains
            .iter()
            .find(|d| d.id == id)
            .context("unknown projection clock domain")?
    } else {
        ensure!(
            report.domains.len() == 1,
            "projection requires one clock domain; inspect/select --domain ID"
        );
        &report.domains[0]
    };
    let shards: BTreeSet<_> = report
        .shards
        .iter()
        .filter(|s| s.domain == selected.id)
        .map(|s| s.id.as_str())
        .collect();
    let tracks: BTreeMap<_, _> = report.tracks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut spans = Vec::new();
    for span in &report.spans {
        let Some(track) = tracks.get(span.track.as_str()) else {
            continue;
        };
        if !shards.contains(track.shard.as_str()) {
            continue;
        }
        let mut lane = *track;
        // I/O tracks are per-file children of a physical executing thread.
        if lane.kind == "io" {
            if let Some(parent) = lane.parent.as_ref().and_then(|p| tracks.get(p.as_str())) {
                lane = parent;
            }
        }
        spans.push(ObservedSpan {
            id: span.source_id.clone(),
            name: span.name.clone(),
            category: span.category.clone(),
            lane: lane.id.clone(),
            start_ns: span.start_ns.clone(),
            duration_ns: span.duration_ns.clone(),
            incomplete: span.incomplete,
            enclosing: span.category == "process" && span.name.starts_with("supervised workload"),
            attributes: span
                .attributes
                .iter()
                .take(128)
                .map(|(k, v)| (k.clone(), v.chars().take(4096).collect()))
                .collect(),
        });
    }
    let selected_shards: Vec<_> = report
        .shards
        .iter()
        .filter(|s| shards.contains(s.id.as_str()))
        .collect();
    let mut complete = selected_shards
        .iter()
        .all(|s| s.complete && s.end_reason == "Completed" && s.exit_code == Some(0))
        && !report.summary.truncated_tail
        && report.summary.format != "legacy v1 JSON";
    if let Some(cuda) = &report.cuda {
        complete &= cuda
            .sources
            .iter()
            .filter(|s| s.domain == selected.id)
            .all(|s| {
                s.finalized
                    && s.source_errors == "0"
                    && s.invalid_timing == "0"
                    && s.unattributed == "0"
            });
    }
    if let Some(omp) = &report.openmp {
        complete &= omp
            .runtimes
            .iter()
            .filter(|r| r.domain == selected.id)
            .all(|r| {
                r.finalized
                    && r.sequence_gaps == "0"
                    && r.unmatched_endpoints == "0"
                    && r.abandoned_intervals == "0"
            });
    }
    if let Some(pmu) = &report.pmu {
        complete &= pmu
            .events
            .iter()
            .filter(|e| e.domain == selected.id)
            .all(|e| {
                e.status != "read_error"
                    && (!matches!(e.status.as_str(), "available" | "measured" | "not_counted")
                        || e.final_read)
            });
    }
    if let Some(python) = &report.python {
        complete &= python.malformed == "0"
            && python.runtimes.iter().all(|r| {
                r.initialized
                    && r.finalized
                    && r.source_errors == "0"
                    && r.unmatched == "0"
                    && r.abandoned == "0"
                    && r.inflight == "0"
            });
    }
    let dropped = selected_shards
        .iter()
        .try_fold(0u64, |sum, s| -> Result<u64> {
            sum.checked_add(s.dropped.parse()?)
                .context("loss count overflow")
        })?;
    let mut metadata = BTreeMap::from([
        ("capture_bytes".into(), length.to_string()),
        (
            "snapshot_policy".into(),
            "frozen initial byte prefix".into(),
        ),
        ("provenance".into(), report.provenance.kind.clone()),
        ("capture_format".into(), report.summary.format.clone()),
        (
            "observed_hosts".into(),
            selected_shards
                .iter()
                .map(|s| &s.hostname)
                .collect::<BTreeSet<_>>()
                .len()
                .to_string(),
        ),
        (
            "recorded_dependency_links".into(),
            report.summary.dependencies.clone(),
        ),
    ]);
    if let Some(pmu) = &report.pmu {
        for host in pmu.hosts.iter().filter(|h| h.domain == selected.id) {
            for (key, value) in &host.metadata {
                if metadata.len() < 256 {
                    metadata.insert(format!("{}.{}", host.shard, key), value.clone());
                }
            }
        }
    }
    if let Some(cuda) = &report.cuda {
        for d in cuda.devices.iter().filter(|d| d.domain == selected.id) {
            if metadata.len() < 250 {
                metadata.insert(
                    format!("{}.gpu.{}", d.shard, d.device),
                    format!(
                        "{}; {}; memory {:?}; CC {:?}.{:?}",
                        d.name, d.identity, d.memory_bytes, d.compute_major, d.compute_minor
                    ),
                );
            }
        }
    }
    Ok(Baseline {
        capture_sha256,
        session_id: report.summary.session_id,
        domain: selected.id.clone(),
        duration_ns: selected.end_ns.clone(),
        complete,
        dropped: dropped.to_string(),
        omitted: report.summary.omitted_events,
        clock_uncertainty_ns: selected
            .calibration
            .as_ref()
            .map(|c| c.uncertainty_ns.clone()),
        warnings: report.summary.warnings,
        metadata,
        spans,
    })
}
fn text(projection: &Projection, out: &mut dyn Write) -> Result<()> {
    writeln!(
        out,
        "PROJECTED / {} — {}",
        projection.engine_version, projection.request.workload.name
    )?;
    writeln!(
        out,
        "Observed window: {} ns; modeled coverage {}%; source reconstruction {} ns (consistency check)",
        projection.baseline_runtime_ns,
        projection.coverage_percent,
        projection.source_reconstructed_ns
    )?;
    for (name, hash) in &projection.hashes {
        writeln!(out, "SHA256 {name}: {hash}")?;
    }
    for assumption in &projection.assumptions {
        writeln!(out, "Assumption: {assumption}")?;
    }
    for s in &projection.scenarios {
        writeln!(
            out,
            "\n{} / {} nodes / {:?}: {}",
            s.target_name, s.nodes, s.scaling, s.status
        )?;
        if let Some(r) = &s.runtime {
            writeln!(
                out,
                "  Estimated runtime: {} ns; evaluated range {}..{} ns; speedup {}x; efficiency {}",
                r.central_ns,
                r.optimistic_ns,
                r.pessimistic_ns,
                s.speedup.as_deref().unwrap_or("unknown"),
                s.efficiency.as_deref().unwrap_or("not applicable")
            )?;
        }
        for issue in &s.issues {
            writeln!(out, "  Unresolved: {issue}")?;
        }
        for warning in &s.warnings {
            writeln!(out, "  Note: {warning}")?;
        }
        for t in &s.tasks {
            writeln!(
                out,
                "  {} [{}]: observed {} ns -> projected {} ns at {} ns; {}; critical={}",
                t.name,
                t.model,
                t.baseline_duration_ns,
                t.duration_ns,
                t.start_ns,
                t.bottleneck,
                t.critical
            )?;
            writeln!(out, "    {}", t.assumption)?;
        }
        if !s.critical_path.is_empty() {
            writeln!(
                out,
                "  Modeled critical phase path: {}",
                s.critical_path.join(" -> ")
            )?;
        }
        for r in &s.resources {
            writeln!(
                out,
                "  Resource {}: {} busy ns across {} modeled slots ({}% occupancy)",
                r.resource, r.busy_ns, r.slots, r.modeled_busy_percent
            )?;
        }
        for v in &s.validation {
            writeln!(
                out,
                "  Held-out observation {} ns; signed error {}%; {}",
                v.observed_ns, v.signed_error_percent, v.provenance.source
            )?;
        }
    }
    for warning in &projection.warnings {
        writeln!(out, "Note: {warning}")?;
    }
    Ok(())
}
pub fn run(args: ProjectArgs) -> Result<()> {
    ensure!(
        !args.inspect
            || (args.source_hardware.is_none()
                && args.target_hardware.is_empty()
                && args.workload_model.is_none()
                && args.request.is_none()
                && args.nodes.is_empty()
                && args.scaling.is_none()
                && args.validation.is_none()
                && args.format == Format::Json),
        "--inspect emits JSON evidence and does not take model inputs"
    );
    ensure!(
        args.inspect
            || args.request.is_some()
            || (args.source_hardware.is_some()
                && !args.target_hardware.is_empty()
                && args.workload_model.is_some()),
        "projection requires --source-hardware, --target-hardware, and --workload-model (or --request); use --inspect to inspect baseline evidence"
    );
    ensure!(
        args.request.is_none()
            || (args.source_hardware.is_none()
                && args.target_hardware.is_empty()
                && args.workload_model.is_none()),
        "--request cannot be combined with hardware/workload files"
    );
    if let Some(path) = &args.output {
        ensure!(!path.exists(), "output {} already exists", path.display());
    }
    let mut request: Option<Request> = args
        .request
        .as_ref()
        .map(|p| load(p, MAX_REQUEST_BYTES))
        .transpose()?;
    let model: Option<WorkloadModel> = args
        .workload_model
        .as_ref()
        .map(|p| load(p, 4 * 1024 * 1024))
        .transpose()?;
    let domain = args
        .domain
        .as_deref()
        .or_else(|| request.as_ref().map(|r| r.baseline.domain.as_str()))
        .or_else(|| model.as_ref().map(|m| m.domain.as_str()));
    let baseline = evidence(&args.input, domain, args.max_events)?;
    let output = if args.inspect {
        serde_json::to_value(&baseline)?
    } else {
        let mut request = if let Some(mut loaded) = request.take() {
            ensure!(
                loaded.baseline.capture_sha256 == baseline.capture_sha256,
                "editor request belongs to a different capture"
            );
            loaded.baseline = baseline;
            loaded
        } else {
            Request {
                version: 1,
                baseline,
                source: load(
                    args.source_hardware
                        .as_deref()
                        .context("--source-hardware is required")?,
                    4 * 1024 * 1024,
                )?,
                targets: args
                    .target_hardware
                    .iter()
                    .map(|p| load(p, 4 * 1024 * 1024))
                    .collect::<Result<_>>()?,
                workload: model.context("--workload-model is required")?,
                nodes: vec![],
                scaling: Scaling::Fixed,
                validation: vec![],
            }
        };
        if !args.nodes.is_empty() {
            request.nodes = args.nodes;
        }
        if let Some(scaling) = args.scaling {
            request.scaling = scaling.into();
        }
        if let Some(path) = &args.validation {
            request.validation = load(path, 4 * 1024 * 1024)?;
        }
        ensure!(
            serde_json::to_vec(&request)?.len() <= MAX_REQUEST_BYTES,
            "resolved request exceeds 32 MiB"
        );
        serde_json::to_value(rprof_project::project(request)?)?
    };
    let render = |out: &mut dyn Write| -> Result<()> {
        match args.format {
            Format::Json => {
                serde_json::to_writer_pretty(&mut *out, &output)?;
                writeln!(out)?;
                Ok(())
            }
            Format::Text => text(&serde_json::from_value(output.clone())?, out),
            Format::Html => rprof_report::write_html_payload(
                &output,
                out,
                Some(include_bytes!("../../../ui/dist/project.wasm")),
            ),
        }
    };
    if let Some(path) = args.output {
        crate::report::write_artifact(&path, render)?;
        eprintln!("Projection written to {}", path.display());
    } else {
        render(&mut std::io::stdout().lock())?;
    }
    Ok(())
}
