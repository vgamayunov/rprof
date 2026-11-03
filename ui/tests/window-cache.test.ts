import { test } from 'node:test';
import assert from 'node:assert/strict';
import { WindowCache, emptyScopes, windowIdentity } from '../src/window-cache.ts';
import type { WindowRequest, WindowResponse } from '../src/window-cache.ts';

const request: WindowRequest = { snapshot: 'capture', domain: '0', start_ns: '9007199254740993', end_ns: '9007199254749993',
  pixels: 800, filesystem: 'all', ...emptyScopes() };
const reply = (overrides: Partial<WindowResponse> = {}): WindowResponse => ({ snapshot: request.snapshot, domain: request.domain,
  start_ns: request.start_ns, end_ns: request.end_ns, spans: [], counters: [], dependencies: [], io: {}, limited: false, warnings: [], ...overrides });

test('scroll scopes share a window but zoom, resolution, source, domain and filesystem do not', () => {
  assert.equal(windowIdentity(request), windowIdentity({ ...request, tracks: ['offscreen'], systems: ['cpu'], io_scopes: ['io'] }));
  for (const change of [{ start_ns: '9007199254740994' }, { end_ns: '9007199254759993' }, { pixels: 801 },
    { snapshot: 'new capture' }, { domain: '1' }, { filesystem: 'nfs' }])
    assert.notEqual(windowIdentity(request), windowIdentity({ ...request, ...change }));
});

test('empty and limited replies retain independent raw, counter and I/O scope coverage', () => {
  const cache = new WindowCache();
  cache.add({ ...emptyScopes(), tracks: ['a'], io_scopes: ['total'] }, reply({ io: { total: [] } }));
  cache.add({ ...emptyScopes(), systems: ['a', 'cpu'] }, reply({ limited: true, warnings: ['sample limit'] }));
  assert.deepEqual(cache.missing({ tracks: ['a', 'b'], systems: ['a', 'cpu', 'b'], io_scopes: ['total', 'thread'] }),
    { tracks: ['b'], systems: ['b'], io_scopes: ['thread'] });
  assert.equal(cache.merged()!.limited, true);
  assert.deepEqual(cache.merged()!.warnings, ['sample limit']);
});

test('incremental batches merge without duplicate observations or dependencies and prefer counter LOD', () => {
  const cache = new WindowCache();
  const counter = { track: 'a', name: 'cpu', timestamp_ns: request.start_ns, value: '9007199254740993',
    value_type: 'integer' as const, unit: 'count', attributes: {} };
  const dependency = { from: 'one', to: 'two', relation: 'launches' };
  const span = { track: 'a', id: 1, source_id: 'source', name: 'work', category: 'compute', start_ns: request.start_ns,
    duration_ns: '1', correlation: 'one', incomplete: false, attributes: {} };
  cache.add({ ...emptyScopes(), tracks: ['a'] }, reply({ counters: [counter], spans: [span], dependencies: [dependency] }));
  const lod = { ...counter, attributes: { 'viewer.resolution_ns': '1000', 'system.report_gap_before': 'true' } };
  cache.add({ ...emptyScopes(), systems: ['a'], tracks: ['b'] }, reply({ counters: [lod], spans: [span], dependencies: [dependency] }));
  assert.deepEqual(cache.merged()!.counters, [lod]);
  assert.deepEqual(cache.merged()!.spans, [span]);
  assert.deepEqual(cache.merged()!.dependencies, [dependency]);
  assert.strictEqual(cache.merged(), cache.merged(), 'scrolling must not rebuild merged data');
});

test('cache eviction respects row, payload and observation budgets while retaining a usable latest reply', () => {
  for (const cache of [new WindowCache(1), new WindowCache(1e6, 1), new WindowCache(1e6, 100, 1)]) {
    const result = reply({ dependencies: [{ from: 'a', to: 'b', relation: 'test' }] });
    cache.add({ ...emptyScopes(), tracks: ['a'] }, result);
    cache.add({ ...emptyScopes(), tracks: ['b'] }, result);
    assert.equal(cache.evicted, true);
    assert.deepEqual(cache.missing({ ...emptyScopes(), tracks: ['a', 'b'] }).tracks, ['a']);
    assert.ok(cache.merged());
  }
});
