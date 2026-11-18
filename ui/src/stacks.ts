import type { Track } from './model.js';

export type StackMetric = 'cpu' | 'io_time' | 'io_bytes' | 'mpi_time' | 'mpi_bytes' | 'cuda_api' | 'gpu_time' | 'gpu_bytes';
export type StackOrientation = 'top-down' | 'bottom-up';
export type StackGroup = 'combined' | 'rank' | 'thread';
export interface StackFrame { key: string; name: string; module: string; language: string; address: string }
export interface StackTrace { frames: StackFrame[]; status: string }
export interface StackFact {
  domain: string; track: string; stack: string | null; correlation: string | null; name: string;
  metric: StackMetric; start_ns: string; end_ns: string; weight: string;
}
export interface StackLink { from: string; to: string; priority: number }
export interface StackData {
  traces: Record<string, StackTrace>; facts: StackFact[]; sources: Record<string, string[]>;
  links: StackLink[]; complete: boolean; observations: string; warnings: string[];
}
export interface StackQuery {
  snapshot: string; domain: string; start_ns: string; end_ns: string; metric: StackMetric;
  orientation: StackOrientation; group: StackGroup; filter: string;
}
export interface StackNode {
  id: number; parent: number | null; frame: StackFrame; total: string; self_weight: string; observations: string;
}
export interface StackTree {
  snapshot: string; domain: string; metric: StackMetric; nodes: StackNode[];
  total: string; observations: string; unattributed: string; warnings: string[];
}
export const STACK_METRICS: { value: StackMetric; label: string; note: string }[] = [
  { value: 'cpu', label: 'CPU · sampled estimate', note: 'User-space CPU sampling weight, not exact function time or call counts. Sleeping time is not sampled.' },
  { value: 'mpi_time', label: 'MPI · API duration', note: 'Inclusive MPI API duration, not network execution time. Request lifetimes are excluded.' },
  { value: 'mpi_bytes', label: 'MPI · known logical bytes', note: 'Known logical bytes at call completion, not wire volume. Unknown byte counts are excluded, not zero.' },
  { value: 'cuda_api', label: 'CUDA · host API duration', note: 'Host API durations can overlap/nest. They are not GPU execution time.' },
  { value: 'gpu_time', label: 'CUDA · GPU activity duration', note: 'GPU work attributed to host launch stacks through explicit correlations. Streams can overlap; this is not GPU utilization or a device call stack.' },
  { value: 'gpu_bytes', label: 'CUDA · transfer/memset bytes', note: 'Known bytes at GPU activity completion. Launch stacks can originate outside the selected interval.' },
  { value: 'io_time', label: 'I/O · POSIX call duration', note: 'POSIX call duration attributed to issuing stacks, not physical storage time.' },
  { value: 'io_bytes', label: 'I/O · completed bytes', note: 'Completed POSIX bytes at call completion. Bytes are not prorated across a selected range.' },
];
export function stackDuration(metric: StackMetric): boolean {
  return metric === 'io_time' || metric === 'mpi_time' || metric === 'cuda_api' || metric === 'gpu_time';
}
export function aggregateStacks(data: StackData, tracks: readonly Track[], query: StackQuery): StackTree {
  if (!data.complete) throw new Error('Complete stack filtering data is unavailable; use rprof view');
  const begin = BigInt(query.start_ns), end = BigInt(query.end_ns);
  if (begin >= end) throw new Error('Function range must have positive duration');
  const trackMap = new Map(tracks.map(t => [t.id, t]));
  const terms = query.filter.toLowerCase().trim().split(/\s+/).filter(Boolean);
  const nodes: StackNode[] = [], lookup = new Map<string, number>();
  const totals: bigint[] = [], self: bigint[] = [], counts: bigint[] = [];
  let total = 0n, observations = 0n, unattributed = 0n;
  const gpuCandidates = new Map<string, { priority: number; sources: Set<string> }>();
  for (const link of data.links) {
    let entry = gpuCandidates.get(link.to);
    if (!entry || link.priority > entry.priority) {
      entry = { priority: link.priority, sources: new Set() }; gpuCandidates.set(link.to, entry);
    }
    if (link.priority === entry.priority) for (const source of data.sources[link.from] ?? []) entry.sources.add(source);
  }
  const gpuSources = new Map([...gpuCandidates].map(([key, value]) => [key, value.sources.size === 1 ? [...value.sources][0]! : null]));
  for (const fact of data.facts) {
    if (fact.domain !== query.domain || fact.metric !== query.metric) continue;
    const start = BigInt(fact.start_ns), finish = BigInt(fact.end_ns);
    if (finish < start) throw new Error('Function observation ends before it starts');
    const duration = stackDuration(fact.metric);
    const left = start > begin ? start : begin, right = finish < end ? finish : end;
    const count = duration && !(finish >= begin && finish < end) ? 0n : 1n;
    if (duration ? right <= left && count === 0n : start < begin || start >= end) continue;
    const weight = duration ? (right > left ? right - left : 0n) : BigInt(fact.weight);
    if (weight < 0n) throw new Error('Function observation has negative weight');
    const track = trackMap.get(fact.track);
    if (!track) throw new Error('Function observation has no track');
    const category = fact.metric.startsWith('gpu') || fact.metric === 'cuda_api' ? 'cuda' : fact.metric.split('_')[0]!;
    let text = `${fact.name} ${fact.metric} ${category} ${track.rank === null ? '' : `rank ${track.rank} rank:${track.rank}`} `;
    let ancestor: Track | undefined = track;
    for (let depth = 0; ancestor && depth < 128; depth++) {
      text += `${ancestor.name} `;
      ancestor = ancestor.parent ? trackMap.get(ancestor.parent) : undefined;
    }
    if (!terms.every(term => text.toLowerCase().includes(term))) continue;
    let stack = fact.stack;
    if (!stack && (fact.metric === 'gpu_time' || fact.metric === 'gpu_bytes') && fact.correlation)
      stack = gpuSources.get(fact.correlation) ?? null;
    let frames = stack ? data.traces[stack]?.frames : undefined;
    total += weight; observations += count;
    if (!frames?.length) {
      unattributed += weight;
      frames = [{ key: 'missing-stack', name: '[stack not captured]', module: '', language: 'unknown', address: '' }];
    }
    const path = query.orientation === 'bottom-up' ? [...frames].reverse() : [...frames];
    let selfIndex = query.orientation === 'bottom-up' ? 0 : path.length - 1;
    if (query.group !== 'combined') {
      let owner = track;
      if (query.group === 'thread') for (let depth = 0; depth < 128; depth++) {
        if (['thread', 'gpu_stream', 'process'].includes(owner.kind)) break;
        const parent = owner.parent ? trackMap.get(owner.parent) : undefined;
        if (!parent) break;
        owner = parent;
      }
      path.unshift({
        key: query.group === 'rank' ? `rank:${owner.shard}:${owner.rank}` : `track:${owner.id}`,
        name: query.group === 'rank' ? `${owner.rank === null ? 'Unranked' : `Rank ${owner.rank}`} / ${owner.shard}` : owner.name,
        module: '', language: 'scope', address: '',
      });
      selfIndex++;
    }
    let parent: number | null = null;
    path.forEach((frame, position) => {
      const key = JSON.stringify([parent, frame.key]);
      let id = lookup.get(key);
      if (id === undefined) {
        if (nodes.length >= 20_000) throw new Error('Function tree exceeds 20000 nodes; narrow the time/rank/thread scope');
        id = nodes.length;
        lookup.set(key, id);
        nodes.push({ id, parent, frame, total: '0', self_weight: '0', observations: '0' });
        totals.push(0n); self.push(0n); counts.push(0n);
      }
      totals[id] = totals[id]! + weight; counts[id] = counts[id]! + count;
      if (position === selfIndex) self[id] = self[id]! + weight;
      parent = id;
    });
  }
  nodes.forEach((node, i) => { node.total = totals[i]!.toString(); node.self_weight = self[i]!.toString(); node.observations = counts[i]!.toString(); });
  return { snapshot: query.snapshot, domain: query.domain, metric: query.metric, nodes,
    total: total.toString(), observations: observations.toString(), unattributed: unattributed.toString(), warnings: data.warnings };
}
