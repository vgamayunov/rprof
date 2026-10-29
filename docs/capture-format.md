# Capture format and lifecycle

## Versioning and framing

`record` writes version 2 (`*.rprof.jsonl`): compact JSON records separated by
newlines. The newline commits a complete frame. Each frame is at most **1 MiB**,
including its newline. Frames are:

1. **Header:** format name, schema version, session UUID, recovery provenance.
2. **Shard:** shard UUID, optional parent shard, hostname, Linux boot ID, monotonic
   origin, descriptive wall-clock start, supervisor PID, command, collectors/config.
3. **Chunk:** shard UUID, contiguous sequence number, cumulative loss counts, events.
4. **End:** shard UUID, elapsed duration, workload exit/signal, supervisor interruption
   signal, completion reason, and final loss counts.

A file has one header and one or more shards. Each shard is declared before its
chunks, and its footer comes last. Interleaving shards is supported. The merger
canonicalizes shard ordering by UUID and retains each shard's original sequence.

The reader rejects unsupported versions, invalid framing/JSON, duplicate identities,
unknown track references, invalid parents, span timestamp overflow, backward
observation times, chunk gaps, decreasing loss counts, and records after a footer.
Unknown additive JSON fields are ignored. New required fields or event variants
require an explicit compatibility decision and, when breaking, a version bump.

The CLI still reads bootstrap version-1 whole-document captures up to 16 MiB.
New captures and `merge` use v2 only; v1 lacks session/clock identity, so it cannot
be safely merged by inventing that information. Schema types for v1 remain at
the `rprof-core` crate root, while v2 types live in `rprof_core::stream`.

## Identity, timestamps, and event semantics

- A process instance belongs to a shard UUID; PID alone is insufficient because
  PIDs can be reused or namespaced. Tracks and correlation IDs are addressed by
  `(session UUID, shard UUID, local ID)`.
- Numeric local IDs remain unchanged during merge. Identical IDs in different
  shards cannot collide. Duplicate shard UUIDs are rejected rather than deduplicated.
- Event timestamps are observation times relative to that shard's monotonic origin.
  They must be nondecreasing; nested/overlapping spans may have earlier starts.
- Hostname plus Linux boot ID identifies a clock domain. For shards sharing a
  domain, `monotonic_origin_ns + timestamp_ns` gives the common monotonic timestamp
  (use checked arithmetic or wide integers). Cross-host/boot domains stay unaligned.
  Wall-clock start is descriptive only. Clock calibration/drift/uncertainty is MPI work.
- `ActivityStart` preserves open-operation evidence if the final span is lost.
  A finished span with the same correlation ID closes that activity. The process
  activity begins after successful spawn; its final span also includes launch overhead.
- `Dependency` connects shard-local correlation IDs. Forward references are allowed,
  and loss/partial captures can leave unresolved edges. Reporting must display that
  incompleteness rather than fabricate endpoints. Cross-shard communication edges
  and vendor-specific correlation mappings will be introduced with collectors.
- Typed attributes can hold text, integer, finite floating-point, and boolean values.
  Examples for later collectors: bytes, path, communicator, peer, and kernel name.
- Integer counters/timestamps remain exact `u64` values. Browser readers must use
  lossless parsing/BigInt or a lossless export representation, not ordinary JS numbers.

## Collector/writer lifecycle

The safe Rust staging API is `rprof_capture::Recorder`:

1. Validate configuration; create an output with exclusive/no-clobber semantics.
2. Write and sync header/shard metadata before launching the workload.
3. Declare tracks and open activities with `reliable()`. These queue submissions
   can block and must be made outside instrumentation hot paths.
4. Submit data through `emit()`. Events accumulate into size/count-bounded chunks.
   A full writer queue drops an entire data chunk and increments loss counters.
   Track declarations cannot use the lossy path.
