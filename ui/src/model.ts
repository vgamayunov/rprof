export interface Report {
  version: 1;
  provenance: { kind: 'measured' | 'synthetic'; label: string; source: string };
  summary: {
    format: string; session_id: string; complete: boolean; truncated_tail: boolean;
    recovered_inputs: string; tracks: string; spans: string; counters: string;
    dependencies: string; dropped: string; omitted_events: string; warnings: string[];
    categories: { name: string; count: string; total_ns: string }[];
  };
  domains: { id: string; name: string; origin_ns: string; end_ns: string;
    calibration?: { method: string; uncertainty_ns: string; max_drift_ns: string; complete: boolean } }[];
  shards: {
    id: string; domain: string; name: string; hostname: string; command: string[];
    collectors: string[]; origin_ns: string; end_ns: string; complete: boolean;
    exit_code: number | null; signal: number | null; interrupted_by: number | null;
    end_reason: string; dropped: string;
  }[];
  tracks: {
    id: string; shard: string; parent: string | null; name: string;
    kind: 'process' | 'thread' | 'gpu_device' | 'gpu_stream' | 'io';
    rank: number | null; details: Record<string, string>;
  }[];
  spans: {
    id: number; track: string; name: string; category: string;
    start_ns: string; duration_ns: string; correlation: string | null;
    incomplete: boolean; attributes: Record<string, string>;
  }[];
  counters: {
    track: string; name: string; timestamp_ns: string; value: string;
    value_type: 'integer' | 'float'; unit: string;
    attributes: Record<string, string>;
  }[];
  dependencies: { from: string; to: string; relation: string }[];
  // Absent in older v1 reports.
  io?: IoSummary[];
  mpi?: MpiCallSummary[];
  openmp?: OpenmpSummary | null;
  cuda?: CudaSummary | null;
}

export interface CudaSummary {
  runtimes: CudaRuntime[]; devices: CudaDevice[]; activities: CudaActivity[];
  apis: CudaApi[]; sources: CudaSource[];
}
export interface CudaRuntime {
  shard: string; domain: string; instance: string; pid: string;
  cupti_version: string; cuda_version: string; initialized: boolean;
  clock_method: string; clock_uncertainty: string | null;
  capabilities: CudaCapability[]; errors: string[];
}
export interface CudaCapability { name: string; supported: boolean; optional: boolean; result: string; detail: string }
export interface CudaDevice {
  shard: string; domain: string; instance: string; pid: string; device: string;
  name: string; identity: string; visible: boolean | null; memory_bytes: string | null;
  compute_major: string | null; compute_minor: string | null; multiprocessors: string | null;
  kernels: string; copies: string; memsets: string; kernel_ns: string; copy_ns: string;
  copy_bytes: string; memset_bytes: string; window_ns: string;
}
export interface CudaActivity {
  shard: string; domain: string; instance: string; device: string; context: string; stream: string;
  kind: string; name: string; count: string; total_ns: string; max_ns: string; bytes: string;
}
export interface CudaApi {
  shard: string; domain: string; instance: string; api_domain: string; name: string;
  calls: string; errors: string; total_ns: string; max_ns: string;
}
export interface CudaSource {
  shard: string; domain: string; initialized_images: string; finalized: boolean;
  attempted: string; received: string; ingress_dropped: string; cupti_dropped: string;
  source_errors: string; invalid_timing: string; unattributed: string;
  unresolved_correlations: string; ambiguous_correlations: string;
}

// Match the complete device identity, not just the ordinal (which repeats per image/shard).
export function deviceActivities(report: Report, device: CudaDevice): CudaActivity[] {
  return (report.cuda?.activities ?? []).filter(activity => activity.domain === device.domain &&
    activity.shard === device.shard && activity.instance === device.instance && activity.device === device.device);
}

export interface MpiCallSummary {
  shard: string; rank: number; name: string; calls: string; errors: string;
  total_ns: string; max_ns: string; logical_bytes: string; calls_with_bytes: string;
}

