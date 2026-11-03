export interface Report {
  version: 1;
  provenance: { kind: 'measured' | 'synthetic' | 'imported' | 'mixed'; label: string; source: string };
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
    kind: 'process' | 'thread' | 'gpu_device' | 'gpu_stream' | 'io' | 'system';
    rank: number | null; details: Record<string, string>;
  }[];
  spans: {
    id: number; source_id?: string; track: string; name: string; category: string;
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
  io_timeline?: { filesystems: IoFilesystem[]; omitted_events: string };
  mpi?: MpiCallSummary[];
  openmp?: OpenmpSummary | null;
  cuda?: CudaSummary | null;
  pmu?: PmuSummary | null;
  python?: PythonSummary | null;
  pytorch?: PytorchSummary | null;
}

export interface PythonSummary { runtimes: PythonRuntime[]; functions: PythonFunction[]; receiver_dropped: string; malformed: string }
export interface PythonRuntime {
  shard: string; domain: string; source: string; pid: string; instance: string; interpreter: string;
  backend: string; version: string; clock: string; detail: string; initialized: boolean; finalized: boolean;
  attempted: string; received: string; source_dropped: string; omitted_callbacks: string;
  sequence_gaps: string; unmatched: string; abandoned: string; inflight: string; source_errors: string; errors: string[];
}
export interface PythonFunction {
  shard: string; domain: string; source: string; kind: string; symbol: string; name: string;
  file: string; line: string; execution_kind: string; segments: string; total_ns: string;
  self_ns: string | null; max_ns: string; yields: string; unwinds: string;
}
export interface PytorchSummary { sources: PytorchSource[]; activities: PytorchActivity[]; memory: PytorchMemory[] }
export interface PytorchSource { shard: string; domain: string; metadata: Record<string, string>; diagnostics: Record<string, string> }
export interface PytorchActivity {
  shard: string; domain: string; role: string; name: string; device: string | null;
  count: string; total_ns: string; self_ns: string | null; max_ns: string; bytes: string | null;
}
export interface PytorchMemory {
  shard: string; domain: string; track: string; name: string; device: string | null;
  samples: string; peak_bytes: string; last_bytes: string;
}

export function domainClockLabel(report: Report, domainId: string | undefined): string {
  const domain = report.domains.find(candidate => candidate.id === domainId);
  if (domain?.calibration) return 'ESTIMATED MPI CLOCK ALIGNMENT';
  if (report.pytorch?.sources.some(source => source.domain === domainId))
    return report.domains.length > 1 ? 'IMPORTED PYTORCH µS→NS RELATIVE CLOCK · UNALIGNED WITH NATIVE' : 'IMPORTED PYTORCH µS→NS RELATIVE CLOCK · NOT NATIVE MONOTONIC';
  return report.domains.length > 1 ? 'CLOCK DOMAINS ARE UNALIGNED' : 'SAME-DOMAIN MONOTONIC TIME';
}

export interface PmuSummary { hosts: PmuHost[]; events: PmuEvent[]; metrics: PmuMetrics[] }
export interface PmuHost { shard: string; domain: string; metadata: Record<string, string> }
export interface PmuEvent {
  shard: string; domain: string; name: string; pid: string; scope: string; privilege: string;
  event_type: string; config: string; unit: string; status: string; error: string | null;
  raw: string | null; time_enabled_ns: string | null; time_running_ns: string | null;
  scaled_estimate: string | null; running_percent: string | null; multiplexed: boolean;
  samples: string; final_read: boolean;
}
export interface PmuMetrics {
  shard: string; domain: string; ipc_estimate: string | null; cache_miss_percent_estimate: string | null;
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
  gomp?: GompSummary | null;
}
export interface GompSummary {
  apis: GompApi[]; bodies: GompBody[];
  task_creations: string; task_body_spans: string; task_lifetimes_to_body_return: string;
}
export interface GompApi {
  shard: string; domain: string; instance: string; name: string;
  calls: string; total_ns: string; max_ns: string;
}
export interface GompBody {
  shard: string; domain: string; instance: string; tid: string; kind: string; codeptr: string;
  count: string; total_ns: string; max_ns: string;
}
export interface OpenmpRuntime {
  shard: string; domain: string; instance: string; pid: string; version: string; omp_version: string;
  backend?: string; // Older reports omit this (OMPT).
  initialized: boolean; finalized: boolean; callbacks: OpenmpCallback[];
  sequence_gaps: string; unmatched_endpoints: string; abandoned_intervals: string;
}
export interface OpenmpCallback { name: string; status: string; observed: string | null; observed_complete: boolean; detail?: string }
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

