import assert from 'node:assert/strict';
import { test } from 'node:test';
import { aggregateIoBins, averageNs, defaultTimelineCollapsed, deviceActivities, domainClockLabel, formatNs, indexReport, indexSpans, integerPlotValues, ioBinMetrics, isCollectorHealth, matchesFilter, observedRate, regionThreads, ROW_HEIGHT, SPAN_PITCH, systemGapPrefix, systemPointsConnected, timelineRows, timeToX, trackRowHeight, visibleSpans, xToTime } from '../src/model.ts';
import type { CudaDevice, IoFilesystem, IoTimeBin, OpenmpRegion, Report, Span } from '../src/model.ts';

test('I/O bins use actual short durations, exact counts, weighted latency and unknown empty-bin latency', () => {
  const bin: IoTimeBin = { start_ns: '1000000000', duration_ns: '250000000', read_bytes: '1048576', write_bytes: '0',
    read_ops: '1', write_ops: '3', other_ops: '2', errors: '1', read_latency_ns: '4000000',
    write_latency_ns: '6000000', other_latency_ns: '9999999', max_latency_ns: '9999999' };
  const metrics = ioBinMetrics(bin);
  assert.equal(metrics.readBytesPerSec, '4194304.000');
  assert.equal(metrics.readOpsPerSec, '4.000');
  assert.equal(metrics.writeOpsPerSec, '12.000');
  assert.equal(metrics.meanLatencyNs, '2500000.000 ns'); // 10 ms / 4 calls, not mean(4 ms, 2 ms)
  assert.equal(metrics.readLatencyNs, '4000000.000 ns');
  const fs = (id: string, value: IoTimeBin, domain = 'd'): IoFilesystem => ({ id, domain, host: id,
    filesystem_type: 'ext4', mount_point: '/', device: '/dev/x', kind: 'filesystem', bin_width_ns: '1000000000', bins: [value] });
  const other = { ...bin, read_bytes: '90071992547409931234', read_ops: '2', read_latency_ns: '2000000', errors: '3' };
  const result = aggregateIoBins([fs('a', bin), fs('b', other)])!;
  assert.equal(result[0]!.read_bytes, (90071992547409931234n + 1048576n).toString());
  assert.equal(result[0]!.errors, '4');
  assert.equal(result[0]!.max_latency_ns, bin.max_latency_ns);
  assert.equal(ioBinMetrics(result[0]!).meanLatencyNs, '2000000.000 ns'); // 12 ms / 6 calls
  assert.equal(aggregateIoBins([fs('a', bin), fs('b', other, 'foreign')]), null);
  assert.equal(aggregateIoBins([fs('a', bin), fs('b', { ...other, duration_ns: '100000000' })]), null);
  const empty = { ...bin, read_ops: '0', write_ops: '0', read_latency_ns: '0', write_latency_ns: '0' };
  assert.equal(ioBinMetrics(empty).meanLatencyNs, '—');
  assert.equal(ioBinMetrics({ ...empty, duration_ns: '0' }).readOpsPerSec, '—');
});

const span = (id: number, start_ns: string, duration_ns: string): Span => ({
  id, track: 't', name: `span ${id}`, category: 'compute', start_ns, duration_ns,
  correlation: null, incomplete: false, attributes: {},
});

function systemFixture(): Report {
  const report: Report = {
    version: 1, provenance: { kind: 'measured', label: '', source: '' },
    summary: { format: '', session_id: '', complete: true, truncated_tail: false, recovered_inputs: '0', tracks: '0', spans: '0', counters: '0', dependencies: '0', dropped: '0', omitted_events: '0', warnings: [], categories: [] },
    domains: [{ id: 'aligned', name: 'aligned', origin_ns: '0', end_ns: '9000000000' }, { id: 'other', name: 'other', origin_ns: '0', end_ns: '9000000000' }],
    shards: ['s1', 's2', 'duplicate', 'isolated'].map((id, i) => ({ id, domain: i === 3 ? 'other' : 'aligned', name: id, hostname: i === 0 || i === 2 ? 'alpha' : 'beta', command: [], collectors: [], origin_ns: '0', end_ns: '9000000000', complete: true, exit_code: 0, signal: null, interrupted_by: null, end_reason: '', dropped: '0' })),
    tracks: [], spans: [], counters: [], dependencies: [],
  };
  for (const shard of report.shards) {
    for (const kind of ['cpu', 'memory'] as const) {
      const id = `${shard.id}:${kind}`;
      report.tracks.push({ id, shard: shard.id, parent: null, name: kind, kind: 'system', rank: null,
        details: { resource: kind, 'system.host_id': shard.hostname === 'alpha' ? 'alpha:boot' : 'beta:boot' } });
      report.tracks.push({ id: `${id}:child`, shard: shard.id, parent: id, name: 'child', kind: 'system', rank: null,
        details: { resource: kind === 'cpu' ? 'cpu:3' : 'node:1' } });
      for (const time of shard.id === 's2' ? [1, 2, 4] : [1, 2, 3, 4]) for (const target of [id, `${id}:child`]) {
        const memory = kind === 'memory';
        report.counters.push({ track: target, name: memory ? 'system.memory.used' : 'system.cpu.utilization',
          timestamp_ns: `${time}000000000`, value: memory ? shard.id === 's2' ? '90071992547409930000' : '90071992547409930001' : shard.id === 's2' ? '80' : '20',
          value_type: memory ? 'integer' : 'float', unit: memory ? 'bytes' : '%', attributes: {
            'system.interval_ns': '1000000000', 'system.sample_index': String(time),
            ...(memory ? { 'system.memory.total_bytes': '100000000000000000000' } : {}),
          } });
      }
    }
  }
  return report;
}

