# Linux PMU and software perf counters

`--collect pmu` enables counting through Linux `perf_event_open`. The Rust
`rprof-perf` crate has no SDK, preload library, or `perf` executable dependency.
`perf` is needed only for independent reference validation.

```sh
cargo build --release -p rprof-cli
target/release/rprof record --collect pmu -o counts.rprof.jsonl -- ./application
target/release/rprof report counts.rprof.jsonl --format html -o counts.html

# Explicit event set, 20 ms counter-read interval, root thread only:
target/release/rprof record --collect pmu --pmu-events cycles,instructions,task-clock \
  --pmu-scope root-thread --pmu-interval-ms 20 -o root.rprof.jsonl -- ./application
```

## Events, scope, and startup

Default events are `cycles,instructions,cache-references,cache-misses,task-clock`.
Other supported names are `branches`, `branch-misses`, `bus-cycles`, `ref-cycles`,
`cpu-clock`, `page-faults`, `minor-faults`, `major-faults`, `context-switches`, and
`cpu-migrations`. Perf's `cpu-cycles` and `branch-instructions` aliases normalize
to `cycles` and `branches`; requesting both names for the same event is rejected
as a duplicate. Hardware-specific `rHEX` encodings map directly to
`PERF_TYPE_RAW/config`; use only encodings appropriate to the recorded CPU. Named
sysfs aliases, uncore PMUs, `config1/config2`, and event modifier strings are not
implemented. At most 32 unique events can be requested.

Counters are installed **in the workload child before exec**, disabled with
`enable_on_exec`, and transferred to the parent via `SCM_RIGHTS`. The kernel enables
them on the workload's first successful exec. No post-launch attach race misses
short-lived programs, and dynamic-loader/startup execution is included. The
post-fork hook uses stack storage and syscalls without allocation or locks. The
supervisor receives close-on-exec descriptors; the application does not inherit
open counter descriptors. Failed exec retains normal launch-failure semantics.
When combined with in-process native collectors, their executed instrumentation
code contributes to the workload counts; that overhead is not subtracted. The
separate supervisor/reporting process is not counted.

Two scopes are available:

- **`workload-tree` (default):** the root thread plus future inherited threads and
  forked children, aggregated by the kernel. Short-lived workers contribute to
  the final count. These totals do not identify individual worker threads or
  attribute counts to MPI calls, OpenMP regions, or Python operators.
- **`root-thread`:** only the initial thread, including later execs in that task.
  It excludes newly created threads and children.

Each MPI rank can run `rprof record --collect mpi,pmu ...` and merge shards in the
usual way. Rank-local totals stay on the corresponding shard/root track. Wrapping
an MPI launcher alone does not establish per-rank attribution. Inherited counters
follow descendants even if they escape the supervised process group; collection
ends when the supervisor disables the counters after normal group cleanup. Such
escaped processes remain outside the supervisor's lifecycle guarantee.

The requested privilege filter defaults to userspace (`exclude_kernel` and
`exclude_hv`). `--pmu-kernel` requests user+kernel counting, still excluding
hypervisor execution. There is no silent privilege fallback. Event-specific Linux
software-counter semantics apply: a software clock or fault event is not a
hardware instruction/cycle counter, and some software events ignore privilege
filters. Comparisons must use the same event definitions and modifiers.

## Samples, scaling, and derived metrics

The default read interval is 100 ms (`--pmu-interval-ms`, 10–60,000 ms). Reads are
driven by the supervisor heartbeat, so this is approximate polling, not interrupt
sampling. A busy read or scheduler delay can lengthen the interval. Each snapshot
contains:

- cumulative raw `u64` count;
- kernel-provided `time_enabled` and `time_running` in nanoseconds;
- a host-monotonic read-start/read-end bracket;
- event type/config, scope, requested privilege, PID, unit, and final-read status.

The timeline plots cumulative **raw counts**, not rates. These are counter
observations, not sampled instruction addresses or stacks. The inspector retains
the exact count and timing attributes. `pmu.meta.*` records feed capabilities and
host metadata rather than chart rows. Unscheduled readings are also excluded from
plots rather than displayed as measured zeros.

