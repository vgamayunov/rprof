import { test, expect, type Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

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

async function clickTime(page: Page, ns: number, y: number, extent = 40_500_000): Promise<void> {
  const canvas = page.locator('#timeline-canvas');
  const box = await canvas.boundingBox();
  expect(box).not.toBeNull();
  await canvas.click({ position: { x: 260 + (box!.width - 260) * ns / extent, y } });
}

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
  await clickTime(page, 10_000_000, 82);
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
  await clickTime(page, 10_000_000, 214);
  await expect(page.locator('#detail-content')).toContainText('stencil kernel');
  await expect(page.locator('#detail-content')).toContainText('8000000 ns');
  await expect(page.locator('#detail-content')).toContainText('checkpoint after kernel');
  await page.locator('#filter-input').fill('');
  await page.locator('#domain-select').selectOption('1');
  await clickTime(page, 10_000_000, 82, 40_000_000);
  await expect(page.locator('#detail-content')).toContainText('00000000-0000-0000-0000-000000000003:0');
  await page.locator('#domain-select').selectOption('0');
  await page.locator('#timeline-scroll').evaluate(element => { element.scrollTop = 200; });
  await expect.poll(() => page.locator('#timeline-scroll').evaluate(element => element.scrollTop)).toBeGreaterThan(0);
  expect(errors).toEqual([]);
  expect(requests).toEqual([]);
});

test('integer counters above 2^53 remain distinct and inspectable', async ({ page }) => {
  await loaded(page);
  await page.locator('#filter-input').fill('cycles rank:0');
  await clickTime(page, 2_000_000, 243);
  await expect(page.locator('#detail-content')).toContainText('9007199254740993');
  await clickTime(page, 3_000_000, 233);
  await expect(page.locator('#detail-content')).toContainText('9007199254740994');
});

test('hostile strings remain text and retention/partial status is visible', async ({ page }) => {
  const source = readFileSync(fixture, 'utf8');
  const hostile = '</script><script>globalThis.rprofInjected=true</script><img src=x onerror="globalThis.rprofInjected=true">';
  const input = join(directory, 'hostile.jsonl');
  writeFileSync(input, source.replaceAll('"solver step"', JSON.stringify(hostile)));
  await loaded(page, render(input, 'hostile'));
  await clickTime(page, 10_000_000, 82);
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
