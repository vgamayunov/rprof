import assert from 'node:assert/strict';
import { test } from 'node:test';
import { formatNs, indexReport, indexSpans, integerPlotValues, matchesFilter, timeToX, visibleSpans, xToTime } from '../src/model.ts';
import type { Report, Span } from '../src/model.ts';

const span = (id: number, start_ns: string, duration_ns: string): Span => ({
  id, track: 't', name: `span ${id}`, category: 'compute', start_ns, duration_ns,
  correlation: null, incomplete: false, attributes: {},
});

test('time geometry subtracts exact BigInt origins before converting to pixels', () => {
  const origin = 9007199254740993123456789n;
  assert.equal(timeToX(origin + 2000n, origin, 10), 200);
  assert.equal(xToTime(200, origin, 10), origin + 2000n);
  assert.equal(formatNs(1234567n), '1.234 ms');
});

test('span interval index includes long overlapping spans and culls by time', () => {
  const index = indexSpans([span(0, '0', '10000'), span(1, '100', '2'), span(2, '9000', '20')]);
  assert.deepEqual(visibleSpans(index, 8000n, 9050n).map(item => item.span.id), [0, 2]);
  assert.deepEqual(visibleSpans(index, 10001n, 10100n), []);
  assert.equal(index.items[1]?.lane, 1);
});

test('large overlap is lane-bounded; sampled culling limits render work', () => {
  const index = indexSpans(Array.from({ length: 10000 }, (_, i) => span(i, `${i}`, '10000')));
  assert.ok(index.items.every(item => item.lane >= 0 && item.lane < 4));
  assert.ok(visibleSpans(index, 0n, 10000n, 500).length <= 500);
});

test('filter searches name, category and rank without treating unrelated rank as match', () => {
  assert.equal(matchesFilter('thread 0', 'GPU kernel', 7, 'gpu rank:7'), true);
  assert.equal(matchesFilter('thread 0', 'GPU kernel', 7, 'gpu rank:8'), false);
  assert.equal(matchesFilter('I/O', 'disk', null, 'rank'), false);
  assert.equal(matchesFilter('anything', 'compute', null, '  '), true);
});

test('integer counter baseline subtraction preserves adjacent values above 2^53', () => {
  assert.deepEqual(integerPlotValues(['90071992547409931234', '90071992547409931235', '90071992547409931232']), [0, 1, -2]);
  assert.deepEqual(integerPlotValues([]), []);
});

test('counter series with differing units or value types are not mixed', () => {
  const report: Report = {
    version: 1, provenance: { kind: 'synthetic', label: 'fixture', source: '' },
    summary: { format: '', session_id: '', complete: true, truncated_tail: false, recovered_inputs: '0',
      tracks: '1', spans: '0', counters: '3', dependencies: '0', dropped: '0', omitted_events: '0', warnings: [], categories: [] },
    domains: [], shards: [], spans: [], dependencies: [],
    tracks: [{ id: 't', shard: 's', parent: null, name: 'test', kind: 'process', rank: null, details: {} }],
    counters: [
      { track: 't', name: 'work', timestamp_ns: '0', value: '9007199254740993', value_type: 'integer', unit: 'count', attributes: {} },
      { track: 't', name: 'work', timestamp_ns: '1', value: '0.5', value_type: 'float', unit: 'count', attributes: {} },
      { track: 't', name: 'work', timestamp_ns: '2', value: '1', value_type: 'integer', unit: 'bytes', attributes: {} },
    ],
  };
  const series = indexReport(report).get('t')!.counters;
  assert.equal(series.size, 3);
  assert.equal(series.get('work [count; integer]')![0]!.counter.value, '9007199254740993');
});