5. Call `flush()` periodically, even for low-rate collectors. The process supervisor
   emits heartbeat checkpoints at `--flush-interval-ms` (default 100 ms, subject to
   scheduling and its polling interval). This interval is caller-driven, not a
   separate timer hidden inside `Recorder`.
6. Call `finish()` to drain pending events, write a footer, join the writer, and
   sync the file. Write failures propagate; unwinding/error cleanup drains already
   queued frames without inventing a successful footer.

The staging API owns its recorder on one producer thread. Native collectors will
feed it through per-thread, allocation-free ingress buffers; the current Rust API
does allocate and serialize. It must not be used directly from allocator wrappers
or reentrant MPI/OMPT/CUPTI callbacks. After fork, injected collectors will need
explicit child reinitialization; the current recorder is not fork-safe to reuse.

### Resource bounds

| Resource | Default | Limit / behavior |
| --- | --- | --- |
| Events per chunk | 256 | 1–4096; byte limit can flush earlier |
| Pending encoded event bytes | — | Below 1 MiB with frame-overhead headroom |
| Encoded event | — | 64 KiB; oversized data events are dropped/countable |
| Writer queue | 8 chunks | 1–256 chunks; each frame at most 1 MiB |
| Flush interval | 100 ms | 1–60000 ms |
| Reader frame | — | 1 MiB, enforced during reading, before JSON parsing |
| Shards per file/merge | — | 4096 |
| Declared tracks per file/merge | — | 65536 |
| Serialized shard metadata per file/merge | — | 16 MiB |

Memory scales with configured queue capacity and bounded identity metadata, not
capture duration. Rust event/JSON objects add overhead beyond encoded-byte limits.
Reader summaries retain shard metadata and track ID sets, not event history.

## Recovery and data loss

Each written frame is flushed to the OS; initial metadata and final completion are
synced to storage. This protects readable prefixes against profiler termination;
power-loss durability between sync points depends on the filesystem/storage.

A missing footer means an incomplete shard. An unterminated final frame is ignored
and marked truncated, even if its JSON text happens to parse. A malformed frame
that *has* a newline is corruption and fails validation. Empty/truncated headers
are not readable captures.

Loss counters distinguish queue-full batches from oversized events. They are
cumulative and checkpointed in chunks and the footer. In incomplete captures they
are a **lower bound**; queued/staged events and the most recent loss updates may
not have reached disk. A footer indicates finalized storage, not workload success:
`completed`, `interrupted`, `launch_failed`, and `collector_failed` are distinct.

Merge reads inputs once, validates and spools complete records to temporary
per-shard files, and copies those files in UUID order. It uses bounded RAM and one
spool file descriptor at a time, at the cost of temporary disk space roughly equal
to input data plus output. Final output is validated, synced, and published without
overwriting an existing path. A malformed input aborts publication. Incomplete shards
retain missing footers, and discarded tails are recorded as recovery provenance.
Input order does not affect merged bytes. Merged frames are grouped by shard,
not globally sorted by timestamp; the report layer performs domain-aware queries.

## Linux process supervision

The workload runs in a new process group. The supervisor uses Linux subreaper mode
to adopt and reap orphaned descendants in that group. Fork/exec preserves membership.
SIGINT, SIGTERM, SIGHUP, and SIGQUIT delivered to the profiler are forwarded to the
workload group. Expiry of `--grace-ms` escalates to SIGKILL. When the group leader
exits with surviving descendants, those descendants receive SIGTERM and the same
grace policy. Root exit status remains available independently of interruption.

On a foreground terminal, terminal ownership is transferred before workload exec
and restored after cleanup. Only one supervisor is supported per CLI process.
Callback/storage errors trigger workload cleanup before returning to the caller.

This phase supervises the original workload process group. Deliberate `setsid` or
`setpgid` escapes require a future cgroup supervision backend. Individual fork/exec
events and descendant spans are not inferred by polling; native instrumentation
will provide them. SIGKILL of the profiler itself cannot run cleanup, but complete
capture frames remain reportable; surviving workload groups need external cleanup.
