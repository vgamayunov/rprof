# Implementation plan

## 0. Bootstrap — implemented

- Rust workspace, CLI, shared schema, and architecture documentation.
- `rprof record -o capture.rprof.json -- command ...` captures process wall time.
- `rprof report capture.rprof.json` prints a text summary.

## 1. Capture foundation and supervision — implemented

- Define collector lifecycle/configuration, session/host/process identity, schema
  compatibility, operation attributes, and correlation/dependency semantics.
- Add bounded event buffers, chunked streaming storage, loss accounting, and
  per-process shards with deterministic merge.
- Handle process trees, fork/exec, termination signals, partial captures, and cleanup.
- Add tiny fixture workloads and integration checks for workload exit behavior,
  collector lifecycle, timestamp ordering, merge identity, and event loss.

**Exit criterion:** repeatable low-overhead captures with observable data loss,
bounded memory use, and useful output after workload failure or interruption.

Delivered: version-2 streaming schema, bounded writer queue and chunk staging,
loss checkpoints, strict streaming reader with tail recovery, namespaced per-process
shards, deterministic disk-backed merge, Linux process-group supervision and
subreaper cleanup, and CPU-only unit/integration fixtures. See
[capture format](capture-format.md) for the precise lifecycle and limits, and
[testing](testing.md) for verification and performance probes.

Fork/exec descendants in the original workload process group are supervised;
fine-grained process/thread events and injected per-child writers arrive with the
native collectors. Deliberately detached process groups require a future cgroup
backend. Native allocation-free ingress buffers are also part of collector work.

## 2. Timeline and reports — implemented

- Extract report analysis from the CLI; add `report --format text|html`.
- Build a self-contained HTML timeline: hierarchical tracks, zoom/pan, filtering,
  span details, and counter plots. Use synthetic rank/thread/GPU/I/O captures to
  exercise the UI before all collectors are available.
- Add Chrome Trace/Perfetto export, safe HTML data embedding, and large-integer handling.
- Share the renderer with a local browser GUI; make captures usable offline.
- Design shared report inputs to support measured/projected comparisons later,
  including provenance labels and separate baseline/scenario timelines.

**Exit criterion:** a captured command and a representative multitrack fixture
render correctly in an offline HTML report and a reference trace viewer.

Delivered: shared `rprof-report` analysis; text/HTML/Chrome Trace output; an offline
TypeScript/Canvas timeline with hierarchical lanes, filtering, zoom/pan, exact-value
inspectors, counters, and correlations; and `rprof view` serving the same snapshot
on loopback. Browser-facing integer values are lossless. HTML retention limits are
explicit, summaries cover the full input snapshot, and multiple clock domains stay
separate. Synthetic multitrack fixtures, headless browser tests, and Perfetto import
validation exercise the collection-to-report path. See [reporting](reporting.md).

## 3. I/O profiling via preload — implemented

- Create an optional preload shared library with thin C wrappers, Rust buffering,
  and an injection path in `rprof record`; verify exported interposition symbols.
- Start with open/openat, close, read/write, pread/pwrite, and fsync; preserve ABI,
  `errno`, partial results, descriptor lifetimes, and recursion safety.
- Capture requested/completed bytes, duration, path/resource identity, and thread.
- Retain operation sizes, access offsets/patterns, concurrency, and durability
  semantics for later storage projection; distinguish logical API I/O from measured
  physical device traffic when that evidence is available.
- Report bandwidth, latency, operation counts, and I/O timeline tracks.

**Exit criterion:** fixture results match actual I/O and remain correct with
multiple threads, descriptor reuse, failures, and nested library calls.

Delivered: C ABI wrappers and an allocation-free Rust runtime; nonblocking local
IPC with bounded external conversion; explicit native/writer loss accounting;
CLI collector selection; process-image/thread/resource identity across normal
fork/exec; actual I/O spans; and per-file text/HTML byte, latency, error, and rate
summaries. Native and browser fixtures validate the full path. See
[I/O profiling](io-profiling.md) for supported operations and measured overhead.

## 4. MPI profiling — initial implementation complete

- Pick one initial MPI implementation based on the development environment;
  compile a thin native adapter against its headers, isolate ABI differences, and
  add the second implementation to the support matrix.
- Capture initialization/finalization, blocking point-to-point, waits/tests,
  nonblocking request lifetimes, and common collectives through PMPI.