export interface IoTimeBin {
  start_ns: string; duration_ns: string; read_bytes: string; write_bytes: string;
  read_ops: string; write_ops: string; other_ops: string; errors: string;
  read_latency_ns: string; write_latency_ns: string; other_latency_ns: string; max_latency_ns: string;
}
export interface IoFilesystem {
  id: string; domain: string; host: string; filesystem_type: string; mount_point: string;
  device: string; kind: 'filesystem' | 'non_filesystem' | 'unknown'; bin_width_ns: string; bins: IoTimeBin[];
}

const IO_BIN_FIELDS = ['read_bytes', 'write_bytes', 'read_ops', 'write_ops', 'other_ops', 'errors',
  'read_latency_ns', 'write_latency_ns', 'other_latency_ns'] as const;

// Sum counts before deriving rates/means: averaging filesystem averages weights a quiet
// filesystem the same as a busy one. Never combine unaligned grids or clock domains.
export function aggregateIoBins(filesystems: readonly IoFilesystem[]): IoTimeBin[] | null {
  if (!filesystems.length) return [];
  const first = filesystems[0]!;
  if (filesystems.some(fs => fs.domain !== first.domain || fs.bin_width_ns !== first.bin_width_ns ||
    fs.bins.length !== first.bins.length || fs.bins.some((bin, i) =>
      bin.start_ns !== first.bins[i]!.start_ns || bin.duration_ns !== first.bins[i]!.duration_ns))) return null;
  return first.bins.map((bin, i) => {
    const sum = Object.fromEntries(IO_BIN_FIELDS.map(field => [field, filesystems.reduce((total, fs) =>
      total + BigInt(fs.bins[i]![field]), 0n).toString()])) as unknown as Pick<IoTimeBin, typeof IO_BIN_FIELDS[number]>;
    return { ...bin, ...sum, max_latency_ns: filesystems.reduce((max, fs) => {
      const value = BigInt(fs.bins[i]!.max_latency_ns); return value > max ? value : max;
    }, 0n).toString() };
  });
}

export function ioRate(value: string, durationNs: string): string {
  const duration = BigInt(durationNs);
  if (duration <= 0n) return '—';
  const milli = BigInt(value) * 1_000_000_000_000n / duration;
  return `${milli / 1000n}.${(milli % 1000n).toString().padStart(3, '0')}`;
}

export function ioBinMetrics(bin: IoTimeBin): { readBytesPerSec: string; writeBytesPerSec: string;
  readOpsPerSec: string; writeOpsPerSec: string; readLatencyNs: string; writeLatencyNs: string; meanLatencyNs: string } {
  const reads = BigInt(bin.read_ops), writes = BigInt(bin.write_ops);
  return { readBytesPerSec: ioRate(bin.read_bytes, bin.duration_ns), writeBytesPerSec: ioRate(bin.write_bytes, bin.duration_ns),
    readOpsPerSec: ioRate(bin.read_ops, bin.duration_ns), writeOpsPerSec: ioRate(bin.write_ops, bin.duration_ns),
    readLatencyNs: averageNs(bin.read_latency_ns, bin.read_ops), writeLatencyNs: averageNs(bin.write_latency_ns, bin.write_ops),
    meanLatencyNs: averageNs((BigInt(bin.read_latency_ns) + BigInt(bin.write_latency_ns)).toString(), (reads + writes).toString()) };
}

