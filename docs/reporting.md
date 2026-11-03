# Reports and local viewing

## Commands

```sh
rprof report capture.rprof.jsonl
rprof report capture.rprof.jsonl --format text -o summary.txt
rprof report capture.rprof.jsonl --format html -o timeline.html
rprof report capture.rprof.jsonl --format trace -o trace.json
rprof view capture.rprof.jsonl --port 8080 --open
```

`text` is the default. `trace` produces Chrome Trace Event JSON accepted by Perfetto;
`chrome` and `perfetto` are aliases for that format, not alternate binary encodings.
Output goes to stdout unless `-o` is specified. File artifacts are published
atomically with no-clobber semantics; failed renders do not leave a final output.

All formats share validated capture analysis and read a fixed-length file snapshot.
Version-1 bootstrap JSON is supported up to 16 MiB, using an explicitly unknown
legacy host/session identity. Saved partial v2 captures preserve missing-footer,
truncation, and checkpointed loss status in reports.

## Offline timeline

`timeline.html` embeds its CSS, JavaScript, and safely escaped JSON. Open it directly
with a modern BigInt-capable browser. It needs no server, CDN, external font, or
network request. Cargo embeds the checked-in bundles, so native builds do not need
Node. TypeScript source/build scripts and browser tests live under `ui/`.

Controls:

- **Theme:** the top-right Light mode/Dark mode button updates panels, tables,
  timeline, and counter plots. The initial theme follows the system preference;
  an explicit choice is saved per browser origin when storage is available.
- **Clock domain:** select a host/boot clock domain; other domains are not overlaid.
- **Filter:** search names, categories, and rank labels; for example `cuda rank:0`.
- **Zoom:** wheel over the plot, use `+`/`−`, or click the zoom buttons.
- **Pan:** drag horizontally; when the plot is focused, use left/right arrows.
- **Fit:** click *Fit all* or press Home while the plot is focused.
- **Scroll:** wheel over labels or Shift+wheel over the plot.
- **Hierarchy:** click expandable shard/rank/track labels to collapse or expand.
- **Inspect:** click a span or counter sample for exact values, attributes, and
  correlations. Selecting a correlated span highlights related operations and
  draws visible dependency links.

CPU/process/rank, OpenMP thread, GPU device/stream, and I/O tracks are supported by
the model. The current process collector supplies the real command lifetime;
`fixtures/multitrack.jsonl` demonstrates the richer track types synthetically.

With `--collect io`, real POSIX operations appear on thread/file lanes. The I/O
dashboard defaults to filesystem throughput, IOPS and latency graphs over time,
with host/mount/type/device selection. Full-capture rate bins remain available beyond
raw timeline retention. Per-file completed/requested bytes, errors, observed-window
bandwidth and operation latency remain in a collapsed, paginated detail section.
Counts reflect captured events; see [I/O profiling](io-profiling.md) for loss and
coverage semantics. MPI calls use a distinct color and full-name hover/inspection;
actual MPI events are supplied by `--collect mpi`; see [MPI profiling](mpi-profiling.md).

`--collect openmp` adds real OMPT parallel/work/task/synchronization events. The
OpenMP panel is domain-filtered and shows runtime capabilities, received callbacks,
recorded work/wait spread, and selectable thread details. Native collectors share
physical thread tracks; scheduled task intervals remain distinct from task lifetime
and are not measurements of CPU utilization. See [OpenMP profiling](openmp-profiling.md).

Incomplete activities have a distinct outline and stop at the last observed event.
Their missing final duration is not presented as a completed measurement. Category
totals sum inclusive spans, so nesting and overlap can double-count elapsed time.
The header/status section identifies synthetic versus measured data, partial input,
collector loss, omitted report events, and unresolved dependency endpoints.

## Resource limits

Full-capture text summaries stream without retaining event history. HTML keeps the
first observations fitting both **200,000 events** and a **64 MiB encoded-event
budget** by default. Change these with `--max-events` and `--max-report-mib` on
`report` or `view` (maximum 2 million events / 512 MiB). Starts count as observations;
a completed span can close a retained start even if that completion is omitted.
Track declarations are retained separately, subject to a 16 MiB encoded metadata
limit. The underlying reader's shard/track/frame limits still apply.

Summary counts and category totals include all validated events in the snapshot.
Timeline omissions are distinct from collector loss and explicitly displayed.
These budgets are encoded-size limits, not a promise of identical RSS: Rust objects,
JSON escaping, and browser indices add memory overhead. Very large standalone HTML
artifacts should use conservative limits; indexed server-side queries are a future
extension.

The Canvas renderer culls by visible rows and time and samples extremely dense rows
at up to 5,000 span candidates or 3,000 counter samples per draw. Up to four overlap
lanes share a track, with excess overlaps sharing the last lane. Filtering and
zooming help inspect dense regions. Display sampling does not change capture data
or full-capture summaries.

Rows are compact by default: 26px shard headers, 24px rank/empty/single-lane tracks,
38/52/66px tracks for two/three/four overlap lanes, and 48px counter rows. Collector
health snapshots are displayed in capture status rather than one-point chart rows.

## Time and numeric precision

Shards sharing hostname/boot identity are positioned using their monotonic origin
offsets. Cross-host/boot domains remain unaligned unless explicit MPI calibration
is present; calibrated domains are labeled as estimates with uncertainty and drift.
HTML timestamps and large
integer values are decimal strings, parsed with BigInt; pixel coordinates use
view-relative differences. Integer counter plots subtract an exact integer
baseline before numeric plotting, retaining adjacent values above `2^53`.

Trace JSON uses numeric microseconds with fractional nanoseconds, serialized
without passing timestamps through `f64`. Exact span timestamps/durations are also
stored as strings in args; counters retain exact value/unit/timestamp strings in
their `rprof` extension. Reference viewers may round numeric plot values, especially
for long traces or very large integer counters. Use rprof's inspector for exact data.

## Reference-viewer export

For a multi-domain capture, select one domain explicitly:

```sh
rprof report fixtures/multitrack.jsonl
rprof report fixtures/multitrack.jsonl --format trace --domain 0 -o multitrack.trace.json
```

The text report lists domain IDs. Export assigns collision-free numeric process and
track IDs, emits track metadata, complete slices, counters, incomplete activities,
and correlation/dependency flows. It streams all selected-domain spans and counters
regardless of HTML retention limits. Flow reconstruction retains at most 200,000
endpoints and 200,000 dependency descriptions; omissions/unresolved links are
recorded in export metadata. More than 200,000 simultaneous open activities fails
export explicitly. Input and report clock identities remain in metadata.

The synthetic domain-0 export was loaded in Perfetto UI v59.1 and independently
queried using its reference trace processor: **16 slices, 12 counter samples,
6 flows, and zero import errors**. The two outstanding logical dependencies in that
domain deliberately reference absent endpoints and are counted as omitted flows.

## Local viewer

`rprof view` renders a snapshot to a temporary file and serves the same HTML over
`127.0.0.1`. Port 0 (the default) selects a free port and prints the URL. `--open`
launches `xdg-open`; without it, open the printed URL manually. Ctrl-C or SIGTERM
stops the viewer and removes its temporary artifact. Only the report routes are
served; this is not a filesystem browser or a remote command API.

This viewer is read-only and snapshot-based: refreshing the browser does not
re-read the capture. Re-run `view` to obtain a newer snapshot. Live updates and
indexed viewport queries are future capabilities. For remote viewing, use the
existing server through a manual SSH tunnel, as described in
[remote operation](remote-operation.md). Local-GUI access to existing remote files
is a later read-only feature.
