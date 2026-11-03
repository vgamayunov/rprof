import type { Report, StorageBackend } from '../src/model.ts';
import { ioFixture } from './io-fixture.ts';

export function storageFixture(): Report {
  const report = ioFixture();
  const shard = report.shards[0]!;
  const domain = shard.domain;
  const add = (id: string, backend: StorageBackend, filesystem: string, host: string, resource: string, read: number, ops: number, latency: number, write = true): void => {
    report.tracks.push({ id, shard: shard.id, parent: null, rank: null, kind: 'storage', name: resource,
      details: { 'storage.backend': backend, 'storage.host_id': host, 'storage.filesystem': filesystem,
        'storage.mount_point': '/data', resource, 'storage.scope': 'host-wide device/client RPC' } });
    for (const [index, time] of [1, 2, 4].entries()) for (const [direction, enabled] of [['read', true], ['write', write]] as const) {
      if (!enabled || index === 1 && id === 'block-b') continue;
      const attrs = { 'storage.backend': backend, 'storage.scope': backend === 'block' ? 'host whole-device counters' : 'client RPC',
        'storage.interval_ns': '1000000000', 'storage.window_ns': '1000000000', 'storage.sample_index': String(time),
        [`storage.${direction}_ops`]: String(ops), [`storage.${direction}_latency_ns`]: String(latency * 1_000_000 * ops),
        [`storage.${direction}_bytes`]: String(read * 1048576), 'storage.metadata_rpc_ops': '7' };
      for (const [name, value, unit] of [['bytes_per_second', String(read * 1048576), 'B/s'],
        ['operations_per_second', String(ops), 'ops/s'], ['latency_ms', String(latency), 'ms']] as const)
        report.counters.push({ track: id, name: `storage.${direction}.${name}`, timestamp_ns: String(time * 1e9),
          value, value_type: 'float', unit, attributes: attrs });
    }
  };
  add('block-a', 'block', 'ext4 /data', 'host-a', 'disk-1', 1, 1, 10);
  add('block-b', 'block', 'ext4 /data', 'host-b', 'disk-2', 2, 9, 2, false);
  add('block-copy', 'block', 'ext4 /data', 'host-a', 'disk-1', 999, 1, 10);
  // The repeated namespace source has fewer observations; it must never double count.
  report.counters = report.counters.filter(counter => counter.track !== 'block-copy' || counter.timestamp_ns === '1000000000');
  add('block-shared', 'block', 'shared device (unmapped)', 'host-a', 'disk-3', 3, 1, 1);
  add('lustre', 'lustre', 'lustrefs', 'host-a', 'OSC aggregate', 4, 2, 3);
  add('nfs', 'nfs', 'nfs4 server:/export', 'host-a', 'server:/export', 5, 2, 4);
  // A second domain has no storage; never merge it into the first.
  report.domains.find(item => item.id === domain)!.end_ns = '5000000000';
  return report;
}