Scaled estimates are `raw × time_enabled / time_running`, rounded to the nearest
integer with `u128` arithmetic. Exact values remain decimal strings in reports,
including values above JavaScript's integer range. Zero running time means
**not counted**, with no scaled estimate; unsupported/denied events have no raw
value. Running time greater than enabled time is invalid, not silently scaled.

Events are independent, unpinned counters, so a busy PMU can multiplex them.
Grouping is deliberately not used with inherited per-task reads. Running coverage
and multiplexing are visible. Scaling assumes the scheduled portion represents
the whole workload; ratios of aggregated inherited counts/times can be biased
when descendants have different scheduling coverage. Perf's inherited enabled/
running times are not interchangeable with elapsed application wall time.

IPC and cache-miss/reference percentages are shown only when both inputs have
usable final counts with matching scopes/privilege. These are estimates from
independently scheduled events, not atomic grouped measurements. Generic cache
event definitions are CPU-dependent; they do not establish memory-traffic bytes,
bandwidth, FLOPs, or CPU utilization. Intermediate snapshots are never summed
into a total: the summary uses the latest cumulative reading.

## Diagnostics, bounds, and source metadata

Every requested event records its open status and errno. Permission denial,
unsupported events, invalid/unsupported configurations, resource exhaustion,
unscheduled counters, and read failures remain distinct. If at least one requested
event is measured, collection can succeed with explicit unavailable-event warnings.
If none is usable, the capture ends as collector-failed while preserving the
application exit code. Neither sysctl values nor capabilities are modified.

After group cleanup, each open counter is disabled and read one final time,
including signal termination. Final records are reliable writer events. Killing
the profiler can leave a readable prefix with checkpoint values; a missing final
read is reported explicitly. Periodic samples use the existing bounded writer
queue and normal loss accounting. User-space event state is bounded to 32 counter
descriptors; inherited kernel state grows with the workload's threads/processes.

Host metadata records kernel release, `perf_event_paranoid`, CPU vendor/model,
online CPUs, launch affinity/NUMA masks, CPU0 topology/cache sharing and available
frequency-governor/bounds, and advertised perf event sources. Metadata reads and
report cardinality are bounded. CPU0/cache/frequency fields are contextual snapshots,
not evidence that the workload ran on CPU0 or that its frequency stayed constant.
Missing metadata is omitted. Continuous topology/frequency tracking, per-thread
counting, sampling/symbolization, and uncore/memory-controller support are future work.

## Validation and current coverage

```sh
# CPU tests plus a five-run comparison; output directory must be new.
bash scripts/test-pmu.sh reports/pmu-reference

# Or compare already-built binaries on a target with perf installed:
python3 scripts/compare-pmu.py --rprof target/release/rprof \
  --workload target/release/pmu-workload --output-dir reports/pmu-reference
```

The fixture touches 4,096 separate 4 KiB pages (huge pages disabled) and runs a
fixed arithmetic loop. Thread and fork modes verify inheritance independently.
Reference comparison uses five separate captures/tool runs, medians, matching
userspace modifiers, and predeclared tolerances: 10% for instructions/faults,
20% for task-clock/cycles, and 50% for cache misses. The latter is a coarse smoke
check for a noisy, architecture-dependent event, not an accuracy guarantee.
`comparison.json` includes all observations, statuses, tolerances, results, and
the raw/enabled/running evidence behind each scaled count. Reference runtime and
running percentage are retained too. `--require-multiplexing` requires measured
running coverage below 98% in both tools on every run. `--workload-arg threads`
or `--workload-arg fork`, `--scope root-thread`, and `--kernel` support scope and
privilege comparisons with matching `perf stat` settings.

On `tst9` / `h10096VKQ6` (Xeon Platinum 8480C virtual machine):

