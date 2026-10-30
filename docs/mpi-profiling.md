# MPI profiling

## Build and run one profiler per rank

Build against the application's actual MPI installation:

```sh
module load mpi/hpcx
make build PROFILE=release
make mpi PROFILE=release MPICC=mpicc
export RPROF_SESSION_ID=$(cat /proc/sys/kernel/random/uuid)
mpirun -np 2 -x RPROF_SESSION_ID -x LD_LIBRARY_PATH \
  /shared/rprof/target/release/rprof record --collect mpi --mpi-clock-sync \
  --output-dir /shared/captures/run-1 -- /shared/apps/solver
rprof merge -o run-1.rprof.jsonl /shared/captures/run-1/*.rprof.jsonl
rprof report run-1.rprof.jsonl --format html -o run-1.html
rprof view run-1.rprof.jsonl --port 8080
```

Replace paths/launcher options with the actual environment. Use a fresh session
UUID and capture directory per run. `make mpi` produces `librprof_mpi.so`; the Rust
workspace itself builds without MPI headers/libraries. `--mpi-library PATH` selects
another target-built adapter. See [glibc compatibility](glibc-compatibility.md).

The profiler runs **inside** the MPI launch: every rank gets its own local IPC
channel and unique shard. Wrapping only `mpirun` does not collect distributed ranks;
inherited IPC descriptors do not cross machines. The runtime reports actual world
rank/size and thread support after initialization; standard launcher rank variables
also label early tracks. `--collect mpi,io` enables both collectors, preserves
existing LD_PRELOAD entries, and sums their ingress loss correctly.

## Supported C-binding calls

- Init, Init_thread, Finalize, and an Abort entry marker.
- Send, Ssend, Bsend, Rsend, Recv, Sendrecv.
- Isend, Issend, Ibsend, Irsend, Irecv.
- Wait, Waitall, Waitany, Waitsome; Test, Testall, Testany, Testsome.
- Request_free and Cancel.
- Barrier, Bcast, Reduce, Allreduce, Gather, Scatter, Allgather, Alltoall.
- Ibarrier, Ibcast, Iallreduce.
- Comm_rank, Comm_size, Comm_dup, Comm_split, Comm_free.

All names above have the `MPI_` prefix. Calls appear by their full names on
rank/thread lanes with a distinct color, filtering, hover, and exact-value
inspection. Intervals bracket the PMPI call, with metadata work outside that
interval. Completed-call intervals are emitted on return; a blocking call that
never returns does not yet have a separate open API-call span.

## Requests, metadata, and summaries

Nonblocking request lifetimes have separate child lanes. They run from initiation
start to completion observation, not necessarily the instant hardware finished.
Links connect initiation → request → Wait/Test observation. Test false leaves the
request open. Cancel alone does not complete a request; status determines actual
cancellation. Request_free ends tracking and is **not** proof of completion;
released/unresolved intervals remain marked incomplete.

Association uses handle plus application storage address and a unique generation.
Copied/reused indistinguishable handles can produce explicitly flagged ambiguity.
Metadata locks are not held across blocking MPI calls. MPI_THREAD_MULTIPLE is
supported when the underlying MPI/transport build supports it.

Details include rank/size, thread level, communicator identity, communicator-local
peer/tag, count, logical bytes where known, result, request state, and process/thread
identity. Short/wildcard receives use actual completion status and bytes. Irecv
initiation bytes describe posted capacity; completion bytes describe received data.
Cancelled receives have zero completed bytes. Collective byte fields are local
logical extents, **not network wire volume**. In-place calls use the relevant local
extent; unknown/intercommunicator contexts omit unsafe estimates. Sendrecv records
receive-side information separately from its send extent.

World/self communicators are recognized. Dup/split/free use rank-local generation
IDs and bounded translated membership descriptions, with explicit truncation.
Other creation APIs may leave context unknown. Dynamic IDs must not be assumed to
match globally. Cross-rank message matching and full collective dependency graphs
are future extensions; current API/request links are rank-local.

Text reports include per-rank API counts, total/max duration, errors, and logical
bytes, excluding request lifetimes. The overall MPI category is inclusive and may
double-count overlapping lifetimes and metadata.

## Optional clock calibration

`--mpi-clock-sync` must be applied consistently to **every rank in MPI_COMM_WORLD**.
It duplicates the world communicator, takes eight rank-0 ping-pong samples per
peer after initialization, and repeats before finalization. These extra operations
perturb wrapper/startup time but are outside measured Init/Finalize API intervals.
Without this flag, there are no calibration collectives and host/boot domains stay
unaligned.

Captures retain the best-RTT offset, sample anchor, half-RTT uncertainty, group, and
stage. Reports apply the initial offset and show a budget including final-sample
uncertainty and observed offset drift. This is an **offset-only estimate**, not an
exact synchronized clock or statistical confidence interval; outside-anchor timing
is extrapolated. Missing/conflicting metadata keeps affected native domains separate.

Raw captures are never rewritten. The HTML timeline labels estimated alignment and
uncertainty; trace export uses the same offsets. Calibration uses ordinary counter
attributes, so older readers can retain it without applying alignment.

## Bounds and coverage

The nonblocking same-host transport uses shared loss counters and bounded external
conversion. `--native-buffer-events` defaults to 16,384 per collector
(`--io-buffer-events` is an alias); MPI observations are 472 bytes. Opaque MPI
handles and target pointers never enter the wire/capture formats.

The adapter bounds outstanding requests at 8,192, communicator tracking at 2,048,
and array-operation scratch at 4,096 requests. Scratch uses bounded allocation.
Limits are explicit and may conservatively release tracking without confirming
completion. The receiver does not retain an unbounded map entry per API call.

Persistent requests, Fortran bindings, sessions, RMA, neighborhood/variable-count
collectives, MPI-IO APIs, and unlisted calls do not yet have complete coverage.
MPI/vendor plugins must match the target build. Simultaneous I/O profiling can
include MPI-runtime and profiling-helper I/O; it remains POSIX API observation.

## HBv4 validation

Job `372968` provisioned `tst4-hbv4-1` and `tst4-hbv4-2`; its allocation was released
after testing. The cross-node transport was restricted to InfiniBand using:

```sh
export UCX_TLS=rc,sm,self
export UCX_NET_DEVICES=mlx5_ib0:1
export UCX_MODULES=ib,ib_mlx5,rdmacm,cma
```

Propagate these variables and LD_LIBRARY_PATH to each rank. HCOLL/UCC plugins were
disabled for correctness testing. MPI_THREAD_MULTIPLE required the supplied
`hpcx-mt` module, which selects `ucx/mt`; the ordinary UCX variant rejected that mode.
The Open MPI library/headers remain the same.

| Fixture | Result |
| --- | --- |
| Basic, short/wildcard receives, in-place collectives, cancellation/free | 93 native MPI records; zero drops |
| Wait/Test all/any/some plus intentional truncation error | 138 native MPI records; zero drops; expected error retained |
| Combined MPI + POSIX I/O | 93 MPI + 983 I/O records; zero drops |
| Two MPI threads per rank | 1,806 MPI records; zero drops; 512 completed request lifetimes |

Unprofiled basic/multithreaded fixtures passed too. The first calibrated basic
report had approximately 5 microseconds of uncertainty budget and 45 ns observed
offset drift. These are integration checks, not scaling/overhead benchmarks.
Fixtures live in `crates/rprof-mpi/tests/fixtures`; `bash scripts/test-mpi-local.sh`
runs local two-rank Open MPI checks, and `scripts/validate-mpi-report.mjs` verifies
generated report semantics.
