import assert from 'node:assert/strict';
import { test } from 'node:test';
import { averageNs, formatNs, indexReport, indexSpans, integerPlotValues, isCollectorHealth, matchesFilter, observedRate, regionThreads, ROW_HEIGHT, SPAN_PITCH, timelineRows, timeToX, trackRowHeight, visibleSpans, xToTime } from '../src/model.ts';
import type { OpenmpRegion, Report, Span } from '../src/model.ts';

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

test('empty, serial, and overlapping spans use only the lanes they need', () => {
  assert.equal(trackRowHeight(indexSpans([])), ROW_HEIGHT.emptyTrack);
  assert.equal(trackRowHeight(indexSpans([span(0, '0', '10'), span(1, '10', '10')])), ROW_HEIGHT.emptyTrack);
  for (let lanes = 2; lanes <= 6; lanes++) {
    const index = indexSpans(Array.from({ length: lanes }, (_, i) => span(i, '0', '10')));
    assert.equal(trackRowHeight(index), ROW_HEIGHT.emptyTrack + SPAN_PITCH * (Math.min(lanes, 4) - 1));
  }
});

test('timeline rows remain contiguous with compact group, empty track and counter rows', () => {
  const report: Report = {
    version: 1, provenance: { kind: 'synthetic', label: '', source: '' },
    summary: { format: '', session_id: '', complete: true, truncated_tail: false, recovered_inputs: '0',
      tracks: '2', spans: '2', counters: '1', dependencies: '0', dropped: '0', omitted_events: '0', warnings: [], categories: [] },
    domains: [{ id: 'd', name: 'd', origin_ns: '0', end_ns: '100' }],
    shards: [{ id: 's', domain: 'd', name: 's', hostname: 's', command: [], collectors: [], origin_ns: '0', end_ns: '100',
      complete: true, exit_code: 0, signal: null, interrupted_by: null, end_reason: '', dropped: '0' }],
    tracks: [
      { id: 'empty', shard: 's', parent: null, name: 'empty', kind: 'process', rank: 0, details: {} },
      { id: 'full', shard: 's', parent: 'empty', name: 'full', kind: 'thread', rank: 0, details: {} },
    ],
    spans: [span(0, '0', '10'), span(1, '0', '10')].map(item => ({ ...item, track: 'full' })),
    counters: [{ track: 'full', name: 'cycles', timestamp_ns: '1', value: '1', value_type: 'integer', unit: 'count', attributes: {} }],
    dependencies: [],
  };
  const rows = timelineRows(report, indexReport(report), 'd', '');
  assert.deepEqual(rows.map(row => row.height), [ROW_HEIGHT.shard, ROW_HEIGHT.rank, ROW_HEIGHT.emptyTrack,
    ROW_HEIGHT.emptyTrack + SPAN_PITCH, ROW_HEIGHT.counter]);
  for (let i = 1; i < rows.length; i++) assert.equal(rows[i]!.top, rows[i - 1]!.top + rows[i - 1]!.height);
  assert.deepEqual(timelineRows(report, indexReport(report), 'd', '', new Set(['track:empty'])).map(row => row.kind),
    ['shard', 'rank', 'track']);
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

test('observed-window rates and operation averages retain exact large decimal strings', () => {
  assert.equal(observedRate('9007199254740993', '1000000000'), '9007199254740993.000 B/s');
  assert.equal(observedRate('5', '2000000000'), '2.500 B/s');
  assert.equal(observedRate('5', '0'), '—');
  assert.equal(averageNs('9007199254740993', '3'), '3002399751580331.000 ns');
  assert.equal(averageNs('5', '2'), '2.500 ns');
  assert.equal(averageNs('0', '0'), '—');
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
      { track: 't', name: 'mpi.received_events', timestamp_ns: '2', value: '17', value_type: 'integer', unit: 'count', attributes: {} },
    ],
  };
  const series = indexReport(report).get('t')!.counters;
  assert.equal(series.size, 3);
  assert.equal(series.get('work [count; integer]')![0]!.counter.value, '9007199254740993');
});

test('OpenMP diagnostic metadata is excluded from plotted counters and region matching uses full identity', () => {
  const region: OpenmpRegion = { shard: 's', domain: 'd', instance: 'image', pid: '9', parallel_id: '9007199254740993',
    requested_threads: '2', observed_threads: '1', duration_ns: null, work_ns: '0', wait_ns: '0',
    work_min_ns: null, work_max_ns: null, work_spread_ns: null };
  const threads = [
    { shard: 's', domain: 'd', instance: 'image', pid: '9', parallel_id: region.parallel_id, tid: '1', team_index: null,
      work_ns: '0', wait_ns: '0', sync_ns: '0', scheduled_ns: '0', execution_segments: '0' },
    { shard: 's', domain: 'other', instance: 'image', pid: '9', parallel_id: region.parallel_id, tid: '2', team_index: null,
      work_ns: '0', wait_ns: '0', sync_ns: '0', scheduled_ns: '0', execution_segments: '0' },
    { shard: 's', domain: 'd', instance: 'other-image', pid: '9', parallel_id: region.parallel_id, tid: '3', team_index: null,
      work_ns: '0', wait_ns: '0', sync_ns: '0', scheduled_ns: '0', execution_segments: '0' },
  ];
  const report: Report = { version: 1, provenance: { kind: 'synthetic', label: '', source: '' },
    summary: { format: '', session_id: '', complete: true, truncated_tail: false, recovered_inputs: '0', tracks: '1',
      spans: '0', counters: '0', dependencies: '0', dropped: '0', omitted_events: '0', warnings: [], categories: [] },
    domains: [], shards: [], tracks: [{ id: 't', shard: 's', parent: null, name: '', kind: 'process', rank: null, details: {} }],
    spans: [], dependencies: [], openmp: { runtimes: [], regions: [region], threads, tasks_created: '0', tasks_completed: '0', execution_segments: '0' },
    counters: ['openmp.attempted_events', 'openmp.received_events', 'openmp.ingress_dropped_events', 'openmp.initialized_images',
      'openmp.meta.capability.parallel_begin', 'openmp.meta.observed.parallel_begin', 'cycles'].map(name =>
      ({ track: 't', name, timestamp_ns: '1', value: '9007199254740993', value_type: 'integer', unit: 'count', attributes: {} })) };
  assert.deepEqual(regionThreads(report, region).map(thread => thread.tid), ['1']);
  assert.deepEqual([...indexReport(report).get('t')!.counters.keys()], ['cycles [count; integer]']);
  assert.equal(isCollectorHealth('openmp.meta.runtime'), true);
  assert.equal(isCollectorHealth('openmp.work'), false);
  assert.deepEqual(regionThreads({ ...report, openmp: undefined }, region), []);
});
