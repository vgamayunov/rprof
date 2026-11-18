# Stack profiling and function trees

This is the first frame-pointer implementation of the stack feature. It collects
user-space CPU samples and optional native API call sites, and presents top-down
and bottom-up function trees directly below the timeline.

## Collect

```sh
make build
target/debug/rprof record --collect stacks --stack-hz 99 -o cpu.jsonl -- ./application
target/debug/rprof record --collect stacks,io --no-storage -o io.jsonl -- ./application
target/debug/rprof record --collect stacks,python,io --no-storage -o python.jsonl -- python3 app.py
target/debug/rprof view python.jsonl --open
```

Collection currently targets **Linux x86-64**. Linux perf must permit user-space
sampling of the workload. A denial fails collection explicitly; the collector
does not change permissions, enable kernel profiling, or substitute counters.

Build native code and relevant libraries with frame pointers, keep their ELF
symbols, and avoid tail-call elimination when retaining callers is important:

```sh
cc -O2 -g -fno-omit-frame-pointer -fno-optimize-sibling-calls app.c -o application
```

Frame pointers are required throughout the call chain. Optimized libraries such
as libffi, GPU drivers and framework runtimes can interrupt unwinding. Missing
callers cannot be reconstructed from a nearby sample. DWARF unwinding, inline
frame expansion and automatic runtime-frame folding are not implemented yet.

`--collect stacks,python` selects the Python `stacks` backend when the backend is
`auto`. This uses CPython's perf trampolines, **not function tracing**. It requires
a GIL-enabled CPython 3.12-3.14 build with frame-pointer support. Unsupported builds,
existing trampoline ownership and incompatible tracing-backend choices produce
diagnostics. Explicit user ranges remain available. Python symbol files are
pinned during initialization so they can outlive their interpreter.

For MPI, build the matching adapter and keep the existing one-profiler-per-rank
launch pattern, shared `RPROF_SESSION_ID`, and shard merge. Add `stacks` to each
rank's `--collect mpi,...` list. For CUDA, rebuild the target CUDA adapter and use
`--collect stacks,cuda`. MPI, CUDA and I/O capture call sites on the calling thread;
GPU launch attribution uses supplied CUPTI correlations.

Rebuild native adapters with the CLI. The I/O wire is now version 3 (400 bytes),
and native stack capability counters detect adapters without call-site support.

## Read the function panel

**Top-down** displays callers followed by callees. **Bottom-up** starts with sampled
leaves or attributed call sites and expands their callers. Both orientations use
the same recorded weight. Recursion retains distinct call paths. Rows are
virtualized, branches are individually collapsible, and arrow keys navigate them.

Choose combined functions, rank/shard groups, or thread/stream groups. The timeline
filter applies to event/track labels, category and rank. **Find function** filters
displayed functions and keeps ancestor context; it does not change totals.

**Filter by selection** recomputes function totals for the selected interval.
Panning, zooming, scrolling and collapsing timeline tracks do not change the
function scope. Clearing the range restores the complete selected clock domain.
Existing collector summary tables keep their previous scope.

| Metric | Meaning |
| --- | --- |
| CPU | Recorded cpu-clock sampling periods attributed to mixed stacks. Self weight belongs to the sampled leaf. This estimates CPU time; samples are not calls. |
| MPI API duration | Inclusive host API duration, including waits. Nonblocking request lifetime is excluded; this is not network execution time. |
| MPI bytes | Known logical bytes at API completion, not network wire volume. Unknown byte counts are excluded. |
| CUDA host API duration | Host API time, separately from GPU execution. Nested runtime/driver API durations can overlap. |
| GPU duration / bytes | Timed kernel/copy/memset work and known transfer/memset bytes attributed to host launch stacks. Streams can overlap; these are not utilization or GPU device stacks. |
| I/O duration / bytes | POSIX call duration and completed bytes attributed to issuing stacks. Storage device, NFS and Lustre measurements are separate layers. |

Ranges are half-open. Point samples and bytes are selected by their timestamp;
bytes use completion time and are never prorated. Durations are clipped to the
range intersection. Completed-observation counts use completion time independently
of duration: a boundary completion can have a count but no duration in the window.
GPU work can retain its launch stack even when the host launch was outside the range.

Do not add CPU, MPI, GPU and I/O durations into a wall-clock total. Percentages use
the selected channel's recorded weight, including observations without stacks.
Unknown frames and truncated callers remain explicit; no missing work is
extrapolated. Sampled CPU excludes off-CPU waiting, but can include busy waits and
profiler callback overhead within the application process.

Raw Python/JIT identities remain process/image scoped. Native functions combine
across ranks using ELF build/symbol identity; absent build IDs are not guessed to
identify equivalent functions across captures. Unaligned clock domains stay
separate, including imported framework clocks.

## Bounds, formats and indexed viewing

Sampling uses per-CPU perf rings over the workload's initial affinity, with
inherited threads/children and a pre-exec mapping handshake. The initial affinity
must contain 1-240 CPUs. A later move outside that affinity is outside collection
scope. Each ring has eight data pages; the drain batch is capped at 65,536 records.
Frequency is configurable from 1 to 1,000 Hz (default 99). Kernel call-chain limits
apply, with at most 128 sample frames retained; native API snapshots retain at
most 64 return addresses.

Capture v3 adds reliable, shard-local `stack_trace` definitions and weighted
`stack_sample` records. API spans use an optional `stack.id` attribute; missing
references remain unattributed. A file's stack dictionary is capped at 65,536
entries / 32 MiB. Dictionary and unwind failures retain the underlying sample or
operation where possible rather than fabricating a stack. Symbol/name metadata is
bounded, and source losses, throttling and symbol failures are reported separately.
Arguments, local values and raw stack-memory dumps are not written.

Readers still accept v1 and v2 captures. New recording and merging write v3, which
older binaries cannot read. Existing captures cannot gain unrecorded stack data.
Chrome Trace export currently omits stack definitions/samples and reports that
omission explicitly; use HTML or the indexed viewer for function analysis.

The SQLite index stores complete stack facts and queries the selected scope
independently of visible timeline rows and raw-event retention. Function queries
return at most 20,000 tree nodes; larger trees report a limit error and require a
narrower scope. Query failures retain visibly stale data until retry or scope change.

Standalone HTML embeds a separate complete filtering dataset when it fits
100,000 normalized facts / 16 MiB. Aggregation runs in an embedded worker without
external requests. If that dataset does not fit, exact offline function analysis
is disabled with guidance to use `rprof view`; a retained prefix is not shown as
a complete profile.

## Remaining feature-plan work

Mixed native/Python chains work where frame pointers and trampoline metadata are
available. Broader optimized-library coverage needs deferred DWARF unwinding.
PyTorch's framework-specific operator/worker provenance and normalization of
imported framework stacks remain separate work; CPU sampling alone cannot invent
forward-call context on an autograd worker. Native CUDA and framework-owned Kineto
must still obey their existing exclusive CUPTI ownership rules.

OpenMP worker computation can be sampled, but dedicated OpenMP event call-site
stacks, general scheduler/off-CPU profiling, GPU instruction-level stacks and
lazy server-side expansion of very large trees are not included in this increment.
