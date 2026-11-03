import type { IoFilesystem, IoThreadSeries, IoTimeBin, Report } from '../src/model.ts';

export function ioFixture(fileCount = 3): Report {
  const bin = (bytes: string): IoTimeBin => ({ start_ns: '0', duration_ns: '1000000000', read_bytes: bytes,
    write_bytes: '0', read_ops: bytes === '0' ? '0' : '1', write_ops: '0', other_ops: '0', errors: '0',
    read_latency_ns: bytes === '0' ? '0' : '1000000', write_latency_ns: '0', other_latency_ns: '0', max_latency_ns: bytes === '0' ? '0' : '1000000' });
  const bins = (bytes: string) => [bin(bytes), { ...bin('0'), start_ns: '1000000000' }];
  const shards = ['alpha', 'beta', 'gamma'].map((hostname, index) => ({ id: `s${index}`, domain: index === 2 ? '1' : '0',
    name: `shard ${index}`, hostname, command: ['./app'], collectors: ['io'], origin_ns: '0', end_ns: '2000000000',
    complete: true, exit_code: 0, signal: null, interrupted_by: null, end_reason: 'Completed', dropped: '0' }));
  const threads: IoThreadSeries[] = [
    ['f0', 's0', 0, '101', '10', 's0:t0', '10'],
    ['f2', 's0', 0, '101', '10', 's0:t0', '20'],
    ['f0', 's0', 0, '101', '11', 's0:t1', '30'],
    ['f0', 's0', 0, '202', '10', 's0:t2', '5'],
    ['f1', 's1', 1, '101', '10', 's1:t0', '40'],
    ['f3', 's2', 0, '101', '10', 's2:t0', '999'],
  ].map(([filesystem_id, shard, rank, process_instance, tid, thread_track, bytes]) => ({
    filesystem_id: String(filesystem_id), domain: shard === 's2' ? '1' : '0', shard: String(shard), rank: Number(rank),
    process_instance: String(process_instance), pid: '10', tid: String(tid), thread_track: String(thread_track),
    bin_width_ns: '1000000000', bins: bins(String(bytes)),
  }));
  const filesystems: IoFilesystem[] = [
    { id: 'f0', host: 'alpha', domain: '0', mount_point: '/data' },
    { id: 'f1', host: 'beta', domain: '0', mount_point: '/data' },
    { id: 'f2', host: 'alpha', domain: '0', mount_point: '/scratch' },
    { id: 'f3', host: 'gamma', domain: '1', mount_point: '/foreign' },
  ].map(fs => {
    const sources = threads.filter(t => t.filesystem_id === fs.id);
    const samples = bins(sources.reduce((sum, t) => sum + BigInt(t.bins[0]!.read_bytes), 0n).toString());
    samples[0]!.read_ops = String(sources.length);
    samples[0]!.read_latency_ns = String(sources.length * 1000000);
    return { ...fs, filesystem_type: 'ext4', device: '8:1', kind: 'filesystem', bin_width_ns: '1000000000', bins: samples };
  });
  const report: Report = { version: 1, provenance: { kind: 'synthetic', label: 'I/O hierarchy fixture', source: '' },
    summary: { format: 'fixture', session_id: 'test', complete: true, truncated_tail: false, recovered_inputs: '0', tracks: '0', spans: '0',
      counters: '0', dependencies: '0', dropped: '0', omitted_events: '0', warnings: [], categories: [] },
    domains: [{ id: '0', name: 'aligned', origin_ns: '0', end_ns: '2000000000' }, { id: '1', name: 'other', origin_ns: '0', end_ns: '2000000000' }],
    shards, tracks: [], spans: [], counters: [], dependencies: [], io: [],
    io_timeline: { filesystems, threads, omitted_events: '0', scope_omitted_events: '0' },
  };
  for (const thread of threads) {
    if (report.tracks.some(track => track.id === thread.thread_track)) continue;
    const process = `${thread.shard}:process:${thread.process_instance}`;
    if (!report.tracks.some(track => track.id === process)) report.tracks.push({ id: process, shard: thread.shard, parent: null,
      name: 'process 10', kind: 'process', rank: thread.rank, details: { pid: '10' } });
    report.tracks.push({ id: thread.thread_track, shard: thread.shard, parent: process,
      name: `thread ${thread.tid}`, kind: 'thread', rank: thread.rank, details: { tid: thread.tid } });
    const count = thread.thread_track === 's0:t0' ? fileCount : 1;
    for (let i = 0; i < count; i++) {
      const id = `${thread.thread_track}:file:${i}`;
      report.tracks.push({ id, shard: thread.shard, parent: thread.thread_track,
        name: `/data/file-${i}`, kind: 'io', rank: thread.rank, details: { resource: `/data/file-${i}` } });
      if (i === 0) report.spans.push({ id: report.spans.length, track: id, name: 'pread', category: 'io', start_ns: '250000000', duration_ns: '10000000',
        correlation: null, incomplete: false, attributes: { 'io.completed_bytes': '10', 'io.file': '/data/file-0' } });
    }
  }
  return report;
}