export type Span = Report['spans'][number];
export type Counter = Report['counters'][number];
export type Track = Report['tracks'][number];
export type Shard = Report['shards'][number];
export type Domain = Report['domains'][number];

export interface IndexedSpan { span: Span; start: bigint; end: bigint; lane: number }
export interface IndexedCounter { counter: Counter; time: bigint }
export interface SpanIndex { items: IndexedSpan[]; maxEnds: bigint[] }
export interface TrackData { spans: SpanIndex; counters: Map<string, IndexedCounter[]>; systemGaps?: Map<string, Uint32Array> }

export interface TimelineRow {
  key: string; kind: 'shard' | 'rank' | 'track' | 'counter'; label: string; depth: number;
  height: number; top: number; shard: Shard; track?: Track; series?: string; hasChildren: boolean;
  system?: 'cpu' | 'memory'; capacity?: string; expanded?: boolean;
}

// Canvas row geometry (CSS pixels). Span bars start 7px from the top, are 11px
// high, and use a 14px lane pitch; the remaining space protects row boundaries.
export const ROW_HEIGHT = { shard: 26, rank: 24, emptyTrack: 24, counter: 48 } as const;
export const SPAN_TOP = 7;
export const SPAN_PITCH = 14;
export const COUNTER_PLOT_TOP = 8;
export const COUNTER_PLOT_HEIGHT = 18;

const SYSTEM_SERIES = { cpu: 'system.cpu.utilization [%; float]', memory: 'system.memory.used [bytes; integer]' } as const;

interface SystemGraph { key: string; label: string; track: Track; shard: Shard; series: string;
  kind: 'cpu' | 'memory'; capacity?: string; children: SystemGraph[]; search: string }
const systemCache = new WeakMap<Map<string, TrackData>, Map<string, SystemGraph[]>>();

export function defaultTimelineCollapsed(report: Report): Set<string> {
  const shards = new Map(report.shards.map(shard => [shard.id, shard]));
  const keys = new Set<string>();
  for (const track of report.tracks) {
    if (track.kind !== 'system' || track.parent || !['cpu', 'memory'].includes(track.details.resource ?? '')) continue;
    const domain = shards.get(track.shard)?.domain;
    if (domain) {
      keys.add(`system:${domain}:${track.details.resource}`);
      keys.add(`system:${domain}:${track.details.resource}:host:${track.details['system.host_id'] || shards.get(track.shard)?.hostname || track.shard}`);
    }
  }
  return keys;
}

export function systemGapPrefix(samples: readonly IndexedCounter[]): Uint32Array {
  const gaps = new Uint32Array(samples.length + 1);
  for (let i = 1; i < samples.length; i++) {
    const current = samples[i]!, previous = samples[i - 1]!;
    const index = current.counter.attributes['system.sample_index'];
    const priorIndex = previous.counter.attributes['system.sample_index'];
    const width = current.counter.attributes['system.bin_width_ns'] ?? current.counter.attributes['system.interval_ns'] ?? '1000000000';
    const missing = current.counter.attributes['system.gap_before'] === 'true' ||
      (index !== undefined && priorIndex !== undefined && BigInt(index) > BigInt(priorIndex) + 1n) ||
      current.time - previous.time > BigInt(width) * 2n;
    gaps[i + 1] = gaps[i]! + Number(missing);
  }
  return gaps;
}

// For decimated points, a line is safe only when *every* skipped observation is continuous.
export function systemPointsConnected(gaps: Uint32Array, previous: number, current: number): boolean {
  return gaps[current + 1] === gaps[previous + 1];
}

function maxSystemCapacity(samples: readonly IndexedCounter[]): string | undefined {
  let max: bigint | undefined;
  for (const sample of samples) {
    const value = sample.counter.attributes['system.memory.total_bytes'];
    if (value !== undefined) {
      const capacity = BigInt(value);
      if (max === undefined || capacity > max) max = capacity;
    }
  }
  return max?.toString();
}

