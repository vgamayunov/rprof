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
remain visibly unaligned unless explicit MPI clock calibration is recorded.

**Hardware what-if projection** estimates how a
profiled application would perform on a different CPU, GPU, interconnect, storage
system, or node count, including hardware unavailable for direct testing. Outputs
include runtime/scaling scenarios, modeled bottleneck/critical-path changes,
explicit assumptions, evaluated parameter ranges, and projected phase timelines.
Initial models use user-authored/calibrated hardware profiles and an explicit
phase graph. See [projection models and validation](docs/projection-model.md).

[Python and PyTorch profiling](docs/python-pytorch.md) supports selected CPython
execution segments/ranges, worker processes, scheduled framework-owned collection,
and importing operator/autograd/GPU traces. Source coverage, clock domains, and
instrumentation overhead remain explicit.

[Remote operation](docs/remote-operation.md) uses direct collection on the remote
system and an SSH tunnel to the existing snapshot viewer. This needs no additional
rprof code. Opening existing remote capture files from a local GUI is a later,
read-only feature.

## Implemented: capture, MPI/I/O/OpenMP/CUDA profiling, and timeline reports

- Rust workspace: shared schema, streaming capture library, and CLI.
- Linux process-group supervision, signal forwarding, escalation, and descendant reaping.
- Bounded chunk buffers and writer queue, with explicit dropped-event accounting.
- Recoverable JSON Lines captures with session/shard IDs and clock-domain metadata.
- Deterministic, bounded-memory shard merging and text summaries.
- Tracks, spans, typed attributes, dependency links, and counter records for future collectors.
- Shared report analysis, offline Canvas timeline, exact-value inspectors, and counter plots.
- Chrome Trace/Perfetto JSON export and a loopback-only local browser viewer.
- Synthetic multitrack fixtures, browser tests, and source/bundle consistency checks.
- Optional C/Rust POSIX I/O preload collector with process/thread/file attribution.
- Per-file byte counts, errors, latency summaries, and I/O call timeline spans.
- I/O timeline graphs with throughput/IOPS/latency, rank/process/thread drill-down and optional file lanes.
- Target-built PMPI adapter with named calls, request lifetimes, and rank/thread identity.
- Optional MPI clock calibration with explicit uncertainty and drift reporting.
- OMPT collection for LLVM/Intel OpenMP runtimes, with task/work/wait timelines and capability reporting.
- Target-built GCC 13/libgomp collection, with selectable region/body/API detail and explicit coverage gaps.
- Sampled host CPU/memory timelines, expandable from overview to hosts, logical CPUs and NUMA nodes.
- Optional CUDA 13 CUPTI injection adapter with GPU kernels/copies/memsets, CPU API calls,
  stream tracks, launch correlations, device metadata, and source-health reporting.
- Linux perf counters with pre-exec setup, explicit scope, raw/multiplexing values,
  capability diagnostics, and PMU text/HTML summaries.
- Versioned hardware/workload projection profiles, dependency/resource schedules,
  explicit strong/weak node sweeps, and an offline editor running the Rust engine in WASM.
- CPython instrumented segments and ranges, plus SDK-free PyTorch/Kineto imports
  with CPU/GPU correlations, framework memory counters, and source diagnostics.

Hardware counters and multiplexing have been checked against `perf stat` on HBv4;
software events and unsupported-event behavior were also validated on the H100 VM.
GCC/libgomp uses the explicit `--openmp-backend gomp` adapter; LLVM/Intel use OMPT.
See [current capacity estimates](docs/capacity.md) before scaling up captures.
The process span covers the supervised workload,
including launch, wait, and descendant cleanup. Wrapping only `mpirun` measures
the launcher; use the per-rank launch pattern below for MPI call collection.

## Quick start

See the [brief profiling quick start](docs/quickstart.md) for collection commands
covering I/O, MPI, OpenMP, CUDA, counters, Python and PyTorch.

Install a current stable Rust toolchain (edition 2024; minimum declared Rust 1.85)
and a C compiler for the bundled SQLite viewer index.

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
fixed `--port` when desired. It builds/reuses a persistent index and loads the visible
time window and expanded file pages on demand. See [indexed viewing](docs/indexed-viewer.md).
It serves a fixed capture snapshot, not live capture updates.

```sh
target/debug/rprof report fixtures/multitrack.jsonl --format html -o multitrack.html
target/debug/rprof report fixtures/multitrack.jsonl --format trace --domain 0 -o trace.json
```

The checked-in fixture demonstrates ranks, OpenMP threads, GPUs, I/O, counters, and
dependencies without requiring those collectors or hardware. Use the domain
selector, name/category/rank filter, vertical wheel scrolling, zoom/reset buttons,
drag pan, and clickable bars/points.
See [reporting](docs/reporting.md) for controls, retention limits, clock handling,
and reference-viewer compatibility.

### Collect real POSIX I/O

```sh
make build PROFILE=release
target/release/rprof record --collect io -o io.rprof.jsonl -- ./application
target/release/rprof report io.rprof.jsonl --format html -o io.html
```

