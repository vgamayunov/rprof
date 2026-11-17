import type { Report } from '../src/model.ts';

export function gpuFixture(): Report {
  const report: Report = {
    version: 1, provenance: { kind: 'synthetic', label: 'GPU gauge fixture', source: '' },
    summary: { format: '', session_id: '', complete: true, truncated_tail: false, recovered_inputs: '0',
      tracks: '0', spans: '0', counters: '0', dependencies: '0', dropped: '0', omitted_events: '0', warnings: [], categories: [] },
    domains: ['aligned', 'other'].map(id => ({ id, name: id, origin_ns: '0', end_ns: '9000000000' })),
    shards: ['alpha', 'beta', 'duplicate', 'foreign'].map(id => ({ id, domain: id === 'foreign' ? 'other' : 'aligned',
      name: id, hostname: id === 'duplicate' ? 'alpha' : id, command: [], collectors: ['gpu'], origin_ns: '0', end_ns: '9000000000',
      complete: true, exit_code: 0, signal: null, interrupted_by: null, end_reason: '', dropped: '0' })),
    tracks: [], spans: [], counters: [], dependencies: [],
  };
  const bytes = 9007199254740993n;
  for (const shard of report.shards) {
    const count = shard.id === 'beta' ? 2 : 1;
    for (const resource of ['gpu', 'gpu_memory'] as const) {
      const memory = resource === 'gpu_memory', root = `${shard.id}:${resource}`;
      report.tracks.push({ id: root, shard: shard.id, parent: null, name: resource, kind: 'system', rank: null,
        details: { resource, 'system.host_id': `${shard.hostname}:boot` } });
      for (let device = 0; device < count; device++) report.tracks.push({
        id: `${root}:${device}`, shard: shard.id, parent: root, name: `GPU ${device} · NVIDIA test`,
        kind: 'system', rank: null, details: { resource: `${resource}:GPU-${shard.hostname}-${device}` },
      });
      for (const time of shard.id === 'beta' ? [1, 2, 4] : [1, 2, 3, 4]) {
        for (let device = -1; device < count; device++) {
          const contributors = device < 0 ? count : 1;
          report.counters.push({ track: device < 0 ? root : `${root}:${device}`,
            name: memory ? 'system.gpu.memory.used' : 'system.gpu.utilization',
            timestamp_ns: `${time}000000000`, value: memory ? (bytes * BigInt(contributors)).toString() : shard.id === 'beta' ? '80' : '20',
            value_type: memory ? 'integer' : 'float', unit: memory ? 'bytes' : '%', attributes: {
              'system.interval_ns': '1000000000', 'system.sample_index': String(time),
              'system.gpu.count': String(contributors), 'system.scope': 'NVML device-wide; includes other processes',
              ...(memory ? { 'system.memory.total_bytes': (bytes * 2n * BigInt(contributors)).toString() } : {}),
            } });
        }
      }
    }
  }
  return report;
}
