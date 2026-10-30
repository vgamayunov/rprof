import { test, expect, type Page } from '@playwright/test';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { COUNTER_PLOT_HEIGHT, COUNTER_PLOT_TOP, indexReport, integerPlotValues, SPAN_PITCH, SPAN_TOP, timelineRows } from '../../src/model.ts';
import type { Report } from '../../src/model.ts';

const root = resolve(fileURLToPath(new URL('../../..', import.meta.url)));
const binary = join(root, 'target/debug/rprof');
const fixture = join(root, 'fixtures/multitrack.jsonl');
let directory: string;
let html: string;

function render(input: string, name: string, args: string[] = []): string {
  const output = join(directory, `${name}.html`);
  execFileSync(binary, ['report', input, '--format', 'html', '-o', output, ...args]);
  return pathToFileURL(output).href;
}

function injected(name: string, change: (report: any) => void): string {
  const output = join(directory, `${name}.html`);
  const source = readFileSync(join(directory, 'multitrack.html'), 'utf8');
  const match = source.match(/(<script type="application\/json" id="rprof-data">)([\s\S]*?)(<\/script>)/);
  if (!match) throw new Error('Missing embedded report');
  const report = JSON.parse(match[2]!);
  change(report);
  // Match the native HTML encoder: never allow user data to close a script tag.
  const payload = JSON.stringify(report).replaceAll('<', '\\u003c').replaceAll('>', '\\u003e').replaceAll('&', '\\u0026');
  writeFileSync(output, source.replace(match[0], `${match[1]}${payload}${match[3]}`));
  return pathToFileURL(output).href;
}

test.beforeAll(() => {
  execFileSync('cargo', ['build', '-p', 'rprof-cli'], { cwd: root });
  directory = mkdtempSync(join(tmpdir(), 'rprof-browser-'));
  html = render(fixture, 'multitrack');
});

test.afterAll(() => rmSync(directory, { recursive: true, force: true }));

async function loaded(page: Page, url = html): Promise<void> {
  await page.goto(url);
  await expect(page.locator('#timeline-canvas')).toBeVisible();
  await expect(page.locator('#range-label')).toContainText('domain-relative');
}

async function position(page: Page, name: string, kind: 'span' | 'counter', rank?: number, ns?: number): Promise<{ x: number; y: number }> {
  const report: Report = await page.locator('#rprof-data').evaluate(element => JSON.parse(element.textContent!));
  const domainId = await page.locator('#domain-select').inputValue();
  const query = await page.locator('#filter-input').inputValue();
  const data = indexReport(report);
  const rows = timelineRows(report, data, domainId, query);
  const track = report.tracks.find(track => report.shards.some(shard => shard.id === track.shard && shard.domain === domainId) &&
    (rank === undefined || track.rank === rank) &&
    (kind === 'span' ? report.spans.some(span => span.track === track.id && span.name === name) :
      report.counters.some(counter => counter.track === track.id && counter.name === name)));
  if (!track) throw new Error(`Missing ${kind} ${name}`);
  const row = rows.find(row => row.track?.id === track.id && (kind === 'span' ? row.kind === 'track' : row.kind === 'counter' && row.series?.startsWith(`${name} [`)));
  if (!row) throw new Error(`Missing visible row for ${name}`);
  const scrollTop = await page.locator('#timeline-scroll').evaluate(element => element.scrollTop);
  let time: bigint;
  let y: number;
  if (kind === 'span') {
    const span = report.spans.find(span => span.track === track.id && span.name === name)!;
    const indexed = data.get(track.id)!.spans.items.find(item => item.span.id === span.id)!;
    time = ns === undefined ? indexed.start + (indexed.end - indexed.start) / 2n : BigInt(ns);
    y = row.top - scrollTop + SPAN_TOP + indexed.lane * SPAN_PITCH + 5;
  } else {
    const samples = data.get(track.id)!.counters.get(row.series!)!;
    const sample = ns === undefined ? samples[0]! : samples.find(sample => sample.time === BigInt(ns))!;
    const values = samples[0]!.counter.value_type === 'integer' ? integerPlotValues(samples.map(sample => sample.counter.value)) : samples.map(sample => Number(sample.counter.value));
    const value = values[samples.indexOf(sample)]!;
    time = sample.time;
    y = row.top - scrollTop + COUNTER_PLOT_TOP + COUNTER_PLOT_HEIGHT * (1 - (value - Math.min(...values)) / (Math.max(...values) - Math.min(...values) || 1));
  }
  const canvas = page.locator('#timeline-canvas');
  const box = (await canvas.boundingBox())!;
  const extent = Number(report.domains.find(domain => domain.id === domainId)!.end_ns);
  return { x: 260 + (box.width - 260) * Number(time) / extent, y };
}

