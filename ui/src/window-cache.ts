import type { Report, IoTimeBin } from './model.js';

export type WindowResponse = { snapshot: string; domain: string; start_ns: string; end_ns: string;
  spans: Report['spans']; counters: Report['counters']; dependencies: Report['dependencies'];
  io: Record<string, IoTimeBin[]>; limited: boolean; warnings: string[] };
export type WindowScopes = { tracks: string[]; systems: string[]; io_scopes: string[] };
export type WindowRequest = WindowScopes & { snapshot: string; domain: string; start_ns: string; end_ns: string;
  pixels: number; filesystem: string };
const kinds = ['tracks', 'systems', 'io_scopes'] as const;
export const emptyScopes = (): WindowScopes => ({ tracks: [], systems: [], io_scopes: [] });
export const hasScopes = (scopes: WindowScopes): boolean => kinds.some(kind => scopes[kind].length > 0);
export function windowIdentity(request: WindowRequest): string {
  const { tracks: _tracks, systems: _systems, io_scopes: _io, ...window } = request;
  return JSON.stringify(window);
}

// Keep completed batches for one time/resolution/source window. Empty and limited
// replies also cover their requested scopes: neither warrants an automatic loop.
// Budgets bound payload size, observations, and empty row metadata independently.
export class WindowCache {
  private combined: WindowResponse | undefined;
  private readonly maxBytes: number;
  private readonly maxPoints: number;
  private readonly maxRows: number;
  private entries: { scopes: WindowScopes; result: WindowResponse; bytes: number; points: number; rows: number }[] = [];
  evicted = false;
  constructor(maxBytes = 64 * 1024 * 1024, maxPoints = 100_000, maxRows = 4096) {
    this.maxBytes = maxBytes; this.maxPoints = maxPoints; this.maxRows = maxRows;
  }

  missing(wanted: WindowScopes): WindowScopes {
    const result = emptyScopes();
    for (const kind of kinds) {
      const covered = new Set(this.entries.flatMap(entry => entry.scopes[kind]));
      result[kind] = wanted[kind].filter(id => !covered.has(id));
    }
    // Touch batches contributing to this viewport before admitting new rows.
    const needed = new Set(kinds.flatMap(kind => wanted[kind].map(id => `${kind}:${id}`)));
    this.entries.sort((a, b) => Number(kinds.some(kind => a.scopes[kind].some(id => needed.has(`${kind}:${id}`)))) -
      Number(kinds.some(kind => b.scopes[kind].some(id => needed.has(`${kind}:${id}`)))));
    return result;
  }

  add(scopes: WindowScopes, result: WindowResponse): void {
    this.combined = undefined;
    this.entries.push({ scopes, result, bytes: JSON.stringify(result).length * 2,
      points: result.spans.length + result.counters.length + result.dependencies.length + Object.values(result.io).reduce((n, bins) => n + bins.length, 0),
      rows: kinds.reduce((n, kind) => n + scopes[kind].length, 0) });
    const total = (key: 'bytes' | 'points' | 'rows') => this.entries.reduce((n, entry) => n + entry[key], 0);
    // Retain at least the latest bounded server reply so even an oversized row
    // can be displayed without repeatedly requesting it.
    while (this.entries.length > 1 && (total('bytes') > this.maxBytes || total('points') > this.maxPoints || total('rows') > this.maxRows)) {
      this.entries.shift(); this.evicted = true;
    }
  }

  covers(kind: keyof WindowScopes, id: string): boolean {
    return this.entries.some(entry => entry.scopes[kind].includes(id));
  }

  merged(): WindowResponse | undefined {
    if (this.combined) return this.combined;
    const last = this.entries.at(-1)?.result;
    if (!last) return undefined;
    const unique = <T>(values: T[]): T[] => [...new Map(values.map(value => [JSON.stringify(value), value])).values()];
    const systems = new Set(this.entries.flatMap(entry => entry.scopes.systems));
    return this.combined = { ...last,
      spans: unique(this.entries.flatMap(entry => entry.result.spans)),
      // Raw track replies can include counters too. Prefer explicitly requested
      // counter/LOD data to avoid displaying raw and reduced copies together.
      counters: unique(this.entries.flatMap(entry => entry.result.counters.filter(counter =>
        !systems.has(counter.track) || entry.scopes.systems.includes(counter.track)))),
      dependencies: unique(this.entries.flatMap(entry => entry.result.dependencies)),
      io: Object.assign({}, ...this.entries.map(entry => entry.result.io)),
      limited: this.entries.some(entry => entry.result.limited),
      warnings: unique(this.entries.flatMap(entry => entry.result.warnings ?? [])),
    };
  }
}
