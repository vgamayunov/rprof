# CUDA report extension (phase 6)

`Report.cuda` is optional, with the exact Rust types in
`crates/rprof-report/src/model.rs`: runtimes, devices, activities, APIs, and per-shard
source diagnostics. IDs/counts/bytes/nanoseconds are decimal strings; unknown
device metadata and clock uncertainty are null. Every row has a clock `domain`.

- Runtime: SDK/CUPTI versions, actual initialization, normalized clock method,
  capabilities and source errors. CUPTI normalizes GPU timestamps using the
  registered CLOCK_MONOTONIC callback; its interpolation uncertainty is unknown.
- Device: identity/visibility/compute capability/capacity, counts and inclusive
  kernel/copy durations, copy/memset bytes, observed GPU activity window.
- Activity: per-device/context/stream/kind/name totals. Durations overlap across
  streams; their sum is not GPU utilization or elapsed application time.
- API: runtime and driver host-call durations/errors. Runtime/driver calls can nest;
  host call duration is not kernel execution time.
- Sources: native attempted/received/ingress drops, CUPTI drops, source errors,
  successful final flush, unavailable timing, and missing/ambiguous attribution.
  `finalized` requires a successful flush for every initialized process image;
  absent final diagnostics produce a warning and counts are lower bounds.

Only streams with actual timed GPU work get timeline tracks. Device metadata may
describe invisible or unused devices; the panel should make that distinction clear.
`cuda.meta.*` and diagnostic counters are metadata, not one-point performance plots.
Use the same compact layout, theme handling, safe text rendering, pagination, and
exact-number behavior as the MPI/OpenMP/I/O reports.