async function clickEvent(page: Page, name: string, kind: 'span' | 'counter' = 'span', rank?: number, ns?: number): Promise<void> {
  await page.locator('#timeline-canvas').click({ position: await position(page, name, kind, rank, ns) });
}

async function canvasCorner(page: Page): Promise<number[]> {
  return page.locator('#timeline-canvas').evaluate(element =>
    Array.from((element as HTMLCanvasElement).getContext('2d')!.getImageData(1, 1, 1, 1).data));
}

test('offline theme toggle redraws the canvas, preserves the view, and remembers the choice', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await loaded(page);
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await expect.poll(() => canvasCorner(page)).toEqual([30, 51, 68, 255]);
  await page.locator('#filter-input').fill('rank:0 stencil');
  await clickEvent(page, 'stencil kernel', 'span', 0);
  const detail = await page.locator('#detail-content').textContent();
  const beforeZoom = await page.locator('#range-label').textContent();
  await page.locator('#zoom-in').click();
  await expect(page.locator('#range-label')).not.toHaveText(beforeZoom!);
  const range = await page.locator('#range-label').textContent();
  await page.getByRole('button', { name: 'Switch to light theme' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.locator('.topbar')).toHaveCSS('background-color', 'rgb(255, 255, 255)');
  await expect.poll(() => canvasCorner(page)).toEqual([221, 232, 240, 255]);
  await expect(page.locator('#detail-content')).toHaveText(detail!);
  await expect(page.locator('#filter-input')).toHaveValue('rank:0 stencil');
  await expect(page.locator('#range-label')).toHaveText(range!);
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.getByRole('button', { name: 'Switch to dark theme' }).click();
  await expect.poll(() => canvasCorner(page)).toEqual([30, 51, 68, 255]);
});

test('theme follows the system initially and works when browser storage is blocked', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.emulateMedia({ colorScheme: 'light' });
  await page.addInitScript(() => {
    Object.defineProperty(window, 'localStorage', { get() { throw new DOMException('Storage blocked', 'SecurityError'); } });
  });
  await loaded(page);
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.getByRole('button', { name: 'Switch to dark theme' }).click();
  await expect.poll(() => canvasCorner(page)).toEqual([30, 51, 68, 255]);
  expect(errors).toEqual([]);
});

test('the local HTTP viewer shares the persistent theme toggle', async ({ page }) => {
  const server = spawn(binary, ['view', fixture], { stdio: ['ignore', 'pipe', 'ignore'] });
  try {
    const url = await new Promise<string>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Viewer did not start')), 10000);
      let output = '';
      server.stdout!.on('data', chunk => {
        output += chunk.toString();
        const match = output.match(/Viewer: (http:\/\/127\.0\.0\.1:\d+\/)/);
        if (match) { clearTimeout(timer); resolve(match[1]!); }
      });
      server.once('error', error => { clearTimeout(timer); reject(error); });
      server.once('exit', code => { clearTimeout(timer); reject(new Error(`Viewer exited: ${code}`)); });
    });
    await page.emulateMedia({ colorScheme: 'light' });
    await loaded(page, url);
    await page.getByRole('button', { name: 'Switch to dark theme' }).click();
    await expect.poll(() => canvasCorner(page)).toEqual([30, 51, 68, 255]);
    await page.reload();
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  } finally {
    if (server.exitCode === null && server.signalCode === null) {
      const stopped = once(server, 'exit');
      server.kill('SIGTERM');
      await stopped;
    }
  }
});

