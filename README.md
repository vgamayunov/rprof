# rprof

A Rust-based application profiler for MPI, OpenMP, CUDA, I/O, and hardware
performance counters. Linux is the initial target.

Recommended implementation: a **Rust core with thin C/C++ instrumentation adapters**
where native APIs warrant them, plus a **TypeScript browser UI**. See the
[language decision](docs/architecture.md#language-decision) for alternatives.

The main report will be an interactive HTML timeline showing MPI ranks, CPU
threads, GPU streams, and I/O activity on a shared time axis. Text summaries and
self-contained HTML reports will support command-line and batch workflows.

Another planned capability is **hardware what-if projection**: estimate how a
profiled application would perform on a different CPU, GPU, interconnect, storage
system, or node count, including hardware unavailable for direct testing. Outputs
will include runtime/scaling estimates, bottleneck analysis, assumptions and
uncertainty ranges, and a projected timeline alongside the measured baseline.
See the [projection plan](docs/hardware-projection.md).

## Implemented: capture foundation

- Rust workspace: shared schema, streaming capture library, and CLI.
- Linux process-group supervision, signal forwarding, escalation, and descendant reaping.
- Bounded chunk buffers and writer queue, with explicit dropped-event accounting.
- Recoverable JSON Lines captures with session/shard IDs and clock-domain metadata.
- Deterministic, bounded-memory shard merging and text summaries.
- Tracks, spans, typed attributes, dependency links, and counter records for future collectors.

MPI, OpenMP, CUDA, I/O interception, PMU collection, and HTML rendering are planned;
they are not implemented yet. The process span covers the supervised workload,
including launch, wait, and descendant cleanup. Running `mpirun` currently measures
the launcher; automatic rank/thread discovery comes with the respective collectors.

## Quick start

Install a current stable Rust toolchain (edition 2024; minimum declared Rust 1.85).

```sh
cargo build --workspace
cargo run -p rprof-cli -- --help
cargo run -p rprof-cli -- record -o demo.rprof.jsonl -- /bin/sleep 0.1
cargo run -p rprof-cli -- report demo.rprof.jsonl
```

Workloads inherit the current environment, working directory, and standard I/O.
`record` returns the workload's exit code, or 128 + signal if the workload dies by
signal. If the profiler receives an interruption signal, it forwards that signal
and returns 128 + the interruption signal, retaining the actual workload status in
the capture. The default termination grace is two seconds (`--grace-ms`). Remaining
members of the workload process group are terminated when its leader exits.

Existing output files are never overwritten. Reports can read incomplete captures
and bootstrap v1 JSON captures. Executable arguments are passed losslessly to the
child; non-UTF-8 bytes in the JSON command description are replaced for display.

### Multiple shards

Use one session UUID across related captures and `--output-dir` for unique names:

```sh
export RPROF_SESSION_ID=00000000-0000-4000-8000-000000000001
target/debug/rprof record --output-dir captures -- /bin/sleep 0.1
target/debug/rprof record --output-dir captures -- /bin/sleep 0.2
target/debug/rprof merge -o merged.rprof.jsonl captures/*.rprof.jsonl
target/debug/rprof report merged.rprof.jsonl
unset RPROF_SESSION_ID
```

Use a fresh UUID for each real profiling session. `--session UUID` overrides the
inherited session; otherwise `record` generates one. The session and current shard
IDs are propagated to child environments for nested captures and future collectors.
Each `record` invocation creates one shard; forked children do not yet emit separate
instrumentation shards. Merge preserves separate clock domains and reports
cross-host timestamps as unaligned.

See [capture format and lifecycle](docs/capture-format.md) for buffering limits,
recovery semantics, and the process-group supervision boundary.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

See [architecture](docs/architecture.md), the [implementation plan](docs/roadmap.md),
and [testing](docs/testing.md), including the GPU/MPI test environments.
