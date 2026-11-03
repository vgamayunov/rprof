# OpenMP profiling through OMPT

For actual GCC/libgomp, use the separate [GNU backend](gomp-profiling.md).
This page describes the default OMPT backend for LLVM/Intel.

## Build and collect

```sh
make build PROFILE=release
make ompt PROFILE=release
target/release/rprof record --collect openmp -o omp.rprof.jsonl -- ./application
target/release/rprof report omp.rprof.jsonl
target/release/rprof view omp.rprof.jsonl --open
```

`make ompt` builds `librprof_ompt.so` from C callbacks and the Rust native transport.
It uses the vendored standard OMPT header and does not link to an OpenMP runtime.
`--openmp-library PATH` selects another build. The CLI enables `OMP_TOOL` and sets
`OMP_TOOL_LIBRARIES` for the child. An existing tool list is rejected explicitly:
one OMPT tool owns the callback slots, so another profiler is not silently replaced.

The application's own runtime remains selected by its normal build/loading rules.
No runtime is substituted and the application is run only once. If no OMPT
initialization is observed, collection fails with a readable capture retaining the
application exit status. That can mean an unsupported runtime, disabled tool
loading, or a program that never initialized OpenMP.

## Runtime support

The support goal covers GCC `libgomp`, LLVM `libomp`, and Intel `libiomp5`.
The default backend uses OMPT for LLVM/Intel. GCC 13/libgomp uses the implemented
target-fingerprinted GNU adapter (`--openmp-backend gomp`). See
[runtime support](openmp-runtimes.md) for the full matrix and coverage differences.

Verified during phase 5:

| Compiler/code generation | Loaded runtime | Result |
| --- | --- | --- |
| Clang 20.1.8 | LLVM libomp 20.1.8, built with OMPT | Native fixtures and two-node hybrid capture pass |
| Clang 20.1.8 | Intel libiomp5 from oneAPI 2025.3 | Native fixtures and two-node hybrid capture pass |
| GCC 13.2 | GCC libgomp 13.2 | OMPT absence is reported; the separate GNU backend passes native and two-node 128-thread/rank hybrid checks |
| GCC 13.2 GNU ABI | LLVM libomp 20.1.8 | Workload passes, but incomplete single-executor work callbacks are exposed; not GNU-runtime support |

Intel compiler (`icx`) code generation remains a separate validation item because
that compiler was unavailable in the test environment. The Intel runtime itself
was exercised. Runtime-reported version strings and OpenMP version dates are
preserved verbatim; they are not inferred from the compiler executable name.

## Events and interpretation

The tool registers and records availability for:

- Thread begin/end and parallel-region begin/end.
- Implicit tasks, requested/observed teams, and team indices.
- Worksharing scopes (loops, sections, single, and other runtime-reported kinds).
- Synchronization scopes and synchronization **wait** intervals, including barriers,
  taskwait, and taskgroup.
- Task creation, scheduling, completion/cancellation, detach/fulfillment notifications,
  and task-dependence edges when the runtime supplies them.

Metadata includes OMPT thread/task/parallel IDs, native PID/TID, parent task,
runtime-reported work count and flags, and opaque code addresses for attribution.
Code addresses are not dereferenced or symbolized in this phase. Work counts are
runtime-reported metadata, not necessarily iterations actually executed by a thread.

Parallel/implicit-task envelopes and explicit-task lifetimes have child lanes;
scheduled execution segments, work, synchronization, waits, and MPI calls use the
physical native thread lanes. MPI, I/O, and OpenMP collectors share process-image
and thread identity. Exec gets a new identity, and observed OpenMP thread reuse can
create a fresh thread track.

Task lifetime is distinct from task execution. A task may yield or move between
threads; each observed execution segment uses the executing thread. Late detached
task fulfillment must not stop another task running on the notifying thread. Initial
serial-task envelopes are omitted from the timeline to avoid duplicating the whole
process lifetime. Detached and unusual runtime scheduling paths have conservative
handling; no completion is invented when a terminal event is absent.

**Scheduled task time is not CPU utilization.** It can include runtime work, waits,
or time descheduled by the OS. Work/sync durations are inclusive observations and
can overlap. Region work spread is the maximum minus minimum **recorded work time**
across observed thread-region rows; it is not a hardware efficiency estimate.