test('offline timeline renders, filters, inspects, zooms, pans, and separates clocks', async ({ page }) => {
  const errors: string[] = [];
  const requests: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('request', request => { if (/^https?:/.test(request.url())) requests.push(request.url()); });
  await page.context().setOffline(true);
  await loaded(page);
  await expect(page.locator('#header-meta')).toContainText('SYNTHETIC');
  await expect(page.locator('#domain-select option')).toHaveCount(2);
  await expect(page.locator('#category-table')).toContainText('openmp');
  await expect(page.locator('#capture-status')).toContainText('endpoints missing');
  await clickEvent(page, 'solver step', 'span', undefined, 10_000_000);
  await expect(page.locator('#detail-content')).toContainText('solver step');
  await expect(page.locator('#detail-content')).toContainText('40000000 ns');
  const initial = await page.locator('#range-label').textContent();
  await page.locator('#zoom-in').click();
  await expect(page.locator('#range-label')).not.toHaveText(initial!);
  const zoomed = await page.locator('#range-label').textContent();
  await page.locator('#timeline-scroll').focus();
  await page.keyboard.press('ArrowRight');
  await expect(page.locator('#range-label')).not.toHaveText(zoomed!);
  await page.locator('#fit-view').click();
  await page.locator('#filter-input').fill('rank:0 stencil');
  await clickEvent(page, 'stencil kernel', 'span', 0, 10_000_000);
  await expect(page.locator('#detail-content')).toContainText('stencil kernel');
  await expect(page.locator('#detail-content')).toContainText('8000000 ns');
  await expect(page.locator('#detail-content')).toContainText('checkpoint after kernel');
  await page.locator('#filter-input').fill('');
  await page.locator('#domain-select').selectOption('1');
  await clickEvent(page, 'solver step', 'span', undefined, 10_000_000);
  await expect(page.locator('#detail-content')).toContainText('00000000-0000-0000-0000-000000000003:0');
  // Compact captures may fit without scrolling; use enough empty tracks to exercise overflow.
  await loaded(page, injected('many-empty-tracks', report => {
    const process = report.tracks.find((track: any) => track.rank === 0 && track.parent === null);
    report.tracks.push(...Array.from({ length: 40 }, (_, i) => ({ ...process, id: `empty-${i}`, name: `empty ${i}` })));
  }));
  await page.locator('#timeline-scroll').evaluate(element => { element.scrollTop = 200; });
  await expect.poll(() => page.locator('#timeline-scroll').evaluate(element => element.scrollTop)).toBeGreaterThan(0);
  expect(errors).toEqual([]);
  expect(requests).toEqual([]);
});

test('integer counters above 2^53 remain distinct and inspectable', async ({ page }) => {
  await loaded(page);
  await page.locator('#filter-input').fill('cycles rank:0');
  const canvas = page.locator('#timeline-canvas');
  await canvas.hover({ position: await position(page, 'cycles', 'counter', 0, 2_000_000) });
  await expect(page.locator('#timeline-tooltip')).toHaveText('cycles');
  await clickEvent(page, 'cycles', 'counter', 0, 2_000_000);
  await expect(page.locator('#detail-content')).toContainText('9007199254740993');
  await clickEvent(page, 'cycles', 'counter', 0, 3_000_000);
  await expect(page.locator('#detail-content')).toContainText('9007199254740994');
});

