import { readFileSync } from 'node:fs';
import type { Report, Track } from '../src/model.ts';
import type { StackData } from '../src/stacks.ts';

export function stackFixture(): Report {
  const fixture: { data: StackData; tracks: Track[] } = JSON.parse(readFileSync(new URL('../../fixtures/stack-profile.json', import.meta.url), 'utf8'));
  return {
    version: 1, provenance: { kind: 'synthetic', label: 'Synthetic stack fixture', source: 'stack-profile.json' },
    summary: { format: 'fixture', session_id: 'stacks', complete: true, truncated_tail: false, recovered_inputs: '0',
      tracks: '5', spans: '1', counters: '0', dependencies: '0', dropped: '0', omitted_events: '0', warnings: [], categories: [] },
    domains: [{ id: 'd', name: 'First domain', origin_ns: '0', end_ns: '1000' },
      { id: 'other', name: 'Unaligned domain', origin_ns: '0', end_ns: '1000' }],
    shards: ['s', 'other'].map((id, i) => ({ id, domain: i ? 'other' : 'd', name: `Rank ${i}`, hostname: `host-${i}`,
      command: ['solver'], collectors: ['stacks'], origin_ns: '0', end_ns: '1000', complete: true,
      exit_code: 0, signal: null, interrupted_by: null, end_reason: 'completed', dropped: '0' })),
    tracks: fixture.tracks, stacks: fixture.data,
    spans: [{ id: 0, track: 's:t', name: 'work', category: 'process', start_ns: '0', duration_ns: '1000',
      correlation: null, incomplete: false, attributes: {} }],
    counters: [], dependencies: [],
  };
}
