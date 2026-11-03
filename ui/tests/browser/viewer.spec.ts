import { test, expect, type Page } from '@playwright/test';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { COUNTER_PLOT_HEIGHT, COUNTER_PLOT_TOP, defaultTimelineCollapsed, indexReport, integerPlotValues, SPAN_PITCH, SPAN_TOP, timelineRows } from '../../src/model.ts';
import type { Report } from '../../src/model.ts';
import { ioFixture } from '../io-fixture.ts';

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
  if (!process.env.RPROF_UI_SKIP_CARGO_BUILD) execFileSync('cargo', ['build', '-p', 'rprof-cli'], { cwd: root });
  directory = mkdtempSync(join(tmpdir(), 'rprof-browser-'));
  html = render(fixture, 'multitrack');
  // For injected-model UI iteration when a concurrent native build is unavailable:
  // the existing binary supplies the report JSON; use the just-built UI assets.
  if (process.env.RPROF_UI_SKIP_CARGO_BUILD) {
    const output = join(directory, 'multitrack.html');
    const source = readFileSync(output, 'utf8');
    writeFileSync(output, source.replace(/<style>[\s\S]*?<\/style>/,
      `<style>${readFileSync(join(root, 'ui/dist/viewer.css'), 'utf8')}</style>`)
      .replace(/<script>"use strict";[\s\S]*?<\/script>/,
        `<script>${readFileSync(join(root, 'ui/dist/viewer.js'), 'utf8')}</script>`));
  }
});

test.afterAll(() => { if (directory) rmSync(directory, { recursive: true, force: true }); });

async function loaded(page: Page, url = html): Promise<void> {
  await page.goto(url);
  await expect(page.locator('#timeline-canvas')).toBeVisible();
  await expect(page.locator('#range-label')).toContainText('domain-relative');
}

