import test from 'node:test';
import assert from 'node:assert/strict';
import { aggregateStacks } from '../src/stacks.ts';
import type { StackMetric, StackQuery } from '../src/stacks.ts';
import { stackFixture } from './stack-fixture.ts';

function query(metric: StackMetric, start = '0', end = '1000'): StackQuery {
  return { snapshot: 'fixture', domain: 'd', metric, start_ns: start, end_ns: end,
    orientation: 'top-down', group: 'combined', filter: '' };
}
test('both function orientations preserve recursion, missing stacks and exact CPU weights above 2^53', () => {
  const report = stackFixture(), data = report.stacks!;
  data.facts[0]!.weight = '9007199254740993'; data.facts[1]!.weight = '9007199254740994';
  const q = query('cpu');
  const down = aggregateStacks(data, report.tracks, q);
  assert.equal(down.total, '18014398509481999');
  assert.equal(down.observations, '4'); assert.equal(down.unattributed, '5');
  assert.equal(down.nodes.filter(n => n.frame.key === 'work').length, 4);
  assert.equal(down.nodes.filter(n => n.parent === null).reduce((sum, n) => sum + BigInt(n.total), 0n).toString(), down.total);
  const up = aggregateStacks(data, report.tracks, { ...q, orientation: 'bottom-up' });
  assert.equal(up.total, down.total);
  assert.equal(up.nodes.find(n => n.parent === null && n.frame.key === 'work')!.self_weight, '18014398509481994');
});
test('function range queries clip duration but retain discrete bytes and real launch attribution outside the range', () => {
  const report = stackFixture();
  const run = (metric: StackMetric, start: string, end: string) => aggregateStacks(report.stacks!, report.tracks, query(metric, start, end));
  assert.equal(run('io_time', '75', '125').total, '50');
  assert.equal(run('io_time', '75', '125').observations, '0');
  assert.equal(run('io_bytes', '75', '125').total, '0');
  assert.equal(run('io_bytes', '150', '151').total, '9007199254740993');
  assert.equal(run('io_time', '150', '151').total, '0');
  assert.equal(run('io_time', '150', '151').observations, '1');
  assert.equal(run('cpu', '20', '30').total, '100');
  const gpu = run('gpu_time', '130', '150');
  assert.equal(gpu.total, '20'); assert.equal(gpu.unattributed, '0');
  assert.ok(gpu.nodes.some(n => n.frame.name === 'train_step'));
  assert.equal(aggregateStacks(report.stacks!, report.tracks, { ...query('gpu_time', '130', '150'), filter: 'cuda' }).total, '20');
  assert.equal(run('cuda_api', '130', '150').total, '0');
});
test('function scope grouping and incomplete/ambiguous attribution never fabricate complete totals', () => {
  const report = stackFixture(), data = report.stacks!;
  const q = { ...query('io_time'), group: 'thread' as const, filter: 'rank:0 thread 2' };
  const tree = aggregateStacks(data, report.tracks, q);
  assert.equal(tree.nodes[0]!.frame.key, 'track:s:t');
  assert.equal(aggregateStacks(data, report.tracks, { ...q, filter: 'rank:1' }).total, '0');
  data.sources['host-launch']!.push('compute');
  assert.equal(aggregateStacks(data, report.tracks, query('gpu_time')).unattributed, '40');
  data.complete = false;
  assert.throws(() => aggregateStacks(data, report.tracks, q), /Complete stack filtering data is unavailable/);
});