export interface OpenmpSummary {
  runtimes: OpenmpRuntime[]; regions: OpenmpRegion[]; threads: OpenmpThread[];
  tasks_created: string; tasks_completed: string; execution_segments: string;
}
export interface OpenmpRuntime {
  shard: string; domain: string; instance: string; pid: string; version: string; omp_version: string;
  initialized: boolean; finalized: boolean; callbacks: OpenmpCallback[];
  sequence_gaps: string; unmatched_endpoints: string; abandoned_intervals: string;
}
export interface OpenmpCallback { name: string; status: string; observed: string | null; observed_complete: boolean }
export interface OpenmpRegion {
  shard: string; domain: string; instance: string; pid: string; parallel_id: string;
  requested_threads: string; observed_threads: string; duration_ns: string | null;
  work_ns: string; wait_ns: string; work_min_ns: string | null; work_max_ns: string | null; work_spread_ns: string | null;
}
export interface OpenmpThread {
  shard: string; domain: string; instance: string; pid: string; tid: string; parallel_id: string;
  team_index: string | null; work_ns: string; wait_ns: string; sync_ns: string;
  scheduled_ns: string; execution_segments: string;
}

// Parallel identifiers alone are not unique across runtime images or shards.
export function regionThreads(report: Report, region: OpenmpRegion): OpenmpThread[] {
  return (report.openmp?.threads ?? []).filter(thread => thread.domain === region.domain &&
    thread.shard === region.shard && thread.instance === region.instance &&
    thread.pid === region.pid && thread.parallel_id === region.parallel_id);
}

export interface IoSummary {
  domain: string; host: string; file: string; operations: string; errors: string;
  read_bytes: string; write_bytes: string;
  requested_read_bytes: string; requested_write_bytes: string;
  total_ns: string; window_ns: string;
  by_operation: { name: string; count: string; errors: string; total_ns: string; max_ns: string }[];
}

export type Span = Report['spans'][number];
export type Counter = Report['counters'][number];
export type Track = Report['tracks'][number];
export type Shard = Report['shards'][number];
export type Domain = Report['domains'][number];

export interface IndexedSpan { span: Span; start: bigint; end: bigint; lane: number }
export interface IndexedCounter { counter: Counter; time: bigint }
export interface SpanIndex { items: IndexedSpan[]; maxEnds: bigint[] }
export interface TrackData { spans: SpanIndex; counters: Map<string, IndexedCounter[]> }

export interface TimelineRow {
  key: string; kind: 'shard' | 'rank' | 'track' | 'counter'; label: string; depth: number;
  height: number; top: number; shard: Shard; track?: Track; series?: string; hasChildren: boolean;
}

// Canvas row geometry (CSS pixels). Span bars start 7px from the top, are 11px
// high, and use a 14px lane pitch; the remaining space protects row boundaries.
export const ROW_HEIGHT = { shard: 26, rank: 24, emptyTrack: 24, counter: 48 } as const;
export const SPAN_TOP = 7;
export const SPAN_PITCH = 14;
export const COUNTER_PLOT_TOP = 8;
export const COUNTER_PLOT_HEIGHT = 18;

export function trackRowHeight(index: SpanIndex): number {
  let maxLane = 0;
  for (const item of index.items) maxLane = Math.max(maxLane, item.lane);
  return ROW_HEIGHT.emptyTrack + SPAN_PITCH * maxLane;
}