test('system overviews precede shards, deduplicate ranks, show exact memory sums and gaps without cross-domain leakage', () => {
  const report = systemFixture();
  assert.equal(isCollectorHealth('system.meta.read_errors'), true);
  report.counters.push({ track: 's1:cpu', name: 'system.meta.read_errors', timestamp_ns: '1', value: '2', value_type: 'integer', unit: 'count', attributes: {} });
  const data = indexReport(report);
  const collapsed = defaultTimelineCollapsed(report);
  let rows = timelineRows(report, data, 'aligned', '', collapsed);
  assert.deepEqual(rows.slice(0, 2).map(row => row.key), ['system:aligned:cpu', 'system:aligned:memory']);
  assert.equal(rows.filter(row => row.key.startsWith('system:aligned:cpu:host:')).length, 0);
  assert.equal(rows.some(row => row.track?.kind === 'system' && row.key.startsWith('track:')), false);
  const cpu = data.get('system:aligned:cpu')!.counters.values().next().value!;
  assert.deepEqual(cpu.map(item => [item.time.toString(), item.counter.value]), [['1000000000', '50'], ['2000000000', '50'], ['4000000000', '50']]);
  assert.equal(cpu[2]!.counter.attributes['system.gap_before'], 'true');
  assert.match(cpu[0]!.counter.attributes['system.coverage']!, /fresh observations/);
  const memory = data.get('system:aligned:memory')!.counters.values().next().value!;
  assert.equal(memory[0]!.counter.value, '180143985094819860001');
  assert.equal(memory.length, 3);
  collapsed.delete('system:aligned:cpu');
  collapsed.delete('system:aligned:cpu:host:alpha:boot');
  rows = timelineRows(report, data, 'aligned', '', collapsed);
  assert.equal(rows.filter(row => row.key === 'system:aligned:cpu:host:alpha:boot:cpu:3').length, 1);
  assert.match(rows.find(row => row.key === 'system:aligned:cpu:host:alpha:boot')!.label, /1 of 2 rank sources/);
  assert.equal(rows.some(row => row.key.includes('duplicate:cpu')), false);
  assert.deepEqual(timelineRows(report, data, 'other', '', collapsed).slice(0, 2).map(row => row.key), ['system:other:cpu', 'system:other:memory']);
  assert.equal(timelineRows(report, data, 'aligned', 'node:1', collapsed).some(row => row.key.endsWith('node:1')), true);
  assert.equal(timelineRows(report, data, 'aligned', 'node:1', defaultTimelineCollapsed(report)).find(row => row.key === 'system:aligned:memory')?.expanded, true);
});

test('system gaps are indexed once and decimation never connects across missing samples or coarse aggregation bins', () => {
  const report = systemFixture();
  const data = indexReport(report);
  const cpu = data.get('s2:cpu')!;
  const gaps = cpu.systemGaps!.get('system.cpu.utilization [%; float]')!;
  assert.equal(systemPointsConnected(gaps, 0, 1), true);
  assert.equal(systemPointsConnected(gaps, 0, 2), false);
  assert.equal(systemPointsConnected(data.get('system:aligned:cpu')!.systemGaps!.values().next().value!, 0, 2), false);
  const samples = Array.from({ length: 10_000 }, (_, i) => ({ time: BigInt(i) * 5_000_000_000n,
    counter: { track: 't', name: 'system.cpu.utilization', timestamp_ns: String(i * 5_000_000_000), value: '1',
      value_type: 'float' as const, unit: '%', attributes: { 'system.bin_width_ns': '5000000000', 'system.gap_before': i === 5333 ? 'true' : 'false' } } }));
  const coarse = systemGapPrefix(samples);
  assert.equal(systemPointsConnected(coarse, 100, 103), true);
  assert.equal(systemPointsConnected(coarse, 5331, 5334), false);
  assert.equal(systemPointsConnected(coarse, 6000, 6003), true);
});

