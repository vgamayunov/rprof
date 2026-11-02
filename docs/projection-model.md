# Projection model v1

## Workflow

```sh
# Inspect a frozen capture snapshot. Select --domain ID for multiple clock domains.
rprof project baseline.jsonl --inspect -o evidence.json

# User-authored JSON; fixtures/projection/ is a complete example.
rprof project baseline.jsonl --source-hardware source.json \
  --target-hardware target.json --workload-model workload.json \
  --format html -o projection.html

# Repeat targets; --nodes forms a bounded node sweep.
rprof project baseline.jsonl --source-hardware source.json \
  --target-hardware target-a.json --target-hardware target-b.json \
  --workload-model workload.json --nodes 1,2,4 --scaling strong -o scenarios.json

# Replay the browser's Download request JSON, re-reading the original evidence.
rprof project baseline.jsonl --request downloaded.json --format text
```

The default format is JSON; text and HTML use the same result. Outputs are
no-clobber and atomically published. HTML embeds all assets and the Rust WASM
engine, with no network service. `rprof view` continues to serve capture reports;
projection HTML opens directly. Editing scenarios does not modify the capture.

The editor keeps baseline evidence and held-out observations immutable. Invalid
inputs preserve the previous result. Engine-version/source-hash mismatches prevent
recalculation with stale embedded code. Downloads contain the last successfully
evaluated inputs; apply edits with **Recalculate** before exporting.

## Evidence identity and coverage

The CLI freezes the capture's initial byte prefix in a temporary file, hashes it,
and analyzes that snapshot. Its byte length is recorded in evidence metadata.
Temporary disk use is proportional to the capture size; memory is bounded.
A growing capture has a different hash, so replay requires the original snapshot
or prefix rather than silently accepting changed evidence.

`--inspect` emits the selected clock domain, capture SHA-256, source metadata,
collector warnings, and observed spans. Span IDs are source keys of the form
`span:SHARD:CHUNK:EVENT`, with `open:` for unfinished activity observations. They
are independent of report-array indices and stable across retention changes.
The native timeline inspector also shows **Source observation** for authoring.

A workload specifies a region of interest (ROI) using decimal-string
`window_start_ns` and `window_duration_ns`, plus its capture hash and clock domain.
Selected spans must lie within that ROI. Each can be used once; overlapping or
nested observations on one execution lane are rejected. An enclosing workload
span can support a coarse single-phase hypothesis, but cannot be combined with
its contents. I/O tracks share their parent thread's lane; GPU streams have
separate lanes.

Coverage is the union of selected source intervals divided by ROI duration—not
the sum of overlapping component times. Uncovered time defaults to `unmodeled:
"reject"`. Use `"unchanged"` with an explicit `unmodeled_assumption` to retain that
time. Partial captures, lost/omitted events, unfinished spans, collector-finalization
gaps, and legacy captures without loss accounting require `allow_partial_evidence`.
Warnings remain visible; parameter ranges do not account for unknown missing work.
Unaligned domains are never merged into a fictitious global timeline. Estimated
cross-host clock uncertainty remains separate from model uncertainty.

CPU work splits, GPU work/traffic counts, network service time, and device-storage
traffic are not inferred from inclusive API durations or process wall time.
Captured attributes help author assumptions; the workload file is an explicit
model, not an automatic causal reconstruction of every recorded correlation.

## Hardware profiles

The authoritative, strictly validated types are in
`crates/rprof-project/src/model.rs`. Unknown fields and versions are rejected.
Each profile contains:

- `version: 1`, `id`, `name`, and `nodes`;
- `placement`: integer `ranks_per_node`, `threads_per_rank`, `gpus_per_rank`;
- `provenance`: `kind`, `source`, `date`, `conditions`;
- `resources`: named CPU, GPU, network, or storage service resources.

A resource has `kind`, `description`, `slots_per_node`, nullable
`capacity_bytes_per_node`, a declared supported `features` list, and named `metrics`.
Slots are effective service concurrency, not inferred physical core/GPU counts.
A GPU can have several declared concurrent kernel slots. Separate resource names
can express devices or service classes. Descriptions/conditions retain relevant
hardware, precision/work mix, compiler, topology, placement, and benchmark context.