test('hostile strings remain text and retention/partial status is visible', async ({ page }) => {
  const source = readFileSync(fixture, 'utf8');
  const hostile = '</script><script>globalThis.rprofInjected=true</script><img src=x onerror="globalThis.rprofInjected=true">';
  const input = join(directory, 'hostile.jsonl');
  writeFileSync(input, source.replaceAll('"solver step"', JSON.stringify(hostile)));
  await loaded(page, render(input, 'hostile'));
  await clickEvent(page, hostile, 'span', undefined, 10_000_000);
  await expect(page.locator('#detail-content h3')).toHaveText(hostile);
  expect(await page.evaluate(() => 'rprofInjected' in globalThis)).toBe(false);
  expect(await page.locator('img').count()).toBe(0);
  const partial = join(directory, 'partial.jsonl');
  writeFileSync(partial, source.trimEnd().split('\n').slice(0, -1).join('\n') + '\n');
  await loaded(page, render(partial, 'partial', ['--max-events', '2']));
  await expect(page.locator('#capture-status')).toContainText('Partial capture');
  await expect(page.locator('#capture-status')).toContainText('omitted');
});

test('empty capture still opens and reports its status', async ({ page }) => {
  const input = join(directory, 'empty.jsonl');
  writeFileSync(input, readFileSync(fixture, 'utf8').split('\n')[0] + '\n');
  await page.goto(render(input, 'empty'));
  await expect(page.locator('#timeline-canvas')).toBeVisible();
  await expect(page.locator('#domain-select')).toBeDisabled();
  await expect(page.locator('#range-label')).toHaveText('No clock domain');
  await expect(page.locator('#capture-status')).toContainText('Partial capture');
});

test('MPI call names survive filtering and narrow timeline bars have hover titles and inspector detail', async ({ page }) => {
  const url = injected('mpi-calls', report => {
    report.domains = [report.domains[0]];
    report.shards = [report.shards[0], { ...report.shards[0], id: 'rank-one', name: 'rank one' }];
    const process = report.tracks.find((track: any) => track.shard === report.shards[0].id && track.rank === 0 && track.parent === null);
    report.tracks = [process, { ...process, id: 'rank-one-process', shard: 'rank-one', rank: 1 }];
    report.spans = [
      { id: 10000, track: process.id, name: 'MPI_Send', category: 'mpi', start_ns: '2000000', duration_ns: '100000', correlation: null, incomplete: false, attributes: {} },
      { id: 10001, track: 'rank-one-process', name: 'MPI_Recv', category: 'mpi', start_ns: '2000000', duration_ns: '100000', correlation: null, incomplete: false, attributes: {} },
      { id: 10002, track: process.id, name: 'MPI_Waitall', category: 'mpi', start_ns: '9000000', duration_ns: '100000', correlation: null, incomplete: false, attributes: {} },
    ];
    report.counters = [];
    report.dependencies = [];
  });
  await loaded(page, url);
  await expect(page.locator('.mpi-key')).toHaveCSS('background-color', 'rgb(255, 153, 112)');
  await page.locator('#filter-input').fill('MPI_Send rank:0');
  const canvas = page.locator('#timeline-canvas');
  const send = await position(page, 'MPI_Send', 'span', 0);
  await canvas.hover({ position: send });
  await expect(page.locator('#timeline-tooltip')).toHaveText('MPI_Send');
  await expect(canvas).toHaveAttribute('title', 'MPI_Send');
  await canvas.click({ position: send });
  await expect(page.locator('#detail-content h3')).toHaveText('MPI_Send');
  await page.locator('#filter-input').fill('MPI_Recv rank:1');
  const recv = await position(page, 'MPI_Recv', 'span', 1);
  await canvas.hover({ position: recv });
  await expect(page.locator('#timeline-tooltip')).toHaveText('MPI_Recv');
  await canvas.click({ position: recv });
  await expect(page.locator('#detail-content h3')).toHaveText('MPI_Recv');
  await page.locator('#filter-input').fill('MPI_Waitall rank:0');
  await clickEvent(page, 'MPI_Waitall', 'span', 0);
  await expect(page.locator('#detail-content h3')).toHaveText('MPI_Waitall');
  await page.locator('#filter-input').fill('MPI_Send rank:1');
  await expect(page.locator('#timeline-canvas')).toBeVisible();
  await expect(page.locator('#timeline-tooltip')).toBeHidden();
});

