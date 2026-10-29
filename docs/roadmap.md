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

## 2. Timeline and reports — early vertical slice

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

## 3. I/O profiling via preload

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

## 4. MPI profiling

- Pick one initial MPI implementation based on the development environment;
  compile a thin native adapter against its headers, isolate ABI differences, and
  add the second implementation to the support matrix.
- Capture initialization/finalization, blocking point-to-point, waits/tests,
  nonblocking request lifetimes, and common collectives through PMPI.
- Add rank discovery, launcher environment propagation, per-rank output, and merge.
- Introduce cross-host clock calibration and uncertainty reporting before treating
  distributed timestamps as directly comparable.
- Retain message volumes, communicator membership, collective types, completion
  dependencies, and rank/node placement for interconnect and node-count projection.

**Exit criterion:** a multi-rank fixture produces distinct rank/thread tracks,
communication correlations, and trustworthy timing metadata.

## 5. OpenMP profiling

- Integrate OMPT callbacks for threads, parallel regions, tasks, work sharing,
  and synchronization; document supported runtimes.
- Add imbalance, barrier-wait, and task execution summaries.
- Preserve work/task dependencies, concurrency, affinity, and available work-size
  annotations for CPU and thread-count scaling models.

**Exit criterion:** nested parallelism and task fixtures have correct lifetimes
and relationships, and combined MPI+OpenMP runs produce coherent tracks.

## 6. CUDA profiling

- Add optional CUPTI bindings, capability detection, activity buffering, and
  callback/activity correlation.
- Record kernels, memory copies, synchronization, devices, streams, and host APIs.
- Calibrate host/device clocks and report dropped activities.
- Preserve launch geometry, transfer volumes, stream dependencies, GPU placement,
  and available kernel work/memory metrics for GPU projection. Make any additional
  profiling passes explicit when CUPTI activity traces alone lack those metrics.

**Exit criterion:** a multi-stream workload shows actual GPU overlap and its CPU
launch relationships, with duration checks against a reference profiler.

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
- Provide a planned `rprof project` CLI and HTML scenario editor/comparison view.
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

## 9. Combined-workload validation and release

- Validate mixed MPI+OpenMP+CUDA+I/O workloads and distributed merging at scale.
- Validate projection reports and reproducibility across hardware-profile/model
  versions, including sensitivity to missing data, event loss, and clock uncertainty.
- Measure capture overhead, event loss, memory use, report size, and UI responsiveness.
- Add CI for the base build and CPU fixtures; use environment-specific jobs for
  MPI/runtime matrices, GPUs, and privileged/permission-dependent PMU tests.
- Package the CLI and optional tools, publish the support matrix and examples,
  and document installation, collection, report generation, and troubleshooting.

## Suggested next task

Deliver phase 2's HTML timeline vertical slice on the streaming reader, including
synthetic rank/thread/GPU/I/O fixtures and a shared text/HTML report analysis layer.

The proposed order can be adjusted once the primary MPI implementation, OpenMP
runtime, workload scale, and deployment constraints are known. Preferred test
environments are `tst9` / `h100` for GPU work and `tst4` / `hbv4` with InfiniBand
for MPI work; details are recorded in [testing](testing.md).
