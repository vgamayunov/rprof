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
C interop overhead; it offers little benefit over Rust for this project. No Python
component is proposed. TypeScript is for browser interaction/rendering; aggregation
stays in Rust, with WebAssembly an option only if browser-side analysis needs it.

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
          ↓ projection engine (planned)
       scenario estimates + projected timeline + assumptions/uncertainty
          ↓ report engine / HTML comparison UI
```

The CLI, shared data model, supervised process capture, streaming storage,
deterministic merge, and text summaries are implemented. Hardware/runtime collectors,
the projection engine, and HTML rendering remain proposed components.

## Workspace boundaries

| Component | Responsibility |
| --- | --- |
| `crates/rprof-core` (exists) | Capture schema: tracks, spans, counters, metadata |
| `crates/rprof-capture` (exists) | Bounded writer, recovery/validation reader, shard merge and loss accounting |
| `crates/rprof-cli` (exists) | CLI, Linux process supervision, capture configuration, text summary |
| `rprof-collector` (planned) | Native callback lifecycle and allocation-free per-thread ingress buffers |
| `rprof-preload` (planned shared library) | Dynamically linked POSIX I/O interception, C wrappers and Rust buffering |
| `rprof-mpi` (planned) | PMPI wrappers; implementation-specific ABI adapters where needed |
| `rprof-ompt` (planned) | OpenMP Tools interface callbacks |
| `rprof-cuda` (planned) | CUPTI activity/callback integration and device clock mapping |
| `rprof-perf` (planned) | Linux PMU counting and sampling |
| `rprof-report` (planned) | Shared analysis, text and self-contained HTML export |
| `rprof-project` (planned) | Hardware profiles, workload models, dependency-aware projection and scenario comparison |
| `ui/` (planned) | Browser timeline and local HTML GUI using the same report assets |

Keep MPI and CUDA dependencies optional so the base CLI builds without vendor
SDKs or an MPI installation. Cargo remains the top-level build entry point;
adapter crates can compile small C/C++ sources through the `cc` build dependency.
Add a larger native build system only if integration complexity justifies it.
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

Collectors should avoid blocking, formatting, symbolization, and allocation on hot
paths where possible. Use bounded thread-local buffering, explicit event-loss
counts, and deferred processing. Start with per-process/per-rank files to avoid a
central writer bottleneck. Measure overhead against uninstrumented baselines.

## Reporting

The first HTML milestone should show even the initial process span, then expand
to rank/thread/GPU/I/O tracks as collectors land. Support zoom, pan, filtering,
span details, counters, and correlation highlighting. Render dense timelines with
Canvas/WebGL and viewport-based loading rather than one DOM element per event.

Use common analysis code for text and HTML summaries. Escape workload-provided
labels and embed data safely in standalone HTML; support offline use without a CDN.
Add Chrome Trace/Perfetto export early as an independent way to inspect and
validate captures. Decide on a frontend framework after the timeline prototype.

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
