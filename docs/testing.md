# Testing

## Local capture-foundation checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The CPU-only fixtures cover:

- Nonzero exits, inherited standard I/O/environment, and session propagation.
- Signal forwarding, ignored-signal escalation, and fork/exec descendant cleanup.
- Cleanup after callback failure without consuming an unrelated child's status.
- Foreground terminal handoff, tested with a pseudo-terminal and a reading workload.
- Partial capture/report recovery after killing the profiler.
- Queue saturation, oversized events, loss checkpoints, and exact event accounting.
- Every truncated prefix of a footer; malformed and oversized frames.
- Ordering, timestamp overflow, identity/reference validation, and unsupported versions.
- Deterministic merges, overlapping local IDs, distinct clock domains, duplicate
  shards, unrelated sessions, no-clobber behavior, and recovery provenance.
- Reading legacy bootstrap captures.

### Synthetic storage probe

Build once, then measure the executable directly so compilation is excluded:

```sh
cargo build --release --workspace --bins --examples
target/release/examples/capture-load small.rprof.jsonl 100000
target/release/examples/capture-load large.rprof.jsonl 1000000
target/release/rprof report large.rprof.jsonl
```

Use fresh output paths. The probe prints Linux peak resident memory from
`/proc/self/status`. Compare maximum resident set size, elapsed time, file size,
and reported event drops across input counts. This tests the JSON staging/writer
and streaming reader; it is not an MPI/CUDA callback-overhead benchmark. Queue
saturation is valid and must be reflected in `recorded + dropped = attempted`.

For workload supervision overhead, compare repeated direct and instrumented runs
of the same CPU fixture, excluding build time. The default 10 ms supervision poll
can add up to roughly one poll interval to short-command measurements; the initial
process span explicitly includes launch, wait, and descendant cleanup overhead.
Native-collector overhead targets will be set using representative HPC workloads.

### Initial local measurements

One release-build run on the development host (Rust 1.96, local filesystem):

| Attempted synthetic counters | Recorded | Dropped | Capture time | Peak resident memory |
| --- | --- | --- | --- | --- |
| 100,000 | 100,000 | 0 | 0.138 s | 3,240 KiB |
| 1,000,000 | 999,232 | 768 | 1.327 s | 3,396 KiB |

These runs exercised default bounded buffering and streaming summary validation.
The tenfold event-count increase left peak memory nearly flat; transient queue
saturation in the larger run was fully accounted for. Capture throughput was about
0.73–0.75 million attempted events/s for this synthetic event shape. Filesystem
caching, scheduling, and event shape affect these figures.

A separate two-second sleep probe took 2.002 s directly versus 2.013 s supervised;
supervised user+system CPU time was 0.017 s, with no dropped events. This is a
sanity check of idle supervision, not a substitute for representative HPC overhead
measurements once native collectors are implemented.

## Native I/O checks

`make test` builds the optional preload shared library, then runs the full Rust
suite including native C fixtures. The I/O integration suite also builds the library
on demand under `cargo test`; set `RPROF_TEST_IO_LIBRARY` to use a prebuilt artifact.
A C compiler, make, and Linux/glibc development headers are required for these tests.

Fixtures cover byte counts/content, return values/errno, file modes, invalid
pointers, partial operations, large-file/fortified aliases, descriptor reuse and
duplication, multiple threads, normal fork/exec, signal termination, deferred
pthread cancellation, long/non-UTF-8 paths, and ingress/writer loss accounting.
Browser tests include a real preload capture, per-file summaries, and named MPI
call filtering/hover/inspection. See [I/O profiling](io-profiling.md) for the support
matrix and cached-I/O overhead probe.

## CUDA checks

SDK-free CUDA tests are part of `cargo test --workspace`: invalid timing, API/GPU
identity, ambiguous synchronization attribution, bounded correlation caches,
exact byte counts, separate source losses, partial multi-image finalization, and
noninitializing/conflicting injection paths.

