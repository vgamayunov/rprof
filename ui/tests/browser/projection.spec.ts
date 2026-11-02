import { test, expect, type Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = resolve(fileURLToPath(new URL('../../..', import.meta.url)));
const native = join(root, 'target/debug/rprof');
const baseline = join(root, 'fixtures/projection/baseline.jsonl');
const hostile = '<img src=x onerror="globalThis.rprofInjected=true">';
const provenance = { kind: 'measured', source: hostile, date: '2026-01-01', conditions: 'local' };
const profile = (id: string) => ({ version: 1, id, name: hostile, nodes: 1,
  placement: { ranks_per_node: 1, threads_per_rank: 1, gpus_per_rank: 0 }, provenance,
  resources: { cpu: { kind: 'cpu', description: 'test', slots_per_node: 1, capacity_bytes_per_node: null, features: [],
    metrics: { rate: { unit: 'work_per_second', low: 10, central: 20, high: 30, provenance } } } } });
const span = (i: number) => ({ id: `span-${i}`, name: `source-${i}`, category: 'cpu', lane: '0', start_ns: '9007199254740993',
  duration_ns: '0', incomplete: false, enclosing: false, attributes: {} });
const task = (i: number) => ({ id: `phase-${i}`, span_id: `span-${i}`, name: `${hostile}-${i}`, resource: 'cpu', model: 'throughput',
  assumption: 'test only', bottleneck: 'cpu', baseline_start_ns: String(i), baseline_duration_ns: '0',
  start_ns: String(i), duration_ns: '0', preserved_lag_ns: '0', critical: i === 0 });
const scenario = (i: number) => ({ id: `scenario-${i}`, target_id: 'target', target_name: `target-${i}`, nodes: 1, scaling: 'fixed',
  status: i === 0 ? 'unsupported' : 'estimated', issues: i === 0 ? [hostile] : [], warnings: [],
  runtime: i === 0 ? null : { optimistic_ns: '90', central_ns: '100', pessimistic_ns: '120' },
  speedup: i === 0 ? null : '1.000', efficiency: i === 0 ? null : '1.000',
  tasks: i === 1 ? Array.from({ length: 101 }, (_, index) => task(index)) : [task(0)],
  resources: [{ resource: 'cpu', slots: 1, busy_ns: '50', modeled_busy_percent: '50.0' }],
  critical_path: ['phase-0'], validation: [{ observed_ns: '110', signed_error_percent: '10', provenance }] });
const artifact = { kind: 'rprof-projection', version: 1, engine_version: 'rprof-projection/1', hashes: { workload: 'hash' },
  request: { version: 1, baseline: { capture_sha256: 'hash', session_id: hostile, domain: 'domain', duration_ns: '100',
    complete: true, dropped: '0', omitted: '0', clock_uncertainty_ns: null, warnings: [], metadata: {}, spans: [span(0)] },
    source: profile('source'), targets: [profile('target')], workload: { version: 1, name: 'workload', baseline_sha256: 'hash',
      domain: 'domain', window_start_ns: '9007199254740993', window_duration_ns: '100', assumptions: 'representative phase',
      unmodeled: 'reject', unmodeled_assumption: '', allow_partial_evidence: false, scaling_assumption: 'fixed',
      placement_assumption: 'same', memory: {}, required_features: {}, phases: [{ id: 'phase-0', span_id: 'span-0', name: hostile,
        resource: 'cpu', model: { kind: 'unchanged' }, after: [], node_scaling: { kind: 'fixed' }, assumption: 'sample phase' }] },
    nodes: [1], scaling: 'fixed', validation: [{ target_id: 'target', nodes: 1, observed_ns: '110',
      workload_sha256: 'hash', target_profile_sha256: 'hash', provenance }] },
  baseline_runtime_ns: '100', covered_ns: '100', coverage_percent: '100.000', source_reconstructed_ns: '100',
  assumptions: ['model only'], warnings: [], scenarios: Array.from({ length: 101 }, (_, i) => scenario(i)) };

let directory: string;
test.beforeAll(() => {
  if (!process.env.RPROF_UI_SKIP_CARGO_BUILD) execFileSync('cargo', ['build', '-p', 'rprof-cli'], { cwd: root });
  directory = mkdtempSync(join(tmpdir(), 'rprof-projection-ui-'));
});
test.afterAll(() => { if (directory) rmSync(directory, { recursive: true, force: true }); });
function html(): string {
  const file = join(directory, 'projection.html');
  // Match native HTML escaping: script contents cannot be closed by untrusted evidence.
  const json = JSON.stringify(artifact).replaceAll('<', '\\u003c').replaceAll('>', '\\u003e').replaceAll('&', '\\u0026');
  writeFileSync(file, `<!doctype html><html><head><style>${readFileSync(join(root, 'ui/dist/viewer.css'), 'utf8')}</style></head><body><div id="app"></div><script type="application/json" id="rprof-data">${json}</script><script>${readFileSync(join(root, 'ui/dist/viewer.js'), 'utf8')}</script></body></html>`);
  return pathToFileURL(file).href;
}

test('projection remains offline, escapes hostile strings, paginates scenarios and tasks, and distinguishes missing estimates', async ({ page }) => {
  const requests: string[] = []; page.on('request', request => { if (/^https?:/.test(request.url())) requests.push(request.url()); });
  await page.context().setOffline(true); await page.goto(html());
  await expect(page.locator('#projection-scenarios tbody tr')).toHaveCount(100);
  await expect(page.locator('#projection-scenarios tbody tr').first()).toContainText('Unavailable');
  await expect(page.locator('#projection-scenarios tbody tr').first()).not.toContainText('0 / 0 / 0');
  await page.locator('#projection-scenario-pager button').last().click();
  await expect(page.locator('#projection-scenario-pager')).toContainText('101–101 of 101');
  await page.locator('#projection-scenario-pager button').first().click();
  await page.getByRole('button', { name: 'Inspect scenario scenario-1', exact: true }).click();
  await expect(page.locator('#projection-tasks tbody tr')).toHaveCount(100);
  await page.locator('#projection-task-pager button').last().click();
  await expect(page.locator('#projection-tasks tbody tr')).toHaveCount(1);
  await expect(page.locator('#projection-tasks tbody tr')).toContainText('phase-100');
  expect(await page.evaluate(() => 'rprofInjected' in globalThis)).toBe(false);
  expect(await page.locator('img').count()).toBe(0);
  expect(requests).toEqual([]);
});

test('projection theme persists; missing WASM reports error without losing the previous result', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'light' }); await page.goto(html());
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.getByRole('button', { name: 'Switch to dark theme' }).click();
  await page.reload(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.locator('#projection-editor').fill('{');
  await page.getByRole('button', { name: 'Recalculate' }).click();
  await expect(page.locator('#projection-error')).toBeVisible();
  await expect(page.locator('#projection-scenarios tbody tr')).toHaveCount(100);
  await page.getByRole('button', { name: 'Restore original' }).click();
  await page.getByRole('button', { name: 'Recalculate' }).click();
  await expect(page.locator('#projection-error')).toContainText('not embedded');
  await expect(page.locator('#projection-scenarios tbody tr')).toHaveCount(100);
});