// Derived rows and multi-host estimates are cached once per indexed report; the JSON is never changed.
function indexSystem(report: Report, data: Map<string, TrackData>): Map<string, SystemGraph[]> {
  const domains = new Map<string, SystemGraph[]>();
  if (!report.shards || !report.domains) return new Map();
  const shards = new Map(report.shards.map(shard => [shard.id, shard]));
  const children = new Map<string, Track[]>();
  const sourceGroups = new Map<string, Map<'cpu' | 'memory', Map<string, Track[]>>>();
  for (const track of report.tracks) {
    if (track.kind !== 'system') continue;
    if (track.parent) {
      if (!children.has(track.parent)) children.set(track.parent, []);
      children.get(track.parent)!.push(track);
      continue;
    }
    const shard = shards.get(track.shard);
    const kind = track.details.resource;
    if (!shard || (kind !== 'cpu' && kind !== 'memory')) continue;
    if (!sourceGroups.has(shard.domain)) sourceGroups.set(shard.domain, new Map());
    const byKind = sourceGroups.get(shard.domain)!;
    if (!byKind.has(kind)) byKind.set(kind, new Map());
    const hosts = byKind.get(kind)!;
    const hostId = track.details['system.host_id'] || shard.hostname || shard.id;
    if (!hosts.has(hostId)) hosts.set(hostId, []);
    hosts.get(hostId)!.push(track);
  }
  for (const domain of report.domains) {
    const graphs: SystemGraph[] = [];
    for (const kind of ['cpu', 'memory'] as const) {
      const hosts = sourceGroups.get(domain.id)?.get(kind) ?? new Map<string, Track[]>();
      const hostGraphs: SystemGraph[] = [];
      for (const [hostId, sources] of [...hosts].sort(([a], [b]) => a.localeCompare(b))) {
        // Duplicate MPI rank shards are the same host, not independent contributors.
        const root = sources.sort((a, b) => {
          const count = (track: Track) => data.get(track.id)?.counters.get(SYSTEM_SERIES[kind])?.length ?? 0;
          return count(b) - count(a) || a.id.localeCompare(b.id);
        })[0]!;
        const shard = shards.get(root.shard)!;
        const series = SYSTEM_SERIES[kind];
        const samples = data.get(root.id)?.counters.get(series) ?? [];
        const capacity = kind === 'memory' ? maxSystemCapacity(samples) : undefined;
        const prefix = `system:${domain.id}:${kind}`;
        const host: SystemGraph = { key: `${prefix}:host:${hostId}`, label: `${shard.hostname || hostId} · ${kind === 'cpu' ? 'CPU average' : 'Memory used incl cache'}${sources.length > 1 ? ` · 1 of ${sources.length} rank sources` : ''}`,
          track: root, shard, series, kind, capacity, children: [], search: `${hostId} ${shard.hostname} host ${kind} ${series}` };
        const seen = new Set<string>();
        for (const child of (children.get(root.id) ?? []).sort((a, b) => {
          const id = (track: Track) => Number(track.details.resource?.split(':')[1]);
          return id(a) - id(b) || a.id.localeCompare(b.id);
        })) {
          const resource = child.details.resource ?? '';
          if (seen.has(resource) || !resource.startsWith(kind === 'cpu' ? 'cpu:' : 'node:')) continue;
          seen.add(resource);
          const childSamples = data.get(child.id)?.counters.get(series) ?? [];
          const childCapacity = kind === 'memory' ? maxSystemCapacity(childSamples) : undefined;
          const number = resource.split(':')[1];
          host.children.push({ key: `${host.key}:${resource}`, label: kind === 'cpu' ? `Logical CPU ${number}` : `NUMA node ${number}`,
            track: child, shard, series, kind, capacity: childCapacity, children: [], search: `${host.search} ${resource} ${kind === 'cpu' ? 'logical cpu' : 'numa node'} ${number}` });
        }
        hostGraphs.push(host);
      }
      if (!hostGraphs.length) continue;
      const first = hostGraphs[0]!;
      let track = first.track;
      let series = first.series;
      const key = `system:${domain.id}:${kind}`;
      if (hostGraphs.length > 1) {
        track = { ...first.track, id: key, name: kind === 'cpu' ? 'Host CPU average (sampled estimate)' : 'Host memory total (sampled estimate)' };
        const samples = aggregateSystem(hostGraphs, data, kind, track.id, domain);
        data.set(track.id, { spans: indexSpans([]), counters: new Map([[series, samples]]) });
      }
      const capacity = kind === 'memory' && hostGraphs.every(host => host.capacity !== undefined) ?
        hostGraphs.reduce((sum, host) => sum + BigInt(host.capacity!), 0n).toString() : undefined;
      graphs.push({ key, label: kind === 'cpu' ? 'CPU utilization · host average' : 'Memory used incl cache · host total',
        track, series, shard: first.shard, kind, capacity, children: hostGraphs,
        search: `${kind} system ${series} ${hostGraphs.map(host => host.search).join(' ')}` });
    }
    domains.set(domain.id, graphs);
  }
  return domains;
}

