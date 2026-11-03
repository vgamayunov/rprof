# GCC/libgomp profiling

The GNU backend profiles the application's **actual libgomp** through reviewed
GNU ABI entry points. Initial support is Linux x86_64, GCC 13, dynamically linked
libgomp; GCC 13.2 was exercised locally and on HBv4. LLVM/Intel use the separate
[OMPT backend](openmp-profiling.md).

## Build on the target runtime image

```sh
cargo build --release -p rprof-cli
make gomp PROFILE=release GOMP_CC=gcc
target/release/rprof record --collect openmp --openmp-backend gomp \
  -o gomp.rprof.jsonl -- ./application
target/release/rprof report gomp.rprof.jsonl --format html -o gomp.html
```

`target/release/rprof-gomp-build.txt` records compiler, library path and SHA-256.
The adapter verifies the loaded target's SHA-256 before using typed wrappers,
and checks each resolved entry point belongs to that DSO. Rebuild after runtime
updates. The fingerprint binds to the build target; it is not a claim that every
vendor-patched GCC 13 runtime has been independently tested.

`--openmp-library PATH` selects a target-built adapter. The CLI uses `LD_PRELOAD`;
the adapter has no link dependency on libgomp, libomp or libiomp5. A mismatched
runtime is diagnosed and original arguments are tail-forwarded unchanged. No
initialization produces a collector failure with the application's exit status
retained in a readable capture. Backend selection is explicit and happens before
launch; the default remains `--openmp-backend ompt`.

## Choose observation volume

| `--gomp-detail` | Observations |
| --- | --- |
| `regions` (default) | Joined parallel regions and inclusive outlined parallel bodies |
| `bodies` | Also supported ordinary tied-task creation, bodies and creation-to-body-return intervals |
| `full` | Also reviewed API calls, observable signed loop chunks and sections |

Start real applications with `regions`. `full` can generate about four observations
per dynamic work chunk, making `schedule(dynamic,1)` effectively per-iteration
traffic. Coarse modes forward unselected APIs directly. Capability metadata records
disabled observations explicitly. See [capacity planning](capacity.md).

## Meaning and coverage

- Parallel-body duration is inclusive wall time inside the compiler's outlined
  function. Nested work, runtime calls, waits and OS descheduling can be included.
- API-call duration can include execution of other tasks. It is not pure wait time.
- Task creation to outlined-body return is not proof of runtime task completion.
  No task scheduler callbacks or task-dependence edges are inferred.
- PID/TID and MPI/OpenMP physical thread identity are shared. GNU task/region IDs
  describe the nearest observed outlined-body context, not exact scheduler state.
- ThreadBegin means first observed use; worker-pool ThreadEnd is unavailable.
  Code pointers are opaque addresses, not symbolized source locations.
- Compiler-inlined static chunks, unsigned/ordered loop chunks, single-executor
  body extents, legacy split parallel bodies, offload, and runtime-owned reduction
  layouts have partial or unavailable coverage.
- Untied, detached, taskloop, reduction and unknown task forms retain their original
  payloads and execute unchanged, with API-only coverage in full mode. Ordinary
  tied tasks support firstprivate copy callbacks, including over-aligned C++ data,
  up to 1 MiB payload and 4,096-byte alignment. Task-body replacement is disabled
  when OpenMP cancellation is enabled.
- Work-context nesting is bounded at 128; decoder and capture structural limits
  also apply. Missing endpoints stay incomplete. Fork after observed/in-progress
  initialization disables child instrumentation and emits a diagnostic; use fresh
  exec/rank-local collection for HPC workloads.

The reviewed surface is listed in
[`symbols.def`](../crates/rprof-gomp/native/symbols.def) (72 entry points). Reports
show backend, target identity, selected detail, capabilities, observed counts and
loss diagnostics. GNU API/body aggregates are independent of per-region detail
retention. Wait and scheduled-time fields are **unobserved**, not measured zero.
The GNU OpenMP date is the target compiler's macro, not a runtime conformance claim.

## Hybrid MPI and validation

Build `make mpi PROFILE=release` using the application's MPI module. Launch one
profiler per rank with `--collect mpi,openmp --openmp-backend gomp`, a shared
`RPROF_SESSION_ID`, and `--output-dir`. Use `--mpi-clock-sync` only when every
participating rank is profiled with the same setting. See [MPI launch examples](mpi-profiling.md).

`bash scripts/test-gomp.sh` checks all three detail levels, nested regions/work/tasks,
aligned C++ firstprivate copies (deferred and undeferred), unsupported task forms,
no initialization, errno/fork behavior, cancellation-enabled task passthrough,
and a deliberately wrong fingerprint. CI pins the compiler to
GCC 13. A local full-detail smoke collected 12,841 native records without loss,
including 32 regions and 57 task bodies/creation-to-return intervals.

On 2026-11-02, tst3 allocation 17 supplied `hbv404EX3U` and `hbv4A7I7HY`:

- One rank, 128 threads, 20 regions: 5,302 OpenMP native records, 2,560 bodies.
- Two nodes/ranks, 128 threads each, 20 regions each: 5,302 OpenMP and 27 MPI native
  records per rank, 5,120 complete bodies in the merged report, 262 tracks.
- Application checksums matched; no ingress/writer drops, sequence gaps, unmatched
  endpoints or abandoned intervals. MPI clocks were explicitly calibrated.
- The successful hybrid used Open MPI 5.0.10, CPU TCP transport restricted to `eth0`,
  and matching `LD_LIBRARY_PATH` on both nodes. HPC-X CUDA/NVML initialization failed
  on the CPU image; unrestricted TCP also failed during unprofiled runs. The test
  used `--mca pml ob1 --mca btl self,tcp --mca btl_tcp_if_include eth0
  --mca coll ^hcoll --mca osc sm`. The allocation was released.

These are short correctness/integration checks using a controlled low-rate body,
not an application-overhead or sustained-throughput benchmark. The fixture is
`crates/rprof-gomp/tests/fixtures/capacity.c`; build with GCC `-fopenmp`, or add
`-DRPROF_MPI` with the matching MPI compiler wrapper.