// A single layout source for drawing, hit testing, and external consumers of
// timeline geometry. Heights depend on indexed spans, never the zoom level.
export function timelineRows(report: Report, data: Map<string, TrackData>, domainId: string | undefined,
  query: string, collapsed: ReadonlySet<string> = new Set()): TimelineRow[] {
  const rows: TimelineRow[] = [];
  const byShard = new Map<string, Track[]>();
  const byParent = new Map<string, Track[]>();
  for (const track of report.tracks) {
    const group = track.parent ? byParent : byShard;
    const key = track.parent ?? track.shard;
    if (!group.has(key)) group.set(key, []);
    group.get(key)!.push(track);
  }
  const matchCache = new Map<string, boolean>();
  const matchesTree = (track: Track, visited = new Set<string>()): boolean => {
    if (matchCache.has(track.id)) return matchCache.get(track.id)!;
    if (visited.has(track.id)) return false;
    visited.add(track.id);
    const trackData = data.get(track.id);
    const match = !query || matchesFilter(track.name, track.kind, track.rank, query) ||
      !!trackData?.spans.items.some(item => matchesFilter(`${track.name} ${item.span.name}`, item.span.category, track.rank, query)) ||
      !!trackData && [...trackData.counters.keys()].some(name => matchesFilter(`${track.name} ${name}`, 'counter', track.rank, query)) ||
      (byParent.get(track.id) ?? []).some(child => matchesTree(child, visited));
    matchCache.set(track.id, match);
    return match;
  };
  let top = 0;
  const add = (row: Omit<TimelineRow, 'top'>): void => { rows.push({ ...row, top }); top += row.height; };
  const addTrack = (track: Track, shard: Shard, depth: number, ancestors = new Set<string>()): void => {
    if (ancestors.has(track.id) || !matchesTree(track)) return;
    const children = byParent.get(track.id) ?? [];
    const trackData = data.get(track.id);
    const series = [...(trackData?.counters.keys() ?? [])].filter(name => !query || matchesFilter(`${track.name} ${name}`, 'counter', track.rank, query));
    const key = `track:${track.id}`;
    add({ key, kind: 'track', label: track.name, depth, height: trackData ? trackRowHeight(trackData.spans) : ROW_HEIGHT.emptyTrack,
      shard, track, hasChildren: children.length > 0 || series.length > 0 });
    if (collapsed.has(key)) return;
    const next = new Set(ancestors); next.add(track.id);
    for (const child of children) addTrack(child, shard, depth + 1, next);
    for (const name of series) add({ key: `counter:${track.id}:${name}`, kind: 'counter', label: name,
      depth: depth + 1, height: ROW_HEIGHT.counter, shard, track, series: name, hasChildren: false });
  };
  for (const shard of report.shards) {
    if (shard.domain !== domainId) continue;
    const own = byShard.get(shard.id) ?? [];
    if (query && !matchesFilter(`${shard.name} ${shard.hostname}`, 'shard', null, query) && !own.some(track => matchesTree(track))) continue;
    const key = `shard:${shard.id}`;
    add({ key, kind: 'shard', label: `${shard.name}${shard.hostname && shard.hostname !== shard.name ? ` · ${shard.hostname}` : ''}`,
      depth: 0, height: ROW_HEIGHT.shard, shard, hasChildren: own.length > 0 });
    if (collapsed.has(key)) continue;
    const ranked = new Map<number | null, Track[]>();
    for (const track of own) {
      if (!ranked.has(track.rank)) ranked.set(track.rank, []);
      ranked.get(track.rank)!.push(track);
    }
    for (const [rank, group] of ranked) {
      if (rank === null) { for (const track of group) addTrack(track, shard, 1); continue; }
      if (!group.some(track => matchesTree(track))) continue;
      const rankKey = `rank:${shard.id}:${rank}`;
      add({ key: rankKey, kind: 'rank', label: `Rank ${rank}`, depth: 1, height: ROW_HEIGHT.rank, shard, hasChildren: true });
      if (!collapsed.has(rankKey)) for (const track of group) addTrack(track, shard, 2);
    }
  }
  return rows;
}

export function lowerBound<T>(items: readonly T[], predicate: (item: T) => boolean): number {
  let low = 0;
  let high = items.length;
  while (low < high) {
    const mid = (low + high) >>> 1;
    if (predicate(items[mid]!)) high = mid;
    else low = mid + 1;
  }
  return low;
}

// Bounded lane allocation: even a heavily overlapping capture costs O(n log n),
// and an overflow lane retains events instead of making track height unbounded.
export function indexSpans(spans: Span[], maxLanes = 4): SpanIndex {
  const items = spans.map(span => ({ span, start: BigInt(span.start_ns),
    end: BigInt(span.start_ns) + BigInt(span.duration_ns), lane: 0 }));
  items.sort((a, b) => a.start < b.start ? -1 : a.start > b.start ? 1 : a.span.id - b.span.id);
  const laneEnds = Array<bigint>(maxLanes).fill(-1n);
  const maxEnds: bigint[] = [];
  let max = -1n;
  for (const item of items) {
    const free = laneEnds.findIndex(end => end <= item.start);
    item.lane = free < 0 ? maxLanes - 1 : free;
    laneEnds[item.lane] = item.end > laneEnds[item.lane]! ? item.end : laneEnds[item.lane]!;
    if (item.end > max) max = item.end;
    maxEnds.push(max);
  }
  return { items, maxEnds };
}