function aggregateSystem(hosts: SystemGraph[], data: Map<string, TrackData>, kind: 'cpu' | 'memory', track: string, domain: Domain): IndexedCounter[] {
  const lists = hosts.map(host => data.get(host.track.id)?.counters.get(host.series) ?? []);
  const intervals = lists.flatMap(list => list.slice(0, 1).map(item => {
    const value = item.counter.attributes['system.interval_ns'];
    return value && /^\d+$/.test(value) ? BigInt(value) : 1_000_000_000n;
  }));
  const maxInterval = intervals.reduce((a, b) => a > b ? a : b, 1n);
  const width = (BigInt(domain.end_ns) / 199_999n + 1n) > maxInterval ? BigInt(domain.end_ns) / 199_999n + 1n : maxInterval;
  const bins = new Map<bigint, Map<number, IndexedCounter>>();
  for (let host = 0; host < lists.length; host++) for (const item of lists[host]!) {
    const bin = item.time / width;
    if (!bins.has(bin)) bins.set(bin, new Map());
    // The latest observation in the bin wins; do not forward-fill missing hosts.
    bins.get(bin)!.set(host, item);
  }
  const result: IndexedCounter[] = [];
  let previous: bigint | undefined;
  for (const [bin, observed] of [...bins].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)) {
    const values = new Map(observed);
    // Accept a neighboring-bin observation only when it is close to an actual
    // observation in this bin; never carry a host forward across a missing read.
    if (values.size < hosts.length && values.size) {
      const reference = [...values.values()].reduce((latest, item) => item.time > latest ? item.time : latest, 0n);
      for (const [host, item] of bins.get(bin - 1n) ?? []) {
        if (!values.has(host) && reference >= item.time && reference - item.time <= maxInterval / 2n) values.set(host, item);
      }
    }
    if (values.size !== hosts.length) continue;
    const members = [...values.values()];
    const gap = previous === undefined || bin !== previous + 1n;
    previous = bin;
    const value = kind === 'memory' ? members.reduce((sum, item) => sum + BigInt(item.counter.value), 0n).toString() :
      (members.reduce((sum, item) => sum + Number(item.counter.value), 0) / members.length).toString();
    // Choose an observed timestamp, never a midpoint outside a partial final bin.
    const latest = members.reduce((time, item) => item.time > time ? item.time : time, 0n);
    const time = latest > BigInt(domain.end_ns) ? BigInt(domain.end_ns) : latest;
    result.push({ time, counter: { track, name: kind === 'cpu' ? 'system.cpu.utilization' : 'system.memory.used',
      timestamp_ns: time.toString(), value, value_type: kind === 'cpu' ? 'float' : 'integer', unit: kind === 'cpu' ? '%' : 'bytes',
      attributes: { 'system.aggregate': kind === 'cpu' ? 'equal-weight mean of host CPU utilization; sampled estimate' : 'sum of host used bytes; sampled estimate',
        'system.coverage': `${hosts.length}/${hosts.length} hosts with fresh observations (same or adjacent bin within half a sample interval)`,
        'system.aggregate_time': latest === time ? 'latest contributing host observation, not a simultaneous measurement' :
          'latest contributing host observation clamped to clock-domain end; not a simultaneous measurement',
        'system.bin_width_ns': width.toString(),
        'system.gap_before': String(gap) } } });
  }
  return result;
}

