import { averageNs, COUNTER_PLOT_HEIGHT, COUNTER_PLOT_TOP, formatNs, indexReport, integerPlotValues, isCollectorHealth, lowerBound, matchesFilter, observedRate, regionThreads, SPAN_PITCH, SPAN_TOP, timelineRows, timeToX, visibleSpans, xToTime } from './model.js';
import type { Domain, IndexedCounter, IndexedSpan, IoSummary, OpenmpRegion, OpenmpRuntime, Report, Shard, TimelineRow, Track, TrackData } from './model.js';
import { canvasThemes, initialTheme, rememberTheme } from './theme.js';
import type { Theme } from './theme.js';

const startingTheme = initialTheme();
document.documentElement.dataset.theme = startingTheme;

type Selection = { kind: 'span'; item: IndexedSpan } | { kind: 'counter'; item: IndexedCounter };
type Hit = { x: number; y: number; width: number; height: number; selection: Selection };

const LABEL_WIDTH = 260;
const MAX_DRAWN_SPANS = 5000;
const IO_PAGE_SIZE = 100;
const COLORS = ['#6bcdb0', '#65a8ec', '#e5b46d', '#b29be6', '#e17d91', '#86c6d9', '#cbc988'];
const MPI_COLOR = '#ff9970';
const OPENMP_COLOR = '#d5a2ff';

function node<K extends keyof HTMLElementTagNameMap>(tag: K, className = '', text?: string): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}

function put(parent: Element, tag: keyof HTMLElementTagNameMap, label: string, value: string): void {
  const row = node('div', 'detail-row');
  row.append(node('dt', '', label), node('dd', '', value));
  parent.append(row);
}

function badge(parent: Element, label: string, tone = ''): void {
  parent.append(node('span', `badge ${tone}`, label));
}

function colorFor(value: string, colors: readonly string[] = COLORS): string {
  if (value.toLowerCase() === 'mpi') return MPI_COLOR;
  if (value.toLowerCase() === 'openmp') return OPENMP_COLOR;
  let hash = 0;
  for (let i = 0; i < value.length; i++) hash = (Math.imul(hash, 31) + value.charCodeAt(i)) | 0;
  return colors[(hash >>> 0) % colors.length]!;
}

function clamp(value: number, min: number, max: number): number { return Math.min(max, Math.max(min, value)); }
function optionalNs(value: string | null): string { return value === null ? '—' : `${value} ns`; }

class Viewer {
  private readonly report: Report;
  private readonly data: Map<string, TrackData>;
  private readonly root: HTMLElement;
  private readonly domainSelect: HTMLSelectElement;
  private readonly filter: HTMLInputElement;
  private readonly scroll: HTMLElement;
  private readonly spacer: HTMLElement;
  private readonly canvas: HTMLCanvasElement;
  private readonly ruler: HTMLCanvasElement;
  private readonly detail: HTMLElement;
  private readonly rangeLabel: HTMLElement;
  private readonly tooltip: HTMLElement;
  private readonly collapsed = new Set<string>();
  private readonly hits: Hit[] = [];
  private rows: TimelineRow[] = [];
  private domain: Domain | undefined;
  private start = 0n;
  private nsPerPx = 1;
  private selection: Selection | undefined;
  private related = new Set<string>();
  private drag: { x: number; y: number; start: bigint; moved: boolean } | undefined;
  private frame = 0;
  private ioPage = 0;
  private operationPage = 0;
  private selectedIo: IoSummary | undefined;
  private regionPage = 0;
  private threadPage = 0;
  private selectedRegion: OpenmpRegion | undefined;
  private runtimePage = 0;
  private callbackPage = 0;
  private selectedRuntime: OpenmpRuntime | undefined;
  private theme: Theme = startingTheme;
  private colors = canvasThemes[startingTheme];

