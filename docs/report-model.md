# Browser report model — version 1

Phase-2 implementation contract. Rust generates this derived JSON model for the
offline HTML report and local viewer. It is separate from the capture schema.
All nanoseconds, local IDs, counter integer values, and potentially large totals
are **decimal strings**, so ordinary JSON parsing does not round them. Array
indices and bounded event counts used for rendering are JavaScript numbers.

```text
Report {
  version: 1,
  provenance: { kind: "measured" | "synthetic", label: string, source: string },
  summary: {
    format: string, session_id: string, complete: boolean,
    truncated_tail: boolean, recovered_inputs: string,
    tracks: string, spans: string, counters: string, dependencies: string,
    dropped: string, omitted_events: string, warnings: string[],
    categories: [{ name: string, count: string, total_ns: string }]
  },
  domains: [{ id: string, name: string, origin_ns: string, end_ns: string }],
  shards: [{
    id: string, domain: string, name: string, hostname: string,
    command: string[], collectors: string[], origin_ns: string,
    end_ns: string, complete: boolean, exit_code: number|null,
    signal: number|null, interrupted_by: number|null, end_reason: string,
    dropped: string
  }],
  tracks: [{
    id: string, shard: string, parent: string|null, name: string,
    kind: "process"|"thread"|"gpu_device"|"gpu_stream"|"io",
    rank: number|null, details: { [key: string]: string }
  }],
  spans: [{
    id: number, track: string, name: string, category: string,
    start_ns: string, duration_ns: string, correlation: string|null,
    incomplete: boolean, attributes: { [key: string]: string }
  }],
  counters: [{
    track: string, name: string, timestamp_ns: string, value: string,
    value_type: "integer"|"float", unit: string,
    attributes: { [key: string]: string }
  }],
  dependencies: [{ from: string, to: string, relation: string }]
}
```

IDs for tracks and correlations are globally unique strings containing their shard
namespace. Span `id` is a report-local array identifier. The UI joins tracks to
shards to domains. Track hierarchy is host/clock domain → shard/rank → track and
its children. `origin_ns` on each shard is an offset from its domain's minimum
monotonic origin. All span/counter timestamps are offsets from that domain origin;
`end_ns` gives the observed domain extent. Domain `origin_ns` preserves the original
monotonic value for provenance. Different domains must be selected separately and
never displayed as a synchronized global timeline.

Open activities become explicitly incomplete spans ending at the shard's last
observed time, unless a matching completed span was observed. Attribute values
are display strings preserving exact integers. Counter charts may approximate
coordinates using Number after subtracting an exact BigInt baseline for integer
series; details must always retain original values. Loss, event-retention limits,
missing correlation endpoints, and partial captures remain visible in the report.

Summary category durations sum inclusive spans and can double-count nested or
overlapping work; they are not total elapsed time. Summary counts describe the
entire validated capture. Retained event arrays are a bounded subset and omitted
event counts are separate from collector loss. Measured and synthetic provenance
are supported now; projected scenario artifacts will extend this contract later.

The HTML shell contains an inert `<script type="application/json" id="rprof-data">`
with safely escaped JSON, followed by a self-contained script bundle. The UI
initializes from that element. It makes no network requests and uses no CDN.