test('I/O panel paginates exact values, shows per-operation latency, escapes hostile names and switches domains', async ({ page }) => {
  const hostile = '<img src=x onerror="globalThis.rprofInjected=true">';
  const url = injected('io', report => {
    const file = (domain: string, name: string) => ({
      domain, host: 'host-0', file: name, operations: '9007199254740993', errors: '2',
      read_bytes: '9007199254740993', write_bytes: '5', requested_read_bytes: '9007199254740995',
      requested_write_bytes: '7', total_ns: '11', window_ns: '2000000000',
      by_operation: [{ name: 'read', count: '2', errors: '1', total_ns: '5', max_ns: '4' },
        { name: 'write', count: '0', errors: '1', total_ns: '0', max_ns: '0' }],
    });
    report.io = [file('0', hostile), ...Array.from({ length: 100 }, (_, i) => file('0', `file-${i}`)), file('1', 'other-domain')];
    report.io[0].by_operation.push(...Array.from({ length: 99 }, (_, i) => ({
      name: `operation-${i}`, count: '1', errors: '0', total_ns: '3', max_ns: '3',
    })));
  });
  await loaded(page, url);
  await expect(page.locator('#io-count')).toHaveText('101 files');
  await expect(page.locator('#io-table tbody tr')).toHaveCount(100);
  await expect(page.locator('#io-table')).not.toContainText('other-domain');
  await expect(page.locator('#io-table')).toContainText('9007199254740993 / 9007199254740995 B');
  await expect(page.locator('#io-table')).toContainText('4503599627370496.500 B/s');
  await page.locator('#io-table .io-file').first().click();
  await expect(page.locator('#io-detail')).toContainText('2.500 ns');
  await expect(page.locator('#io-detail')).toContainText('4 ns');
  await expect(page.locator('#io-detail')).toContainText('—');
  await expect(page.locator('#io-operations tbody tr')).toHaveCount(100);
  await page.locator('#io-operation-pager button').last().click();
  await expect(page.locator('#io-operation-pager')).toContainText('101–101 of 101');
  await expect(page.locator('#io-operations tbody')).toContainText('operation-98');
  expect(await page.evaluate(() => 'rprofInjected' in globalThis)).toBe(false);
  expect(await page.locator('img').count()).toBe(0);
  await page.locator('#io-pager button').last().click();
  await expect(page.locator('#io-pager')).toContainText('101–101 of 101');
  await expect(page.locator('#io-table tbody tr')).toHaveCount(1);
  await page.locator('#domain-select').selectOption('1');
  await expect(page.locator('#io-count')).toHaveText('1 files');
  await expect(page.locator('#io-table')).toContainText('other-domain');
  await expect(page.locator('#io-detail')).toBeEmpty();
});

test('older reports without an io field show an empty I/O panel', async ({ page }) => {
  await loaded(page, injected('old-model', report => { delete report.io; delete report.openmp; }));
  await expect(page.locator('#io-count')).toHaveText('0 files');
  await expect(page.locator('#io-table')).toContainText('No per-file I/O recorded');
  await expect(page.locator('#openmp-panel')).toHaveCount(0);
});

