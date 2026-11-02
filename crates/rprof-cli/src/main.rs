#[cfg(not(target_os = "linux"))]
compile_error!("rprof process supervision currently requires Linux");

mod capture_sink;
mod cuda_collector;
mod mpi_collector;
mod native_collector;
mod openmp_collector;
mod pmu_collector;
mod project;
mod python_collector;
mod pytorch_import;
mod record;
mod report;
mod supervisor;
mod view;

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{Parser, Subcommand};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "rprof",
    version,
    about = "Application profiler for HPC workloads"
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Supervise a workload and stream a recoverable process capture.
    Record(RecordArgs),
    /// Generate a text summary, offline HTML timeline, or Chrome Trace/Perfetto JSON.
    Report(report::ReportArgs),
    /// Project a captured workload onto declared hardware profiles and assumptions.
    Project(project::ProjectArgs),
    /// Convert torch.profiler/Kineto Chrome JSON into a capture with a separate source clock.
    ImportPytorch(pytorch_import::ImportArgs),
    /// Serve an offline report snapshot on a loopback-only local browser endpoint.
    View(view::ViewArgs),
    /// Merge same-session capture shards deterministically, preserving clock domains.
    Merge {
        #[arg(short, long)]
        output: PathBuf,
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
    },
}

#[derive(clap::Args)]
pub(crate) struct RecordArgs {
    /// Output file (default: capture.rprof.jsonl). Must not already exist.
    #[arg(short, long, conflicts_with = "output_dir")]
    output: Option<PathBuf>,
    /// Generate a unique per-shard filename in this directory.
    #[arg(long)]
    output_dir: Option<PathBuf>,
    /// Shared session UUID; otherwise inherit RPROF_SESSION_ID or generate one.
    #[arg(long)]
    session: Option<Uuid>,
    #[arg(long, default_value_t = 256)]
    chunk_events: usize,
    #[arg(long, default_value_t = 8)]
    queue_chunks: usize,
    #[arg(long, default_value_t = 100)]
    flush_interval_ms: u64,
    /// Collectors to enable; process supervision is always active.
    #[arg(
        long = "collect",
        value_enum,
        value_delimiter = ',',
        default_value = "process"
    )]
    collectors: Vec<CollectorKind>,
    /// Path to librprof_io.so (default: alongside the rprof executable).
    #[arg(long)]
    io_library: Option<PathBuf>,
    /// Bounded raw-event queue per collector (Python supports at most 32768).
    #[arg(long = "native-buffer-events", alias = "io-buffer-events", default_value_t = 16384, value_parser = clap::value_parser!(u32).range(1..=262144))]
    io_buffer_events: u32,
    /// Target-built PMPI library (default: librprof_mpi.so alongside rprof).
    #[arg(long)]
    mpi_library: Option<PathBuf>,
    /// Calibrate MPI rank clocks at init/finalize using a private communicator.
    #[arg(long)]
    mpi_clock_sync: bool,
    /// OMPT tool library (default: librprof_ompt.so alongside rprof).
    #[arg(long)]
    openmp_library: Option<PathBuf>,
    /// Target-built CUPTI injection library (default: librprof_cuda.so alongside rprof).
    #[arg(long)]
    cuda_library: Option<PathBuf>,
    /// Perf event names or rHEX raw encodings (default: cycles,instructions,cache-references,cache-misses,task-clock).
    #[arg(long, value_delimiter = ',')]
    pmu_events: Vec<String>,
    /// Count the root thread alone, or aggregate it with inherited threads/children.
    #[arg(long, value_enum, default_value_t = PmuScope::WorkloadTree)]
    pmu_scope: PmuScope,
    /// Include kernel execution (default: userspace only; no permission fallback).
    #[arg(long)]
    pmu_kernel: bool,
    /// Approximate counter-read interval; final counts are always read after cleanup.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u64).range(10..=60000))]
    pmu_interval_ms: u64,
    /// Python source roots to instrument (repeatable; default: working directory).
    #[arg(long)]
    python_include: Vec<PathBuf>,
    /// CPython instrumentation backend; auto uses sys.monitoring when available.
    #[arg(long, value_enum, default_value_t=PythonBackend::Auto)]
    python_backend: PythonBackend,
    /// Include <string>/<stdin> code; also enabled for a direct python -c/- launch.
    #[arg(long)]
    python_inline: bool,
    /// Grace before SIGKILL after interruption or leader exit with live descendants.
    #[arg(long, default_value_t = 2000, value_parser = clap::value_parser!(u64).range(0..=60000))]
    grace_ms: u64,
    /// Executable and arguments, following --.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<OsString>,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum CollectorKind {
    Process,
    Io,
    Mpi,
    #[value(alias = "omp")]
    Openmp,
    Cuda,
    Pmu,
    Python,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum PythonBackend {
    Auto,
    Monitoring,
    Profile,
    Ranges,
}
impl PythonBackend {
    fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Monitoring => "monitoring",
            Self::Profile => "profile",
            Self::Ranges => "ranges",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum PmuScope {
    WorkloadTree,
    RootThread,
}
impl PmuScope {
    fn name(self) -> &'static str {
        match self {
            Self::WorkloadTree => "workload-tree",
            Self::RootThread => "root-thread",
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("rprof: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<u8> {
    match cli.command {
        Action::Record(args) => record::record(args),
        Action::Report(args) => {
            report::report(args)?;
            Ok(0)
        }
        Action::Project(args) => {
            project::run(args)?;
            Ok(0)
        }
        Action::ImportPytorch(args) => {
            pytorch_import::run(args)?;
            Ok(0)
        }
        Action::View(args) => {
            view::view(args)?;
            Ok(0)
        }
        Action::Merge { output, inputs } => {
            let summary = rprof_capture::merge(&inputs, &output)?;
            eprintln!(
                "Merged {} shards into {} ({})",
                summary.shards.len(),
                output.display(),
                if summary.is_complete() {
                    "complete"
                } else {
                    "partial/recovered"
                }
            );
            Ok(0)
        }
    }
}
