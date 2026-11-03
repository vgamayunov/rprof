# Architecture proposal

## Language decision

**Recommendation: Rust for the core, thin C/C++ adapters for native instrumentation,
and TypeScript for the browser UI.** Keep the current Rust workspace.

Rust is suitable for the supervisor, buffered capture pipeline, storage, merging,
analysis, and report generation. It provides native performance and useful memory
and concurrency guarantees in safe code. Instrumentation still needs carefully
audited unsafe code, ABI correctness, and explicit hot-path performance budgets;
choosing Rust alone does not guarantee low overhead.

Use C/C++ selectively for vendor-facing integration:

- POSIX interposition includes variadic APIs such as `open`/`openat`; stable Rust
  cannot define C-variadic wrappers. A small C shim can conditionally read the mode
  argument and forward to a fixed-signature Rust entry point.
- MPI handles/constants and language bindings vary across implementations. Compile
  PMPI adapters against the selected implementation's headers, rather than assume
  one portable binary ABI. Fortran coverage needs an explicit support decision.
- OMPT and CUPTI offer C interfaces and can be bound directly from Rust. Use
  generated bindings or small C/C++ adapters according to header/version complexity;
  keep vendor-specific details outside the core schema and buffering code.
- Define ownership, callback/thread lifetimes, and error handling at every FFI
  boundary. Neither Rust panics nor C++ exceptions may cross these boundaries.

**Best single-native-language alternative: C++20.** It has the most direct fit with
MPI, OpenMP, CUDA, vendor examples, and existing HPC instrumentation libraries.
Choose it for the whole native stack if seamless native integration and team C++
expertise outweigh Rust's safety benefits. Its main costs are manual lifetime and
concurrency correctness and a more involved dependency/build setup. C alone is a
good adapter language but less attractive for the complete analysis/storage stack.

Go is less suitable for injected hot-path collectors because of its runtime and
C interop overhead; it offers little benefit over Rust for this project. Optional
Python/PyTorch integration will use a small application-facing package and native
adapters where needed. Collection infrastructure, analysis, and reports remain
Rust-based. TypeScript handles browser interaction/rendering, with WebAssembly an
option only if browser-side analysis needs it.

## Data flow

```text
rprof record -- workload / launcher
  ├─ process supervisor and collector configuration
  ├─ per-process instrumentation (preload / PMPI / OMPT / CUPTI)
  └─ Linux perf_event_open collector
          ↓ bounded per-thread buffers / per-rank capture shards
       merge + clock normalization + symbolization
          ↓ versioned capture
       report engine
          ├─ text summaries
          └─ HTML report / browser timeline

versioned captures + source/target hardware profiles + workload scaling model
          ↓ projection engine (explicit phase models)
       scenario estimates + projected timeline + assumptions/uncertainty
          ↓ report engine / HTML comparison UI
```

The CLI, shared data model, supervised process capture, streaming storage,
deterministic merge, shared report analysis, offline HTML timeline, local viewer,
and Chrome Trace export are implemented, along with POSIX I/O interposition and
per-file analysis. Initial PMPI call/request profiling and estimated MPI clock
alignment are also implemented. OMPT host collection and cross-runtime capability
reporting are implemented for LLVM/Intel. CUDA 13 CUPTI and Linux perf counting are
implemented, including hardware PMU validation on HBv4. Initial evidence-aware
projection models and the WASM scenario editor are implemented. The target-built
GNU-runtime adapter supports GCC 13/libgomp; broader projection models remain planned.

## Workspace boundaries