test('OpenMP panel filters by domain, preserves exact numbers and unknowns, and pages every detail table', async ({ page }) => {
  const huge = '9007199254740993';
  const url = injected('openmp-injected', report => {
    const region = (domain: string, parallel_id: string) => ({ shard: 's0', domain, instance: 'image-0', pid: '45', parallel_id,
      requested_threads: huge, observed_threads: '2', duration_ns: null, work_ns: huge, wait_ns: '5',
      work_min_ns: null, work_max_ns: huge, work_spread_ns: null });
    const thread = (domain: string, parallel_id: string, tid: string) => ({ shard: 's0', domain, instance: 'image-0', pid: '45',
      parallel_id, tid, team_index: tid === '0' ? null : huge, work_ns: huge, wait_ns: '7', sync_ns: '8',
      scheduled_ns: huge, execution_segments: '3' });
    const runtime = (domain: string, instance: string) => ({ shard: 's0', domain, instance, pid: '45', version: 'runtime-v1',
      omp_version: '202011', initialized: false, finalized: false, sequence_gaps: '2', unmatched_endpoints: '3', abandoned_intervals: '4',
      callbacks: [{ name: 'parallel_begin', status: 'always', observed: null, observed_complete: false },
        ...Array.from({ length: 100 }, (_, i) => ({ name: `callback-${i}`, status: 'never', observed: huge, observed_complete: true }))] });
    report.openmp = { runtimes: [runtime('0', 'image-0'), ...Array.from({ length: 100 }, (_, i) => runtime('0', `image-${i + 1}`)), runtime('1', 'other-runtime')],
      regions: [region('0', huge), ...Array.from({ length: 100 }, (_, i) => region('0', `region-${i}`)), region('1', 'other-domain')],
      threads: [thread('0', huge, '0'), ...Array.from({ length: 100 }, (_, i) => thread('0', huge, `${i + 1}`)),
        thread('1', huge, 'foreign-domain'), { ...thread('0', huge, 'foreign-image'), instance: 'other-image' }],
      tasks_created: huge, tasks_completed: '12', execution_segments: '13' };
    const track = report.tracks[0];
    report.counters.push(...['openmp.attempted_events', 'openmp.received_events', 'openmp.ingress_dropped_events',
      'openmp.initialized_images', 'openmp.meta.capability.parallel_begin'].map(name =>
      ({ track: track.id, name, timestamp_ns: '1', value: huge, value_type: 'integer', unit: 'count', attributes: {} })));
  });
  await loaded(page, url);
  await expect(page.locator('#openmp-regions tbody tr')).toHaveCount(100);
  await expect(page.locator('#openmp-count')).toHaveText('101 regions');
  await expect(page.locator('#openmp-regions')).toContainText(`${huge} / 2`);
  await expect(page.locator('#openmp-regions')).toContainText(`— / ${huge} ns / —`);
  await expect(page.locator('#openmp-tasks')).toContainText(`${huge} created · 12 completed · 13 execution segments`);
  await expect(page.locator('#openmp-regions')).not.toContainText('other-domain');
  await expect(page.locator('#capture-status')).toContainText('OPENMP');
  await expect(page.locator('#capture-status')).toContainText(`${huge} initialized images`);
  await expect(page.locator('.openmp-key')).toHaveCSS('background-color', 'rgb(213, 162, 255)');
  await page.locator('#openmp-regions .io-file').first().click();
  await expect(page.locator('#openmp-threads tbody tr')).toHaveCount(100);
  await expect(page.locator('#openmp-detail')).toContainText('not CPU utilization');
  await expect(page.locator('#openmp-threads')).toContainText(`${huge} ns`);
  await expect(page.locator('#openmp-threads')).not.toContainText('foreign-domain');
  await page.locator('#openmp-thread-pager button').last().click();
  await expect(page.locator('#openmp-thread-pager')).toContainText('101–101 of 101');
  await page.locator('#openmp-region-pager button').last().click();
  await expect(page.locator('#openmp-region-pager')).toContainText('101–101 of 101');
  await expect(page.locator('#openmp-runtimes')).not.toHaveAttribute('open', '');
  await page.locator('#openmp-runtimes summary').click();
  await expect(page.locator('#openmp-runtime-list .openmp-runtime-row')).toHaveCount(100);
  await expect(page.locator('#openmp-runtime-list')).toContainText('initialized: false · finalized: false');
  await page.locator('#openmp-runtime-list .io-file').first().click();
  await expect(page.locator('#openmp-callbacks tbody tr')).toHaveCount(100);
  await expect(page.locator('#openmp-callbacks tbody tr').first()).toContainText('always');
  await expect(page.locator('#openmp-callbacks tbody tr').first()).toContainText('Unknown');
  await expect(page.locator('#openmp-callbacks')).toContainText(huge);
  await page.locator('#openmp-callback-pager button').last().click();
  await expect(page.locator('#openmp-callback-pager')).toContainText('101–101 of 101');
  await page.locator('#openmp-runtime-pager button').last().click();
  await expect(page.locator('#openmp-runtime-pager')).toContainText('101–101 of 101');
  await page.locator('#domain-select').selectOption('1');
  await expect(page.locator('#openmp-count')).toHaveText('1 regions');
  await expect(page.locator('#openmp-regions')).toContainText('other-domain');
  await expect(page.locator('#openmp-detail')).toBeEmpty();
  await expect(page.locator('#openmp-runtime-count')).toContainText('1 runtimes');
  await expect(page.locator('#openmp-callback-detail')).toBeEmpty();
});

