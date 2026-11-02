# Hardware what-if projection

## Implemented v1

`rprof project` produces reproducible JSON, text, and self-contained HTML
scenarios from a capture plus versioned hardware/workload profiles. The offline
editor executes the same Rust engine through WebAssembly, with native/WASM parity
tests. Models cover workload-specific throughput ratios, normalized
latency–bandwidth costs, and normalized compute/memory (roofline) costs.

An explicit phase DAG and effective resource slots determine the target schedule.
Execution-lane order, source readiness lags, and trailing time are preserved.
Strong/weak node sweeps use declared representative-partition assumptions;
capacity/feature failures and missing evidence never become numeric speedups.

```sh
cargo build -p rprof-cli
target/debug/rprof project fixtures/projection/baseline.jsonl \
  --source-hardware fixtures/projection/source.json \
  --target-hardware fixtures/projection/target.json \
  --workload-model fixtures/projection/workload.json \
  --nodes 1,2,4 --scaling strong --format html -o projection.html
```

This example is explicitly synthetic: the one-node target changes the critical
path and projects 120 ns from a 160 ns source, rather than adding overlapping
kernel times. Open HTML directly to edit models, recalculate offline, and export
a replayable request. For real data, begin with
`rprof project capture.jsonl --inspect -o evidence.json` and author the phase model
using stable source-observation IDs.

See [projection model and CLI](projection-model.md) for the schema, algorithms,
limits, and validation. The design below also records longer-term goals:
automatic workload decomposition, arbitrary topology remapping, full discrete-event
simulation, and broader empirical calibration remain future work.

## Goal

Use an application profile collected on available hardware to help estimate its
performance on another configuration, **without requiring a run on the target**.
Explore changes to CPU, GPU, interconnect, storage, and node count individually or
in combination. Answer questions such as:

- Would a faster interconnect reduce runtime, or would computation dominate?
- Would a GPU with more compute throughput or memory bandwidth help this workload?
- How would a different CPU, core count, cache/NUMA layout, or memory subsystem behave?
- Would local NVMe or a different shared filesystem improve checkpoint time?
- How would the application scale from 8 to 32 nodes, and where would scaling flatten?

Projection is a derived analysis with its own versioned artifacts. Raw captures
remain the measured evidence; the UI labels projected values and timelines clearly.

## Inputs and evidence

1. **Baseline captures:** phase/iteration boundaries, execution and dependency
   structure, useful compute, messages, transfers, I/O, counters, and observed overlap.
   Multiple problem sizes/node counts improve scaling estimates when available.
2. **Source hardware profile:** CPU/GPU models and counts, memory/cache capacities,
   NUMA and device topology, interconnect/storage layout, placement, and relevant
   software/compiler/runtime settings. Capture automatically where practical and
   accept supplemental metadata.
3. **Target hardware profile:** the same descriptors plus latency, sustained
   throughput, capacity, and concurrency parameters. Support user-authored profiles,
   vendor specifications, and imported microbenchmark/published results; all can
   be supplied offline. Record units, source/version/date, measurement conditions,
   and whether a number is measured, specified peak, calibrated, or assumed.
4. **Workload model:** fixed or scaled global problem size, domain decomposition,
   rank/thread/GPU placement, iteration counts, memory per rank/device, work/byte
   counts, and any algorithm/library changes expected on the target.

Retain evidence coverage: dropped events, unsynchronized clocks, multiplexed PMU
counts, missing dependencies, unobserved traffic, and unprofiled phases. Report
coverage per modeled component and allow explicit assumptions to fill gaps. A
single process wall-time span supports a coarse user-supplied scenario only; it
cannot identify CPU, communication, and storage demands on its own.

## Component models

| Component | Evidence needed | Target parameters and initial model |
| --- | --- | --- |
| CPU | Phase work/instruction mix, supported memory-traffic counters, working-set size, concurrency and NUMA placement | Effective throughput for the relevant work, memory bandwidth/latency, cache/core topology and frequency; calibrated compute/memory bounds with explicit serial and synchronization terms |
| GPU | Kernel identity/geometry, work estimates, available memory metrics, transfer volumes, stream/launch dependencies | Throughput at the actual precision/instruction mix, memory bandwidth/capacity, occupancy constraints, launch overhead and device-link topology; kernel-specific calibrated or roofline-style bounds |
| Interconnect | Message-size distributions, peers, request completion, communicator/collective semantics and placement | Latency/bandwidth curves, injection limits, topology, contention and collective algorithm; begin with `latency + bytes / effective_bandwidth` and extend to shared-resource scheduling |
| Storage | Operation sizes/offsets, read/write mix, concurrency, flush/durability semantics, sharing and observed cache effects | Latency, IOPS, bandwidth versus size/queue depth, metadata rate, filesystem/server topology and capacity; model local-device and shared-storage service limits |
| Node count | Workload decomposition, global/local problem sizes, communication structure, serial fraction, imbalance and memory demand | Repartition work and data, derive communication/collective changes, remap rank/thread/GPU placement, and account for shared network/storage limits |

Peak FLOPS, clock rate, or link speed alone is insufficient to determine achieved
application throughput. Roofline estimates provide bounds and bottleneck hypotheses;
calibration and workload-specific evidence determine tighter estimates. API-level
bytes are not automatically DRAM/HBM/device bytes: cache hits, buffered I/O, data
reuse, and compression can change physical traffic. Unsupported instruction sets,
precision, device features, or capacity requirements must be represented as
infeasible or conditional scenarios rather than a numeric speedup.

