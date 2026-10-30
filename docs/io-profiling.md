# POSIX I/O profiling

## Build and collect

Build the optional preload library with a C compiler and the Rust toolchain:

```sh
make build PROFILE=release
target/release/rprof record --collect io -o capture.rprof.jsonl -- ./application
target/release/rprof report capture.rprof.jsonl
target/release/rprof report capture.rprof.jsonl --format html -o io.html
target/release/rprof view capture.rprof.jsonl --port 8080
```

`--collect io` enables I/O plus the always-active process supervisor. The default
remains process-only. `make preload` builds a debug collector; `make build` also
builds the CLI. Cargo alone continues to build the Rust components without requiring
the C link step. The native shared library is `target/<profile>/librprof_io.so`.

The CLI looks for the library alongside its executable. Use
`--io-library /path/to/librprof_io.so` for another location. It prepends the library
to the workload's `LD_PRELOAD`, preserving existing entries. A missing library is
rejected before launch; a workload in which the collector never initializes is
reported as a collection failure, with its actual exit status retained in metadata.
Loader paths must not contain whitespace or colons, which delimit `LD_PRELOAD`.

The native collector is Linux/glibc-oriented and requires a 64-bit target; tests
currently run on x86-64 Linux. Build/install a compatible CLI and library on the
target system. Remote usage is the existing direct-collection plus SSH-tunneled
viewer workflow in [remote operation](remote-operation.md).

## Intercepted calls

| Calls | Recorded behavior |
| --- | --- |
| `open`, `openat`, `creat` | Flags, result/errno, path, and new descriptor lifetime |
| `read`, `write` | Requested and completed bytes, latency, result/errno |
| `pread`, `pwrite` | The above plus explicit file offset |
| `close` | Descriptor lifetime termination and result/errno |
| `fsync`, `fdatasync` | Flush latency and result/errno |
| `dup`, `dup2`, `dup3` | Descriptor duplication, target descriptor and subsequent lifetime identity |
| `lseek` | Requested offset/whence and resulting position |

Large-file `*64` aliases and glibc fortified open entry points are covered and
normalized to the corresponding operation names above. C wrappers handle variadic
creation modes, including `O_TMPFILE`, and preserve libc return values and `errno`.
Ordinary operations call the real libc entry points, retaining deferred cancellation
behavior. Logging uses raw syscalls and adds no pthread cancellation points.
Recursive loader/hook calls use non-instrumented syscall fallbacks.

Timing brackets the libc operation. Metadata collection, IPC, and later JSON
processing are outside the individual call's measured duration. They can still
perturb application wall time; the supervised-workload span includes that runtime
overhead. Final collector draining/serialization is outside the workload span but
included in the overall capture duration.

## Timeline and summaries

I/O calls appear by their actual operation names under:

```text
supervised workload → process image → thread → file/resource → I/O call spans
```

Each call retains process/thread IDs, process-image identity, descriptor and
descriptor-lifetime ID, file/path flags, requested/completed bytes, result/errno,
and explicit offset where available. An implicit-position `read`/`write` has offset
`-1`; the collector does not issue extra seeks or invent offsets in concurrent code.

Text and HTML reports aggregate captured calls by reported file identity within
a host/boot clock domain. They show completed/requested bytes, errors, operation
counts, average/max operation latency, and observed-window bandwidth. These are
**POSIX API metrics**, not physical disk bytes or device bandwidth. Overlapping
latencies can double-count elapsed time. With event loss, byte/count totals describe
the retained observations and are not extrapolated to the entire workload.

The HTML I/O panel follows the selected clock domain and paginates file/operation
tables. Full-capture summaries are independent of HTML timeline event retention.
Summary metadata is bounded to 8,192 identities and 16 MiB of file-key metadata;
omitted summary events are reported explicitly.

### Identity and paths

Native collectors share a process-image hash derived from Linux exec randomness
and PID; the seed bytes themselves are never serialized. Normal `fork` children reset
their descriptor namespace; exec loads a fresh runtime instance even when the PID
is unchanged. The external profiler combines these process/thread/file tracks in
the invocation's capture shard. No JSON writer threads or locks are inherited by
the application child.

