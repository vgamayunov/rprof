# rprof

A Rust-based application profiler for MPI, OpenMP, CUDA, I/O, and hardware
performance counters. Linux is the initial target.

Recommended implementation: a **Rust core with thin C/C++ instrumentation adapters**
where native APIs warrant them, plus a **TypeScript browser UI**. See the
[language decision](docs/architecture.md#language-decision) for alternatives.

The main report is an interactive HTML timeline with hierarchical rank, CPU-thread,
GPU-stream, and I/O tracks. Text summaries, self-contained HTML reports, and
Chrome Trace/Perfetto export support command-line and batch workflows. Related
shards share a time axis within their monotonic clock domain; separate hosts/boots
remain visibly unaligned until clock calibration is available.

Another planned capability is **hardware what-if projection**: estimate how a
profiled application would perform on a different CPU, GPU, interconnect, storage
system, or node count, including hardware unavailable for direct testing. Outputs
will include runtime/scaling estimates, bottleneck analysis, assumptions and
uncertainty ranges, and a projected timeline alongside the measured baseline.
See the [projection plan](docs/hardware-projection.md).

Planned [remote operation](docs/remote-operation.md) supports both an SSH-tunneled
remote viewer and a local backend connected to target systems over SSH. The current
snapshot viewer already supports manual SSH port forwarding; automated collection,
session management, reconnectable transfer, and live queries will extend it.

## Implemented: capture foundation and timeline reports

- Rust workspace: shared schema, streaming capture library, and CLI.
- Linux process-group supervision, signal forwarding, escalation, and descendant reaping.
- Bounded chunk buffers and writer queue, with explicit dropped-event accounting.
- Recoverable JSON Lines captures with session/shard IDs and clock-domain metadata.
- Deterministic, bounded-memory shard merging and text summaries.
- Tracks, spans, typed attributes, dependency links, and counter records for future collectors.
- Shared report analysis, offline Canvas timeline, exact-value inspectors, and counter plots.
- Chrome Trace/Perfetto JSON export and a loopback-only local browser viewer.
- Synthetic multitrack fixtures, browser tests, and source/bundle consistency checks.

MPI, OpenMP, CUDA, I/O interception, and PMU collection are planned; they are not
implemented yet. The process span covers the supervised workload,
including launch, wait, and descendant cleanup. Running `mpirun` currently measures
the launcher; automatic rank/thread discovery comes with the respective collectors.

## Quick start

Install a current stable Rust toolchain (edition 2024; minimum declared Rust 1.85).

```sh
cargo build --workspace
cargo run -p rprof-cli -- --help
cargo run -p rprof-cli -- record -o demo.rprof.jsonl -- /bin/sleep 0.1
cargo run -p rprof-cli -- report demo.rprof.jsonl
cargo run -p rprof-cli -- report demo.rprof.jsonl --format html -o demo.html
cargo run -p rprof-cli -- view demo.rprof.jsonl --open
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

### Timeline and reference-viewer reports

Open an exported HTML file directly in a modern browser; it works offline. The
same UI is served by `rprof view`, which prints a `127.0.0.1` URL and accepts a
fixed `--port` when desired. It serves a snapshot, not live capture updates.

```sh
target/debug/rprof report fixtures/multitrack.jsonl --format html -o multitrack.html
target/debug/rprof report fixtures/multitrack.jsonl --format trace --domain 0 -o trace.json
```

The checked-in fixture demonstrates ranks, OpenMP threads, GPUs, I/O, counters, and
dependencies without requiring those collectors or hardware. Use the domain
selector, name/category/rank filter, wheel zoom, drag pan, and clickable bars/points.
See [reporting](docs/reporting.md) for controls, retention limits, clock handling,
and reference-viewer compatibility.

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

Cargo embeds the checked-in UI bundle and needs no Node installation. For UI work,
use Node 24 and the commands in [ui/README.md](ui/README.md). GitHub CI checks Rust,
TypeScript, generated assets, and headless browser interactions.

See [architecture](docs/architecture.md), the [implementation plan](docs/roadmap.md),
and [testing](docs/testing.md), including the GPU/MPI test environments.