PMU support and CUPTI activity traces do not guarantee operation or memory-traffic
counts. Allow annotations, optional metric collection passes, or workload-specific
models where needed, and track their perturbation and provenance. Runtime/compiler
changes, MPI collective selection, and GPU library kernel selection may require
different models from simple parameter substitution.

## Scheduling, overlap, and scaling

Build a phase/task/operation dependency graph with CPU, GPU, network, and storage
resource demands. Distinguish work/service time from queueing, synchronization, and
dependency-induced waiting. Recompute scheduling and the critical path under target
resource limits. A faster kernel may expose communication that was previously
hidden; faster storage may move a checkpoint bottleneck into CPU serialization.
Do not independently scale observed wait spans or add overlapping component times.

Use bounded analytical models first, followed by discrete-event scheduling for
supported dependency/resource patterns. Preserve unresolved dependencies and clock
uncertainty instead of assuming a globally ordered trace. Projected timelines show
model-generated schedules and their relationship to measured baseline phases.

For node-count changes, support:

- **Strong scaling:** fixed global problem size; redistribute local work and data,
  account for serial work, imbalance, communication growth, and collective costs.
- **Weak scaling:** fixed per-node workload; grow the global problem, deriving
  communication and shared-storage demand from the application's decomposition.
- **Placement scenarios:** ranks/node, threads/rank, GPUs/rank, affinity, and
  intra-node versus inter-node transfers.
- **Capacity checks:** host/device memory per partition, rank counts/decomposition
  constraints, storage footprint, and topology/resource limits.

Changing node count can change the execution graph and algorithm, not only its
durations. Require a workload scaling model or explicit assumptions; do not create
a larger run by duplicating measured rank lanes. Fit scaling parameters from
multiple baseline runs when possible. With one baseline, expose admissible scenarios
and ranges and identify the additional measurements that would narrow them.

## CLI and reports

Implemented command shape (version-1 profiles are described in [projection model](projection-model.md)):

```sh
rprof project baseline.rprof.jsonl \
  --source-hardware source.json \
  --target-hardware target.json \
  --workload-model workload.json \
  --nodes 32 --scaling strong \
  --output projection.json
```

Node-count sweeps and an HTML scenario editor use the same Rust model engine and
hardware-profile definitions. The reporting goals include:

- Baseline and projected runtime/throughput, speedup, and component contributions.
- Critical-path and bottleneck changes, utilization, and newly exposed waits.
- Node-count scaling curves and parallel efficiency with a stated reference.
- Baseline/projected timelines with corresponding phase/operation details.
- Sensitivity to CPU/GPU speed, bandwidth, latency, placement, and shared resources.
- Optimistic/central/pessimistic scenarios with the assumptions behind each range.
- Feasibility, missing evidence, calibrated model domain, and unresolved components.
- Input capture/profile hashes, parameters, model/calibration versions, and random
  seed when applicable, making saved scenarios reproducible.

Scenario ranges from uncertain assumptions are not statistical confidence
intervals. Report empirical prediction error separately when held-out measurements
exist. Changing several components together should recompute the combined model;
component speedups must not simply be multiplied.

## Incremental delivery and validation

1. **Evidence and profiles:** define versioned hardware/workload profile schemas,
   source metadata, calibration provenance, and collector evidence requirements
   during the report and collector phases.
2. **Fixed-layout sensitivity:** implement interpretable single-component models
   for supported phases; report coverage, assumptions, and bounds. Keep unsupported
   phases visible, with explicit unchanged-time assumptions if the user chooses them.
3. **Combined projection:** add dependency/resource-aware scheduling and projected
   timelines, including overlap, contention, and bottleneck shifts.
4. **Scaling:** add explicit strong/weak-scaling workload models, placement changes,
   node-count sweeps, and capacity/decomposition checks.
5. **Calibration and validation:** predict held-out runs on hardware/configurations
   that are available, compare with measurements, and publish error by model/workload
   class. Keep calibration and validation sets separate and predeclare acceptance
   tolerances before evaluating held-out data.

The initial implementation covers steps 1–4 for explicit, bounded phase graphs
and representative-partition scaling. Step 5 has an independent single-process
LCG throughput-ratio validation between the local Intel host and HBv4: 373.065 ms
predicted versus 325.317 ms observed, **14.68% error against a predeclared 15% limit**.
The same executable ran on both hosts; 120-million-iteration calibration runs and
300-million-iteration held-out runs were kept separate. This is a narrow pass for
that workload, not validation of arbitrary CPU/GPU/network/storage performance.
Other component and scaling models currently have analytical validation. The
measured target was outside the evaluated calibration-derived range, demonstrating
that these cases do not bound all modeling error.

Use analytic fixtures for compute/memory limits, message latency/bandwidth, shared
storage saturation, serial fractions, dependency overlap, and capacity failure.
Test same-hardware reconstruction as a consistency check; it is not independent
validation. Validate changes in one component before combined scenarios, then test
node-count changes and workloads outside the calibration range.

Use `tst4` / `hbv4` for available MPI/node-count configurations and `tst9` / `h100`
for GPU workload characterization. Hardware diversity for independent cross-device
validation should be expanded as machines or suitable published datasets become
available. Predictions for an inaccessible target remain extrapolations with
explicit applicability limits, even when related models validate well elsewhere.
