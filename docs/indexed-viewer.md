# Indexed, windowed viewing

`rprof view` now loads timeline detail on demand from a persistent index. Existing
captures work; no recollection or binary-format conversion is required.

```sh
cargo build --release -p rprof-cli
target/release/rprof view system-io.jsonl --open
```

The first launch builds a SQLite sidecar. Subsequent launches reuse it when the
source file identity, size and timestamps match. The original capture is never
modified. Building the CLI now needs a C compiler for bundled SQLite; no separate
SQLite installation or C++ compiler is needed.

## What changes in the UI

- The initial HTML contains bounded summaries and track/scope metadata, rather than
  a retained prefix of raw timeline events.
- Zooming and panning request the selected time window. A late training phase can
  load its actual spans even if it is far beyond the old HTML retention cutoff.
- Only visible track rows and metric scopes are requested. Wheel scrolling remains
  vertical, and zoom/reset buttons and horizontal dragging control time.
- CPU/memory queries choose a resolution appropriate for the window. Coarse buckets
  retain original **first, last, minimum and maximum** observations, with source-gap
  information. Zooming retrieves finer samples, never invented intermediate values.
- I/O queries use multi-resolution totals for bytes, calls, errors and latency.
  Rates and means are derived after combining counts/totals; they are not averages
  of per-thread averages. The finest I/O metric bucket is 1 ms; raw call intervals
  retain nanosecond precision.
- Expand rank → process → thread → Files to fetch a file page. Previous/Next buttons
  browse 100 file lanes at a time; the browser retains at most eight loaded thread
  pages. File calls are then fetched for the visible time window.
- A status line shows loading, loaded-window coverage, errors and response limits.
  Stale responses cannot overwrite a newer viewport. A failed request leaves the
  previous data visibly marked as potentially stale.

## Cache and remote usage

The default cache is `$XDG_CACHE_HOME/rprof/indexes`, or
`$HOME/.cache/rprof/indexes`. Choose local storage when the capture lives on a
high-latency shared filesystem:

```sh
rprof view /shared/captures/training.jsonl --index-dir /local/scratch/rprof-indexes
rprof view /shared/captures/training.jsonl --reindex
```

The index is disposable and can be removed when no viewer is using it. It contains
indexed event payloads and metric summaries, so it can be comparable to or larger
than the source capture. This is a query index, not the future compressed binary
capture format. Initial construction scans the capture; reuse avoids that scan.

Indexes are built into temporary files and published only after validation. A
capture changing during construction is rejected; retry when recording has stopped.
Saved incomplete captures remain usable, with partial-source diagnostics and
unfinished spans labeled as such. A running viewer serves a fixed snapshot;
restart it to see a changed capture.

The existing SSH workflow carries both the HTML and data requests:

```sh
# On a host that can access the capture:
rprof view /shared/captures/training.jsonl --port 8080

# On your workstation:
ssh -N -L 8080:127.0.0.1:8080 your-viewer-host
```

Open `http://127.0.0.1:8080`. The server stays on loopback and only queries the
selected capture. It does not launch or control remote jobs.

## Bounds and interpretation

Each query is limited to 64 raw tracks, 1,024 counter tracks and 32 I/O scopes.
Raw replies contain at most 20,000 observations, and replies are capped at 16 MiB;
smaller `--max-events` / `--max-report-mib` values also apply. Dense windows show a
limit notice: zoom in or expand fewer tracks. The browser replaces window data and
keeps only two recent responses, rather than accumulating the whole capture.

The index stores sparse metric levels with eightfold coarsening. Raw interval
queries use a spatial candidate index followed by exact integer timestamp checks,
including spans crossing the window boundaries. Timestamps, byte totals and IDs
are not rounded through browser floating-point numbers. Available dependency edges
are loaded for visible correlations, capped at 5,000 per reply.

This solves prefix retention and fixed-resolution graphs. Existing capture-format
limits (including total tracks/shards), bounded summary/identity catalogs and actual
collection losses still apply. The viewer can page all indexed file lanes, but a
new disk-backed *per-file aggregate table* is separate work. The indexed bootstrap
does not compute or embed the standalone per-file table.

## Portable HTML

`rprof report --format html` remains self-contained and offline. Its raw-event and
summary budgets still apply; it does not fetch an index. Use it for sharing a compact
snapshot, and use `rprof view` for deep exploration of a large capture. Saving the
indexed viewer's page from a browser is not a replacement for an offline export.

## Validation

Regression coverage includes late-window access beyond standalone retention,
crossing/unfinished spans, exact timestamps above `2^53`, CPU peak/gap preservation,
finer zoom buckets, paged file metadata, stable source IDs, cache reuse/invalidation,
bounded replies, HTTP validation, stale-response rejection, and offline behavior.
A native I/O/system capture was also opened in Chromium against the real server,
zoomed, and drilled down to an on-demand file page.