| Component | Responsibility |
| --- | --- |
| `crates/rprof-core` (exists) | Capture schema: tracks, spans, counters, metadata |
| `crates/rprof-capture` (exists) | Bounded writer, recovery/validation reader, shard merge and loss accounting |
| `crates/rprof-cli` (exists) | CLI, Linux process supervision, capture configuration, text summary |
| `rprof-collector` (planned) | Native callback lifecycle and allocation-free per-thread ingress buffers |
| `crates/rprof-preload` (exists) | C POSIX wrappers and allocation-free Rust runtime, linked into `librprof_io.so` |
| `crates/rprof-io-protocol` (exists) | Fixed-layout same-host I/O observations and shared counters |
| `crates/rprof-mpi` (exists) | Target-header PMPI wrappers and Rust native transport |
| `crates/rprof-mpi-protocol` (exists) | Pointer-free MPI wire observations |
| `crates/rprof-ompt` (exists) | OMPT callbacks and Rust transport; runtime-negotiated capabilities |
| `crates/rprof-gomp` (exists) | Target-fingerprinted GNU C/assembly ABI adapter; shares the Rust OpenMP transport |
| `crates/rprof-ompt-protocol` (exists) | Pointer-free host callback observations and per-thread sequences |
| `python/rprof_python` (exists) | Optional CPython monitoring/profile/range bootstrap and framework-owned Torch scheduling; sampling remains planned |
| `crates/rprof-pytorch-import` (exists) | SDK-free bounded Kineto/Chrome import, exact source timestamps, memory and flow preservation |
| `crates/rprof-cuda` (exists) | Target-SDK C++ CUPTI injection adapter and Rust native transport |
| `crates/rprof-cuda-protocol` (exists) | Pointer-free CUDA activities, API callbacks, capabilities and diagnostics |
| `crates/rprof-perf` (exists) | Pre-exec Linux perf counters, descriptor handoff, exact readings; sampling remains planned |
| `crates/rprof-report` (exists) | Shared streaming analysis, text/HTML rendering and Chrome Trace export |
| `crates/rprof-project` (exists) | Pure Rust hardware/workload models, bounded phase scheduling, scenario estimates and WASM ABI |
| `ui/` (exists) | TypeScript/Canvas timeline, shared offline/local-viewer assets, browser tests |

Keep MPI and CUDA dependencies optional so the base CLI builds without vendor
SDKs or an MPI installation. Cargo builds the Rust workspace; `make preload` links
the optional C interposition surface around the Rust static library. This separate
link step preserves C variadic exports that a Rust cdylib export map would hide.
Use narrow C ABI boundaries and generated bindings where appropriate. Do not
unwind Rust panics across foreign callbacks.

## Timeline-first data model

- Hierarchy: host/session/shard metadata → process/rank → thread; GPU device →
  stream; I/O resource tracks link operations back to the originating process/thread.
- Spans store monotonic start and duration in integer nanoseconds. Counter samples
  retain integer values for exact hardware counts, or floating-point values for rates.
- Correlation IDs link CPU launches to GPU activity and, eventually, communication
  operations. Typed attributes and shard-local dependency edges are in the streaming
  schema; collector-specific semantics and cross-shard MPI message edges follow later.
- Track IDs and correlation IDs are local to a shard. Composite
  `(session UUID, shard UUID, local ID)` identity avoids collisions during merge.
- Wall-clock start is descriptive metadata, not a synchronization mechanism.
- Cross-host alignment needs offset/drift estimates and reported uncertainty;
  CUPTI timestamps need an explicit mapping to the host clock. Do not infer
  communication overlap from uncalibrated clocks.
- Chunked JSON Lines is the current version-2 streaming format. Frames, writer
  queues, and validation metadata have explicit limits; merger events are spooled
  to disk. A binary/compressed encoding and viewport indices can follow profiling
  of real workloads. See [capture format](capture-format.md) for compatibility.
  JavaScript consumers must preserve u64 timestamps/counters using BigInt-aware
  parsing or a lossless serialized representation rather than plain JS numbers.

## Collector mechanisms

| Area | Preferred mechanism | Important constraints |
| --- | --- | --- |
| MPI | PMPI profiling interface, loaded with the application or via preload | Discover rank/world size after initialization; track nonblocking request lifetimes and communicator identity; handle launcher environment propagation per implementation |
| OpenMP | OMPT tool loaded through `OMP_TOOL_LIBRARIES` | Runtime support varies; correlate parallel regions, tasks, work sharing, and synchronization |
| CUDA | CUPTI activities for kernels/copies plus callbacks for host APIs | Host API duration is not GPU execution duration; correlate device/stream operations; detect support and buffer loss |
| I/O | `LD_PRELOAD` interposition on selected POSIX functions | Reentrancy guards, `errno`, partial I/O, descriptor reuse, fork/exec, and profiler self-I/O exclusion; static binaries/direct syscalls bypass interception |
| PMU | `perf_event_open` counters, then optional sampling | Permissions, event availability, thread/process scope, multiplex scaling via time enabled/running, and CPU topology |
| Python | Optional CPython monitoring/profiling and stack sampling | Interpreter/version compatibility, async/generator semantics, native-thread identity, GIL/free-threaded distinctions |
| PyTorch | `torch.profiler`/Kineto import first, then optional runtime integration | Operator/autograd metadata, CPU–GPU correlation, distributed backends, profiler ownership and version coverage |

Collectors should avoid blocking, formatting, symbolization, and allocation on hot
paths where possible. Use bounded thread-local buffering, explicit event-loss
counts, and deferred processing. Start with per-process/per-rank files to avoid a
central writer bottleneck. Measure overhead against uninstrumented baselines.