test('unsupported OpenMP capabilities display unknown observations rather than zero', async ({ page }) => {
  const hostile = '<img src=x onerror="globalThis.rprofInjected=true">';
  await loaded(page, injected('openmp-unsupported', report => {
    report.openmp = { regions: [], threads: [], tasks_created: '0', tasks_completed: '0', execution_segments: '0',
      runtimes: [{ shard: 's', domain: '0', instance: 'image', pid: '8', version: 'unknown', omp_version: '0', initialized: false,
        finalized: false, sequence_gaps: '0', unmatched_endpoints: '0', abandoned_intervals: '0',
        callbacks: [{ name: hostile, status: 'unsupported', observed: null, observed_complete: false }] }] };
  }));
  await expect(page.locator('#openmp-regions')).toContainText('No OpenMP regions recorded');
  await page.locator('#openmp-runtimes summary').click();
  await page.locator('#openmp-runtime-list .io-file').click();
  await expect(page.locator('#openmp-callbacks tbody')).toContainText('unsupported');
  await expect(page.locator('#openmp-callbacks tbody')).toContainText('Unknown');
  await expect(page.locator('#openmp-callback-detail')).toContainText('not evidence that a callback was received');
  expect(await page.evaluate(() => 'rprofInjected' in globalThis)).toBe(false);
  expect(await page.locator('img').count()).toBe(0);
});

test('estimated clock alignment is labeled with its uncertainty', async ({ page }) => {
  const url = injected('calibrated-clocks', report => {
    report.domains = [{ ...report.domains[0], calibration: { method: 'min-RTT offset', uncertainty_ns: '32', max_drift_ns: '2', complete: true } }];
    report.summary.warnings = ['Estimated MPI clock alignment'];
  });
  await loaded(page, url);
  await expect(page.locator('#domain-note')).toHaveText('ESTIMATED MPI CLOCK ALIGNMENT');
  await expect(page.locator('#range-label')).toContainText('estimated ±32 ns');
});

test('native I/O collection produces inspectable calls and file totals in the browser', async ({ page }) => {
  execFileSync('make', ['preload'], { cwd: root });
  const workload = join(directory, 'io-workload');
  execFileSync('cc', ['-O2', '-pthread', join(root, 'crates/rprof-cli/tests/fixtures/io_workload.c'), '-o', workload]);
  const data = join(directory, 'native-data');
  const capture = join(directory, 'native.jsonl');
  execFileSync(binary, ['record', '--collect', 'io', '-o', capture, '--', workload, 'basic', data]);
  await loaded(page, render(capture, 'native-io'));
  await expect(page.locator('#header-meta')).toContainText('MEASURED');
  await expect(page.locator('#io-table')).toContainText('6 / 17 B');
  await expect(page.locator('#io-table')).toContainText('8 / 8 B');
  await page.locator('#io-table .io-file').first().click();
  await expect(page.locator('#io-operations')).toContainText('fsync');
  await expect(page.locator('#io-operations')).toContainText('pread');
  await page.locator('#filter-input').fill('pwrite');
  await clickEvent(page, 'pwrite');
  await expect(page.locator('#detail-content h3')).toHaveText('pwrite');
  await expect(page.locator('#detail-content')).toContainText('io.completed_bytes');
  await expect(page.locator('#detail-content')).toContainText(data);
});