Each metric declares `unit` (`work_per_second`, `bytes_per_second`, or
`nanoseconds`), ordered `low`, `central`, `high` values, and its own provenance.
Throughput must be positive; latency may be zero. Provenance kinds are `measured`,
`calibrated`, `specified_peak`, and `assumed`. Peaks and assumptions do not become
achieved-performance measurements. Rate units/work mix must match across source
and target; CPU clock or peak FLOPS alone is not application throughput.

The held-out fixture uses `lcg_iterations_per_second`, calibrated from body
durations for the same executable and operation. Profiles can be authored offline
from measurements, published tables, or declared assumptions. Imported values use
this schema; no online lookup is implicit.

## Phase models and scheduling

Each phase supplies `id`, `span_id`, `name`, optional `resource`, a `model`, optional
`after` predecessors, `node_scaling`, and a nonempty `assumption`.

| Model | Parameters | Service cost for source/target ratio |
| --- | --- | --- |
| `unchanged` | none | Hardware factor 1 |
| `throughput` | `metric` | Inverse effective work/byte throughput |
| `transfer` | `bandwidth_metric`, optional `latency_metric`, decimal-string `bytes`, `operations` | `operations × latency + bytes / bandwidth` |
| `roofline` | `compute_metric`, `bandwidth_metric`, decimal-string `work`, `bytes` | `max(work / compute_rate, bytes / bandwidth)` |
| `unsupported` | `reason` | No numeric projection |

For supported phases:

`target duration = observed source duration × target service cost / source service cost × node factor`.

Costs are normalized to observed duration. This preserves a same-hardware baseline;
it does **not** independently validate quantities, hardware parameters, or service/
wait separation. Whole API calls containing queueing, CPU work, or synchronization
need justified decomposition or a coarse assumption. API/logical bytes do not
automatically equal physical DRAM, HBM, network, or storage traffic. Roofline work
must describe the actual operation mix and precision.

The deterministic scheduler:

1. Adds source execution-lane order to explicit finish-to-start `after` edges.
2. Rejects cycles, contradictory timing, or insufficient source concurrency.
3. Derives source readiness lags after predecessors/resource availability; these
   are explicitly assumed unchanged.
4. Uses a fixed topological list (source start, then input order), assigning each
   target phase to the earliest available declared resource slot.
5. Recomputes the modeled critical phase path and resource-slot occupancy;
   trailing residual time remains unchanged.

It is a bounded, nonpreemptive list scheduler, not an optimal scheduler or simulation
of cache behavior, kernel occupancy, network routes, dynamic collective algorithms,
or every queueing policy. Busy percentages refer to modeled slots, not measured
hardware utilization. Changed slot counts can expose contention or new bottlenecks.

## Node scaling, placement, and feasibility

`--scaling fixed` rejects node-count changes. Strong/weak changes require a
`scaling_assumption`; they retain a representative phase graph rather than copying
rank lanes to invent a larger execution:

- `node_scaling: {"kind":"fixed"}` keeps a phase's factor unchanged.
- `{"kind":"distributed"}` uses source nodes / target nodes for strong scaling,
  and factor 1 for weak scaling, assuming ideal balanced partitioning.
- `{"kind":"power","exponent":E}` applies (target/source nodes)^E, E from -2 to 2,
  for declared overhead/demand hypotheses.

Slots remain per representative partition/node; they are not multiplied again
by node count. Rates must remain applicable to the changed work size. The graph
does not infer new communication patterns or collective algorithms. Placement
changes require a `placement_assumption`. Efficiency is relative to the measured
source and includes hardware changes, so it can exceed one.

`memory` maps resources to `bytes_per_source_node` and a `distributed` flag.
Distributed memory divides by the strong-scaling ratio with exact ceiling;
fixed or weak-scaling per-node memory stays unchanged. Insufficient declared
budgets produce `infeasible`; unknown required capacity produces `unsupported`.
`required_features` are checked against the declared supported set; missing required
features are infeasible under that profile. Unspecified memory/features are not
established. Aggregate per-node budgets do not prove detailed per-device placement
or shared/global capacity.

## Ranges, status, and reproducibility

