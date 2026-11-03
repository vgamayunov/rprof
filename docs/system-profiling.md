# CPU utilization and host memory

Collect Linux host metrics on the same timeline as native application activity:

```sh
cargo build --release -p rprof-cli
target/release/rprof record --collect system --system-interval-ms 1000 \
  -o system.jsonl -- ./application
target/release/rprof view system.jsonl --open
```

No preload library or perf permissions are needed. Add `system` to other collector
lists, for example `--collect python,io,system` or `--collect mpi,openmp,system`.
Sampling is opt-in; previous captures cannot supply measurements they did not record.
The interval defaults to 1 second, configurable from 100 ms to 60 seconds.

## Timeline hierarchy

The selected clock domain starts with two compact graphs:

- **CPU utilization · host average** → host → logical CPU.
- **Memory used incl cache · host total** → host → NUMA node.

Click the row's arrow/name to expand a level; the CPU/Memory toolbar controls toggle
the overviews. Graphs remain visible when collapsed. Filtering can reveal a particular
host, CPU or NUMA node. Counter points expose exact values and definitions in the
inspector. Memory is displayed in GiB with exact bytes retained; CPU axes use 0–100%.

System tracks carry a stable `system.host_id` (hostname/boot identity) and a
`resource` key (`cpu`, `cpu:N`, `memory`, or `node:N`). Overview averages are derived
in the viewer; original samples remain intact in the report/capture. Use a current
rprof reader for captures containing the new `system` track kind.

CPU/NUMA rows are canvas-rendered, not thousands of DOM elements. Only visible rows
and bounded samples from the visible time window are drawn. Derived host groups and
aggregate series are cached, and large overview series have bounded time bins.

## What the measurements mean

- CPU utilization uses consecutive `/proc/stat` snapshots:
  `100 × delta(total − idle − iowait) / delta(total)`. Total includes the first eight
  CPU tick fields; guest ticks are not added twice. Steal time is included. Each host
  and logical CPU stays in the range 0–100%; this is not application CPU usage.
- The first CPU snapshot is a baseline, not a fabricated zero. No elapsed ticks,
  failed reads, decreasing counters and newly online CPUs can leave gaps. Aggregate
  CPU deltas are reset when the observed online CPU set changes.
- Memory uses `/proc/meminfo` and `/sys/devices/system/node/nodeN/meminfo`:
  **MemTotal − MemFree**, consistently for hosts and NUMA nodes. It includes cache
  and reclaimable memory. Host `MemAvailable`, when supplied, is retained in sample
  attributes. This is not process RSS, GPU memory or memory pressure.
- Metrics describe the host view exposed by procfs/sysfs, including other workloads
  and the profiler. Containers may expose host-wide values rather than cgroup limits.
  Files are sampled sequentially; snapshots are not atomic across CPUs/nodes/hosts.
- Read/parse/topology failures are reported in `system.meta.read_errors` and report
  warnings; missing values are not zero. Structural/capture limits still apply.

## Multiple hosts and ranks

Pass `--collect system` to every rank with a common `RPROF_SESSION_ID`. rprof uses a
session/host/user-scoped file lock to elect **one concurrent sampler per host**.
The others record that sampling is delegated. Retain all rank captures: the host
samples live in the elected rank's capture, not necessarily rank zero's.

```sh
export RPROF_SESSION_ID=$(cat /proc/sys/kernel/random/uuid)
mpirun -np 4 -x RPROF_SESSION_ID -x LD_LIBRARY_PATH \
  /shared/rprof/target/release/rprof record --collect mpi,system --mpi-clock-sync \
  --output-dir /shared/captures/run1 -- /shared/apps/mpi-app
target/release/rprof merge -o combined.jsonl /shared/captures/run1/*.rprof.jsonl
target/release/rprof view combined.jsonl --open
```

Use your normal allocation, host placement and binding. The example assumes the
matching MPI adapter is built and all MPI ranks enable clock calibration. For Slurm,
place rprof after `srun`, inside the rank's environment/container.

Ranks must share the same temporary-directory view on each host for the lock to
deduplicate them. Zero-byte `rprof-system-*.lock` files remain after a run to avoid
lock-inode races; stale files do not hold locks. Separate container/temp namespaces
can still produce duplicate sources; the viewer selects one retained source per
host identity rather than adding duplicates. Sampling covers the elected rank's
supervised lifetime; a crashed/early-exiting owner is not automatically replaced by
already-running delegated ranks. Gaps remain visible.

**Cross-host averages require aligned clocks.** MPI calibration creates an explicitly
estimated common domain. Within that domain, the overview CPU value is an equal-weight
mean of host utilization; memory is a sum of host used bytes. These are sampled
estimates using bounded time bins, only when every displayed host has a sufficiently
close observation. Missing hosts are not counted as zero or carried indefinitely.
Unaligned hosts remain in separate domains. In particular, imported PyTorch traces
do not automatically align with native system samples.

## Capacity and validation

Per host, each sample adds approximately `logical CPUs + NUMA nodes + 3` counter
observations. For 176 CPUs and 4 NUMA nodes, a one-second interval is about 183
observations/second/host. Collapsing graphs reduces rendering work, not capture
volume or retained timeline memory. Prefer a longer interval and shorter captures
before scaling host count. Capture and browser [capacity limits](capacity.md) apply;
system tracks also count toward the global 65,536-track limit.

The collector bounds observed topology to 8,192 logical CPUs and 1,024 NUMA nodes;
source files are bounded to 2 MiB. Tests cover CPU arithmetic, guest accounting,
counter resets, missing reads, exact memory values, sparse NUMA IDs, source-health
warnings beyond timeline retention, and sampler election.

A short tst3/HBv4 validation used allocation 20 on `hbv404EX3U` and `hbv4A7I7HY`:
four MPI ranks (two per host), 200 ms sampling, Open MPI 5 with estimated clock
alignment. Exactly two host samplers were elected, each exposing 176 logical CPUs
and four NUMA nodes. The merged capture had zero drops, omissions and system read
errors; application checksums passed. The allocation was released. This is a
correctness check, not a sustained large-cluster performance guarantee.

UI tests also exercise 256 hosts × 128 logical CPUs: the collapsed system view has
two graph rows, and opening one host reveals only that host's CPU rows. Full native
workspace tests, 19 UI unit tests and 28 browser tests passed. A generated report of
the real two-host capture was additionally opened and exercised in Chromium.