On an allocated CUDA 13 GPU host, run
`CUDA_VISIBLE_DEVICES=0 PROFILE=release bash scripts/test-cuda.sh`. This builds the
target adapter and fixtures and enables the ignored GPU integration tests. See
[CUDA profiling](cuda-profiling.md#recorded-h100-validation) for the H100 one/two-device
results, CUDA-event and Nsight Systems reference timing, and subscriber/exit controls.
The exported H100 trace also imported into Perfetto with six `timed_add` slices,
35,737,523 ns total kernel duration, 92 flows, and no nonzero import errors.

## Timeline/report checks

Rust tests cover shared summaries, domain offsets/identities, large integers,
retention limits, partial/empty captures, HTML data escaping, trace serialization,
atomic no-clobber reports, real process reports, and loopback viewer behavior.

With Node 24 installed:

```sh
npm --prefix ui ci
npm --prefix ui run typecheck
npm --prefix ui test
npm --prefix ui run check-generated
```

For browser checks, run `npx playwright install chromium` in `ui/`, then
`npm --prefix ui run test:browser`. Tests exercise the generated offline HTML in
Chromium, including filtering, zoom/pan, domain switching, exact counters above
`2^53`, hostile workload labels, and partial/empty states. GitHub CI also runs these
checks and installs Chromium's system dependencies on its Ubuntu runner.

`fixtures/multitrack.jsonl` is deterministic and generated by the `multitrack`
example in `rprof-report`. Its domain-0 trace export was opened in Perfetto UI v59.1.
The reference trace processor reported 16 slices, 12 counters, 6 flows, and zero
import errors. To repeat the independent import check with an installed processor:

```sh
rprof report fixtures/multitrack.jsonl --format trace --domain 0 -o reference.json
trace_processor_shell query reference.json "SELECT name, ts, dur FROM slice"
trace_processor_shell query reference.json "SELECT name, value FROM stats WHERE severity = 'error' AND value > 0"
```

## MPI checks

Pure Rust tests cover request/source timestamps, tracking release, correlation
namespaces, combined collector loss, and explicit-versus-missing clock calibration.
`bash scripts/test-mpi-local.sh` builds the PMPI adapter and runs local two-rank
Open MPI fixtures; GitHub CI has a separate MPI job for it. The script requires
Open MPI, a C compiler, Rust, make, and Node for report assertions.

Two HBv4 nodes were used for real InfiniBand validation of basic calls, wait/test
variants and truncation, combined MPI/I/O, and MPI_THREAD_MULTIPLE. See
[MPI profiling](mpi-profiling.md) for counts, runtime settings, and coverage, and
[glibc compatibility](glibc-compatibility.md) for target artifact requirements.

## OpenMP checks

Base Rust tests exercise cross-collector identity/correlation namespaces, migrated
task segments, detached late fulfillment, and sequence-gap invalidation. The native
OpenMP integration suite includes an explicit no-runtime failure and actual GCC
runtime probing without substitution. Optional Clang/runtime tests run with:

```sh
bash scripts/test-openmp.sh
```

Use the `RPROF_TEST_OMP_*` variables described in [OpenMP profiling](openmp-profiling.md)
for alternate toolchains. LLVM libomp 20.1.8 and Intel libiomp5 2025.3 passed these
checks locally with Clang 20.1.8 code generation. Both also passed two-rank/four-thread
hybrid validation on HBv4. GitHub CI has an OpenMP job using Clang/libomp.
Browser tests cover the optional panel, callback capability versus observation,
domain filters, exact large values, and older models without OpenMP summaries.

## Preferred remote test environments

| Purpose | SSH entry | Preferred partition | Notes |
| --- | --- | --- | --- |
| GPU / CUDA / CUPTI | `ssh tst9` | `h100` | Start on H100. Direct SSH to the selected compute node is available for this work; the existing interactive session does not need to be managed. |
| MPI / multi-rank communication | `ssh tst4` | `hbv4` | InfiniBand is configured; use this environment for MPI transport and cross-node validation. |

These preferences were supplied by the project owner. Phase 1 uses local Linux
fixtures; remote hardware verification begins with the relevant collectors. Before
those tests, discover actual compute-node names, installed MPI implementations,
OpenMP runtimes, CUDA/CUPTI versions, and scheduler layout on the specified hosts.
Record those versions with results rather than assuming an MPI vendor or node name.

Planned hardware fixtures:

- `tst4` / `hbv4`: two-rank ping-pong, nonblocking requests, collectives, multi-node
  clock calibration, and later hybrid MPI+OpenMP execution.
- `tst9` / `h100`: kernel launch, H2D/D2H transfers, multiple streams, overlap,
  host/device correlation, and later combined CPU/GPU/I/O activity.