The optional `librprof_io.so` is found alongside the CLI, or supplied with
`--io-library`. See [I/O profiling](docs/io-profiling.md) for supported calls,
buffering/overhead, descriptor identity, and coverage boundaries.

### Collect MPI calls

Build the adapter with the application's MPI module and run one profiler per rank:

```sh
make mpi PROFILE=release MPICC=mpicc
export RPROF_SESSION_ID=$(cat /proc/sys/kernel/random/uuid)
mpirun -np 2 -x RPROF_SESSION_ID -x LD_LIBRARY_PATH \
  target/release/rprof record --collect mpi --mpi-clock-sync \
  --output-dir captures/mpi -- ./mpi-application
target/release/rprof merge -o mpi.rprof.jsonl captures/mpi/*.rprof.jsonl
target/release/rprof view mpi.rprof.jsonl --open
```

Use `--collect mpi,io` to combine collectors. MPI calls use their full names on
rank/thread lanes, with separate nonblocking-request lifetimes. Compact rows grow
only for overlaps; empty/single-lane tracks are 24px high. Full-name hover and
filtering keep narrow calls inspectable. Collector-health samples appear in capture
status instead of mostly empty chart rows.

See [MPI profiling](docs/mpi-profiling.md) for coverage, all-rank calibration
requirements, and tested HBv4 settings, and [glibc compatibility](docs/glibc-compatibility.md)
for build/deployment choices.

### Collect OpenMP activity

```sh
make ompt PROFILE=release
target/release/rprof record --collect openmp -o omp.rprof.jsonl -- ./openmp-application
target/release/rprof view omp.rprof.jsonl --open
```

Use the application's intended LLVM `libomp` or Intel `libiomp5` runtime;
`--collect mpi,openmp,io` combines collectors. The tool records callback support and
observed coverage, without substituting a runtime. See [OpenMP profiling](docs/openmp-profiling.md)
and the [GCC/LLVM/Intel support matrix](docs/openmp-runtimes.md).

For actual GCC 13/libgomp on Linux x86_64, build on the target runtime image:

```sh
make gomp PROFILE=release GOMP_CC=gcc
target/release/rprof record --collect openmp --openmp-backend gomp \
  --gomp-detail regions -o gomp.rprof.jsonl -- ./gcc-application
```

See [GNU profiling](docs/gomp-profiling.md) for runtime fingerprint matching,
`bodies`/`full` detail, and coverage semantics.

### Collect CUDA activity

Build the optional adapter against the target CUDA 13 toolkit:

```sh
make build cuda PROFILE=release CUDA_HOME=/usr/local/cuda
CUDA_VISIBLE_DEVICES=0 target/release/rprof record --collect cuda \
  -o gpu.rprof.jsonl -- ./cuda-application
target/release/rprof report gpu.rprof.jsonl --format html -o gpu.html
```

The adapter uses CUDA injection and CUPTI's concurrent activity collection. GPU
execution and CPU API durations remain distinct; device/context/stream identities
and CPU–GPU correlations are preserved. See [CUDA profiling](docs/cuda-profiling.md)
for SDK compatibility, clock normalization, buffer losses, and validation.

### Collect Linux perf counters

For host CPU utilization and memory graphs, use `--collect system` (optionally
`--system-interval-ms 1000`). The timeline expands overview → host → logical CPU/NUMA
node. See [system profiling](docs/system-profiling.md) for definitions and multi-host collection.

```sh
target/release/rprof record --collect pmu -o counters.rprof.jsonl -- ./application
target/release/rprof report counters.rprof.jsonl --format html -o counters.html
```

Choose events with `--pmu-events`, scope with `--pmu-scope workload-tree|root-thread`,
and read frequency with `--pmu-interval-ms`. Unavailable events remain explicit;
counts are never replaced with fabricated zeros. See [PMU profiling](docs/pmu-profiling.md)
for scaling semantics, permissions, source hardware metadata, and validation status.

### Hardware what-if scenarios

```sh
target/release/rprof project baseline.rprof.jsonl --inspect -o evidence.json
target/release/rprof project baseline.rprof.jsonl \
  --source-hardware source.json --target-hardware target.json \
  --workload-model workload.json --format html -o projection.html
```

Author the model from stable evidence IDs and explicit workload assumptions.
`fixtures/projection/` provides a complete synthetic example. Unsupported and
infeasible scenarios have no numeric speedup; parameter-case ranges are not
confidence intervals. See [projection model](docs/projection-model.md) for scope,
replay, validation, and editor controls.

### Python and PyTorch

For a real training workload, start with the [Anemoi quick-start guide](docs/quickstart-anemoi.md):
collect a short PyTorch window together with Python ranges and POSIX I/O, then import/view the results.

```sh
target/release/rprof record --collect python -o python.rprof.jsonl -- python3 app.py
target/release/rprof import-pytorch torch-trace.json -o torch.rprof.jsonl
target/release/rprof report torch.rprof.jsonl --format html -o torch.html
```

Use `--python-include` to select source roots and `--python-backend ranges` for
annotation-only collection. Fine-grained function instrumentation can heavily
perturb tiny functions. See [Python collection](docs/python-profiling.md) and
[PyTorch workflows](docs/pytorch-import.md), including exclusive CUPTI ownership.

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
