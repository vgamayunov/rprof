import assert from 'node:assert/strict';
import { test } from 'node:test';
import { defaultTimelineCollapsed, indexReport, isCollectorHealth, storageBins, storageGraphs, timelineRows } from '../src/model.ts';
import { storageFixture } from './storage-fixture.ts';

test('storage keeps backend totals separate, deduplicates host resources and drills through filesystem', () => {
  const report = storageFixture(), domain = report.shards[0]!.domain, data = indexReport(report);
  const roots = storageGraphs(report, domain);
  assert.deepEqual(roots.map(root => root.label), ['Block-device I/O total', 'Lustre bulk RPC I/O total', 'NFS server payload I/O total']);
  assert.equal(roots[0]!.leaves.length, 3);
  assert.equal(roots[0]!.children.length, 2);
  assert.equal(roots[0]!.children[0]!.children.length, 2);
  const collapsed = defaultTimelineCollapsed(report);
  assert.ok(collapsed.has(roots[0]!.key));
  const rows = () => timelineRows(report, data, domain, '', collapsed);
  assert.ok(rows().some(row => row.io), 'POSIX layer remains visible');
  assert.equal(rows().filter(row => row.storage).length, 3);
  collapsed.delete(roots[0]!.key);
  assert.equal(rows().filter(row => row.storage?.backend === 'block').length, 3);
  collapsed.delete(roots[0]!.children[0]!.key);
  assert.equal(rows().filter(row => row.storage?.backend === 'block').length, 5);
  assert.equal(storageGraphs(report, report.shards.find(s => s.domain !== domain)!.domain).length, 0);
  assert.ok(isCollectorHealth('storage.meta.read_errors'));
});

test('storage sums only fresh observed rates, keeps missing writes unknown and weights latency by exact ops', () => {
  const report = storageFixture(), data = indexReport(report), block = storageGraphs(report, report.shards[0]!.domain)[0]!;
  const fs = block.children[0]!;
  const throughput = storageBins(fs, data, 'throughput');
  assert.deepEqual(throughput.map(bin => bin.values[0]), [3, 1, 3]);
  assert.equal(throughput[0]!.coverage[0], '2/2');
  assert.equal(throughput[1]!.coverage[0], '1/2');
  assert.equal(throughput[0]!.coverage[1], '1/2');
  assert.equal(throughput[2]!.gapBefore, true);
  assert.ok(Number.isNaN(storageBins(fs.children[1]!, data, 'throughput')[0]!.values[1]));
  const latency = storageBins(fs, data, 'latency');
  assert.equal(latency[0]!.values[0], 2.8, '(10ms × 1 + 2ms × 9) / 10');
  assert.ok(Number.isNaN(latency[1]!.values[1]) === false);
  assert.equal(storageBins(block, data, 'throughput')[0]!.values[0], 6, 'unmapped disk stays in explicit shared group');
});

test('hundreds of storage resources remain collapsed; diagnostic counters never become timeline lanes', () => {
  const report = storageFixture(), shard = report.shards[0]!;
  for (let i = 0; i < 240; i++) report.tracks.push({ id: `device-${i}`, shard: shard.id,
    parent: null, rank: null, name: `disk ${i}`, kind: 'storage', details: {
      'storage.backend': 'block', 'storage.host_id': `host-${i}`, 'storage.filesystem': 'shared device (unmapped)',
      'storage.mount_point': 'unknown', resource: `disk-${i}` } });
  report.counters.push({ track: 'device-0', name: 'storage.meta.read_errors', timestamp_ns: '1000000000',
    value: '1', value_type: 'integer', unit: 'errors', attributes: {} });
  const data = indexReport(report), collapsed = defaultTimelineCollapsed(report);
  assert.equal(timelineRows(report, data, shard.domain, '', collapsed).filter(row => row.storage).length, 3);
  assert.equal(data.get('device-0')!.counters.size, 0);
  const block = storageGraphs(report, shard.domain)[0]!;
  collapsed.delete(block.key);
  assert.equal(timelineRows(report, data, shard.domain, '', collapsed).filter(row => row.storage?.backend === 'block').length, 3);
  collapsed.delete(block.children.find(child => child.label === 'shared device (unmapped)')!.key);
  assert.equal(timelineRows(report, data, shard.domain, '', collapsed).filter(row => row.storage?.backend === 'block').length, 244);
});

