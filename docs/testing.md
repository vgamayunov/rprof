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