Descriptors below 65,536 get generation IDs. Duplicated descriptors have distinct
descriptor lifetimes, and close/reuse cannot silently reuse the previous generation.
Larger descriptors retain path and call data but are explicitly marked as lacking
tracked lifetime identity. Concurrent close/rebinding of the same descriptor during
an operation remains an application-level race; attribution uses the pre-call snapshot.

Paths are resolved through `/proc/self/fd` at open or first observation and cached
for the descriptor lifetime. Rename/unlink changes are not followed, so the cached
name is the observed/opened name, not necessarily the file's latest pathname.
The bounded path field holds 256 bytes. Truncation/unavailability is explicit, and
truncated identities remain separated by process/resource. Failed opens retain
their supplied argument when readable. Kernel-assisted copying preserves EFAULT
behavior for invalid pointers. Non-UTF-8 paths retain their raw bytes as hex in
addition to a display string. Path aggregation does not assert physical inode or
cross-mount-namespace equivalence.

## Buffering and loss accounting

The injected Rust runtime uses fixed storage and atomics, with no event allocation,
JSON serialization, capture-file output, or blocking queue submission in callbacks.
It sends 352-byte observations over a nonblocking inherited Unix datagram socket.
An external ingress thread feeds a bounded raw-event queue; a separate thread
converts observations into the existing capture writer. This decouples short bursts
from JSON conversion without blocking the workload on capture storage.

- `--io-buffer-events` defaults to 16,384 raw events (about 5.5 MiB payload), with
  a maximum of 262,144. The kernel socket budget is additionally bounded/clamped
  by the OS. Existing `--chunk-events` and `--queue-chunks` control the JSON writer.
- Descriptor IDs use 512 KiB of fixed storage per image. The atomic path cache
  reserves roughly 17 MiB of address space; only touched descriptor pages become
  resident. Fork invalidation avoids touching every cached-path page.
- Shared mmap counters survive fork, exec, `_exit`, and fatal workload signals.
  They count logging attempts, accepted native sends, initialization, and ingress
  drops. The external receiver reconciles attempts versus received valid records
  after workload cleanup and queue draining.
- The v2 loss record adds optional `ingress` (default 0 for older captures), distinct
  from writer queue-full and oversized-event counts. Use the updated reader/report
  code to include this new loss category in totals.
- Final diagnostic counters on the workload track expose `io.attempted_events`,
  `io.received_events`, `io.ingress_dropped_events`, and `io.initialized_images`.
  Missing footers still imply only checkpointed lower bounds.

The transport requires inherited descriptors and collector environment variables.
Programs that close all inherited descriptors or strip the environment can stop
descendant collection. Collection is local to the supervised process tree; a
launcher does not automatically propagate this IPC channel to another machine.

## Coverage boundaries

This phase covers completed calls through the listed dynamically interposed POSIX
entry points. Static/secure-execution binaries, direct syscalls, `mmap` I/O,
`io_uring`, vectored I/O, and libc-internal stdio paths are not covered by these
wrappers. In particular, `fread`/`fwrite` may use hidden libc calls that bypass them.
Pending or cancelled operations that never return have no completed I/O span.
Named MPI communication calls are provided by `--collect mpi`. MPI-IO API wrappers
remain future coverage, although dynamically linked POSIX calls underneath the MPI
runtime may already be visible here.

## Verification and throughput probe

`make test` builds the shared library and runs Rust/native integration tests.
`cargo test --workspace` also builds it on demand in the native test suite, or uses
`RPROF_TEST_IO_LIBRARY` when provided. Native fixtures cover content, errno, modes,
invalid pointers, partial I/O, aliases, descriptor reuse/duplication, threading,
fork/exec, signal termination, deferred pthread cancellation, long/non-UTF-8 paths,
and loss reconciliation.
Browser tests also collect real I/O and inspect its spans and per-file totals.

A release-build cached-I/O stress probe performed 20,000 repeated 4 KiB `pwrite`
calls at the same offset: 0.005 s direct, about 0.043 s for the supervised workload,
and 0.170 s including capture draining/output. Of 20,002 wrapper observations,
19,908 were received and 94 ingress drops were accounted for. This short cached
burst is overhead-sensitive and is not a disk-throughput benchmark. Larger raw
buffers can absorb longer bursts; batching/encoding optimization remains useful
for sustained tiny-call workloads. No native-collector overhead target is claimed
from this one local probe.