test('100 ms native samples draw preceding measured intervals, never future one-second bars', () => {
  const report = storageFixture();
  report.tracks = report.tracks.filter(track => track.kind !== 'storage' || track.id === 'block-a');
  report.counters = report.counters.filter(counter => counter.track !== 'block-a');
  for (const ms of [250, 350]) report.counters.push({ track: 'block-a', name: 'storage.read.bytes_per_second',
    timestamp_ns: String(ms * 1_000_000), value: '1048576', value_type: 'float', unit: 'B/s',
    attributes: { 'storage.interval_ns': '100000000', 'storage.window_ns': '100000000',
      'storage.sample_index': String((ms - 150) / 100) } });
  const graph = storageGraphs(report, report.shards[0]!.domain)[0]!;
  const bins = storageBins(graph, indexReport(report), 'throughput');
  assert.deepEqual(bins.map(bin => [bin.start, bin.start + bin.duration]), [
    [150_000_000n, 250_000_000n], [250_000_000n, 350_000_000n],
  ]);
  assert.deepEqual(bins.map(bin => bin.values[0]), [1, 1]);
  assert.ok(bins.every(bin => Number.isNaN(bin.values[1])));
});

test('indexed read/write representatives merge per side in one 200 ms bucket; gaps OR without contaminating the other side', () => {
  const report = storageFixture();
  report.tracks = report.tracks.filter(track => track.kind !== 'storage' || track.id === 'block-a');
  report.counters = report.counters.filter(counter => counter.track !== 'block-a');
  const add = (ms: number, direction: 'read' | 'write', rate: number, gap: boolean): void => {
    report.counters.push({ track: 'block-a', name: `storage.${direction}.bytes_per_second`,
      timestamp_ns: String(ms * 1_000_000), value: String(rate * 1048576), value_type: 'float', unit: 'B/s',
      attributes: { 'storage.interval_ns': '100000000', 'storage.report_interval_ns': '200000000',
        'storage.window_ns': '100000000', 'storage.report_gap_before': String(gap),
        'storage.sample_index': String(ms / 10) } });
  };
  add(100, 'read', 1, false); add(120, 'write', 1, false);
  add(300, 'read', 2, true); add(220, 'write', 3, false); add(350, 'read', 4, false);
  add(450, 'read', 5, false); add(420, 'write', 6, true);
  const graph = storageGraphs(report, report.shards[0]!.domain)[0]!;
  const bins = storageBins(graph, indexReport(report), 'throughput');
  assert.equal(bins.length, 3);
  assert.deepEqual(bins.map(bin => bin.values), [[1, 1], [4, 3], [5, 6]]);
  assert.deepEqual(bins[1]!.gapBeforeSides, [true, false]);
  assert.deepEqual(bins[2]!.gapBeforeSides, [false, true]);
  assert.deepEqual([bins[1]!.start, bins[1]!.start + bins[1]!.duration], [120_000_000n, 350_000_000n]);
  assert.deepEqual(bins[1]!.members.map(member => member.timestamp_ns), ['350000000', '220000000']);
  assert.equal(bins[2]!.start + bins[2]!.duration, 450_000_000n);
});

test('a replaced indexed window invalidates the source catalog without combining rank copies', () => {
  const report = storageFixture(), domain = report.shards[0]!.domain;
  assert.equal(storageGraphs(report, domain)[0]!.leaves[0]!.id, 'block-a');
  report.counters = report.counters.filter(counter => counter.track !== 'block-a');
  // The new indexed window observes only the alternate rank/namespace source.
  indexReport(report);
  assert.equal(storageGraphs(report, domain)[0]!.leaves[0]!.id, 'block-copy');
  assert.equal(storageGraphs(report, domain)[0]!.leaves.length, 3);
});