  constructor(report: Report, root: HTMLElement) {
    this.report = report;
    this.data = indexReport(report);
    this.root = root;
    // Only application-owned static markup is used here. All report strings enter via textContent or Canvas.
    root.innerHTML = `<header class="topbar"><div class="brand"><div class="mark">r/</div><div><strong>rprof</strong><small>OFFLINE TIMELINE</small></div></div><div class="header-actions"><div class="header-meta" id="header-meta"></div><button id="theme-toggle" type="button"></button></div></header>
      <section class="overview"><div class="overview-copy"><div class="eyebrow">CAPTURE OVERVIEW</div><h1 id="session-title">Session</h1><p id="overview-subtitle"></p></div><div class="stat-grid" id="stat-grid"></div></section>
      <div class="workspace"><section class="timeline-section"><div class="toolbar"><div class="toolbar-top"><div class="toolbar-heading"><h2>Timeline</h2><span id="domain-note">CLOCK DOMAINS ARE UNALIGNED</span></div><div class="zoom-controls"><button id="zoom-out" type="button" title="Zoom out" aria-label="Zoom out">−</button><button id="zoom-in" type="button" title="Zoom in" aria-label="Zoom in">+</button><button id="fit-view" type="button">Fit all</button></div></div><div class="toolbar-fields"><label class="field">Clock domain <select id="domain-select" aria-label="Clock domain"></select></label><label class="field search">Filter <input id="filter-input" type="search" placeholder="Name, category, rank…" aria-label="Filter tracks and events"></label><span id="range-label" class="range-label"></span></div></div><div class="ruler-wrap"><div class="track-header">TRACKS <span>▾ collapse</span></div><canvas id="time-ruler" aria-label="Timeline time axis"></canvas></div><div class="plot-scroll" id="timeline-scroll" tabindex="0" aria-label="Timeline tracks; scroll vertically, wheel over plot to zoom, drag to pan"><canvas id="timeline-canvas" aria-label="Interactive timeline spans and counters"></canvas><div id="timeline-spacer"></div></div><div class="timeline-footer"><span><i class="legend-swatch span-key"></i> span <i class="legend-swatch counter-key"></i> counter <i class="legend-swatch incomplete-key"></i> incomplete</span><span>Wheel: zoom · Drag: pan · Shift + wheel: scroll</span></div></section><aside class="sidebar"><section class="side-card" id="detail-panel"><div class="side-title">INSPECTOR</div><div id="detail-content" class="detail-content"></div></section><section class="side-card"><div class="side-title">CATEGORY SUMMARY <span>INCLUSIVE TIME</span></div><div class="category-wrap"><table id="category-table"><thead><tr><th>Category</th><th>Spans</th><th>Time</th></tr></thead><tbody></tbody></table></div><p class="footnote">Inclusive durations may overlap and double-count nested work. Counts cover the full capture, not just retained events.</p></section><section class="side-card"><div class="side-title">CAPTURE STATUS</div><div id="capture-status"></div></section></aside></div>`;
    this.domainSelect = this.get('domain-select') as HTMLSelectElement;
    // Static structure only: measured file names and values are always inserted as text nodes.
    root.insertAdjacentHTML('beforeend', `<section class="io-section" id="io-panel"><div class="io-heading"><div><h2>Per-file I/O</h2><p>Selected clock domain only · completed / requested bytes · observed-window bandwidth (bytes / window, not physical device bandwidth)</p></div><span id="io-count"></span></div><div class="io-wrap"><table id="io-table"><thead><tr><th>Host / file</th><th>Operations</th><th>Errors</th><th>Read (completed / requested)</th><th>Write (completed / requested)</th><th>Read bandwidth</th><th>Write bandwidth</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="io-pager"></div><div id="io-detail"></div></section>`);
    if (report.openmp) root.insertAdjacentHTML('beforeend', `<section class="io-section openmp-section" id="openmp-panel"><div class="io-heading"><div><h2>OpenMP</h2><p>Selected clock domain · inclusive recorded work / wait (may overlap) · work spread is observed only</p></div><span id="openmp-count"></span></div><p class="io-note" id="openmp-tasks"></p><div class="io-wrap"><table id="openmp-regions"><thead><tr><th>Region · process</th><th>Threads requested / observed</th><th>Region duration</th><th>Recorded work</th><th>Recorded wait</th><th>Observed work min / max / spread</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="openmp-region-pager"></div><div id="openmp-detail"></div><details id="openmp-runtimes"><summary>Runtime and callback registration details <span id="openmp-runtime-count"></span></summary><div id="openmp-runtime-list"></div><div class="io-pager" id="openmp-runtime-pager"></div><div id="openmp-callback-detail"></div></details></section>`);
    const legend = root.querySelector('.timeline-footer span')!;
    legend.append(node('i', 'legend-swatch mpi-key'), document.createTextNode(' MPI'));
    if (report.openmp) legend.append(node('i', 'legend-swatch openmp-key'), document.createTextNode(' OpenMP'));
    this.tooltip = node('div', 'timeline-tooltip');
    this.tooltip.id = 'timeline-tooltip';
    this.tooltip.setAttribute('role', 'tooltip');
    this.tooltip.hidden = true;
    root.append(this.tooltip);
    this.filter = this.get('filter-input') as HTMLInputElement;
    this.scroll = this.get('timeline-scroll');
    this.spacer = this.get('timeline-spacer');
    this.canvas = this.get('timeline-canvas') as HTMLCanvasElement;
    this.ruler = this.get('time-ruler') as HTMLCanvasElement;
    this.detail = this.get('detail-content');
    this.rangeLabel = this.get('range-label');
    this.setupOverview();
    this.get('theme-toggle').addEventListener('click', () => {
      this.setTheme(this.theme === 'dark' ? 'light' : 'dark');
      rememberTheme(this.theme);
    });
    this.setTheme(this.theme);
    this.get('domain-note').textContent = report.domains.length > 1 ? 'CLOCK DOMAINS ARE UNALIGNED' : 'SAME-DOMAIN MONOTONIC TIME';
    for (const domain of report.domains) {
      const option = node('option', '', `${domain.name} · ${domain.id}${report.domains.length > 1 ? ' (unaligned)' : ''}`);
      option.value = domain.id;
      this.domainSelect.append(option);
    }
    this.domainSelect.disabled = !report.domains.length;
    this.domainSelect.addEventListener('change', () => this.setDomain(this.report.domains.find(d => d.id === this.domainSelect.value)));
    this.filter.addEventListener('input', () => { this.buildRows(); this.schedule(); });
    this.get('zoom-in').addEventListener('click', () => this.zoom(0.5));
    this.get('zoom-out').addEventListener('click', () => this.zoom(2));
    this.get('fit-view').addEventListener('click', () => this.fit());
    this.scroll.addEventListener('scroll', () => { this.hideTooltip(); this.schedule(); });
    this.scroll.addEventListener('keydown', event => {
      if (event.key === '+' || event.key === '=') { this.zoom(0.5); event.preventDefault(); }
      else if (event.key === '-') { this.zoom(2); event.preventDefault(); }
      else if (event.key === 'Home') { this.fit(); event.preventDefault(); }
      else if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
        const step = BigInt(Math.round(this.plotWidth() * this.nsPerPx / 5));
        this.start = this.clampStart(this.start + (event.key === 'ArrowRight' ? step : -step));
        this.schedule(); event.preventDefault();
      }
    });
    this.canvas.addEventListener('wheel', event => {
      event.preventDefault();
      if (event.shiftKey || event.clientX - this.canvas.getBoundingClientRect().left < LABEL_WIDTH) {
        this.scroll.scrollTop += event.deltaY;
      } else {
        this.zoom(Math.exp(clamp(event.deltaY, -500, 500) * 0.0015), event.offsetX - LABEL_WIDTH);
      }
    }, { passive: false });
    this.canvas.addEventListener('pointerdown', event => {
      if (event.button !== 0) return;
      this.canvas.setPointerCapture(event.pointerId);
      this.drag = { x: event.clientX, y: event.clientY, start: this.start, moved: false };
    });
    this.canvas.addEventListener('pointermove', event => {
      this.hover(event);
      if (!this.drag) return;
      const dx = event.clientX - this.drag.x;
      if (Math.abs(dx) + Math.abs(event.clientY - this.drag.y) > 4) this.drag.moved = true;
      if (this.drag.moved && this.drag.x - this.canvas.getBoundingClientRect().left >= LABEL_WIDTH) {
        this.start = this.clampStart(this.drag.start - BigInt(Math.round(dx * this.nsPerPx)));
        this.schedule();
      }
    });
    this.canvas.addEventListener('pointerup', event => {
      if (!this.drag) return;
      const moved = this.drag.moved;
      this.drag = undefined;
      if (!moved) this.onClick(event.offsetX, event.offsetY);
    });
    this.canvas.addEventListener('pointercancel', () => { this.drag = undefined; });
    this.canvas.addEventListener('pointerleave', () => this.hideTooltip());
    new ResizeObserver(() => { this.buildRows(); this.schedule(); }).observe(this.scroll);
    window.addEventListener('resize', () => { this.buildRows(); this.schedule(); });
    this.setDomain(report.domains[0]);
    this.showDetails();
  }

  private get(id: string): HTMLElement { return this.root.querySelector<HTMLElement>(`#${id}`)!; }
  private setTheme(theme: Theme): void {
    this.theme = theme;
    this.colors = canvasThemes[theme];
    document.documentElement.dataset.theme = theme;
    const target = theme === 'dark' ? 'light' : 'dark';
    const button = this.get('theme-toggle');
    button.textContent = target === 'light' ? 'Light mode' : 'Dark mode';
    button.setAttribute('aria-label', `Switch to ${target} theme`);
    this.schedule();
  }
  private plotWidth(): number { return Math.max(1, this.scroll.clientWidth - LABEL_WIDTH); }
  private extent(): bigint { return this.domain ? BigInt(this.domain.end_ns) : 0n; }
  private visibleDuration(): bigint { return BigInt(Math.max(1, Math.round(this.plotWidth() * this.nsPerPx))); }
  private clampStart(value: bigint): bigint { return value < 0n ? 0n : value > this.extent() - this.visibleDuration() ? (this.extent() > this.visibleDuration() ? this.extent() - this.visibleDuration() : 0n) : value; }

  private setupOverview(): void {
    const { summary, provenance } = this.report;
    this.get('session-title').textContent = summary.session_id || 'Untitled capture';
    this.get('overview-subtitle').textContent = `${summary.format} · ${provenance.label || provenance.kind} · ${provenance.source}`;
    const meta = this.get('header-meta');
    badge(meta, provenance.kind.toUpperCase(), provenance.kind === 'synthetic' ? 'amber' : 'green');
    badge(meta, summary.complete ? 'COMPLETE' : 'PARTIAL', summary.complete ? 'green' : 'red');
    const stats = this.get('stat-grid');
    for (const [label, value] of [['TRACKS', summary.tracks], ['SPANS', summary.spans], ['COUNTERS', summary.counters], ['DEPENDENCIES', summary.dependencies]]) {
      const card = node('div', 'stat');
      card.append(node('strong', '', value), node('span', '', label));
      stats.append(card);
    }
    const body = this.get('category-table').querySelector('tbody')!;
    for (const category of summary.categories) {
      const row = node('tr');
      row.append(node('td', '', category.name), node('td', '', category.count), node('td', '', formatNs(BigInt(category.total_ns))));
      body.append(row);
    }
    if (!summary.categories.length) {
      const cell = node('td', 'empty-table', 'No categories recorded'); cell.colSpan = 3;
      const row = node('tr'); row.append(cell); body.append(row);
    }
    const status = this.get('capture-status');
    const flags = node('div', 'status-flags');
    badge(flags, summary.complete ? 'Complete' : 'Partial capture', summary.complete ? 'green' : 'red');
    if (summary.truncated_tail) badge(flags, 'Truncated tail', 'red');
    if (BigInt(summary.dropped) > 0n) badge(flags, `${summary.dropped} dropped`, 'red');
    if (BigInt(summary.omitted_events) > 0n) badge(flags, `${summary.omitted_events} omitted`, 'amber');
    status.append(flags);
    const dl = node('dl', 'details-list');
    put(dl, 'div', 'Provenance', `${provenance.kind} · ${provenance.label}`);
    put(dl, 'div', 'Source', provenance.source);
    put(dl, 'div', 'Recovered inputs', summary.recovered_inputs);
    put(dl, 'div', 'Collector loss', summary.dropped);
    put(dl, 'div', 'Retention omitted', summary.omitted_events);
    const tracks = new Map(this.report.tracks.map(track => [track.id, track]));
    const health = new Map<string, { label: string; values: Map<string, string> }>();
    for (const counter of this.report.counters.filter(counter => isCollectorHealth(counter.name))) {
      const collector = counter.name.split('.')[0]!;
      const track = tracks.get(counter.track);
      const key = `${collector}:${track?.shard ?? counter.track}`;
      if (!health.has(key)) health.set(key, { label: `${collector.toUpperCase()} · ${track?.rank == null ? (track?.shard.slice(0, 8) ?? 'capture') : `rank ${track.rank}`}`, values: new Map() });
      health.get(key)!.values.set(counter.name.slice(collector.length + 1), counter.value);
    }
    for (const group of health.values()) {
      const value = group.values;
      const parts: string[] = [];
      if (value.has('attempted_events')) parts.push(`${value.get('received_events') ?? '—'}/${value.get('attempted_events')} received`);
      if (value.has('ingress_dropped_events')) parts.push(`${value.get('ingress_dropped_events')} dropped`);
      if (value.has('initialized_images')) parts.push(`${value.get('initialized_images')} initialized images`);
      if (value.has('clock.uncertainty_ns')) parts.push(`clock ±${value.get('clock.uncertainty_ns')} ns`);
      if (parts.length) put(dl, 'div', group.label, parts.join(' · '));
    }
    status.append(dl);
    if (summary.warnings.length) {
      const warnings = node('div', 'warnings');
      warnings.append(node('strong', '', 'Warnings'));
      for (const warning of summary.warnings) warnings.append(node('p', '', warning));
      status.append(warnings);
    }
  }

  private setDomain(domain: Domain | undefined): void {
    this.domain = domain;
    this.get('domain-note').textContent = domain?.calibration ? 'ESTIMATED MPI CLOCK ALIGNMENT' : this.report.domains.length > 1 ? 'CLOCK DOMAINS ARE UNALIGNED' : 'SAME-DOMAIN MONOTONIC TIME';
    this.hideTooltip();
    this.ioPage = 0;
    this.selectedIo = undefined;
    this.renderIo();
    this.regionPage = 0;
    this.threadPage = 0;
    this.selectedRegion = undefined;
    this.runtimePage = 0;
    this.callbackPage = 0;
    this.selectedRuntime = undefined;
    if (this.report.openmp) this.renderOpenmp();
    this.selection = undefined;
    this.related.clear();
    this.showDetails();
    if (domain) this.domainSelect.value = domain.id;
    this.buildRows();
    this.fit();
    this.schedule();
  }

  private renderIo(): void {
    const files = (this.report.io ?? []).filter(file => file.domain === this.domain?.id);
    const tbody = this.get('io-table').querySelector('tbody')!;
    tbody.replaceChildren();
    this.get('io-count').textContent = `${files.length} files`;
    for (const file of files.slice(this.ioPage * IO_PAGE_SIZE, (this.ioPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr');
      const button = node('button', 'io-file', `${file.host} · ${file.file}`);
      button.type = 'button';
      button.addEventListener('click', () => { this.selectedIo = file; this.operationPage = 0; this.renderIoDetail(); });
      const identity = node('td'); identity.append(button);
      row.append(identity, node('td', '', file.operations), node('td', '', file.errors),
        node('td', '', `${file.read_bytes} / ${file.requested_read_bytes} B`),
        node('td', '', `${file.write_bytes} / ${file.requested_write_bytes} B`),
        node('td', '', observedRate(file.read_bytes, file.window_ns)),
        node('td', '', observedRate(file.write_bytes, file.window_ns)));
      tbody.append(row);
    }
    if (!files.length) {
      const row = node('tr'); const cell = node('td', 'empty-table', 'No per-file I/O recorded in this clock domain');
      cell.colSpan = 7; row.append(cell); tbody.append(row);
    }
    this.renderPager(this.get('io-pager'), files.length, this.ioPage, page => { this.ioPage = page; this.renderIo(); });
    this.renderIoDetail();
  }

  private renderPager(parent: HTMLElement, total: number, page: number, change: (page: number) => void): void {
    parent.replaceChildren();
    if (total <= IO_PAGE_SIZE) return;
    const previous = node('button', '', 'Previous'); previous.type = 'button'; previous.disabled = page === 0;
    previous.addEventListener('click', () => change(page - 1));
    const next = node('button', '', 'Next'); next.type = 'button'; next.disabled = (page + 1) * IO_PAGE_SIZE >= total;
    next.addEventListener('click', () => change(page + 1));
    parent.append(previous, node('span', '', `Rows ${page * IO_PAGE_SIZE + 1}–${Math.min(total, (page + 1) * IO_PAGE_SIZE)} of ${total}`), next);
  }

  private renderIoDetail(): void {
    const detail = this.get('io-detail');
    detail.replaceChildren();
    const file = this.selectedIo;
    if (!file) return;
    detail.append(node('h3', '', `${file.host} · ${file.file}`),
      node('p', 'io-note', `Observed window: ${file.window_ns} ns · Total operation latency: ${file.total_ns} ns`));
    const wrap = node('div', 'io-wrap');
    const table = node('table'); table.id = 'io-operations';
    const head = node('thead'); const heading = node('tr');
    for (const label of ['Operation', 'Count', 'Average latency', 'Max latency', 'Errors']) heading.append(node('th', '', label));
    head.append(heading); table.append(head);
    const body = node('tbody');
    for (const operation of file.by_operation.slice(this.operationPage * IO_PAGE_SIZE, (this.operationPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr');
      row.append(node('td', '', operation.name), node('td', '', operation.count),
        node('td', '', averageNs(operation.total_ns, operation.count)),
        node('td', '', `${operation.max_ns} ns`), node('td', '', operation.errors));
      body.append(row);
    }
    if (!file.by_operation.length) {
      const row = node('tr'); const cell = node('td', 'empty-table', 'No operation breakdown recorded');
      cell.colSpan = 5; row.append(cell); body.append(row);
    }
    table.append(body); wrap.append(table); detail.append(wrap);
    const pager = node('div', 'io-pager'); pager.id = 'io-operation-pager'; detail.append(pager);
    this.renderPager(pager, file.by_operation.length, this.operationPage, page => { this.operationPage = page; this.renderIoDetail(); });
  }

  private renderOpenmp(): void {
    const summary = this.report.openmp;
    if (!summary) return;
    const regions = summary.regions.filter(region => region.domain === this.domain?.id);
    const runtimes = summary.runtimes.filter(runtime => runtime.domain === this.domain?.id);
    this.get('openmp-count').textContent = `${regions.length} regions`;
    this.get('openmp-tasks').textContent = `Capture-wide task totals: ${summary.tasks_created} created · ${summary.tasks_completed} completed · ${summary.execution_segments} execution segments (not filtered by domain)`;
    const body = this.get('openmp-regions').querySelector('tbody')!;
    body.replaceChildren();
    for (const region of regions.slice(this.regionPage * IO_PAGE_SIZE, (this.regionPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr');
      const identity = node('td');
      const button = node('button', 'io-file', `Region ${region.parallel_id} · PID ${region.pid} · instance ${region.instance} · shard ${region.shard}`);
      button.type = 'button';
      button.addEventListener('click', () => { this.selectedRegion = region; this.threadPage = 0; this.renderOpenmpDetail(); });
      identity.append(button);
      row.append(identity, node('td', '', `${region.requested_threads} / ${region.observed_threads}`),
        node('td', '', optionalNs(region.duration_ns)), node('td', '', `${region.work_ns} ns`),
        node('td', '', `${region.wait_ns} ns`),
        node('td', '', `${optionalNs(region.work_min_ns)} / ${optionalNs(region.work_max_ns)} / ${optionalNs(region.work_spread_ns)}`));
      body.append(row);
    }
    if (!regions.length) {
      const row = node('tr'); const cell = node('td', 'empty-table', 'No OpenMP regions recorded in this clock domain');
      cell.colSpan = 6; row.append(cell); body.append(row);
    }
    this.renderPager(this.get('openmp-region-pager'), regions.length, this.regionPage, page => { this.regionPage = page; this.renderOpenmp(); });
    this.renderOpenmpDetail();
    this.get('openmp-runtime-count').textContent = `· ${runtimes.length} runtimes in this domain`;
    const runtimeList = this.get('openmp-runtime-list'); runtimeList.replaceChildren();
    for (const runtime of runtimes.slice(this.runtimePage * IO_PAGE_SIZE, (this.runtimePage + 1) * IO_PAGE_SIZE)) {
      const line = node('div', 'openmp-runtime-row');
      const select = node('button', 'io-file', `PID ${runtime.pid} · ${runtime.instance} · shard ${runtime.shard}`);
      select.type = 'button';
      select.addEventListener('click', () => { this.selectedRuntime = runtime; this.callbackPage = 0; this.renderOpenmpCallbacks(); });
      line.append(select, node('span', '', `Runtime ${runtime.version} · OpenMP ${runtime.omp_version} · initialized: ${runtime.initialized} · finalized: ${runtime.finalized} · sequence gaps: ${runtime.sequence_gaps} · unmatched endpoints: ${runtime.unmatched_endpoints} · abandoned intervals: ${runtime.abandoned_intervals}`));
      runtimeList.append(line);
    }
    if (!runtimes.length) runtimeList.append(node('p', 'io-note', 'No OpenMP runtime recorded in this clock domain; instrumentation may be unsupported or not initialized.'));
    this.renderPager(this.get('openmp-runtime-pager'), runtimes.length, this.runtimePage, page => { this.runtimePage = page; this.renderOpenmp(); });
    this.renderOpenmpCallbacks();
  }

  private renderOpenmpDetail(): void {
    const detail = this.get('openmp-detail'); detail.replaceChildren();
    const region = this.selectedRegion;
    if (!region) return;
    const threads = regionThreads(this.report, region);
    detail.append(node('h3', '', `Region ${region.parallel_id} · PID ${region.pid}`),
      node('p', 'io-note', 'Recorded inclusive intervals; scheduled time includes logical task context (including waits), not CPU utilization or CPU cycles. Unknown durations are shown as —.'));
    const wrap = node('div', 'io-wrap'); const table = node('table'); table.id = 'openmp-threads';
    const head = node('thead'); const heading = node('tr');
    for (const label of ['Thread ID', 'Team index', 'Work', 'Wait', 'Sync', 'Scheduled interval', 'Execution segments']) heading.append(node('th', '', label));
    head.append(heading); table.append(head);
    const body = node('tbody');
    for (const thread of threads.slice(this.threadPage * IO_PAGE_SIZE, (this.threadPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr');
      row.append(node('td', '', thread.tid), node('td', '', thread.team_index ?? '—'), node('td', '', `${thread.work_ns} ns`),
        node('td', '', `${thread.wait_ns} ns`), node('td', '', `${thread.sync_ns} ns`),
        node('td', '', `${thread.scheduled_ns} ns`), node('td', '', thread.execution_segments));
      body.append(row);
    }
    if (!threads.length) {
      const row = node('tr'); const cell = node('td', 'empty-table', 'No per-thread intervals recorded for this region');
      cell.colSpan = 7; row.append(cell); body.append(row);
    }
    table.append(body); wrap.append(table); detail.append(wrap);
    const pager = node('div', 'io-pager'); pager.id = 'openmp-thread-pager'; detail.append(pager);
    this.renderPager(pager, threads.length, this.threadPage, page => { this.threadPage = page; this.renderOpenmpDetail(); });
  }

  private renderOpenmpCallbacks(): void {
    const detail = this.get('openmp-callback-detail'); detail.replaceChildren();
    const runtime = this.selectedRuntime;
    if (!runtime) return;
    detail.append(node('h3', '', `Callbacks · PID ${runtime.pid} · ${runtime.instance}`),
      node('p', 'io-note', 'Registration status is not evidence that a callback was received. Unknown observed counts are not zero.'));
    const wrap = node('div', 'io-wrap'); const table = node('table'); table.id = 'openmp-callbacks';
    const head = node('thead'); const heading = node('tr');
    for (const label of ['Callback', 'Registration status', 'Received callbacks', 'Observation complete']) heading.append(node('th', '', label));
    head.append(heading); table.append(head);
    const body = node('tbody');
    for (const callback of runtime.callbacks.slice(this.callbackPage * IO_PAGE_SIZE, (this.callbackPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr'); row.append(node('td', '', callback.name), node('td', '', callback.status),
        node('td', '', callback.observed ?? 'Unknown'), node('td', '', callback.observed_complete ? 'Yes' : 'No'));
      body.append(row);
    }
    if (!runtime.callbacks.length) {
      const row = node('tr'); const cell = node('td', 'empty-table', 'No callback registration data recorded');
      cell.colSpan = 4; row.append(cell); body.append(row);
    }
    table.append(body); wrap.append(table); detail.append(wrap);
    const pager = node('div', 'io-pager'); pager.id = 'openmp-callback-pager'; detail.append(pager);
    this.renderPager(pager, runtime.callbacks.length, this.callbackPage, page => { this.callbackPage = page; this.renderOpenmpCallbacks(); });
  }

  private hitAt(x: number, y: number): Hit | undefined {
    for (let i = this.hits.length - 1; i >= 0; i--) {
      const hit = this.hits[i]!;
      if (x >= hit.x && x <= hit.x + hit.width && y >= hit.y && y <= hit.y + hit.height) return hit;
    }
    return undefined;
  }

  private hideTooltip(): void {
    this.tooltip.hidden = true;
    this.canvas.removeAttribute('title');
  }

  private hover(event: PointerEvent): void {
    if (this.drag) { this.hideTooltip(); return; }
    const bounds = this.canvas.getBoundingClientRect();
    const hit = this.hitAt(event.clientX - bounds.left, event.clientY - bounds.top);
    if (!hit) { this.hideTooltip(); return; }
    const label = hit.selection.kind === 'span' ? hit.selection.item.span.name : hit.selection.item.counter.name;
    this.tooltip.textContent = label;
    this.canvas.title = label;
    this.tooltip.hidden = false;
    this.tooltip.style.left = `${Math.max(8, Math.min(event.clientX + 12, window.innerWidth - this.tooltip.offsetWidth - 8))}px`;
    this.tooltip.style.top = `${Math.max(8, Math.min(event.clientY + 16, window.innerHeight - this.tooltip.offsetHeight - 8))}px`;
  }

  private fit(): void {
    const extent = this.extent();
    // Numeric conversion is restricted to the view-relative domain extent.
    this.nsPerPx = Math.max(1, Number(extent) / this.plotWidth());
    this.start = 0n;
    this.schedule();
  }

  private zoom(factor: number, cursorX = this.plotWidth() / 2): void {
    if (!this.domain) return;
    const anchor = xToTime(cursorX, this.start, this.nsPerPx);
    this.nsPerPx = clamp(this.nsPerPx * factor, 1, Math.max(1, Number(this.extent()) * 4 / this.plotWidth()));
    this.start = this.clampStart(anchor - BigInt(Math.round(cursorX * this.nsPerPx)));
    this.schedule();
  }

  private buildRows(): void {
    this.rows = timelineRows(this.report, this.data, this.domain?.id, this.filter.value, this.collapsed);
    const top = this.rows.at(-1)?.top ?? 0;
    const height = this.rows.at(-1)?.height ?? 0;
    this.scroll.style.height = `${Math.min(Math.max(260, Math.min(640, window.innerHeight * 0.64)), Math.max(260, top + height))}px`;
    this.spacer.style.height = `${Math.max(0, top + height - this.scroll.clientHeight)}px`;
    this.scroll.scrollTop = Math.min(this.scroll.scrollTop, Math.max(0, top + height - this.scroll.clientHeight));
  }

  private resize(canvas: HTMLCanvasElement, width: number, height: number): CanvasRenderingContext2D {
    const ratio = window.devicePixelRatio || 1;
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
    const pixelWidth = Math.max(1, Math.round(width * ratio));
    const pixelHeight = Math.max(1, Math.round(height * ratio));
    if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) { canvas.width = pixelWidth; canvas.height = pixelHeight; }
    const ctx = canvas.getContext('2d')!;
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, width, height);
    return ctx;
  }

  private schedule(): void {
    if (this.frame) return;
    this.frame = requestAnimationFrame(() => { this.frame = 0; this.draw(); });
  }

  private draw(): void {
    this.hideTooltip();
    const width = this.scroll.clientWidth;
    const height = this.scroll.clientHeight;
    if (!width || !height) return;
    const ctx = this.resize(this.canvas, width, height);
    this.hits.length = 0;
    ctx.fillStyle = this.colors.background; ctx.fillRect(0, 0, width, height);
    this.drawRuler(width);
    const from = lowerBound(this.rows, row => row.top + row.height > this.scroll.scrollTop);
    const viewEnd = this.start + this.visibleDuration();
    const anchors = new Map<string, { x: number; y: number }>();
    for (let i = from; i < this.rows.length; i++) {
      const row = this.rows[i]!;
      const y = row.top - this.scroll.scrollTop;
      if (y >= height) break;
      ctx.fillStyle = row.kind === 'shard' ? this.colors.shard : row.kind === 'rank' ? this.colors.rank : i % 2 ? this.colors.odd : this.colors.even;
      ctx.fillRect(0, y, width, row.height);
      ctx.strokeStyle = this.colors.border; ctx.beginPath(); ctx.moveTo(0, y + row.height - .5); ctx.lineTo(width, y + row.height - .5); ctx.stroke();
      ctx.save(); ctx.beginPath(); ctx.rect(LABEL_WIDTH + 1, y, width - LABEL_WIDTH - 1, row.height); ctx.clip();
      if (row.kind === 'track') this.drawSpans(ctx, row, y, viewEnd, anchors);
      if (row.kind === 'counter') this.drawCounter(ctx, row, y, viewEnd);
      ctx.restore();
      ctx.fillStyle = row.kind === 'shard' ? this.colors.heading : this.colors.label;
      ctx.font = row.kind === 'shard' || row.kind === 'rank' ? '600 12px system-ui' : '12px system-ui';
      const indent = Math.min(125, 15 + row.depth * 17);
      if (row.hasChildren) { ctx.fillStyle = this.colors.muted; ctx.fillText(this.collapsed.has(row.key) ? '▸' : '▾', indent, y + row.height / 2 + 4); }
      ctx.fillStyle = row.kind === 'shard' ? this.colors.heading : this.colors.label;
      const labelX = indent + (row.hasChildren ? 16 : 4);
      const maxWidth = LABEL_WIDTH - labelX - 10;
      ctx.fillText(row.label, labelX, y + row.height / 2 + 4, maxWidth);
      if (row.kind === 'track' && row.track && row.height >= 52) {
        ctx.font = '10px system-ui'; ctx.fillStyle = this.colors.muted;
        ctx.fillText(`${row.track.kind.replace('_', ' ')}${row.track.rank === null ? '' : ` · rank ${row.track.rank}`}`, labelX, y + row.height / 2 + 19, maxWidth);
      }
      if (row.kind === 'shard' && !row.shard.complete) {
        ctx.font = '10px system-ui'; ctx.fillStyle = this.colors.partial; ctx.fillText('PARTIAL', LABEL_WIDTH - 55, y + row.height / 2 + 4);
      }
    }
    this.drawLinks(ctx, anchors);
    ctx.fillStyle = this.colors.border; ctx.fillRect(LABEL_WIDTH - 1, 0, 1, height);
    if (!this.rows.length) {
      ctx.fillStyle = this.colors.muted; ctx.font = '14px system-ui';
      ctx.fillText(this.domain ? 'No tracks match this view. Try clearing the filter.' : 'No clock domains recorded. This capture has no timeline events.', 25, 50);
    }
    this.rangeLabel.textContent = this.domain ? `${formatNs(this.start)} – ${formatNs(this.start + this.visibleDuration())} · domain-relative${this.domain.calibration ? ` · estimated ±${this.domain.calibration.uncertainty_ns} ns` : this.report.domains.length > 1 ? ' · unaligned' : ''}` : 'No clock domain';
  }

  private drawRuler(width: number): void {
    const ctx = this.resize(this.ruler, Math.max(1, width - LABEL_WIDTH), 35);
    const plotWidth = width - LABEL_WIDTH;
    ctx.fillStyle = this.colors.ruler; ctx.fillRect(0, 0, plotWidth, 35);
    ctx.font = '11px ui-monospace, monospace'; ctx.fillStyle = this.colors.muted; ctx.strokeStyle = this.colors.tick;
    const duration = this.nsPerPx * 115;
    const exponent = 10 ** Math.floor(Math.log10(Math.max(1, duration)));
    const step = BigInt(Math.max(1, Math.round(([1, 2, 5, 10].find(n => n * exponent >= duration) ?? 10) * exponent)));
    const first = (this.start / step) * step;
    for (let tick = first, count = 0; count < 150 && tick <= this.start + this.visibleDuration(); tick += step, count++) {
      const x = timeToX(tick, this.start, this.nsPerPx);
      if (x < 0) continue;
      ctx.beginPath(); ctx.moveTo(x + .5, 26); ctx.lineTo(x + .5, 35); ctx.stroke();
      ctx.fillText(formatNs(tick), x + 5, 20);
    }
  }

  private highlighted(correlation: string | null): boolean {
    if (!correlation || this.selection?.kind !== 'span') return false;
    const selected = this.selection.item.span.correlation;
    return correlation === selected || this.related.has(correlation);
  }

  private drawSpans(ctx: CanvasRenderingContext2D, row: TimelineRow, y: number, end: bigint,
    anchors: Map<string, { x: number; y: number }>): void {
    const track = row.track!;
    const data = this.data.get(track.id);
    if (!data) return;
    const query = this.filter.value;
    const wholeTrack = matchesFilter(track.name, track.kind, track.rank, query);
    const spans = visibleSpans(data.spans, this.start, end, MAX_DRAWN_SPANS);
    for (const item of spans) {
      const span = item.span;
      if (query && !wholeTrack && !matchesFilter(`${track.name} ${span.name}`, span.category, track.rank, query)) continue;
      const x = LABEL_WIDTH + timeToX(item.start, this.start, this.nsPerPx);
      const endX = LABEL_WIDTH + timeToX(item.end, this.start, this.nsPerPx);
      const left = Math.max(LABEL_WIDTH + 1, x);
      const right = Math.min(this.scroll.clientWidth, Math.max(x + 2, endX));
      if (right <= left) continue;
      const spanY = y + SPAN_TOP + item.lane * SPAN_PITCH;
      const selected = this.selection?.kind === 'span' && this.selection.item.span.id === span.id;
      ctx.fillStyle = colorFor(span.category);
      ctx.globalAlpha = this.selection && !selected && !this.highlighted(span.correlation) ? .47 : .95;
      ctx.fillRect(left, spanY, right - left, 11);
      ctx.globalAlpha = 1;
      if (span.incomplete) { ctx.strokeStyle = this.colors.incomplete; ctx.setLineDash([2, 2]); ctx.strokeRect(left, spanY, right - left, 11); ctx.setLineDash([]); }
      if (selected || this.highlighted(span.correlation)) { ctx.strokeStyle = this.colors.selected; ctx.lineWidth = 2; ctx.strokeRect(left - 1, spanY - 1, right - left + 2, 13); ctx.lineWidth = 1; }
      if (right - left > 46) { ctx.fillStyle = '#10212d'; ctx.font = '10px system-ui'; ctx.fillText(span.name, left + 4, spanY + 9, right - left - 7); }
      this.hits.push({ x: left, y: spanY, width: Math.max(3, right - left), height: 12, selection: { kind: 'span', item } });
      if (span.correlation && !anchors.has(span.correlation)) anchors.set(span.correlation, { x: clamp((left + right) / 2, LABEL_WIDTH, this.scroll.clientWidth), y: spanY + 5 });
    }
  }

  private drawCounter(ctx: CanvasRenderingContext2D, row: TimelineRow, y: number, end: bigint): void {
    const samples = this.data.get(row.track!.id)?.counters.get(row.series!) ?? [];
    const first = lowerBound(samples, sample => sample.time >= this.start);
    const last = lowerBound(samples, sample => sample.time > end);
    const count = last - first;
    if (!count) return;
    const stride = Math.max(1, Math.ceil(count / 3000));
    const points: IndexedCounter[] = [];
    for (let i = first; i < last; i += stride) points.push(samples[i]!);
    const integer = points.every(point => point.counter.value_type === 'integer');
    const values = integer ? integerPlotValues(points.map(point => point.counter.value)) : points.map(point => Number(point.counter.value));
    const finite = values.filter(Number.isFinite);
    if (!finite.length) return;
    const min = Math.min(...finite);
    const max = Math.max(...finite);
    const range = max - min || 1;
    const color = colorFor(row.series!, this.colors.counters);
    ctx.strokeStyle = color; ctx.lineWidth = 1.5; ctx.beginPath();
    let started = false;
    for (let i = 0; i < points.length; i++) {
      const value = values[i]!;
      if (!Number.isFinite(value)) { started = false; continue; }
      const x = LABEL_WIDTH + timeToX(points[i]!.time, this.start, this.nsPerPx);
      const py = y + COUNTER_PLOT_TOP + COUNTER_PLOT_HEIGHT * (1 - (value - min) / range);
      if (!started) { ctx.moveTo(x, py); started = true; } else ctx.lineTo(x, py);
      const selected = this.selection?.kind === 'counter' && this.selection.item === points[i];
      ctx.fillStyle = selected ? this.colors.selected : color;
      ctx.fillRect(x - (selected ? 3 : 2), py - (selected ? 3 : 2), selected ? 6 : 4, selected ? 6 : 4);
      this.hits.push({ x: x - 5, y: py - 7, width: 10, height: 14, selection: { kind: 'counter', item: points[i]! } });
    }
    ctx.stroke(); ctx.lineWidth = 1;
    ctx.fillStyle = this.colors.muted; ctx.font = '10px system-ui';
    ctx.fillText(`${points.length} samples · ${row.series}`, LABEL_WIDTH + 8, y + row.height - 4);
  }

  private drawLinks(ctx: CanvasRenderingContext2D, anchors: Map<string, { x: number; y: number }>): void {
    if (this.selection?.kind !== 'span' || !this.selection.item.span.correlation) return;
    const selected = this.selection.item.span.correlation;
    let drawn = 0;
    for (const edge of this.report.dependencies) {
      if (edge.from !== selected && edge.to !== selected) continue;
      const a = anchors.get(edge.from), b = anchors.get(edge.to);
      if (!a || !b || drawn++ >= 40) continue;
      ctx.save(); ctx.strokeStyle = this.colors.link; ctx.lineWidth = 1.5; ctx.setLineDash([4, 4]);
      ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.bezierCurveTo((a.x + b.x) / 2, a.y, (a.x + b.x) / 2, b.y, b.x, b.y); ctx.stroke(); ctx.restore();
    }
  }

  private onClick(x: number, y: number): void {
    if (x < LABEL_WIDTH) {
      const row = this.rows.find(item => y + this.scroll.scrollTop >= item.top && y + this.scroll.scrollTop < item.top + item.height);
      if (row?.hasChildren) {
        if (this.collapsed.has(row.key)) this.collapsed.delete(row.key); else this.collapsed.add(row.key);
        this.buildRows(); this.schedule();
      } else if (row?.kind === 'shard') this.showShard(row.shard);
      else if (row?.track) this.showTrack(row.track);
      return;
    }
    const hit = this.hitAt(x, y);
    this.hideTooltip();
    this.selection = hit?.selection;
    this.updateRelated();
    this.showDetails(); this.schedule();
  }

  private updateRelated(): void {
    this.related.clear();
    if (this.selection?.kind !== 'span') return;
    const correlation = this.selection.item.span.correlation;
    for (const edge of this.report.dependencies) {
      if (edge.from === correlation) this.related.add(edge.to);
      if (edge.to === correlation) this.related.add(edge.from);
    }
  }

  private showTrack(track: Track): void {
    this.selection = undefined;
    this.related.clear();
    this.detail.replaceChildren(node('h3', '', track.name), node('p', 'muted', `${track.kind.replace('_', ' ')} · ${track.rank === null ? 'unranked' : `rank ${track.rank}`}`));
    const dl = node('dl', 'details-list');
    put(dl, 'div', 'Track ID', track.id);
    for (const [key, value] of Object.entries(track.details)) put(dl, 'div', key, value);
    this.detail.append(dl); this.schedule();
  }

  private showShard(shard: Shard): void {
    this.selection = undefined;
    this.related.clear();
    this.detail.replaceChildren(node('h3', '', shard.name));
    const dl = node('dl', 'details-list');
    for (const [key, value] of Object.entries({ Host: shard.hostname, Command: shard.command.join(' '), Collectors: shard.collectors.join(', '),
      'Start offset': `${shard.origin_ns} ns`, 'Observed end': `${shard.end_ns} ns`, Complete: String(shard.complete),
      'End reason': shard.end_reason, Dropped: shard.dropped, 'Exit code': String(shard.exit_code ?? '—'), Signal: String(shard.signal ?? '—') })) put(dl, 'div', key, value);
    this.detail.append(dl); this.schedule();
  }

  private showDetails(): void {
    this.detail.replaceChildren();
    if (!this.selection) {
      this.detail.append(node('div', 'empty-detail', 'Select a span or counter sample to inspect exact values, attributes, and correlations.'));
      return;
    }
    const dl = node('dl', 'details-list');
    if (this.selection.kind === 'span') {
      const { span, start, end } = this.selection.item;
      this.detail.append(node('h3', '', span.name));
      const flags = node('div', 'status-flags');
      badge(flags, span.category); if (span.incomplete) badge(flags, 'INCOMPLETE', 'amber');
      this.detail.append(flags);
      put(dl, 'div', 'Start', `${start} ns`);
      put(dl, 'div', 'Duration', `${span.duration_ns} ns (${formatNs(BigInt(span.duration_ns))})`);
      put(dl, 'div', 'End', `${end} ns`);
      put(dl, 'div', 'Track', span.track);
      put(dl, 'div', 'Correlation', span.correlation ?? '—');
      const links = this.report.dependencies.filter(edge => edge.from === span.correlation || edge.to === span.correlation);
      for (const edge of links) put(dl, 'div', `Dependency · ${edge.relation}`, `${edge.from} → ${edge.to}`);
      if (span.correlation && !links.length) put(dl, 'div', 'Dependency', 'No linked endpoint retained');
      for (const [key, value] of Object.entries(span.attributes)) put(dl, 'div', key, value);
    } else {
      const { counter, time } = this.selection.item;
      this.detail.append(node('h3', '', counter.name));
      put(dl, 'div', 'Timestamp', `${time} ns`);
      put(dl, 'div', 'Exact value', counter.value);
      put(dl, 'div', 'Type / unit', `${counter.value_type} · ${counter.unit}`);
      put(dl, 'div', 'Track', counter.track);
      for (const [key, value] of Object.entries(counter.attributes)) put(dl, 'div', key, value);
    }
    this.detail.append(dl);
  }
}

const root = document.getElementById('app');
const payload = document.getElementById('rprof-data');
if (root) {
  try {
    if (!payload?.textContent) throw new Error('No report data found.');
    const report: Report = JSON.parse(payload.textContent);
    if (report.version !== 1) throw new Error(`Unsupported report version: ${String(report.version)}`);
    new Viewer(report, root);
  } catch (error) {
    root.replaceChildren(node('main', 'load-error', `Unable to open report: ${error instanceof Error ? error.message : 'invalid data'}`));
  }
}