The engine evaluates fast, central, and slow parameter cases. The displayed range
is the envelope of those three schedules, not a guarantee about all mixed parameter
combinations or scheduling anomalies, and not a statistical confidence interval.
Source uncertainty is included in ratios; identical source/target profiles share
calibration identity and cancel it. Modeling error, missing work, and clock
uncertainty are not silently absorbed into parameter bounds.

Scenarios are `estimated`, `unsupported`, or `infeasible`. The latter two have null
runtime/speedup fields, not fabricated zeros. Artifacts include the request,
assumptions, warnings, engine version/source digest, capture/evidence/profile/model
hashes, source reconstruction, projected tasks, and critical paths. Counts and
nanoseconds are strings; service ratios use floating-point arithmetic and schedules
are rounded integers. Nanosecond predictive accuracy is not implied. Measured
values are preserved exactly, including values above 2^53.

Capture hashes cover raw bytes. Profile/workload/evidence hashes cover normalized
Rust-schema JSON serialization (defined field order, sorted maps), not input-file
whitespace; use hashes emitted by the engine. Replay reloads evidence and refuses
a changed capture hash. Edited requests get new parameter hashes. Held-out
observations match workload/target hashes and node count, so changed hypotheses
cannot silently reuse old validation errors.

Bounds: 10,000 retained evidence observations (32 MiB retention budget), 4,096
phases, 65,536 explicit edges, 64 resources/profile, 1,024 slots/resource, 32 targets,
64 scenarios, and 16,384 phase/scenario combinations. Model files are at most 4 MiB;
resolved requests at most 32 MiB. `--max-events` controls retention up to 10,000.
Omissions require an explicit partial-evidence assumption; absent IDs are rejected.

## Independent measurement validation

`compute_fixture` emits real stream-v2 `lcg.work` annotations from monotonic-clock
measurements, including iteration count, checksum, and algorithm hash:

```sh
cargo build --release -p rprof-project --example compute_fixture
python3 scripts/validate-projection.py collect \
  --fixture PATH/compute_fixture --output-dir NEW_COLLECTION_DIR
python3 scripts/validate-projection.py compare \
  --source-dir SOURCE_COLLECTION --target-dir TARGET_COLLECTION \
  --rprof PATH/rprof --output-dir NEW_RESULT_DIR
```

Use the **same executable bytes** on both machines. The workflow records a plan
before collecting: one warm-up, three 120-million-iteration calibration runs,
three 300-million-iteration held-out runs, and a 15% tolerance. Target held-out
durations never fit profiles; prediction is saved before target comparison.
Collection records executable/capture/algorithm hashes, CPU identity, affinity,
kernel, and available toolchain context. Compiler differences cannot explain this
experiment because executable hashes are identical.

Recorded cross-host result (single CPU pinned on each host):

| Quantity | Value |
| --- | ---: |
| Source | Intel Core Ultra 7 165H, local WSL |
| Target | AMD EPYC 9V33X, HBv4 `tst4-hbv4-1` |
| Source calibrated median | 518,555,555.549 LCG iterations/s |
| Target calibrated median | 921,413,014.193 LCG iterations/s |
| Source held-out median body duration | 662,894,045 ns |
| Predicted target body duration | 373,065,481 ns |
| Target held-out median | 325,317,461 ns |
| Relative error / tolerance | 14.68% / 15% — pass |

Source timings were variable, and the target observation lay outside the evaluated
calibration-derived range. This narrow pass supports this LCG-ratio experiment
only, not arbitrary workloads or universal CPU speedups. Roofline, transfers,
contention, capacity, and strong/weak scaling have analytical tests; independent
GPU/network/storage/multinode calibration matrices remain follow-ups. The HBv4
allocation was released. Captures and artifacts are under `reports/phase8/`
(ignored by Git).

## Development

The base Rust build uses checked-in WASM and requires neither Node nor a WASM SDK.
When engine sources/dependencies change:

```sh
rustup target add wasm32-unknown-unknown
bash scripts/build-projection-wasm.sh
npm --prefix ui run build
npm --prefix ui run check-generated
```

The manifest checks engine/Cargo source hashes and WASM bytes. Browser tests check
native/WASM parity, edited rates, node sweeps, infeasibility, invalid inputs, stale
engines, safe labels, and themes. Rust tests cover analytic critical paths,
contention, source identity under retention, ranges, feasibility, partial evidence,
invalid input, replay integrity, and hash-bound validation.