async function position(page: Page, name: string, kind: 'span' | 'counter', rank?: number, ns?: number, collapsed?: Set<string>): Promise<{ x: number; y: number }> {
  const report: Report = await page.locator('#rprof-data').evaluate(element => JSON.parse(element.textContent!));
  const domainId = await page.locator('#domain-select').inputValue();
  const query = await page.locator('#filter-input').inputValue();
  const data = indexReport(report);
  const rows = timelineRows(report, data, domainId, query, collapsed);
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

test('optional language panels remain absent on old reports', async ({ page }) => {
  await loaded(page);
  await expect(page.locator('#python-panel, #pytorch-panel')).toHaveCount(0);
});

test('system graphs drill into hosts and logical CPUs / NUMA nodes in both themes; filter reveals collapsed descendants', async ({ page }) => {
  const url = injected('system-graphs', report => {
    const domain = report.shards[0].domain;
    report.domains.find((item: any) => item.id === domain).end_ns = '4000000000';
    for (const [index, hostname] of ['alpha', 'beta'].entries()) {
      const shard = `system-host-${index}`;
      report.shards.push({ ...report.shards[0], id: shard, domain, name: shard, hostname });
      for (const resource of ['cpu', 'memory']) {
        const id = `${shard}:${resource}`;
        report.tracks.push({ id, shard, parent: null, name: resource, kind: 'system', rank: null,
          details: { resource, 'system.host_id': `${hostname}:boot` } });
        const child = `${id}:child`;
        report.tracks.push({ id: child, shard, parent: id, name: child, kind: 'system', rank: null,
          details: { resource: resource === 'cpu' ? 'cpu:7' : 'node:1' } });
        for (const time of [1, 2]) for (const track of [id, child]) report.counters.push({ track,
          name: resource === 'cpu' ? 'system.cpu.utilization' : 'system.memory.used', timestamp_ns: `${time}000000000`,
          value: resource === 'cpu' ? index ? '80' : '20' : `${(index + 1) * 1073741824}`, value_type: resource === 'cpu' ? 'float' : 'integer',
          unit: resource === 'cpu' ? '%' : 'bytes', attributes: { 'system.interval_ns': '1000000000', 'system.sample_index': String(time),
            'system.memory.total_bytes': `${(index + 1) * 2147483648}` } });
      }
    }
  });
  await loaded(page, url);
  const report: Report = await page.locator('#rprof-data').evaluate(element => JSON.parse(element.textContent!));
  const domain = report.shards[0]!.domain;
  const collapsed = defaultTimelineCollapsed(report);
  for (const theme of ['light', 'dark']) {
    if (theme === 'dark') await page.locator('#theme-toggle').click();
    await page.locator('#filter-input').fill('cpu:7');
    expect(timelineRows(report, indexReport(report), domain, 'cpu:7', collapsed).some(row => row.key.endsWith('cpu:7'))).toBeTruthy();
    await expect(page.locator('#timeline-canvas')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Collapse cpu system graphs' })).toHaveAttribute('aria-expanded', 'true');
    await page.getByRole('button', { name: 'Collapse cpu system graphs' }).click();
    await expect(page.locator('#filter-input')).toHaveValue('');
    await expect(page.getByRole('button', { name: 'Expand cpu system graphs' })).toHaveAttribute('aria-expanded', 'false');
    await page.locator('#filter-input').fill('node:1');
    expect(timelineRows(report, indexReport(report), domain, 'node:1', collapsed).some(row => row.key.endsWith('node:1'))).toBeTruthy();
    await expect(page.getByRole('button', { name: 'Collapse memory system graphs' })).toHaveAttribute('aria-expanded', 'true');
    await page.locator('#filter-input').fill('');
    expect(timelineRows(report, indexReport(report), domain, '', collapsed)[0]?.key).toBe(`system:${domain}:cpu`);
    const cpu = page.getByRole('button', { name: 'Expand cpu system graphs' });
    await expect(cpu).toHaveAttribute('aria-expanded', 'false');
    await cpu.click();
    await expect(page.getByRole('button', { name: 'Collapse cpu system graphs' })).toHaveAttribute('aria-expanded', 'true');
    await page.getByRole('button', { name: 'Collapse cpu system graphs' }).click();
    await expect(page.getByRole('button', { name: 'Expand memory system graphs' })).toBeVisible();
  }
  await page.locator('#filter-input').fill('node:1');
  await expect(page.locator('#timeline-canvas')).toBeVisible();
  expect(timelineRows(report, indexReport(report), domain, 'node:1', collapsed).some(row => row.key.endsWith('node:1'))).toBeTruthy();
});

test('Python and PyTorch panels filter by domain, page exact string counts, preserve null self and hostile metadata in both themes', async ({ page }) => {
  const huge = '90071992547409931234567890';
  const hostile = '<img src=x onerror=alert(1)><script>window.injected=true</script>';
  const url = injected('language-panels', report => {
    const native = report.domains[0].id;
    const imported = 'import-domain';
    const shard = report.shards[0].id;
    report.provenance.kind = 'mixed';
    report.domains.push({ id: imported, name: 'PyTorch source relative µs', origin_ns: '0', end_ns: '1000000' });
    report.shards.push({ ...report.shards[0], id: 'import-shard', domain: imported, name: 'torch export', collectors: ['pytorch'] });
    report.python = { receiver_dropped: huge, malformed: '4', runtimes: [
      { shard, domain: native, source: 'source-one', pid: '123', instance: 'image', interpreter: 'interp', backend: '', version: '', clock: 'clock', detail: hostile,
        initialized: true, finalized: false, attempted: huge, received: '5', source_dropped: '2', omitted_callbacks: '3', sequence_gaps: '4', unmatched: '5', abandoned: '6', inflight: '7', source_errors: '8', errors: [hostile] },
      { shard: 'import-shard', domain: imported, source: 'other', pid: '999', instance: '', interpreter: '', backend: '', version: '', clock: '', detail: '',
        initialized: false, finalized: false, attempted: '0', received: '0', source_dropped: '0', omitted_callbacks: '0', sequence_gaps: '0', unmatched: '0', abandoned: '0', inflight: '0', source_errors: '0', errors: [] },
      ...Array.from({ length: 101 }, (_, index) => ({ shard, domain: native, source: `source-${index}`, pid: `${index}`, instance: '', interpreter: '', backend: '', version: '', clock: '', detail: '', initialized: false, finalized: false, attempted: '0', received: '0', source_dropped: '0', omitted_callbacks: '0', sequence_gaps: '0', unmatched: '0', abandoned: '0', inflight: '0', source_errors: '0', errors: [] })),
    ], functions: [
      ...Array.from({ length: 102 }, (_, index) => ({ shard, domain: native, source: 'source-one', kind: 'function', symbol: huge, name: `fn-${index}`, file: hostile, line: '14', execution_kind: 'coroutine', segments: huge, total_ns: huge, self_ns: '0', max_ns: huge, yields: '2', unwinds: '1' })),
      ...Array.from({ length: 102 }, (_, index) => ({ shard, domain: native, source: 'source-one', kind: 'range', symbol: '0', name: `range-${index}`, file: '', line: '0', execution_kind: 'range', segments: '1', total_ns: huge, self_ns: null, max_ns: huge, yields: '0', unwinds: '0' })),
      { shard: 'import-shard', domain: imported, source: 'other', kind: 'function', symbol: '1', name: 'import-only', file: '', line: '0', execution_kind: 'function', segments: '1', total_ns: '1', self_ns: '1', max_ns: '1', yields: '0', unwinds: '0' },
    ] };
    report.pytorch = { sources: [{ shard: 'import-shard', domain: imported, metadata: {
      'pytorch.source_loss_known': 'false', 'pytorch.distributedInfo': '{"backend":"gloo","rank":0}', 'pytorch.clock_method': 'source_minimum_ts; relative microseconds converted to nanoseconds; no monotonic alignment',
      'pytorch.ERROR': hostile, 'pytorch.import_source_sha256': 'deadbeef', ...Object.fromEntries(Array.from({ length: 105 }, (_, index) => [`pytorch.helper.${index}`, `value-${index}`])),
    }, diagnostics: { 'pytorch.import.skipped_records': '8' } },
    ...Array.from({ length: 101 }, (_, index) => ({ shard: `other-${index}`, domain: imported, metadata: {}, diagnostics: {} }))], activities: [
      { shard: 'import-shard', domain: imported, role: 'operator', name: hostile, device: null, count: huge, total_ns: huge, self_ns: null, max_ns: huge, bytes: huge },
      { shard: 'import-shard', domain: imported, role: 'kernel', name: 'GPU kernel', device: 'cuda:0', count: '1', total_ns: '2', self_ns: null, max_ns: '2', bytes: '0' },
      ...Array.from({ length: 100 }, (_, index) => ({ shard: 'import-shard', domain: imported, role: 'python', name: `python-${index}`, device: null, count: '1', total_ns: '1', self_ns: '1', max_ns: '1', bytes: '0' })),
    ], memory: [{ shard: 'import-shard', domain: imported, track: 'mem-track', name: 'pytorch.memory.allocated', device: 'cuda:0', samples: '1', peak_bytes: huge, last_bytes: huge },
    ...Array.from({ length: 101 }, (_, index) => ({ shard: 'import-shard', domain: imported, track: `track-${index}`, name: 'pytorch.memory.allocated', device: null, samples: '1', peak_bytes: '1', last_bytes: '1' }))] };
  });
  await loaded(page, url);
  await expect(page.locator('#domain-note')).toContainText('UNALIGNED');
  await expect(page.locator('#python-functions tbody tr')).toHaveCount(100);
  await expect(page.locator('#python-ranges tbody tr')).toHaveCount(100);
  await expect(page.locator('#python-runtimes button')).toHaveCount(100);
  await page.locator('#python-runtime-pager button').last().click();
  await expect(page.locator('#python-runtimes button')).toHaveCount(2);
  await page.locator('#python-runtime-pager button').first().click();
  await expect(page.locator('#python-functions tbody tr').first()).toContainText(huge);
  await expect(page.locator('#python-functions tbody tr').first()).toContainText('0 ns');
  await expect(page.locator('#python-ranges tbody tr').first()).toContainText('N/A (elapsed range)');
  await expect(page.locator('#python-panel')).not.toContainText('import-only');
  await page.locator('#python-function-pager button').last().click();
  await expect(page.locator('#python-functions tbody tr')).toHaveCount(2);
  await page.locator('#python-range-pager button').last().click();
  await expect(page.locator('#python-ranges tbody tr')).toHaveCount(2);
  await page.locator('#python-runtimes button').first().click();
  await expect(page.locator('#python-runtime-detail')).toContainText('Unknown');
  await expect(page.locator('#python-runtime-detail')).toContainText(huge);
  await expect(page.locator('#python-runtime-detail')).toContainText('No');
  await page.locator('#domain-select').selectOption('import-domain');
  await expect(page.locator('#domain-note')).toContainText('IMPORTED PYTORCH µS→NS RELATIVE CLOCK');
  await expect(page.locator('#python-functions tbody tr')).toHaveCount(1);
  await expect(page.locator('#python-functions')).toContainText('import-only');
  await expect(page.locator('#pytorch-activities tbody tr')).toHaveCount(100);
  await expect(page.locator('#pytorch-activities')).toContainText('Unknown');
  await expect(page.locator('#pytorch-activities')).toContainText('N/A (GPU)');
  await expect(page.locator('#pytorch-memory')).toContainText(`${huge} B`);
  await expect(page.locator('#pytorch-memory')).not.toContainText('reserved');
  await page.locator('#pytorch-activity-pager button').last().click();
  await expect(page.locator('#pytorch-activities tbody tr')).toHaveCount(2);
  await page.locator('#pytorch-memory-pager button').last().click();
  await expect(page.locator('#pytorch-memory tbody tr')).toHaveCount(2);
  await expect(page.locator('#pytorch-sources button')).toHaveCount(100);
  await page.locator('#pytorch-source-pager button').last().click();
  await expect(page.locator('#pytorch-sources button')).toHaveCount(2);
  await page.locator('#pytorch-source-pager button').first().click();
  await page.locator('#pytorch-sources button').first().click();
  await expect(page.locator('#pytorch-source-detail')).toContainText('Source loss / export coverage: Unknown');
  await expect(page.locator('#pytorch-source-metadata tbody tr')).toHaveCount(100);
  await expect(page.locator('#pytorch-source-metadata')).toContainText('gloo');
  await page.locator('#pytorch-metadata-pager button').last().click();
  await expect(page.locator('#pytorch-source-metadata tbody tr')).toHaveCount(11);
  await page.locator('#pytorch-metadata-pager button').first().click();
  for (const theme of ['dark', 'light']) {
    if (await page.locator('html').getAttribute('data-theme') !== theme) await page.locator('#theme-toggle').click();
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
    await expect(page.locator('#pytorch-source-detail')).toContainText(hostile);
    await expect(page.locator('#pytorch-panel img, #pytorch-panel script, #python-panel img, #python-panel script')).toHaveCount(0);
    expect(await page.evaluate(() => (window as any).injected)).toBeUndefined();
  }
});

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

test('wheel scrolls rows over both plots and labels without changing zoom; zoom buttons and reset still work', async ({ page }) => {
  await loaded(page, injected('wheel-scroll', report => {
    const process = report.tracks.find((track: any) => track.rank === 0 && track.parent === null);
    report.tracks.push(...Array.from({ length: 80 }, (_, i) => ({ ...process, id: `scroll-${i}`, name: `process ${i}` })));
  }));
  const canvas = page.locator('#timeline-canvas'), scroll = page.locator('#timeline-scroll');
  const box = (await canvas.boundingBox())!;
  const original = await page.locator('#range-label').textContent();
  await page.mouse.move(box.x + 450, box.y + 110);
  await page.mouse.wheel(0, 360);
  await expect.poll(() => scroll.evaluate(element => element.scrollTop)).toBeGreaterThan(100);
  await expect(page.locator('#range-label')).toHaveText(original!);
  const plotScrolled = await scroll.evaluate(element => element.scrollTop);
  await page.mouse.move(box.x + 80, box.y + 110);
  await page.mouse.wheel(0, 200);
  await expect.poll(() => scroll.evaluate(element => element.scrollTop)).toBeGreaterThan(plotScrolled);
  await expect(page.locator('#range-label')).toHaveText(original!);
  await page.locator('#zoom-in').click();
  await expect(page.locator('#range-label')).not.toHaveText(original!);
  const zoomed = await page.locator('#range-label').textContent();
  await page.mouse.move(box.x + 450, box.y + 110);
  const beforeUp = await scroll.evaluate(element => element.scrollTop);
  await page.mouse.wheel(0, -180);
  await expect.poll(() => scroll.evaluate(element => element.scrollTop)).toBeLessThan(beforeUp);
  await expect(page.locator('#range-label')).toHaveText(zoomed!);
  await page.locator('#zoom-out').click();
  await expect(page.locator('#range-label')).not.toHaveText(zoomed!);
  await page.locator('#zoom-in').click();
  const vertical = await scroll.evaluate(element => element.scrollTop);
  await page.getByRole('button', { name: 'Reset zoom', exact: true }).click();
  await expect(page.locator('#range-label')).toHaveText(original!);
  expect(await scroll.evaluate(element => element.scrollTop)).toBe(vertical);
});

test('CPU and memory use distinct filled utilization areas and leave source gaps unpainted in both themes', async ({ page }) => {
  const url = injected('filled-utilization', report => {
    const domain = report.domains[0].id, shard = report.shards[0].id;
    report.domains = [{ ...report.domains[0], end_ns: '6000000000' }];
    report.shards = [{ ...report.shards[0], domain }];
    report.tracks = []; report.spans = []; report.counters = [];
    delete report.io_timeline;
    for (const resource of ['cpu', 'memory']) {
      const id = `filled:${resource}`;
      report.tracks.push({ id, shard, parent: null, name: resource, kind: 'system', rank: null,
        details: { resource, 'system.host_id': 'host/boot' } });
      for (const time of [1, 2, 4, 5]) report.counters.push({ track: id, name: resource === 'cpu' ? 'system.cpu.utilization' : 'system.memory.used',
        timestamp_ns: `${time}000000000`, value: resource === 'cpu' ? '40' : '2147483648', value_type: resource === 'cpu' ? 'float' : 'integer',
        unit: resource === 'cpu' ? '%' : 'bytes', attributes: { 'system.interval_ns': '1000000000', 'system.sample_index': String(time), 'system.memory.total_bytes': '4294967296' } });
    }
  });
  await page.emulateMedia({ colorScheme: 'dark' });
  await loaded(page, url);
  const report: Report = await page.locator('#rprof-data').evaluate(element => JSON.parse(element.textContent!));
  const rows = timelineRows(report, indexReport(report), report.domains[0]!.id, '');
  const cpu = rows.find(row => row.system === 'cpu')!, memory = rows.find(row => row.system === 'memory')!;
  const pixel = async (fraction: number, row: typeof cpu) => page.locator('#timeline-canvas').evaluate((element, args) => {
    const canvas = element as HTMLCanvasElement;
    const x = 260 + (canvas.clientWidth - 260) * args.fraction;
    const y = args.top + args.height - 27;
    return Array.from(canvas.getContext('2d')!.getImageData(Math.round(x * devicePixelRatio), Math.round(y * devicePixelRatio), 1, 1).data);
  }, { fraction, top: row.top, height: row.height });
  for (const theme of ['dark', 'light']) {
    if (theme === 'light') await page.getByRole('button', { name: 'Switch to light theme' }).click();
    await expect.poll(async () => {
      const inside = await pixel(.25, cpu), gap = await pixel(.54, cpu);
      return inside[1]! - inside[0]! - (gap[1]! - gap[0]!);
    }).toBeGreaterThan(8);
    const cpuGap = await pixel(.54, cpu), cpuBefore = await pixel(.08, cpu);
    expect(cpuGap).toEqual(cpuBefore);
    const memoryInside = await pixel(.25, memory), memoryGap = await pixel(.54, memory);
    expect(memoryInside[2]! - memoryInside[0]!).toBeGreaterThan(memoryGap[2]! - memoryGap[0]! + 8);
    expect(memoryGap).toEqual(await pixel(.08, memory));
  }
  const canvas = page.locator('#timeline-canvas'), box = (await canvas.boundingBox())!;
  await canvas.click({ position: { x: 260 + (box.width - 260) * .25, y: cpu.top + cpu.height - 27 } });
  await expect(page.locator('#detail-content h3')).toHaveText('system.cpu.utilization');
  await expect(page.locator('#detail-content')).toContainText('40');
  await canvas.click({ position: { x: 260 + (box.width - 260) * .54, y: cpu.top + cpu.height - 27 } });
  await expect(page.locator('#detail-content')).toContainText('Select a span or counter');
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
  await page.locator('#io-per-file summary').click();
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
  await loaded(page, injected('old-model', report => { delete report.io; delete report.openmp; delete report.cuda; delete report.pmu; }));
  await expect(page.locator('#io-count')).toHaveText('0 files');
  await expect(page.locator('#io-table')).toContainText('No per-file I/O recorded');
  await expect(page.locator('#openmp-panel')).toHaveCount(0);
  await expect(page.locator('#cuda-panel')).toHaveCount(0);
  await expect(page.locator('#pmu-panel')).toHaveCount(0);
});

test('main timeline I/O graph expands rank/process/thread and opt-in files, with shared zoom and filesystem selection', async ({ page }) => {
  const report = ioFixture(1000), data = indexReport(report), collapsed = defaultTimelineCollapsed(report);
  const hostile = '<img src=x onerror="globalThis.rprofInjected=true">';
  report.io_timeline!.filesystems[0]!.mount_point = hostile;
  const url = injected('io-timeline', target => Object.assign(target, report));
  await page.emulateMedia({ colorScheme: 'dark' });
  await loaded(page, url);
  await expect(page.locator('#io-profile')).toHaveCount(0);
  await expect(page.locator('[id^="io-chart-"]')).toHaveCount(0);
  await expect(page.locator('#io-toolbar')).toBeVisible();
  await expect(page.locator('#io-panel')).toBeHidden();
  await expect(page.locator('#io-metric-select')).toHaveValue('throughput');
  await expect(page.locator('#io-fs-select option')).toHaveCount(4);
  const canvas = page.locator('#timeline-canvas');
  const rows = () => timelineRows(report, data, '0', '', collapsed);
  const clickLabel = async (key: string) => {
    const row = rows().find(row => row.key === key)!;
    await page.locator('#timeline-scroll').evaluate((element, top) => { element.scrollTop = Math.max(0, top - 40); }, row.top);
    const scroll = await page.locator('#timeline-scroll').evaluate(element => element.scrollTop);
    await canvas.click({ position: { x: 30 + row.depth * 17, y: row.top - scroll + row.height / 2 } });
    if (collapsed.has(key)) collapsed.delete(key); else collapsed.add(key);
    await page.evaluate(() => new Promise(requestAnimationFrame));
  };
  expect(rows()).toHaveLength(1);
  await canvas.click({ position: { x: 300, y: 30 } });
  await expect(page.locator('#detail-content')).toContainText('105 B / 5');
  await page.locator('#io-metric-select').selectOption('latency');
  await canvas.click({ position: { x: 300, y: 30 } });
  await expect(page.locator('#detail-content')).toContainText('1 ms');
  await expect(page.locator('#detail-content dd[title="1000000.000 ns"]')).toBeVisible();
  const before = await page.locator('#range-label').textContent();
  await page.locator('#zoom-in').click();
  await expect(page.locator('#range-label')).not.toHaveText(before!);
  const box = (await canvas.boundingBox())!;
  await canvas.click({ position: { x: box.width - 20, y: 30 } });
  await expect(page.locator('#detail-content')).toContainText('0 B / 0');
  await page.locator('#fit-view').click();
  await clickLabel('io:0');
  expect(rows().filter(row => row.kind === 'io')).toHaveLength(3);
  await clickLabel(rows()[1]!.key);
  const process = rows().find(row => row.io?.key.startsWith('io:process:'))!;
  await clickLabel(process.key);
  const thread = rows().find(row => row.io?.track?.id === 's0:t0')!;
  await clickLabel(thread.key);
  expect(rows().filter(row => row.track?.kind === 'io')).toHaveLength(0);
  await clickLabel(`${thread.key}:files`);
  expect(rows().filter(row => row.track?.kind === 'io')).toHaveLength(1000);
  const point = await position(page, 'pread', 'span', 0, undefined, collapsed);
  await canvas.click({ position: point });
  await expect(page.locator('#detail-content')).toContainText('/data/file-0');
  await clickLabel(`${thread.key}:files`);
  await page.locator('#timeline-scroll').evaluate(element => { element.scrollTop = 0; });
  await page.locator('#io-fs-select').selectOption('f2');
  await canvas.click({ position: { x: 300, y: 30 } });
  await expect(page.locator('#detail-content')).toContainText('20 B / 1');
  await page.locator('#domain-select').selectOption('1');
  await expect(page.locator('#io-fs-select option')).toHaveCount(2);
  await canvas.click({ position: { x: 300, y: 30 } });
  await expect(page.locator('#detail-content')).toContainText('999 B / 1');
  await page.getByRole('button', { name: 'Switch to light theme' }).click();
  await expect(page.locator('#detail-content')).toContainText('999 B / 1');
  await expect(page.locator('#io-toolbar img')).toHaveCount(0);
  expect(await page.evaluate(() => (globalThis as any).rprofInjected)).toBeUndefined();
});

test('training-sized I/O rates have readable inspector units, explicit vertical scale and bin width', async ({ page }) => {
  await page.addInitScript(() => {
    const original = CanvasRenderingContext2D.prototype.fillText;
    (globalThis as any).ioAxisText = [];
    CanvasRenderingContext2D.prototype.fillText = function(text: string, x: number, y: number, maxWidth?: number) {
      if (text.includes('MiB/s') || text.startsWith('Bin width')) (globalThis as any).ioAxisText.push(text);
      if (maxWidth === undefined) original.call(this, text, x, y); else original.call(this, text, x, y, maxWidth);
    };
  });
  const report = ioFixture();
  for (const series of [...report.io_timeline!.filesystems.filter(f => f.id === 'f2'), ...report.io_timeline!.threads!.filter(t => t.filesystem_id === 'f2')]) {
    Object.assign(series.bins[0]!, { read_bytes: '667129363', read_latency_ns: '97669853', max_latency_ns: '97669853' });
  }
  await loaded(page, injected('readable-io', target => Object.assign(target, report)));
  await page.locator('#io-fs-select').selectOption('f2');
  await page.locator('#timeline-canvas').click({ position: { x: 300, y: 30 } });
  await expect(page.locator('#detail-content')).toContainText('636.22 MiB/s');
  await expect(page.locator('#detail-content')).toContainText('97.67 ms');
  await expect(page.locator('#detail-content')).toContainText('Bin duration');
  await expect(page.locator('#detail-content dd[title^="667129363.000 B/s"]')).toBeVisible();
  await expect.poll(() => page.evaluate(() => (globalThis as any).ioAxisText.some((text: string) => text === '636 MiB/s'))).toBe(true);
  await expect.poll(() => page.evaluate(() => (globalThis as any).ioAxisText.some((text: string) => text.startsWith('Bin width 1.000 s')))).toBe(true);
});

test('legacy I/O retains optional per-file details and does not invent time series', async ({ page }) => {
  await loaded(page, injected('io-legacy-message', report => { delete report.io_timeline; }));
  await expect(page.locator('#io-profile')).toHaveCount(0);
  await expect(page.locator('#io-panel')).toContainText('time series unavailable in this older report; regenerate');
  await expect(page.locator('#io-per-file')).not.toHaveAttribute('open', '');
  await page.locator('#io-per-file summary').click();
  await expect(page.locator('#io-table')).toBeVisible();
});

test('PMU panel preserves exact counts, unavailable reads, final state and multiplex context in dark and light modes', async ({ page }) => {
  const huge = '340282366920938463463374607431768211455';
  const hostile = '<img src=x onerror="globalThis.rprofInjected=true">';
  const url = injected('pmu-values', report => {
    const event = (name: string, status: string, extras: Record<string, unknown> = {}) => ({
      shard: 's0', domain: '0', name, pid: '9007199254740993', scope: 'workload-tree', privilege: 'user+kernel',
      event_type: '0', config: '0', unit: 'count', status, error: null, raw: null, time_enabled_ns: null,
      time_running_ns: null, scaled_estimate: null, running_percent: null, multiplexed: false,
      samples: '0', final_read: false, ...extras,
    });
    report.pmu = { hosts: [{ shard: 's0', domain: '0', metadata: { kernel: '6.12', cpu_model: hostile, affinity: '0-3',
      cpu0_cache0: 'L1', numa: 'node0', frequency: 'launch-time only', [hostile]: hostile } }],
      events: [event('cycles', 'measured', { raw: huge, time_enabled_ns: huge, time_running_ns: '9007199254740993',
        running_percent: '50.000', scaled_estimate: huge, multiplexed: true, final_read: true, samples: '17' }),
      event('task-clock', 'measured', { scope: 'root-thread', privilege: 'user', event_type: '1', unit: 'ns', raw: '0',
        time_enabled_ns: '100', time_running_ns: '100', running_percent: '100.000', scaled_estimate: '0', final_read: true, samples: '1' }),
      event('raw-event', 'measured', { event_type: '4', config: '9007199254740993', raw: '9007199254740993',
        time_enabled_ns: '100', time_running_ns: '0', scaled_estimate: null, final_read: false, samples: '2' }),
      ...['permission_denied', 'unsupported', 'unsupported_or_invalid', 'resource_unavailable', 'read_error', 'not_counted'].map(status =>
        event(status, status, { error: hostile, raw: null, time_enabled_ns: null, time_running_ns: null }))],
      metrics: [{ shard: 's0', domain: '0', ipc_estimate: null, cache_miss_percent_estimate: null }] };
    const track = report.tracks.find((entry: any) => entry.rank === 0 && entry.parent === null);
    report.counters.push({ track: track.id, name: 'pmu.meta.event', timestamp_ns: '100', value: '1',
      value_type: 'integer', unit: 'metadata', attributes: {} },
    { track: track.id, name: 'pmu.cycles.raw', timestamp_ns: '1000000', value: huge,
      value_type: 'integer', unit: 'count', attributes: { 'pmu.time_enabled_ns': huge, 'pmu.time_running_ns': '10', 'pmu.read_window_ns': '1000000' } });
  });
  await page.emulateMedia({ colorScheme: 'dark' });
  await loaded(page, url);
  await expect(page.locator('#pmu-panel')).toHaveCSS('background-color', 'rgb(24, 42, 56)');
  await expect(page.locator('#pmu-count')).toHaveText('9 events');
  await expect(page.locator('#pmu-events tbody tr').first()).toContainText(huge);
  await expect(page.locator('#pmu-events tbody tr').first()).toContainText('Hardware');
  await expect(page.locator('#pmu-events tbody tr').first()).toContainText('9007199254740993');
  await expect(page.locator('#pmu-events tbody tr').first()).toContainText('workload-tree · user+kernel');
  await expect(page.locator('#pmu-events tbody tr').nth(1)).toContainText('Software');
  await expect(page.locator('#pmu-events tbody tr').nth(1)).toContainText('root-thread · user');
  await expect(page.locator('#pmu-events tbody tr').nth(1)).toContainText('0 ns');
  await expect(page.locator('#pmu-events tbody tr').nth(2)).toContainText('Raw hardware');
  await expect(page.locator('#pmu-events tbody tr').nth(2)).toContainText('No · checkpoint, not a workload total');
  await expect(page.locator('#pmu-events tbody tr').nth(2).locator('td').nth(7)).toHaveText('Unavailable');
  for (const status of ['permission_denied', 'unsupported', 'unsupported_or_invalid', 'resource_unavailable', 'read_error', 'not_counted']) {
    const row = page.locator('#pmu-events tbody tr').filter({ has: page.locator('strong', { hasText: new RegExp(`^${status}$`) }) });
    await expect(row.locator('td').nth(3)).toContainText(status);
    await expect(row.locator('td').nth(4)).toHaveText('Unavailable count');
  }
  await expect(page.locator('#pmu-metrics tbody')).toContainText('Unavailable — no valid final pair');
  await expect(page.locator('#pmu-panel')).toContainText('cumulative raw counts, not rates');
  await expect(page.locator('#pmu-panel')).toContainText('independently scheduled');
  await page.locator('#pmu-hosts summary').click();
  await page.locator('#pmu-host-list .io-file').click();
  await expect(page.locator('#pmu-host-metadata')).toContainText('cpu_model');
  await expect(page.locator('#pmu-host-metadata')).toContainText(hostile);
  await page.locator('#filter-input').fill('pmu.meta');
  await expect(page.locator('#timeline-tooltip')).toBeHidden();
  await page.locator('#filter-input').fill('pmu.cycles.raw');
  await clickEvent(page, 'pmu.cycles.raw', 'counter', 0);
  await expect(page.locator('#detail-content')).toContainText(huge);
  await expect(page.locator('#detail-content')).toContainText('pmu.time_enabled_ns');
  await expect(page.locator('#detail-content')).toContainText('pmu.time_running_ns');
  await expect(page.locator('#detail-content')).toContainText('pmu.read_window_ns');
  await page.getByRole('button', { name: 'Switch to light theme' }).click();
  await expect(page.locator('#pmu-panel')).toHaveCSS('background-color', 'rgb(255, 255, 255)');
  expect(await page.evaluate(() => 'rprofInjected' in globalThis)).toBe(false);
  expect(await page.locator('img').count()).toBe(0);
});

test('PMU event, metric, host and host-metadata lists page by domain and reset on domain change', async ({ page }) => {
  await loaded(page, injected('pmu-pages', report => {
    const event = (domain: string, name: string) => ({ shard: 's', domain, name, pid: '3', scope: 'root-thread',
      privilege: 'user', event_type: '0', config: '0', unit: 'count', status: 'measured', error: null,
      raw: '1', time_enabled_ns: '10', time_running_ns: '10', scaled_estimate: '1', running_percent: '100.000',
      multiplexed: false, samples: '1', final_read: true });
    const metric = (domain: string, shard: string) => ({ domain, shard, ipc_estimate: '1.500000', cache_miss_percent_estimate: '2.500000' });
    const host = (domain: string, shard: string) => ({ domain, shard,
      metadata: Object.fromEntries(Array.from({ length: 101 }, (_, i) => [`key-${i}`, `value-${i}`])) });
    report.pmu = { events: [...Array.from({ length: 101 }, (_, i) => event('0', `event-${i}`)), event('1', 'foreign-event')],
      metrics: [...Array.from({ length: 101 }, (_, i) => metric('0', `metric-${i}`)), metric('1', 'foreign-metric')],
      hosts: [...Array.from({ length: 101 }, (_, i) => host('0', `host-${i}`)), host('1', 'foreign-host')] };
  }));
  await expect(page.locator('#pmu-events tbody tr')).toHaveCount(100);
  await expect(page.locator('#pmu-metrics tbody tr')).toHaveCount(100);
  await expect(page.locator('#pmu-events')).not.toContainText('foreign-event');
  await page.locator('#pmu-event-pager button').last().click();
  await expect(page.locator('#pmu-event-pager')).toContainText('101–101 of 101');
  await expect(page.locator('#pmu-events tbody')).toContainText('event-100');
  await page.locator('#pmu-metric-pager button').last().click();
  await expect(page.locator('#pmu-metric-pager')).toContainText('101–101 of 101');
  await expect(page.locator('#pmu-metrics tbody')).toContainText('metric-100');
  await page.locator('#pmu-hosts summary').click();
  await expect(page.locator('#pmu-host-list .openmp-runtime-row')).toHaveCount(100);
  await page.locator('#pmu-host-list .io-file').first().click();
  await expect(page.locator('#pmu-host-metadata tbody tr')).toHaveCount(100);
  await page.locator('#pmu-metadata-pager button').last().click();
  await expect(page.locator('#pmu-metadata-pager')).toContainText('101–101 of 101');
  await page.locator('#pmu-host-pager button').last().click();
  await expect(page.locator('#pmu-host-pager')).toContainText('101–101 of 101');
  await page.locator('#domain-select').selectOption('1');
  await expect(page.locator('#pmu-count')).toHaveText('1 events');
  await expect(page.locator('#pmu-events')).toContainText('foreign-event');
  await expect(page.locator('#pmu-metrics')).toContainText('foreign-metric');
  await expect(page.locator('#pmu-host-list')).toContainText('foreign-host');
  await expect(page.locator('#pmu-host-detail')).toBeEmpty();
});

test('CUDA panel filters domains, pages all arrays, shows exact values and partial source health safely in light mode', async ({ page }) => {
  const huge = '9007199254740993';
  const hostile = '<img src=x onerror="globalThis.rprofInjected=true">';
  const url = injected('cuda-injected', report => {
    const device = (domain: string, id: string) => ({ shard: 'shard-0', domain, instance: 'image-0', pid: '45',
      device: id, name: hostile, identity: 'GPU identity', visible: true, memory_bytes: huge,
      compute_major: '9', compute_minor: '0', multiprocessors: '132', kernels: huge, copies: '2', memsets: '1',
      kernel_ns: huge, copy_ns: '12', copy_bytes: huge, memset_bytes: '8', window_ns: '20' });
    const activity = (domain: string, deviceId: string, stream: string) => ({ shard: 'shard-0', domain,
      instance: 'image-0', device: deviceId, context: 'ctx', stream, kind: 'kernel', name: `kernel-${stream}`,
      count: huge, total_ns: huge, max_ns: '2', bytes: huge });
    const api = (domain: string, name: string) => ({ shard: 'shard-0', domain, instance: 'image-0',
      api_domain: name === 'cudaLaunch' ? 'runtime' : 'driver', name, calls: huge, errors: '2', total_ns: huge, max_ns: '5' });
    const source = (domain: string, shard: string) => ({ shard, domain, initialized_images: '1', finalized: false,
      attempted: huge, received: '3', ingress_dropped: '4', cupti_dropped: '5', source_errors: '6',
      invalid_timing: '7', unattributed: '8', unresolved_correlations: '9', ambiguous_correlations: '10' });
    const runtime = (domain: string, instance: string) => ({ shard: 'shard-0', domain, instance, pid: '45',
      cupti_version: '123', cuda_version: '12080', initialized: false, clock_method: 'CLOCK_MONOTONIC',
      clock_uncertainty: null, capabilities: [{ name: hostile, supported: false, optional: true, result: 'unsupported', detail: 'not available' },
        ...Array.from({ length: 100 }, (_, i) => ({ name: `cap-${i}`, supported: true, optional: false, result: 'ok', detail: 'supported' }))],
      errors: [hostile, ...Array.from({ length: 100 }, (_, i) => `error-${i}`)] });
    report.cuda = {
      devices: [device('0', '0'), ...Array.from({ length: 100 }, (_, i) => device('0', `device-${i}`)), device('1', 'foreign')],
      activities: [activity('0', '0', '0'), ...Array.from({ length: 100 }, (_, i) => activity('0', '0', `${i + 1}`)),
        activity('1', '0', 'foreign'), { ...activity('0', '0', 'wrong-image'), instance: 'other-image' }],
      apis: [api('0', 'cudaLaunch'), ...Array.from({ length: 100 }, (_, i) => api('0', `cuCall-${i}`)), api('1', 'foreign-api')],
      runtimes: [runtime('0', 'image-0'), ...Array.from({ length: 100 }, (_, i) => runtime('0', `image-${i + 1}`)), runtime('1', 'foreign-runtime')],
      sources: [source('0', 'first'), ...Array.from({ length: 100 }, (_, i) => source('0', `source-${i}`)), source('1', 'foreign-source')],
    };
    const track = report.tracks.find((entry: any) => entry.rank === 0 && entry.parent === null);
    report.counters.push(...['cuda.meta.device.name', 'cuda.cupti_dropped_records', 'cuda.invalid_timing_records', 'cuda.source_errors'].map(name =>
      ({ track: track.id, name, timestamp_ns: '1', value: huge, value_type: 'integer', unit: 'count', attributes: {} })));
    report.spans.push({ id: 19999, track: track.id, name: 'CUDA host launch', category: 'cuda',
      start_ns: '9000000', duration_ns: '1000000', correlation: 'cuda-corr', incomplete: false, attributes: { 'cuda.api_domain': 'runtime' } });
  });
  await page.emulateMedia({ colorScheme: 'light' });
  await loaded(page, url);
  await expect(page.locator('#cuda-panel')).toHaveCSS('background-color', 'rgb(255, 255, 255)');
  await expect(page.locator('.cuda-key')).toHaveCSS('background-color', 'rgb(233, 194, 100)');
  await expect(page.locator('#cuda-count')).toHaveText('101 devices');
  await expect(page.locator('#cuda-devices tbody tr')).toHaveCount(100);
  await expect(page.locator('#cuda-devices')).toContainText(`${huge} B`);
  await expect(page.locator('#cuda-devices')).toContainText(`${huge} / 12 ns`);
  await expect(page.locator('#cuda-devices')).not.toContainText('foreign');
  await expect(page.locator('#capture-status')).toContainText('GPU activity may be missing');
  await expect(page.locator('#capture-status')).toContainText('510 CUPTI drops');
  await expect(page.locator('#capture-status')).toContainText(`306/${BigInt(huge) * 102n} native received`);
  await expect(page.locator('#cuda-health')).toContainText('incomplete collection');
  await page.locator('#cuda-devices .io-file').first().click();
  await expect(page.locator('#cuda-activities tbody tr')).toHaveCount(100);
  await expect(page.locator('#cuda-activities')).toContainText(huge);
  await expect(page.locator('#cuda-activities')).not.toContainText('wrong-image');
  await page.locator('#cuda-activity-pager button').last().click();
  await expect(page.locator('#cuda-activity-pager')).toContainText('101–101 of 101');
  await page.locator('#cuda-device-pager button').last().click();
  await expect(page.locator('#cuda-device-pager')).toContainText('101–101 of 101');
  await expect(page.locator('#cuda-apis tbody tr')).toHaveCount(100);
  await expect(page.locator('#cuda-apis')).toContainText('runtime');
  await expect(page.locator('#cuda-apis')).toContainText('driver');
  await page.locator('#cuda-api-pager button').last().click();
  await expect(page.locator('#cuda-api-pager')).toContainText('101–101 of 101');
  await expect(page.locator('#cuda-diagnostics')).not.toHaveAttribute('open', '');
  await page.locator('#cuda-diagnostics summary').click();
  await expect(page.locator('#cuda-runtime-list .openmp-runtime-row')).toHaveCount(100);
  await expect(page.locator('#cuda-runtime-list')).toContainText('uncertainty: Unknown');
  await page.locator('#cuda-runtime-list .io-file').first().click();
  await expect(page.locator('#cuda-capabilities tbody tr')).toHaveCount(100);
  await expect(page.locator('#cuda-capabilities')).toContainText('unsupported');
  await expect(page.locator('.cuda-errors p')).toHaveCount(100);
  await page.locator('#cuda-capability-pager button').last().click();
  await expect(page.locator('#cuda-capability-pager')).toContainText('101–101 of 101');
  await page.locator('#cuda-error-pager button').last().click();
  await expect(page.locator('#cuda-error-pager')).toContainText('101–101 of 101');
  await page.locator('#cuda-runtime-pager button').last().click();
  await expect(page.locator('#cuda-runtime-pager')).toContainText('101–101 of 101');
  await expect(page.locator('#cuda-sources tbody tr')).toHaveCount(100);
  await expect(page.locator('#cuda-sources tbody')).toContainText(`${huge}`);
  await expect(page.locator('#cuda-sources tbody')).toContainText('No · GPU activity may be missing');
  await page.locator('#cuda-source-pager button').last().click();
  await expect(page.locator('#cuda-source-pager')).toContainText('101–101 of 101');
  await page.locator('#filter-input').fill('CUDA host launch');
  await clickEvent(page, 'CUDA host launch');
  await expect(page.locator('#detail-content')).toContainText('cuda.api_domain');
  await page.locator('#domain-select').selectOption('1');
  await expect(page.locator('#cuda-count')).toHaveText('1 devices');
  await expect(page.locator('#cuda-apis')).toContainText('foreign-api');
  await expect(page.locator('#cuda-device-detail')).toBeEmpty();
  await expect(page.locator('#cuda-runtime-detail')).toBeEmpty();
  await expect(page.locator('#cuda-sources tbody tr')).toHaveCount(1);
  expect(await page.evaluate(() => 'rprofInjected' in globalThis)).toBe(false);
  expect(await page.locator('img').count()).toBe(0);
});

test('metadata-only invisible CUDA devices do not imply GPU work, and diagnostics never become plots', async ({ page }) => {
  await loaded(page, injected('cuda-metadata', report => {
    report.cuda = { runtimes: [], activities: [], apis: [], sources: [], devices: [{ shard: 's', domain: '0',
      instance: 'image', pid: '3', device: '0', name: 'unused', identity: 'id', visible: false,
      memory_bytes: null, compute_major: null, compute_minor: null, multiprocessors: null, kernels: '0', copies: '0',
      memsets: '0', kernel_ns: '0', copy_ns: '0', copy_bytes: '0', memset_bytes: '0', window_ns: '0' }] };
    const track = report.tracks[0];
    report.counters.push(...['cuda.meta.device', 'cuda.attempted_events', 'cuda.finalized', 'cuda.unresolved_correlations'].map(name =>
      ({ track: track.id, name, timestamp_ns: '1', value: '1', value_type: 'integer', unit: 'count', attributes: {} })));
  }));
  await expect(page.locator('#cuda-count')).toContainText('1 invisible unused omitted');
  await expect(page.locator('#cuda-devices')).toContainText('No visible CUDA devices');
  await expect(page.locator('#cuda-health')).toContainText('metadata alone does not establish measured GPU work');
  const plotted = await page.locator('#rprof-data').evaluate(element => {
    const report = JSON.parse(element.textContent!);
    return report.counters.filter((counter: any) => counter.name.startsWith('cuda.')).length;
  });
  expect(plotted).toBe(4);
  await page.locator('#filter-input').fill('cuda.meta');
  await expect(page.locator('#timeline-tooltip')).toBeHidden();
  await expect(page.locator('#timeline-canvas')).toBeVisible();
});

test('CUDA GPU stream spans and host API spans retain timeline filtering, inspector and correlation', async ({ page }) => {
  await loaded(page, injected('cuda-timeline', report => {
    const process = report.tracks.find((track: any) => track.rank === 0 && track.parent === null);
    report.tracks.push({ id: 'gpu', shard: process.shard, parent: null, name: 'GPU 0', kind: 'gpu_device', rank: null, details: {} },
      { id: 'stream', shard: process.shard, parent: 'gpu', name: 'stream 7', kind: 'gpu_stream', rank: null, details: { context: '2' } });
    report.spans.push({ id: 30001, track: process.id, name: 'cudaLaunchKernel', category: 'cuda',
      start_ns: '5000000', duration_ns: '2000000', correlation: 'host-api', incomplete: false, attributes: { 'cuda.api_domain': 'runtime' } },
      { id: 30002, track: 'stream', name: 'GPU kernel', category: 'cuda', start_ns: '9000000',
        duration_ns: '2000000', correlation: 'gpu-work', incomplete: false, attributes: { 'cuda.stream': '7' } });
    report.dependencies.push({ from: 'host-api', to: 'gpu-work', relation: 'launches' });
    report.cuda = { runtimes: [], devices: [], activities: [], apis: [], sources: [] };
  }));
  await page.locator('#filter-input').fill('GPU kernel cuda');
  await clickEvent(page, 'GPU kernel');
  await expect(page.locator('#detail-content')).toContainText('gpu-work');
  await expect(page.locator('#detail-content')).toContainText('host-api → gpu-work');
  await expect(page.locator('#detail-content')).toContainText('cuda.stream');
  await page.locator('#filter-input').fill('cudaLaunchKernel');
  await clickEvent(page, 'cudaLaunchKernel');
  await expect(page.locator('#detail-content')).toContainText('gpu-work');
  await expect(page.locator('#detail-content')).toContainText('cuda.api_domain');
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

test('GNU libgomp regions mode shows partial coverage, disabled capabilities and unobserved timing instead of zero', async ({ page }) => {
  const hostile = '<img src=x onerror="globalThis.rprofInjected=true">';
  await loaded(page, injected('gomp-regions', report => {
    report.openmp = {
      tasks_created: '0', tasks_completed: '0', execution_segments: '0',
      runtimes: [{ shard: 's0', domain: '0', instance: 'image', pid: '24', backend: 'gomp',
        version: 'GCC 13 / libgomp; detail=regions', omp_version: '201511', initialized: true, finalized: true,
        sequence_gaps: '0', unmatched_endpoints: '0', abandoned_intervals: '0',
        callbacks: [{ name: 'gomp.parallel_body', status: 'always', observed: '1', observed_complete: true,
          detail: 'Joined parallel bodies only' },
        { name: 'gomp.api', status: 'never', observed: null, observed_complete: false,
          detail: `disabled by detail selection (regions) ${hostile}` },
        { name: 'gomp.task_body', status: 'never', observed: null, observed_complete: false,
          detail: 'disabled by detail selection (regions)' }] }],
      regions: [{ shard: 's0', domain: '0', instance: 'image', pid: '24', parallel_id: '3', requested_threads: '2',
        observed_threads: '2', duration_ns: '100', work_ns: '0', wait_ns: '0',
        work_min_ns: null, work_max_ns: null, work_spread_ns: null }],
      threads: [{ shard: 's0', domain: '0', instance: 'image', pid: '24', parallel_id: '3', tid: '33',
        team_index: '1', work_ns: '0', wait_ns: '0', sync_ns: '0', scheduled_ns: '0', execution_segments: '0' }],
      gomp: { apis: [], bodies: [{ shard: 's0', domain: '0', instance: 'image', tid: '33', kind: 'parallel_body',
        codeptr: '0x1234', count: '1', total_ns: '90', max_ns: '90' }],
        task_creations: '0', task_body_spans: '0', task_lifetimes_to_body_return: '0' },
    };
  }));
  await expect(page.locator('#gomp-panel')).toBeVisible();
  await expect(page.locator('#gomp-coverage')).toContainText('detail regions');
  await expect(page.locator('#openmp-tasks')).not.toContainText('0 completed');
  await expect(page.locator('#gomp-apis tbody')).toContainText('may be disabled by detail selection');
  await expect(page.locator('#openmp-regions tbody')).toContainText('Unobserved (GOMP)');
  await page.locator('#openmp-regions .io-file').click();
  await expect(page.locator('#openmp-threads tbody tr')).toContainText('Unobserved');
  await expect(page.locator('#openmp-detail')).toContainText('not OMPT scheduled execution');
  await expect(page.locator('#gomp-bodies')).toContainText('0x1234');
  await page.locator('#openmp-runtimes summary').click();
  await page.locator('#openmp-runtime-list .io-file').click();
  await expect(page.locator('#openmp-callbacks')).toContainText('disabled by detail selection (regions)');
  await expect(page.locator('#openmp-callbacks')).toContainText('Unknown');
  expect(await page.locator('img').count()).toBe(0);
  expect(await page.evaluate(() => 'rprofInjected' in globalThis)).toBe(false);
});

test('GOMP full-detail API and body aggregates page exact strings and filter by clock domain', async ({ page }) => {
  const huge = '90071992547409931234567890';
  await loaded(page, injected('gomp-full', report => {
    report.domains.push({ ...report.domains[0], id: 'foreign' });
    report.openmp = { runtimes: [
      { shard: 's0', domain: '0', instance: 'image', pid: '9', backend: 'gomp', version: 'GCC 13 detail=full', omp_version: '201511',
        initialized: true, finalized: true, sequence_gaps: '0', unmatched_endpoints: '0', abandoned_intervals: '0', callbacks: [] },
      { shard: 's1', domain: 'foreign', instance: 'image', pid: '8', backend: 'gomp', version: 'GCC 13 detail=bodies', omp_version: '201511',
        initialized: true, finalized: true, sequence_gaps: '0', unmatched_endpoints: '0', abandoned_intervals: '0', callbacks: [] }],
      regions: [], threads: [], tasks_created: '0', tasks_completed: '0', execution_segments: '0',
      gomp: { task_creations: huge, task_body_spans: '11', task_lifetimes_to_body_return: '7',
        apis: [...Array.from({ length: 101 }, (_, i) => ({ shard: 's0', domain: '0', instance: 'image', name: `GOMP_call_${i}`,
          calls: huge, total_ns: huge, max_ns: huge })),
          { shard: 's1', domain: 'foreign', instance: 'image', name: 'foreign-api', calls: '1', total_ns: '1', max_ns: '1' }],
        bodies: [...Array.from({ length: 101 }, (_, i) => ({ shard: 's0', domain: '0', instance: 'image', tid: `${i}`,
          kind: 'task_body', codeptr: `0x${i.toString(16)}`, count: huge, total_ns: huge, max_ns: huge })),
          { shard: 's1', domain: 'foreign', instance: 'image', tid: '0', kind: 'task_body', codeptr: 'foreign-ptr',
            count: '1', total_ns: '1', max_ns: '1' }] },
    };
  }));
  await expect(page.locator('#gomp-api-count')).toContainText('101 API rows');
  await expect(page.locator('#gomp-apis tbody tr')).toHaveCount(100);
  await expect(page.locator('#gomp-bodies tbody tr')).toHaveCount(100);
  await expect(page.locator('#gomp-tasks')).toContainText(`${huge} task creations`);
  await expect(page.locator('#gomp-tasks')).toContainText('not filtered by domain');
  await expect(page.locator('#gomp-apis tbody')).toContainText(`${huge} ns`);
  await expect(page.locator('#gomp-bodies tbody')).toContainText(huge);
  await expect(page.locator('#gomp-apis tbody')).not.toContainText('foreign-api');
  await page.locator('#gomp-api-pager button').last().click();
  await page.locator('#gomp-body-pager button').last().click();
  await expect(page.locator('#gomp-api-pager')).toContainText('101–101 of 101');
  await expect(page.locator('#gomp-body-pager')).toContainText('101–101 of 101');
  await page.locator('#domain-select').selectOption('foreign');
  await expect(page.locator('#gomp-api-count')).toContainText('1 API rows');
  await expect(page.locator('#gomp-apis tbody')).toContainText('foreign-api');
  await expect(page.locator('#gomp-bodies tbody')).toContainText('foreign-ptr');
  await expect(page.locator('#gomp-api-pager')).toBeEmpty();
  await expect(page.locator('#gomp-tasks')).toContainText(`${huge} task creations`);
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

test('native I/O collection produces timeline graphs and opt-in per-thread file calls', async ({ page }) => {
  execFileSync('make', ['preload'], { cwd: root });
  const workload = join(directory, 'io-workload');
  execFileSync('cc', ['-O2', '-pthread', join(root, 'crates/rprof-cli/tests/fixtures/io_workload.c'), '-o', workload]);
  const data = join(directory, 'native-data');
  const capture = join(directory, 'native.jsonl');
  execFileSync(binary, ['record', '--collect', 'io', '-o', capture, '--', workload, 'basic', data]);
  await loaded(page, render(capture, 'native-io'));
  await expect(page.locator('#header-meta')).toContainText('MEASURED');
  await expect(page.locator('#io-profile')).toHaveCount(0);
  await expect(page.locator('#io-toolbar')).toBeVisible();
  await expect(page.locator('#io-panel')).toBeHidden();
  const report: Report = await page.locator('#rprof-data').evaluate(element => JSON.parse(element.textContent!));
  const bins = report.io_timeline!.threads!.flatMap(thread => thread.bins);
  expect(bins.reduce((sum, bin) => sum + BigInt(bin.read_bytes), 0n)).toBe(6n);
  expect(bins.reduce((sum, bin) => sum + BigInt(bin.write_bytes), 0n)).toBe(8n);
  await page.locator('#filter-input').fill('pwrite');
  const collapsed = defaultTimelineCollapsed(report);
  const rows = timelineRows(report, indexReport(report), '0', 'pwrite', collapsed);
  const files = rows.find(row => row.key.endsWith(':files'))!;
  expect(files).toBeTruthy();
  await page.locator('#timeline-canvas').click({ position: { x: 120, y: files.top + files.height / 2 } });
  collapsed.delete(files.key);
  await page.evaluate(() => new Promise(requestAnimationFrame));
  await page.locator('#timeline-canvas').click({ position: await position(page, 'pwrite', 'span', undefined, undefined, collapsed) });
  await expect(page.locator('#detail-content h3')).toHaveText('pwrite');
  await expect(page.locator('#detail-content')).toContainText('io.completed_bytes');
  await expect(page.locator('#detail-content')).toContainText(data);
});