- Display individual calls by their full MPI names on rank/thread timeline lanes
  (`MPI_Send`, `MPI_Recv`, `MPI_Isend`, `MPI_Wait`, `MPI_Waitall`, collectives, etc.).
  Preserve start/duration, peer, communicator, bytes, and request details. Keep
  call duration distinct from an asynchronous request's lifetime, with filtering,
  hover/inspection, and correlation highlighting for both.
- Add rank discovery, launcher environment propagation, per-rank output, and merge.
- Introduce cross-host clock calibration and uncertainty reporting before treating
  distributed timestamps as directly comparable.
- Retain message volumes, communicator membership, collective types, completion
  dependencies, and rank/node placement for interconnect and node-count projection.

**Exit criterion:** a multi-rank fixture produces distinct rank/thread tracks,
communication correlations, and trustworthy timing metadata.

Delivered: target-header PMPI adapter and Rust transport; named API spans;
rank/thread metadata; bounded request tracking with completion, cancellation,
release, and ambiguity semantics; communicator metadata; optional clock calibration
with uncertainty/drift; API summaries; and combined MPI/I/O capture. Two-node
InfiniBand and MPI_THREAD_MULTIPLE fixtures passed on `tst4`/`hbv4`. Timeline rows
are now compact and content-dependent. See [MPI profiling](mpi-profiling.md) and
[glibc compatibility](glibc-compatibility.md).

Remaining MPI coverage includes persistent requests, Fortran, sessions/RMA/MPI-IO,
unlisted calls, complete dynamic-communicator identity, and cross-rank message
matching. Current request correlations are rank-local and calibration is estimated.

## 5. OpenMP profiling — OMPT implementation complete; GNU adapter planned

- Integrate OMPT callbacks for threads, parallel regions, tasks, work sharing,
  and synchronization; document supported runtimes.
- Add imbalance, barrier-wait, and task execution summaries.
- Preserve work/task dependencies, concurrency, affinity, and available work-size
  annotations for CPU and thread-count scaling models.
- Support all three runtime families: GCC `libgomp`, LLVM `libomp`, and Intel
  `libiomp5`. Keep compiler/code-generation ABI and loaded runtime identity separate
  in the support matrix; compiling with GCC and linking LLVM is not libgomp support.
- Deliver OMPT collection first for LLVM/Intel runtimes, with per-callback registration
  results and observed-event counts. Do not equate missing callbacks with zero work.
- Add a separately tested GNU `GOMP_*` ABI adapter for GCC/libgomp versions without
  OMPT. Version-gate that adapter and validate nested teams, tasks/dependencies,
  synchronization, and hybrid MPI+OpenMP before claiming parity. Prefer standard
  OMPT when a future libgomp version implements it.
- Validate runtime/compiler combinations explicitly, including native GCC/libgomp,
  Clang/libomp, and Intel compiler/libiomp5. Preserve the application's runtime;
  backend selection must not silently swap runtimes or execute the workload twice.

**Exit criterion:** nested parallelism and task fixtures have correct lifetimes
and relationships, and combined MPI+OpenMP runs produce coherent tracks.

See [OpenMP runtime support](openmp-runtimes.md) for the runtime-family plan and
the distinction between OMPT registration and effective compiler/runtime coverage.

Delivered: standard OMPT loading, callback capabilities/observations, parallel and
worksharing scopes, synchronization/wait intervals, task lifetimes/execution and
dependencies, sequence-gap handling, shared MPI/I/O/OpenMP thread identities, and
text/HTML summaries. Clang-generated host and two-node hybrid fixtures passed with
LLVM libomp 20.1.8 and Intel libiomp5 2025.3. GCC/libgomp 13.2 is explicitly reported
as lacking OMPT; native GNU-runtime support remains a planned adapter. See
[OpenMP profiling](openmp-profiling.md).

## 6. CUDA profiling

- Add optional CUPTI bindings, capability detection, activity buffering, and
  callback/activity correlation.
- Record kernels, memory copies, synchronization, devices, streams, and host APIs.
- Calibrate host/device clocks and report dropped activities.
- Preserve launch geometry, transfer volumes, stream dependencies, GPU placement,
  and available kernel work/memory metrics for GPU projection. Make any additional
  profiling passes explicit when CUPTI activity traces alone lack those metrics.
- Keep activity/correlation provenance suitable for later PyTorch integration,
  including coexistence with PyTorch/Kineto's ownership of CUDA profiling APIs.

**Exit criterion:** a multi-stream workload shows actual GPU overlap and its CPU
launch relationships, with duration checks against a reference profiler.

