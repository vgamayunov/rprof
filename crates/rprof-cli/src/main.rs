#[cfg(not(target_os = "linux"))]
compile_error!("rprof process supervision currently requires Linux");

mod capture_sink;
mod mpi_collector;
mod native_collector;
mod openmp_collector;
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
    /// Bounded raw-event queue per native collector (I/O or MPI).
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