function nativeProjection(args: string[], name: string): { url?: string; data?: any } {
  const file = join(directory, `${name}.${args.includes('--format') ? 'html' : 'json'}`);
  execFileSync(native, ['project', baseline, ...args, '-o', file], { cwd: root });
  if (args.includes('--format') && process.env.RPROF_UI_SKIP_CARGO_BUILD) {
    // The native executable may predate this UI-only build. Keep its actual artifact and embedded WASM,
    // substituting only the checked-in viewer assets currently under test.
    const source = readFileSync(file, 'utf8');
    writeFileSync(file, source.replace(/<style>[\s\S]*?<\/style>/,
      `<style>${readFileSync(join(root, 'ui/dist/viewer.css'), 'utf8')}</style>`)
      .replace(/<script>([\s\S]*?)<\/script>/,
        `<script>${readFileSync(join(root, 'ui/dist/viewer.js'), 'utf8')}</script>`));
  }
  return args.includes('--format') ? { url: pathToFileURL(file).href } : { data: JSON.parse(readFileSync(file, 'utf8')) };
}
async function downloaded(page: Page, button: string): Promise<any> {
  const pending = page.waitForEvent('download');
  await page.getByRole('button', { name: button }).click();
  const file = await (await pending).path();
  if (!file) throw new Error('No downloaded JSON file');
  return JSON.parse(readFileSync(file, 'utf8'));
}
async function edit(page: Page, change: (data: any) => void): Promise<void> {
  const editor = page.locator('#projection-editor');
  const data = JSON.parse(await editor.inputValue());
  change(data);
  await editor.fill(JSON.stringify(data));
  await page.getByRole('button', { name: 'Recalculate' }).click();
  await expect(page.locator('#projection-recalculate')).toBeEnabled();
  await expect(page.locator('#projection-error')).toBeHidden();
}
function nativeFromRequest(request: unknown, name: string): any {
  const input = join(directory, `${name}-request.json`);
  writeFileSync(input, JSON.stringify(request));
  return nativeProjection(['--request', input], name).data;
}