**Implemented:** target-built CUDA 13/CUPTI injection adapter, concurrent kernel,
copy/memset and synchronization activities, runtime/driver API spans, explicit
CPU–GPU correlations, normalized monotonic timestamps, device/context/stream
identities, and CUDA text/HTML reports. Validated on one and two H100 GPUs with
exact counts/bytes, overlapping streams, zero recorded drops, CUDA-event timing,
and a separate Nsight Systems 2026.1.3 reference run (kernel-duration sum within
0.76%). Conflict ownership and missing-final-flush controls passed. Broader SDK,
peer-copy, CUDA Graph, and mixed-runtime matrices remain follow-ups; activity
traces do not yet provide hardware performance metrics. See [CUDA profiling](cuda-profiling.md).

## 7. PMU counters

- Implement Linux `perf_event_open` counting for cycles, instructions, cache misses,
  and supported hardware events; provide clear capability/permission diagnostics.
- Preserve raw counts and time-enabled/time-running data for multiplex scaling.
- Add scoped counter timelines and derived metrics, then optional sampling and
  deferred symbolization. Define attribution explicitly for threads and ranks.
- Record source CPU/cache/NUMA topology, frequency context, and supported memory
  traffic/work metrics with their coverage and scaling, for calibrated CPU models.

**Exit criterion:** supported counts agree with `perf stat` within an established
tolerance on controlled workloads, and unsupported events are reported accurately.

**Implemented and validated:** pre-exec `perf_event_open` counting,
workload-tree/root-thread scopes, generic hardware/software and raw events, exact
raw/enabled/running values, independent-event scaling and derived estimates,
source CPU/cache/NUMA context, timeline counters, and text/HTML diagnostics.
Five-run `perf stat` comparisons passed on HBv4 for cycles (0.0263%), instructions
(0.000010%), cache and branch counters, thread/fork inheritance, root-thread scope,
and user+kernel counting. Eight hardware events forced real multiplexing at
71.54–77.26% running coverage; all scaled-count comparisons passed unchanged
tolerances. Bus/ref cycles were correctly reported unsupported. Software-event
comparisons also passed on H100. Sampling and broader CPU/event matrices remain
follow-ups. See [PMU profiling](pmu-profiling.md).

## 8. Hardware what-if projection

- Add versioned source/target hardware profiles for CPU, GPU, interconnect, storage,
  memory capacity/topology, node count, and rank/thread/GPU placement. Accept manual
  descriptions, published specifications, and imported benchmark/calibration data
  so projection works without access to the target hardware.
- Derive per-phase work, data movement, dependency, and resource-demand models from
  captures. Track missing evidence and expose user-supplied workload assumptions.
- Start with fixed-topology, single-component sensitivity models; then add
  dependency-aware rescheduling with overlap, contention, and critical-path analysis.
- Add strong/weak-scaling models and node-count sweeps, including decomposition,
  communication changes, collective algorithms, placement, and capacity checks.
- Provide a `rprof project` CLI and HTML scenario editor/comparison view.
  Report projected runtime/speedup, bottlenecks, scaling efficiency, sensitivity,
  assumption ranges, and baseline/projected timelines.
- Validate models by predicting held-out measurements on available machines and
  node counts. Version calibration data and report empirical error separately from
  assumption-driven ranges for hardware that cannot be measured directly.

**Exit criterion:** reproducible projections for supported workloads and scenarios
can be produced from saved captures and an offline target profile. Reports retain
input/model provenance, identify unsupported or underdetermined components, show
uncertainty, and pass predeclared error targets on held-out available configurations.

Detailed design and incremental delivery are in
[hardware projection](hardware-projection.md). Define evidence requirements during
phases 2–7; projection implementation can begin once the relevant collectors supply
enough data, without waiting for every collector to be complete.

**Initial supported models implemented:** versioned provenance-bearing profiles,
capture-bound phase models, throughput/latency–bandwidth/roofline sensitivity,
deterministic resource scheduling and critical paths, explicit representative
strong/weak node sweeps, capacity/features, JSON/text/HTML, and an offline scenario
editor using the same Rust engine in WASM. Analytic cases cover overlap and
bottleneck changes; a separate-calibration cross-host LCG held-out prediction
passed at 14.68% error against a predeclared 15% limit. This is workload-specific;
independent GPU/network/storage/multinode validation and automatic decomposition
remain follow-ups. See [projection model](projection-model.md).

## 9. Remote usage and later remote-file viewing

