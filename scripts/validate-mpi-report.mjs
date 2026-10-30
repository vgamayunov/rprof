// Validate a generated HTML report against the native two-rank fixture contract.
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
const [path, mode = 'basic'] = process.argv.slice(2);
if (!path) throw new Error('usage: node scripts/validate-mpi-report.mjs REPORT.html basic|matrix|threads|combined');
const data = readFileSync(path, 'utf8').match(/<script type="application\/json" id="rprof-data">([\s\S]*?)<\/script>/);
assert.ok(data, 'embedded report model missing');
const report = JSON.parse(data[1]);
assert.equal(report.summary.complete, true);
assert.equal(report.summary.dropped, '0');
assert.equal(report.summary.omitted_events, '0');
assert.equal(report.shards.length, 2);
assert.ok(report.shards.every(s => s.exit_code === 0));
assert.deepEqual([...new Set(report.tracks.map(t => t.rank).filter(r => r !== null))].sort(), [0, 1]);
assert.equal(report.domains.length, 1);
assert.equal(report.domains[0].calibration.complete, true);
const calls = report.spans.filter(s => s.attributes['mpi.record_type'] === 'call');
const requests = report.spans.filter(s => s.attributes['mpi.record_type'] === 'request');
const names = new Set(calls.map(s => s.name));
for (const name of ['MPI_Init_thread', 'MPI_Finalize', 'MPI_Isend', 'MPI_Irecv', 'MPI_Waitall']) assert.ok(names.has(name), name);
assert.ok(requests.every(s => s.attributes['mpi.request_identity_ambiguous'] !== 'true'));
if (mode === 'basic' || mode === 'combined') {
  assert.equal(requests.length, 9);
  assert.ok(requests.some(s => s.name.startsWith('MPI_Irecv') && s.attributes['mpi.count'] === '4' && s.attributes['mpi.bytes'] === '4'));
  assert.ok(requests.some(s => s.attributes['mpi.request_state'] === 'cancelled' && s.attributes['mpi.bytes'] === '0'));
  assert.ok(requests.some(s => s.attributes['mpi.request_state'] === 'tracking_released' && s.incomplete));
}
if (mode === 'matrix') {
  for (const name of ['MPI_Waitany', 'MPI_Waitsome', 'MPI_Testall', 'MPI_Testany', 'MPI_Testsome', 'MPI_Sendrecv']) assert.ok(names.has(name), name);
  assert.ok(requests.some(s => s.attributes['mpi.request_state'] === 'completed_with_error'));
  assert.ok(requests.every(s => !s.incomplete));
}
if (mode === 'threads') {
  assert.equal(requests.length, 512);
  assert.ok(calls.filter(s => s.name === 'MPI_Init_thread').every(s => s.attributes['mpi.thread_level'] === '3'));
  for (const rank of ['0', '1']) assert.equal(new Set(calls.filter(s => s.name === 'MPI_Isend' && s.attributes['mpi.rank'] === rank).map(s => s.attributes.tid)).size, 2);
  assert.ok(requests.every(s => !s.incomplete));
}
if (mode === 'combined') assert.ok(report.io.length > 0 && report.spans.some(s => s.category === 'io'));
console.log(`${mode}: ${calls.length} MPI calls, ${requests.length} request lifetimes, two ranks, calibrated clocks, zero drops`);