## Capability and completeness reporting

Text and HTML reports distinguish callback registration results (`always`,
`sometimes`, `sometimes_paired`, `never`, `impossible`, `error`, or unknown) from
received callback counts. Registration support alone does not establish that all
constructs in a particular compiler/runtime ABI produced complete intervals.

The OpenMP panel shows runtime initialization/finalization, callback counts and
status, parallel-region work/wait totals, observed work spread, per-thread details,
and recorded task totals. Tables are domain-filtered and paginated; unavailable
measurements remain unknown rather than being presented as zero activity.

Per-thread wire sequence numbers detect ingress gaps. Pending scopes on an affected
thread are invalidated rather than incorrectly pairing a later end with an earlier
begin. Missing ends remain incomplete in the report. Sequence gaps, unmatched ends,
and abandoned intervals are reported separately from native ingress/writer loss.
Callback counts are checkpointed periodically and finalized after draining; partial
captures may only have checkpointed counts. Finalized callback counts describe
received native callbacks, not callbacks lost before receipt.

## Resources and limitations

Native observations are fixed 376-byte records. The callback path uses bounded
strings/IDs, preserves errno, and sends nonblocking local IPC; JSON work happens in
the external profiler. `--native-buffer-events` controls the raw queue per collector
(default 16,384). Existing chunk/writer limits still apply.

The decoder bounds active task and interval state at 65,536, nesting at 128, and
track/process metadata using the shared capture limits. Exceeding hard structural
limits is a collection error. Report summaries bound runtime/region/thread metadata
and expose any omissions. Event loss or missing runtime endpoints can make summary
coverage incomplete even when the capture file itself was finalized successfully.

GPU/offload callbacks, source symbolization, exact loop-chunk accounting and OMPT
mutex/lock callbacks remain future work. Multiple OpenMP runtimes in
one process are not supported by this tool instance; a second initialization is
rejected. Fork after OpenMP initialization is not a validated collection mode;
fresh exec/rank-local collection is the supported HPC workflow.

## Hybrid MPI+OpenMP

Build the application with its intended OpenMP runtime and the matching MPI headers
and libraries. Run one profiler per rank as in [MPI profiling](mpi-profiling.md):

```sh
export RPROF_SESSION_ID=$(cat /proc/sys/kernel/random/uuid)
mpirun -np 2 -x RPROF_SESSION_ID -x LD_LIBRARY_PATH \
  /shared/rprof/target/release/rprof record --collect mpi,openmp --mpi-clock-sync \
  --output-dir /shared/captures/hybrid -- /shared/apps/hybrid
```

Every rank must use the same calibration setting. Add `io` for combined POSIX I/O
capture. The current workflow remains direct remote launch plus an SSH-tunneled
viewer; no remote job-control service is required.

## Validation

`scripts/test-openmp.sh` runs the optional Clang/runtime integration suite. Set
`RPROF_TEST_OMP_CC`, `RPROF_TEST_OMP_INCLUDE`, `RPROF_TEST_OMP_LIBDIR`, and optionally
`RPROF_TEST_OMP_COMPILER_LD` for a private compiler/runtime installation. Set
`RPROF_TEST_OMP_RUNTIME=intel` to link with libiomp5. Base Rust tests also cover the
decoder's migration, late fulfillment, loss handling, and shared native identities.

On 2026-10-30, allocation `372969` supplied `tst4-hbv4-1` and `tst4-hbv4-2`.
Slurm step launches had an address-family error, so tests used SSH to those allocated
nodes. Both unprofiled and profiled hybrid fixtures passed the checksum with two
ranks and four threads per rank. LLVM and Intel each produced 258 OpenMP and 20 MPI
native records with zero drops; the Intel combined run also captured 28,737 I/O
records with zero drops. All hybrid OpenMP intervals were complete, with shared
MPI/OpenMP physical thread tracks. The allocation was released afterward.

The combined I/O capture exceeded the bounded per-file summary cardinality;
the report explicitly notes those summary omissions while retaining all captured
events/category totals. These tests establish correctness/integration, not a
runtime overhead or scaling benchmark.
