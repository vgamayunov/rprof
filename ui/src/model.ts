export interface Report {
  version: 1;
  provenance: { kind: 'measured' | 'synthetic'; label: string; source: string };
  summary: {
    format: string; session_id: string; complete: boolean; truncated_tail: boolean;
    recovered_inputs: string; tracks: string; spans: string; counters: string;
    dependencies: string; dropped: string; omitted_events: string; warnings: string[];
    categories: { name: string; count: string; total_ns: string }[];
  };
  domains: { id: string; name: string; origin_ns: string; end_ns: string }[];
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