The I/O collector currently sends fixed observations to the supervising process
through inherited local datagram/shared-memory descriptors. A bounded raw queue
decouples receipt from JSON conversion, and all observed process/thread/file tracks
are written to that invocation's shard. Native callbacks allocate no event objects
and own no writer threads. The format separates ingress drops from writer loss.
See [I/O profiling](io-profiling.md) for lifecycle, resource bounds, and limitations.

## Reporting

The HTML report shows the process span and all declared rank/thread/GPU/I/O tracks.
It supports zoom, pan, filtering, span/counter inspection, hierarchy collapse,
correlation highlighting, and dependency links. TypeScript and Canvas provide
viewport culling and bounded drawing work; no frontend framework or CDN is needed.

`rprof-report` performs full-capture streaming summary analysis and optionally
retains a bounded event subset for the standalone browser model. Counts and
inclusive category totals cover the full validated snapshot; omitted timeline
events and collector loss are reported separately. All large integers are strings
at the browser boundary, with BigInt-based timeline arithmetic. Data is embedded in
inert, escaped JSON, and workload labels use text/Canvas rendering.

`rprof view` serves the same self-contained artifact over loopback HTTP. Indexed
on-demand queries and live updates are future extensions. Chrome Trace export
streams all selected-domain spans/counters independently of HTML retention limits,
with bounded correlation reconstruction. See [reporting](reporting.md) and the
[browser model contract](report-model.md).

## OpenMP runtime coverage

GCC libgomp, LLVM libomp, and Intel libiomp5 are explicit support targets. OMPT
handles LLVM/Intel; an explicit target-built GNU ABI backend handles GCC 13/libgomp
without OMPT. Compiler/runtime pairs are validated separately. Registration status,
received callback counts, unmatched endpoints, and sequence gaps remain visible.
Task lifetime and scheduled execution are separate, and wait/implicit scopes are
inclusive rather than estimates of CPU utilization. GNU outlined bodies and API
calls are separately labeled inclusive observations, not scheduler or pure-wait data.

Native collectors share a process-image hash and physical thread registry, with
collector-specific correlation namespaces. This permits MPI, I/O, and OpenMP events
to appear on the same thread while keeping exec/process instances distinct. See
[OpenMP profiling](openmp-profiling.md) and [runtime support](openmp-runtimes.md).

CUDA API callbacks use the same physical thread registry. GPU work uses separate
device/context/stream tracks and explicit correlation edges. CUPTI normalizes GPU
timestamps with a registered host `CLOCK_MONOTONIC` callback; interpolation
uncertainty remains unknown. The optional target-built adapter uses CUDA injection,
a fixed host buffer pool, and independent source/ingress/final-flush diagnostics.
See [CUDA profiling](cuda-profiling.md) and the [CUDA report model](cuda-report-model.md).

## Remote operation

For the first version, users launch collection directly on the remote system and
view saved captures through an SSH tunnel to the existing loopback `rprof view`
server. This uses current commands and adds no remote service or orchestration
code. A later read-only remote-file source may let the local GUI fetch/cache an
existing capture for local analysis. See [remote operation](remote-operation.md).

## Python and PyTorch

Add language/framework adapters without making Python or PyTorch a dependency of
the base CLI. Normalize imported or collected events into the shared report model,
retaining source units, precision, correlation namespaces, profiler schedules, and
provenance. Python frames, native runtime calls, PyTorch operators, and GPU activity
are separate attribution layers; overlapping durations must not be double-counted
as total application time. See [Python and PyTorch](python-pytorch.md).

## Hardware what-if projection

Add a Rust projection engine that consumes immutable measured captures, source and
target hardware profiles, and workload decomposition/scaling assumptions. Target
profiles may come from user input, published specifications, or imported benchmark
results; the target machine does not need to be accessible.

Separate useful work, data movement, resource contention, and dependency-induced
waiting. Recompute the schedule/critical path for each scenario so changes in CPU,
GPU, communication, or storage performance can change overlap and expose new
bottlenecks. Node-count changes require an explicit strong/weak-scaling model and
rank/thread/GPU placement. Preserve provenance and model versions in derived
projection artifacts, which reference their measured input captures.

The report layer will show baseline and projected timelines, component breakdowns,
scaling curves, sensitivity, feasibility limits, and scenario ranges. Reports must
identify which quantities are measured, calibrated, assumed, or extrapolated.
Collector phases should retain the work/volume/topology evidence needed for these
models. See [hardware projection](hardware-projection.md) for scope, data
requirements, validation, and staged delivery.