test('native HTML and offline WASM agree on original and edited requests; invalid inputs preserve the last valid result', async ({ page }) => {
  test.setTimeout(60_000);
  const requests: string[] = [];
  page.on('request', request => { if (/^https?:/.test(request.url())) requests.push(request.url()); });
  const args = ['--source-hardware', 'fixtures/projection/source.json', '--target-hardware', 'fixtures/projection/target.json',
    '--workload-model', 'fixtures/projection/workload.json'];
  const original = nativeProjection(args, 'original').data;
  const { url } = nativeProjection([...args, '--format', 'html'], 'original');
  await page.context().setOffline(true);
  await page.goto(url!);
  await expect(page.locator('.projection-provenance')).toHaveText('SYNTHETIC BASELINE');
  await expect(page.locator('#projection-scenarios tbody tr')).toContainText('120 / 120 / 120');
  await expect(page.locator('#projection-result')).toContainText('not guaranteed extrema');
  await expect(page.locator('#projection-result')).not.toContainText('confidence interval (95%');
  await page.getByRole('button', { name: 'Recalculate' }).click();
  await expect(page.locator('#projection-error')).toBeHidden();
  expect(await downloaded(page, 'Download artifact JSON')).toEqual(original);
  const immutable = original.request;

  // Both independent target GPU bottlenecks must change for a predictable shift; Rust supplies the arithmetic.
  await edit(page, data => {
    for (const name of ['compute', 'memory']) {
      const metric = data.targets[0].resources.gpu.metrics[name];
      metric.low = metric.central = metric.high = 4e9;
    }
  });
  const edited = await downloaded(page, 'Download artifact JSON');
  const request = await downloaded(page, 'Download request JSON');
  expect(request.baseline).toEqual(immutable.baseline);
  expect(request.validation).toEqual(immutable.validation);
  expect(edited).toEqual(nativeFromRequest(request, 'changed-rates'));
  expect(edited.scenarios[0].runtime.central_ns).not.toBe(original.scenarios[0].runtime.central_ns);
  await expect(page.locator('#projection-scenarios tbody tr')).toContainText(edited.scenarios[0].runtime.central_ns);

  await page.getByRole('button', { name: 'Restore original' }).click();
  await edit(page, data => { data.scaling = 'strong'; data.nodes = [1, 2, 4]; });
  const sweepRequest = await downloaded(page, 'Download request JSON');
  const sweep = await downloaded(page, 'Download artifact JSON');
  expect(sweep).toEqual(nativeFromRequest(sweepRequest, 'strong-sweep'));
  expect(sweep.scenarios.map((scenario: any) => scenario.nodes)).toEqual([1, 2, 4]);
  expect(sweep.scenarios.every((scenario: any) => scenario.status === 'estimated')).toBe(true);
  expect(sweep.scenarios.map((scenario: any) => scenario.runtime.central_ns)).toEqual(['120', '80', '60']);
  await expect(page.locator('#projection-scenarios tbody tr')).toHaveCount(3);

  // Invalid model input is rejected rather than silently adopting it or changing the displayed last valid result.
  const lastValid = sweep;
  const editor = page.locator('#projection-editor');
  const invalid = JSON.parse(await editor.inputValue());
  invalid.targets[0].resources.gpu.metrics.compute.central = 0;
  invalid.targets[0].resources.gpu.metrics.compute.low = 0;
  await editor.fill(JSON.stringify(invalid));
  await page.getByRole('button', { name: 'Recalculate' }).click();
  await expect(page.locator('#projection-error')).toBeVisible();
  await expect(page.locator('#projection-error')).toContainText('throughput must be positive');
  expect(await downloaded(page, 'Download artifact JSON')).toEqual(lastValid);
  expect(await downloaded(page, 'Download request JSON')).toEqual(sweepRequest);

  await editor.fill(JSON.stringify({ ...invalid, nodes: [1], targets: [{ ...invalid.targets[0], resources: {
    ...invalid.targets[0].resources, gpu: { ...invalid.targets[0].resources.gpu,
      capacity_bytes_per_node: '1024', metrics: sweepRequest.targets[0].resources.gpu.metrics },
  } }] }));
  await page.getByRole('button', { name: 'Recalculate' }).click();
  await expect(page.locator('#projection-error')).toBeHidden();
  const capacityRequest = await downloaded(page, 'Download request JSON');
  const capacity = await downloaded(page, 'Download artifact JSON');
  expect(capacity).toEqual(nativeFromRequest(capacityRequest, 'insufficient-capacity'));
  expect(capacity.scenarios.every((scenario: any) => scenario.status === 'infeasible' && scenario.runtime === null)).toBe(true);
  await expect(page.locator('#projection-scenarios tbody tr').first()).toContainText('Unavailable');
  expect(requests).toEqual([]);
});

test('a stale embedded engine cannot silently replace native projection results', async ({ page }) => {
  const args = ['--source-hardware', 'fixtures/projection/source.json', '--target-hardware', 'fixtures/projection/target.json',
    '--workload-model', 'fixtures/projection/workload.json'];
  const original = nativeProjection(args, 'stale-engine').data;
  const { url } = nativeProjection([...args, '--format', 'html'], 'stale-engine');
  const path = fileURLToPath(url!);
  writeFileSync(path, readFileSync(path, 'utf8').replace(original.hashes.engine_source, '0'.repeat(64)));
  await page.goto(url!);
  await page.getByRole('button', { name: 'Recalculate' }).click();
  await expect(page.locator('#projection-error')).toContainText('does not match this artifact');
  await expect(page.locator('#projection-scenarios tbody tr')).toContainText('120 / 120 / 120');
});