| Event | rprof median | perf stat median | Difference |
| --- | ---: | ---: | ---: |
| task-clock | 103,248,782 ns | 102,830,000 ns | 0.407% |
| minor-faults | 4,150 | 4,149 | 0.024% |
| cycles, instructions, cache-misses | unavailable | not supported | matching diagnostics |

### HBv4 hardware and multiplexing validation

Hardware counters are exposed on **HBv4 compute nodes**, even though the `tst4`
login host does not expose them. Validation ran on `tst4-hbv4-1`, AMD EPYC 9V33X
(family 25, model 17), Azure Linux kernel/perf 6.6.139.1, Rust 1.90.0. A two-CPU
Slurm allocation bound the workload and reference runs to the same CPU set.
Each comparison used five runs; the tolerances above were unchanged.

| Event | rprof median | perf stat median | Difference |
| --- | ---: | ---: | ---: |
| cycles | 484,355,727 | 484,483,160 | 0.0263% |
| instructions | 720,185,242 | 720,185,172 | 0.000010% |
| cache-references | 84,967 | 89,367 | 4.9235% |
| cache-misses | 4,851 | 4,814 | 0.7686% |
| branches | 120,046,576 | 120,046,550 | 0.000022% |
| branch-misses | 6,838 | 6,847 | 0.1314% |
| bus-cycles, ref-cycles | unavailable | not supported | matching diagnostics |

The six supported generic hardware events ran at 100% coverage in the baseline.
Adding AMD raw events `r76` and `rc0` forced eight-event multiplexing:

```sh
python3 scripts/compare-pmu.py --rprof target/release/rprof \
  --workload target/release/pmu-workload --output-dir reports/hbv4-multiplex \
  --events cycles,instructions,cache-references,cache-misses,branches,branch-misses,r76,rc0,task-clock \
  --require-multiplexing
```

These raw encodings are specific to the tested AMD CPU. The observed rprof
hardware running coverage was **71.54–77.26%**; both tools multiplexed in all five
runs. Scaled median differences were **1.394% cycles**, **1.708% instructions**,
**2.420% cache references**, **0.328% cache misses**, **0.034% branches**, and
**0.766% branch misses**. Raw `r76`/`rc0` differences were 1.245%/1.627%.
All met the predeclared tolerances; scaling remains an estimate for other workloads.

Additional five-run hardware comparisons passed:

- Two inherited worker threads: 1,440,205,472 retired instructions versus
  1,440,205,280 in `perf stat`; two workers' page faults were included.
- Forked worker: 720,188,100 instructions versus 720,188,025.
- Root-thread-only with two workers: 174,039 instructions versus 173,870;
  worker instructions and faults were excluded.
- User+kernel: 745,619,700 instructions versus 745,563,793 (0.0075%).
- The CLI `cpu-cycles` alias collected real cycles successfully.

This establishes hardware counting and real multiplexing on the tested HBv4
configuration. Local WSL, H100, and the `tst4` login host continue to exercise
software-only/unsupported behavior; their capability results do not describe
HBv4 compute nodes. The temporary validation allocation was released afterward.
Baseline and multiplexed HBv4 HTML reports were checked in Chromium for exact
raw/scaled values, final status, unsupported events, derived IPC, and coverage
warnings. The multiplexed trace imported all nine raw-counter series into Perfetto
with no nonzero import errors and no capture drops.

Integer scaling, unscheduled counts, partial reads, and derived-metric eligibility
are covered by deterministic tests. Native tests cover pre-exec/short-command
collection, thread/fork inheritance, root-only exclusion, forced seccomp denial,
failed exec, and interruption. UI tests cover domain filtering, exact values,
unknowns, partial status, pagination, safe text, and dark/light themes.

The saved H100 capture was rendered as text and standalone HTML, opened in
Chromium, and exported to Perfetto. Its two available raw-counter series imported
without errors. Local validation passed the full Rust suite and 19 browser tests.
Artifacts and the five-run comparison data are under `reports/phase7/`, with the
HBv4 baseline, multiplexing, thread/fork, root-thread, and kernel comparisons under
`reports/phase7/hbv4/` (ignored by Git).
