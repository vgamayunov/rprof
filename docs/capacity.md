# Current capacity and rough operating envelope

As of 2026-11-02: **128 threads per rank have passed a short two-node test; thousands
of ranks and one-hour captures have not been validated.** There is no one-hour
duration limit. Feasibility depends on observation rate, identity cardinality,
storage bandwidth, profiler overhead and the part of the trace you need to view.

| Question | Current indication |
| --- | --- |
| Hundreds of threads? | 128/rank passed with actual GCC/libgomp, including 2 ranks on 2 HBv4 nodes. Busy fine-grained workloads may overwhelm ingress; 256+ threads remain unvalidated. |
| Thousands of ranks? | Rank-local collection avoids a central writer, but one merged capture is limited to 4,096 shards and **65,536 total tracks**. 1,000 ranks × 100 threads already exceeds the latter. No thousand-rank validation yet. |
| One-hour runs? | No elapsed-time cap. A low-rate run is plausible; event volume, repeated regions, live tasks and tracks can become the limits. No one-hour validation yet. |
| Browse an entire large run? | The viewer is a bounded snapshot of the first observations, not an indexed full-capture browser. Summaries scan the capture but have their own row limits. |

## Storage estimate

Use **1 kB per stored observation** as a rough planning unit, then measure a short
representative capture. Attribute-heavy events can be larger. An observation can
be a start, completed span, dependency, counter or metadata item; one runtime call
is not necessarily one stored observation. Raw callback rates and stored event
rates differ. The 128-thread GNU fixture used roughly 6.9 MB for 20 regions.

| Stored observations/s/rank | Per rank-hour | 1,000 rank-hours |
| ---: | ---: | ---: |
| 100 | ~360 MB | ~360 GB |
| 1,000 | ~3.6 GB | ~3.6 TB |
| 10,000 | ~36 GB | ~36 TB |

At 10,000/s/rank, this assumes ~10 MB/s/rank or ~10 GB/s aggregate at 1,000 ranks.
These are decimal-byte arithmetic estimates, **not measured sustainable rates**.
Merge uses per-shard spool files plus output: allow approximately **two additional
input sizes of free space**, alongside retained inputs, and budget validation I/O.
It does not keep one file descriptor open per rank.

## Hard limits and resource costs

| Scope | Bound / behavior |
| --- | --- |
| Reader/merge, globally | 4,096 shards, 65,536 tracks, 16 MiB shard metadata; excess fails validation |
| Writer | 1 MiB frame, 64 KiB data event; oversized data is dropped/counts as loss, structural errors fail |
| Writer defaults | 256 events/chunk, 8 queued chunks, 100 ms flush interval; saturated batches are dropped/counts as loss |
| Native ingress | Nonblocking transport; default 16,384 raw records **per enabled collector per rank**; socket buffers may be clamped by the kernel |
| Native identities, per shard | 65,535 additional track IDs, 4,096 process images, 16,384 correlation namespaces, 48-bit local correlation IDs |
| OpenMP decoder | 65,536 active-scope/task budgets; 128 nested scopes per key; structural excess is an error |
| MPI runtime tracking, per process | 8,192 live requests, 2,048 communicators, 4,096 wait/test scratch entries; saturation loses detail, not world-size support |
| Python instrumentation | 1,024 live instrumented threads, stack depth 128, 65,536 code symbols; omission diagnostics apply |

Raw queue payload capacity at defaults is I/O 6.125 MiB, MPI 7.375 MiB, OpenMP
5.875 MiB, CUDA 9.5 MiB: **28.875 MiB/rank for all four**, excluding channel,
kernel/socket, decoder, writer and metadata memory. CUDA also has an 8 MiB host
activity pool in the application. This is queue sizing, not measured RSS.
Each collector adds receiver/ingress threads. Profiler threads inherit rank CPU
affinity; allow CPU headroom or they compete with application threads.