test('hotplug capacity uses maximum observed BigInt capacity; aggregates stay inside the domain end', () => {
  const report = systemFixture();
  report.domains[0]!.end_ns = '2100000000';
  for (const sample of report.counters) if (sample.track === 's1:memory' && sample.timestamp_ns === '2000000000')
    sample.attributes['system.memory.total_bytes'] = '100000000000000000123';
  for (const sample of report.counters) if (sample.track === 's2:memory' && sample.timestamp_ns === '2000000000')
    sample.attributes['system.memory.total_bytes'] = '100000000000000000456';
  report.counters = report.counters.filter(sample => BigInt(sample.timestamp_ns) <= 2_100_000_000n);
  report.counters = report.counters.filter(sample => !(sample.track === 'duplicate:memory' && sample.timestamp_ns === '2000000000'));
  for (const sample of report.counters) if (sample.timestamp_ns === '2000000000') sample.timestamp_ns = '2099000000';
  const data = indexReport(report);
  const rows = timelineRows(report, data, 'aligned', '');
  assert.equal(rows.find(row => row.key === 'system:aligned:memory')?.capacity, '200000000000000000579');
  const aggregate = data.get('system:aligned:memory')!.counters.values().next().value!;
  assert.equal(aggregate.at(-1)!.time, 2_099_000_000n);
  assert.ok(aggregate.every(sample => sample.time <= BigInt(report.domains[0]!.end_ns)));
});

test('many domains and 256 hosts × 128 CPUs remain compact until explicitly expanded', () => {
  const report = systemFixture();
  report.domains = Array.from({ length: 256 }, (_, i) => ({ id: `d${i}`, name: `d${i}`, origin_ns: '0', end_ns: '1000000000' }));
  report.shards = []; report.tracks = []; report.counters = [];
  for (let host = 0; host < 256; host++) {
    const id = `h${host}`;
    report.shards.push({ id, domain: 'd0', name: id, hostname: id, command: [], collectors: [], origin_ns: '0', end_ns: '1000000000', complete: true, exit_code: 0, signal: null, interrupted_by: null, end_reason: '', dropped: '0' });
    report.tracks.push({ id, shard: id, kind: 'system', parent: null, name: id, rank: null, details: { resource: 'cpu', 'system.host_id': id } });
    report.tracks.push({ id: `${id}:memory`, shard: id, kind: 'system', parent: null, name: 'memory', rank: null, details: { resource: 'memory', 'system.host_id': id } });
    for (let cpu = 0; cpu < 128; cpu++) report.tracks.push({ id: `${id}:${cpu}`, shard: id, kind: 'system', parent: id, name: `CPU ${cpu}`, rank: null, details: { resource: `cpu:${cpu}` } });
  }
  const data = indexReport(report);
  const collapsed = defaultTimelineCollapsed(report);
  assert.equal(timelineRows(report, data, 'd0', '', collapsed).filter(row => row.system).length, 2);
  assert.equal(data.get('system:d0:cpu')!.counters.values().next().value!.length, 0);
  collapsed.delete('system:d0:cpu');
  assert.equal(timelineRows(report, data, 'd0', '', collapsed).filter(row => row.system).length, 258);
  collapsed.delete('system:d0:cpu:host:h0');
  assert.equal(timelineRows(report, data, 'd0', '', collapsed).filter(row => row.system).length, 386);
  assert.equal(timelineRows(report, data, 'd255', '', collapsed).filter(row => row.system).length, 0);
});