export function trackRowHeight(index: SpanIndex): number {
  let maxLane = 0;
  for (const item of index.items) maxLane = Math.max(maxLane, item.lane);
  return ROW_HEIGHT.emptyTrack + SPAN_PITCH * maxLane;
}

// A single layout source for drawing, hit testing, and external consumers of
// timeline geometry. Heights depend on indexed spans, never the zoom level.
export function timelineRows(report: Report, data: Map<string, TrackData>, domainId: string | undefined,
  query: string, collapsed: ReadonlySet<string> = defaultTimelineCollapsed(report)): TimelineRow[] {
  const rows: TimelineRow[] = [];
  const byShard = new Map<string, Track[]>();
  const byParent = new Map<string, Track[]>();
  for (const track of report.tracks) {
    if (track.kind === 'system') continue;
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
  const system = systemCache.get(data) ?? indexSystem(report, data);
  systemCache.set(data, system);
  const addSystem = (graph: SystemGraph, depth: number): void => {
    const selfMatch = !query || matchesFilter(`${graph.label} ${graph.search}`, 'system', null, query);
    const descendantMatch = (item: SystemGraph): boolean => item.children.some(child =>
      matchesFilter(`${child.label} ${child.search}`, 'system', null, query) || descendantMatch(child));
    if (!selfMatch && !descendantMatch(graph)) return;
    add({ key: graph.key, kind: 'counter', label: graph.label, depth, height: ROW_HEIGHT.counter,
      shard: graph.shard, track: graph.track, series: graph.series, hasChildren: graph.children.length > 0,
      system: graph.kind, capacity: graph.capacity, expanded: graph.children.length > 0 && (query ? true : !collapsed.has(graph.key)) });
    if (!query && collapsed.has(graph.key)) return;
    // Filtering reveals matching descendants even when their hosts were collapsed by default.
    for (const child of graph.children) addSystem(child, depth + 1);
  };
  if (domainId) for (const graph of system.get(domainId) ?? []) addSystem(graph, 0);
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
    // An unscheduled perf event returned no measurement, not a measured zero.
    if (counter.attributes['pmu.role'] === 'count' && counter.attributes['pmu.status'] !== 'measured') continue;
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
  systemCache.set(result, indexSystem(report, result));
  const systemTracks = new Set(report.tracks.filter(track => track.kind === 'system').map(track => track.id));
  for (const [id, trackData] of result) if (systemTracks.has(id) || id.startsWith('system:')) {
    trackData.systemGaps = new Map([...trackData.counters].map(([series, samples]) => [series, systemGapPrefix(samples)]));
  }
  return result;
}

export function isCollectorHealth(name: string): boolean {
  if (name.startsWith('system.meta.')) return true;
  return /^(io|mpi|openmp)\.(attempted_events|received_events|ingress_dropped_events|initialized_images)$/.test(name) ||
    name === 'mpi.clock.uncertainty_ns' || name.startsWith('openmp.meta.') || name.startsWith('cuda.meta.') || name.startsWith('pmu.meta.') ||
    name.startsWith('python.meta.') || name === 'python.receiver_dropped' || name === 'python.malformed' || name.startsWith('pytorch.import.') ||
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