The optional `system` collector samples on supervisor heartbeats, without an extra
receiver thread. It elects one concurrent sampler per host/session (shared temp
directory required). Each interval emits roughly `logical CPUs + NUMA nodes + 3`
observations per host, with a one-second default. The overview/host/CPU/NUMA tree is
collapsed initially and canvas rendering is viewport-bounded; expanding fewer rows
does not reduce capture volume. See [system profiling](system-profiling.md).

Track count includes process/thread lifetimes and synthetic resource/task/region
lanes, not only concurrently running threads. Repeated thread creation or unique
resources can exhaust track identity on otherwise modest jobs. Structural or
collector errors may terminate supervised workload cleanup; this differs from
counted nonblocking drops and from report-only omissions.

Sources: `crates/rprof-capture/src/{lib,writer,reader,merge}.rs`,
`crates/rprof-cli/src/{capture_sink,native_collector,openmp_collector}.rs`, and the
native adapters/protocol crates.

## Summary and viewing limits

- OpenMP: 4,096 runtimes, 16,384 region instances, 65,536 region/thread rows,
  **globally**, with explicit summary omission warnings. For example, 256 threads
  × 256 regions fills the thread-row budget; many ranks reach it sooner.
- GNU aggregate tables independently retain 8,192 API groups and 16,384
  per-thread/callsite body groups. Existing aggregate keys keep accumulating after
  per-region retention fills.
- MPI: 8,192 `(shard, rank, call name)` groups globally; 1,000 ranks with more than
  about eight call names can omit detailed rows while the capture remains intact.
- I/O: 8,192 per-file summary keys.
- I/O graphs: 1,024 filesystem groups plus 4,096 thread/filesystem series share
  4 MiB identity metadata and 65,536 bins globally (at most 512 per series). A second
  fixed-snapshot pass derives rates independently of timeline retention; more groups
  reduce time resolution. Rank/process/thread aggregation is cached in the viewer;
  file lanes are opt-in within each thread and only visible rows are drawn.
- HTML/`view`: first **200,000 observations or 64 MiB** by default, configurable up
  to 2 million/512 MiB. Track metadata has a separate 16 MiB bound. This is not a
  random sample and later phases may be absent from the timeline. Text summaries
  stream the capture; HTML summary computation also scans beyond timeline retention.
- Trace export streams selected-domain spans independently of HTML retention;
  flow reconstruction and simultaneously open activities have 200,000-item bounds.

Summary omissions do not mean the underlying capture events were dropped. Likewise,
zero writer loss does not imply full runtime feature coverage. Check both source
health and report retention. See `crates/rprof-report/src/` for the precise budgets.

## Starting real applications

1. Establish an unprofiled checksum/result and elapsed-time baseline on the same
   input and placement. Start with a representative short phase on 2–16 ranks.
2. Select only useful collectors. Start GNU OpenMP at `--gomp-detail regions`;
   compare `bodies`/`full` in short runs before widening. Use Python ranges-only
   initially: tiny-call tracing previously showed ~1,200–1,350× body-time overhead
   on a deliberately fine-grained probe, not representative coarse application work.
3. Measure slowdown, bytes/s/rank, bytes/observation, tracks/rank, profiler RSS,
   native/source and writer drops, unmatched intervals and summary omissions.
4. Extrapolate disk and filesystem bandwidth, then grow duration/ranks in stages
   (for example 64–256 ranks before ~1,000). Keep per-rank files and merge subsets
   when total tracks exceed the global limit. Calibrate MPI clocks only when all
   participating ranks use the option.
5. Confirm report time/RSS and retained timeline coverage before committing to an
   hour-long capture. The existing synthetic writer benchmark is not a native
   callback or distributed-filesystem capacity guarantee.

Likely next scaling work is selectable capture windows/event filtering, indexed
timeline access, and revised global track/summary cardinality limits. Simply raising
queue sizes does not solve sustained overload or browser retention.