- First version: launch `rprof record` directly on the remote system, run the
  existing `rprof view --port` there, and connect a local browser through a manually
  established SSH tunnel. This workflow requires no additional rprof code.
- Use normal shell/scheduler workflows and SSH configuration for compute-node
  access, allocations, and job launch. Verify tunneled viewing during remote
  collector testing on `tst9`/`h100` and `tst4`/`hbv4`.
- Later: let a local GUI open an existing remote capture through a read-only SSH
  file source, initially by fetching/caching completed captures for local analysis.
  Remote job launch/control is outside this feature's scope.

**First-version exit criterion:** a remotely collected capture can be viewed through
an SSH tunnel using existing commands, with results matching its offline report.

See [remote operation](remote-operation.md). Remote-file GUI access is a follow-up,
not a prerequisite for the first release or the next collector phase.

## 10. Python and PyTorch profiling

Prioritized after phase 8 at the user's request; remote-file GUI work remains a
separate follow-up. Current CPU validation uses `tst3` HBv4 while `tst4` is down;
PyTorch GPU validation uses `tst9` H100/H200.

- Add optional CPython integration for Python stacks/functions, thread/interpreter
  identity, and user ranges. Distinguish sampled stacks from instrumented intervals;
  version-gate monitoring/profiling APIs and cover async/generator lifetimes.
- Correlate Python activity with native MPI, OpenMP, I/O, PMU, and CUDA observations
  using shared process/thread identity and explicit clock mappings.
- Start PyTorch support with an importer for `torch.profiler`/Kineto traces, then
  add opt-in collection/bootstrap integration and profiling schedules.
- Show operators, module/user ranges, forward/backward/autograd activity, optimizer
  steps, CPU–GPU launches, kernels/copies, and memory counters in the timeline.
  Preserve shapes/dtypes/stacks only when requested and available.
- Add DataLoader/multiprocessing attribution and distributed training support,
  including DDP/FSDP and actual NCCL/Gloo/MPI backend identities.
- Report operator self/inclusive CPU time, device activity, memory use, data-loading
  stalls, communication overlap, and observed bottlenecks. Track eager versus
  `torch.compile`/fused-kernel attribution and incomplete coverage explicitly.
- Keep Rust as the core and Python/PyTorch dependencies optional. Coordinate CUDA
  collector ownership rather than starting incompatible profiler sessions together.

**Exit criterion:** Python and PyTorch CPU/GPU fixtures produce coherent timelines
with native activity, preserved source correlations, explicit coverage/overhead,
and results checked against PyTorch's profiler on supported version combinations.

**First increments implemented:** SDK-free Kineto/Chrome import, selected CPython
function execution segments and elapsed ranges, worker bootstrap, optional scheduled
Torch collection, Python/native track sharing, and text/HTML summaries. H200 eager
and compiled training, DataLoader workers, CPU/autograd, and single-host two-rank
Gloo reference checks passed. Native Python+CUPTI collection also passed. Source
loss/clock uncertainty and unresolved trace endpoints remain explicit. Fine-grained
Python function tracing has high overhead; sampling, free-threaded/subinterpreter
lifecycle coverage and broader distributed/version matrices remain follow-ups.
See [Python and PyTorch](python-pytorch.md) for supported scope.

## 11. Combined-workload validation and release

- Validate mixed MPI+OpenMP+CUDA+I/O workloads and distributed merging at scale.
- Include Python/PyTorch training and inference, data-loader workers, and distributed
  runs in the supported-runtime/version matrix.
- Validate projection reports and reproducibility across hardware-profile/model
  versions, including sensitivity to missing data, event loss, and clock uncertainty.
- Measure capture overhead, event loss, memory use, report size, and UI responsiveness.
- Add CI for the base build and CPU fixtures; use environment-specific jobs for
  MPI/runtime matrices, GPUs, and privileged/permission-dependent PMU tests.
- Package the CLI and optional tools, publish the support matrix and examples,
  and document installation, collection, report generation, and troubleshooting.

## Suggested next task

Continue phase 9's read-only remote-file viewing, while preserving manual remote
collection and SSH-tunneled viewing. Expand projection calibration/validation
matrices and the OpenMP runtime matrix and GNU adapter
coverage alongside the remaining collector work.

The proposed order can be adjusted once the primary MPI implementation, OpenMP
runtime, workload scale, and deployment constraints are known. Preferred test
environments are `tst9` / `h100` for GPU work and `tst4` / `hbv4` with InfiniBand
for MPI work; details are recorded in [testing](testing.md).