test('Python and PyTorch diagnostic counters stay out of timeline; framework bytes and marker samples remain', () => {
  for (const name of ['python.meta.backend', 'python.receiver_dropped', 'python.malformed',
    'pytorch.import.metadata', 'pytorch.import.skipped_records']) assert.equal(isCollectorHealth(name), true, name);
  for (const name of ['pytorch.memory.allocated', 'pytorch.memory.reserved', '[memory]', 'sensors.watts'])
    assert.equal(isCollectorHealth(name), false, name);
  const report = { tracks: [{ id: 't' }], spans: [], counters: [
    ...['python.meta.backend', 'python.receiver_dropped', 'python.malformed', 'pytorch.import.metadata', 'pytorch.import.skipped_records',
      'pytorch.memory.allocated', 'pytorch.memory.reserved', '[memory]', 'sensors.watts'].map(name => ({
        track: 't', name, timestamp_ns: '0', value: '90071992547409931234', value_type: 'integer', unit: name === '[memory]' ? 'marker' : 'bytes', attributes: {},
      })),
  ] } as unknown as Report;
  assert.deepEqual([...indexReport(report).get('t')!.counters.keys()], ['pytorch.memory.allocated [bytes; integer]',
    'pytorch.memory.reserved [bytes; integer]', '[memory] [marker; integer]', 'sensors.watts [bytes; integer]']);
});

test('imported domain clock never claims native monotonic alignment, even when it is the only domain', () => {
  const report = { domains: [{ id: 'import' }], pytorch: { sources: [{ domain: 'import' }] } } as Report;
  assert.match(domainClockLabel(report, 'import'), /IMPORTED PYTORCH.*NOT NATIVE MONOTONIC/);
  report.domains.push({ id: 'native', name: 'native', origin_ns: '0', end_ns: '10' });
  assert.match(domainClockLabel(report, 'import'), /UNALIGNED WITH NATIVE/);
  assert.equal(domainClockLabel(report, 'native'), 'CLOCK DOMAINS ARE UNALIGNED');
  assert.equal(domainClockLabel({ ...report, pytorch: undefined, domains: [report.domains[1]!] }, 'native'), 'SAME-DOMAIN MONOTONIC TIME');
});

test('CUDA metadata and diagnostic counters are not timeline samples', () => {
  for (const name of ['cuda.meta.device.name', 'cuda.cupti_dropped_records', 'cuda.invalid_timing_records',
    'cuda.attempted_events', 'cuda.received_events', 'cuda.ingress_dropped_events', 'cuda.source_errors',
    'cuda.finalized', 'cuda.initialized_images', 'cuda.unattributed_records', 'cuda.unresolved_correlations',
    'cuda.ambiguous_correlations']) assert.equal(isCollectorHealth(name), true, name);
  assert.equal(isCollectorHealth('cuda.real_metric'), false);
  const report = { tracks: [{ id: 't' }], spans: [], counters: [{ track: 't', name: 'cuda.meta.device.name',
    value: '1', timestamp_ns: '0', value_type: 'integer', unit: 'count', attributes: {} }] } as unknown as Report;
  assert.equal(indexReport(report).get('t')?.counters.size, 0);
});

test('PMU metadata is diagnostic while cumulative raw perf counts remain timeline samples', () => {
  for (const name of ['pmu.meta.event', 'pmu.meta.host.kernel', 'pmu.meta.read_error']) {
    assert.equal(isCollectorHealth(name), true, name);
  }
  assert.equal(isCollectorHealth('pmu.cycles.raw'), false);
  const report = { tracks: [{ id: 't' }], spans: [], counters: [
    { track: 't', name: 'pmu.meta.event', value: '1', timestamp_ns: '0', value_type: 'integer', unit: 'metadata', attributes: {} },
    { track: 't', name: 'pmu.instructions.raw', value: '0', timestamp_ns: '0', value_type: 'integer', unit: 'count',
      attributes: { 'pmu.role': 'count', 'pmu.status': 'not_counted', 'pmu.time_running_ns': '0' } },
    { track: 't', name: 'pmu.cycles.raw', value: '340282366920938463463374607431768211455', timestamp_ns: '1',
      value_type: 'integer', unit: 'count', attributes: { 'pmu.time_enabled_ns': '100', 'pmu.time_running_ns': '50', 'pmu.read_window_ns': '25' } },
  ] } as unknown as Report;
  const series = indexReport(report).get('t')!.counters;
  assert.deepEqual([...series.keys()], ['pmu.cycles.raw [count; integer]']);
  assert.equal(series.values().next().value?.[0]?.counter.attributes['pmu.time_running_ns'], '50');
});

test('CUDA device activity matches full image/shard/domain identity', () => {
  const device = { shard: 's', domain: '0', instance: 'image', device: '0' };
  const activity = { ...device, context: 'c', stream: '1' };
  const report = { cuda: { activities: [activity, { ...activity, domain: '1' },
    { ...activity, shard: 'other' }, { ...activity, instance: 'other' }, { ...activity, device: '1' }] } } as unknown as Report;
  assert.deepEqual(deviceActivities(report, device as CudaDevice), [activity]);
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