export function visibleSpans(index: SpanIndex, start: bigint, end: bigint, limit = Infinity): IndexedSpan[] {
  const first = lowerBound(index.maxEnds, value => value >= start);
  const last = lowerBound(index.items, value => value.start > end);
  const stride = Math.max(1, Math.ceil((last - first) / limit));
  const result: IndexedSpan[] = [];
  for (let i = first; i < last; i += stride) {
    const item = index.items[i]!;
    if (item.end >= start) result.push(item);
  }
  return result;
}

export function indexReport(report: Report): Map<string, TrackData> {
  const spans = new Map<string, Span[]>();
  const counters = new Map<string, Map<string, IndexedCounter[]>>();
  for (const span of report.spans) {
    if (!spans.has(span.track)) spans.set(span.track, []);
    spans.get(span.track)!.push(span);
  }
  for (const counter of report.counters) {
    if (isCollectorHealth(counter.name)) continue;
    if (!counters.has(counter.track)) counters.set(counter.track, new Map());
    const groups = counters.get(counter.track)!;
    const series = `${counter.name} [${counter.unit}; ${counter.value_type}]`;
    if (!groups.has(series)) groups.set(series, []);
    groups.get(series)!.push({ counter, time: BigInt(counter.timestamp_ns) });
  }
  const result = new Map<string, TrackData>();
  for (const track of report.tracks) {
    const groups = counters.get(track.id) ?? new Map<string, IndexedCounter[]>();
    for (const samples of groups.values()) samples.sort((a, b) => a.time < b.time ? -1 : a.time > b.time ? 1 : 0);
    result.set(track.id, { spans: indexSpans(spans.get(track.id) ?? []), counters: groups });
  }
  return result;
}

export function isCollectorHealth(name: string): boolean {
  return /^(io|mpi|openmp)\.(attempted_events|received_events|ingress_dropped_events|initialized_images)$/.test(name) ||
    name === 'mpi.clock.uncertainty_ns' || name.startsWith('openmp.meta.') || name.startsWith('cuda.meta.') ||
    /^cuda\.(attempted_events|received_events|ingress_dropped_events|cupti_dropped_records|source_errors|finalized|initialized_images|invalid_timing_records|unattributed_records|unresolved_correlations|ambiguous_correlations)$/.test(name);
}

export function matchesFilter(name: string, category: string, rank: number | null, query: string): boolean {
  const terms = query.toLocaleLowerCase().trim().split(/\s+/).filter(Boolean);
  const haystack = `${name} ${category} ${rank === null ? '' : `rank ${rank} rank:${rank}`}`.toLocaleLowerCase();
  return terms.every(term => haystack.includes(term));
}

export function integerPlotValues(values: readonly string[]): number[] {
  if (!values.length) return [];
  const base = BigInt(values[0]!);
  return values.map(value => Number(BigInt(value) - base));
}

// Coordinates only ever convert a difference, never an absolute nanosecond timestamp.
export function timeToX(time: bigint, viewStart: bigint, nsPerPx: number): number {
  return Number(time - viewStart) / nsPerPx;
}

export function xToTime(x: number, viewStart: bigint, nsPerPx: number): bigint {
  return viewStart + BigInt(Math.round(x * nsPerPx));
}

export function formatNs(ns: bigint): string {
  const abs = ns < 0n ? -ns : ns;
  if (abs < 1000n) return `${ns} ns`;
  const units: [bigint, string][] = [[1000000000n, 's'], [1000000n, 'ms'], [1000n, 'µs']];
  for (const [scale, label] of units) {
    if (abs >= scale) {
      const whole = ns / scale;
      const fraction = (abs % scale) * 1000n / scale;
      return `${whole}.${fraction.toString().padStart(3, '0')} ${label}`;
    }
  }
  return `${ns} ns`;
}

// Bytes divided by the observed window (not a device throughput measurement).
// Integer arithmetic keeps even values above 2^53 exact.
export function observedRate(bytes: string, windowNs: string): string {
  const window = BigInt(windowNs);
  if (window === 0n) return '—';
  const milliBytesPerSecond = BigInt(bytes) * 1_000_000_000_000n / window;
  return `${milliBytesPerSecond / 1000n}.${(milliBytesPerSecond % 1000n).toString().padStart(3, '0')} B/s`;
}

export function averageNs(totalNs: string, count: string): string {
  const operations = BigInt(count);
  if (operations === 0n) return '—';
  const milliNs = BigInt(totalNs) * 1000n / operations;
  return `${milliNs / 1000n}.${(milliNs % 1000n).toString().padStart(3, '0')} ns`;
}
