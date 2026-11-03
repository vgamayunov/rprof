import { averageNs, COUNTER_PLOT_HEIGHT, COUNTER_PLOT_TOP, defaultTimelineCollapsed, deviceActivities, domainClockLabel, formatNs, indexReport, integerPlotValues, ioBinMetrics, ioGraphBins, ioMetricValues, isCollectorHealth, lowerBound, matchesFilter, observedRate, regionThreads, setIoWindow, SPAN_PITCH, SPAN_TOP, storageBins, systemPointsConnected, timelineRows, timeToX, visibleSpans, xToTime } from './model.js';
import type { CudaDevice, CudaRuntime, Domain, IndexedCounter, IndexedSpan, IoGraph, IoMetric, IoSummary, IoTimeBin, OpenmpRegion, OpenmpRuntime, PmuHost, PytorchSource, PythonRuntime, Report, Shard, StorageBin, StorageGraph, TimelineRow, Track, TrackData } from './model.js';
import { canvasThemes, initialTheme, rememberTheme } from './theme.js';
import type { Theme } from './theme.js';
import { formatBytes, formatByteRate, formatMeanLatency } from './model.js';
import { ProjectionViewer } from './projection.js';
import type { Projection } from './projection.js';
import { WindowCache, emptyScopes, hasScopes, windowIdentity } from './window-cache.js';
import type { WindowRequest, WindowResponse, WindowScopes } from './window-cache.js';

const startingTheme = initialTheme();
document.documentElement.dataset.theme = startingTheme;

type Selection = { kind: 'span'; item: IndexedSpan } | { kind: 'counter'; item: IndexedCounter } | { kind: 'io'; bin: IoTimeBin; graph: IoGraph } | { kind: 'storage'; bin: StorageBin; graph: StorageGraph };
type Hit = { x: number; y: number; width: number; height: number; selection: Selection };
type TimeRange = { start: bigint; end: bigint };

const LABEL_WIDTH = 260;
const MAX_DRAWN_SPANS = 5000;
const IO_PAGE_SIZE = 100;
type FilePage = { thread: string; offset: number; total: number; tracks: Track[] };
function localEndpoint(path: string): string | undefined {
  if (!path.startsWith('/') || path.startsWith('//') || path.includes('\\') || path.includes('#')) return undefined;
  const url = new URL(path, location.href);
  return url.origin === location.origin && (url.protocol === 'http:' || url.protocol === 'https:') ? url.pathname + url.search : undefined;
}
const COLORS = ['#6bcdb0', '#65a8ec', '#e5b46d', '#b29be6', '#e17d91', '#86c6d9', '#cbc988'];
const MPI_COLOR = '#ff9970';
const OPENMP_COLOR = '#d5a2ff';
const CUDA_COLOR = '#e9c264';
const PYTHON_COLOR = '#ae85ed';
const PYTORCH_COLOR = '#e48b93';

function node<K extends keyof HTMLElementTagNameMap>(tag: K, className = '', text?: string): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}

function put(parent: Element, tag: keyof HTMLElementTagNameMap, label: string, value: string, exact?: string): void {
  const row = node('div', 'detail-row');
  const detail = node('dd', '', value);
  if (exact) detail.title = exact;
  row.append(node('dt', '', label), detail);
  parent.append(row);
}

function badge(parent: Element, label: string, tone = ''): void {
  parent.append(node('span', `badge ${tone}`, label));
}

function colorFor(value: string, colors: readonly string[] = COLORS): string {
  if (value.toLowerCase() === 'mpi') return MPI_COLOR;
  if (value.toLowerCase() === 'openmp') return OPENMP_COLOR;
  if (value.toLowerCase() === 'cuda') return CUDA_COLOR;
  if (value.toLowerCase() === 'python') return PYTHON_COLOR;
  if (value.toLowerCase() === 'pytorch') return PYTORCH_COLOR;
  let hash = 0;
  for (let i = 0; i < value.length; i++) hash = (Math.imul(hash, 31) + value.charCodeAt(i)) | 0;
  return colors[(hash >>> 0) % colors.length]!;
}

function clamp(value: number, min: number, max: number): number { return Math.min(max, Math.max(min, value)); }
function optionalNs(value: string | null): string { return value === null ? '—' : `${value} ns`; }

class Viewer {
  private readonly report: Report;
  private data: Map<string, TrackData>;
  private readonly root: HTMLElement;
  private readonly domainSelect: HTMLSelectElement;
  private readonly filter: HTMLInputElement;
  private readonly scroll: HTMLElement;
  private readonly spacer: HTMLElement;
  private readonly canvas: HTMLCanvasElement;
  private readonly ruler: HTMLCanvasElement;
  private readonly detail: HTMLElement;
  private readonly rangeLabel: HTMLElement;
  private readonly systemButtons = new Map<'cpu' | 'memory', HTMLButtonElement>();
  private readonly tooltip: HTMLElement;
  private readonly collapsed: Set<string>;
  private readonly hits: Hit[] = [];
  private readonly labelCache = new Map<string, string>();
  private rows: TimelineRow[] = [];
  private domain: Domain | undefined;
  private start = 0n;
  private nsPerPx = 1;
  private selection: Selection | undefined;
  private related = new Set<string>();
  private drag: { x: number; y: number; start: bigint; moved: boolean; selecting: boolean; anchor: bigint; previous: TimeRange | undefined } | undefined;
  private selectRangeMode = false;
  private timeRange: TimeRange | undefined;
  private timeFilter: TimeRange | undefined;
  private frame = 0;
  private ioPage = 0;
  private operationPage = 0;
  private selectedIo: IoSummary | undefined;
  private ioFilesystem = 'all';
  private ioMetric: IoMetric = 'throughput';
  private ioVisible = true;
  private ioPlots = new WeakMap<IoGraph, Map<IoMetric, { bins: IoTimeBin[]; starts: bigint[]; ends: bigint[]; values: [number, number][]; max: number }>>();
  private storagePlots = new WeakMap<StorageGraph, Map<IoMetric, StorageBin[]>>();
  private windowTimer: ReturnType<typeof setTimeout> | undefined;
  private windowAbort: AbortController | undefined;
  private windowKey = '';
  private loadedWindowKey = '';
  private hasWindowData = false;
  private windowCache = new WindowCache();
  private visibleWindowKey = '';
  private attemptedScopes: WindowScopes = emptyScopes();
  private windowFailed = false;
  private windowScopeLimited = false;
  private currentIoBins: Record<string, IoTimeBin[]> = {};
  private filePages = new Map<string, FilePage>();
  private fileAbort = new Map<string, AbortController>();
  private activeFiles: string | undefined;
  private readonly windowStatus: HTMLElement;
  private readonly fileControls: HTMLElement;
  private regionPage = 0;
  private threadPage = 0;
  private selectedRegion: OpenmpRegion | undefined;
  private runtimePage = 0;
  private callbackPage = 0;
  private selectedRuntime: OpenmpRuntime | undefined;
  private gompApiPage = 0;
  private gompBodyPage = 0;
  private cudaDevicePage = 0;
  private cudaActivityPage = 0;
  private selectedCudaDevice: CudaDevice | undefined;
  private cudaApiPage = 0;
  private cudaRuntimePage = 0;
  private cudaCapabilityPage = 0;
  private cudaErrorPage = 0;
  private cudaSourcePage = 0;
  private selectedCudaRuntime: CudaRuntime | undefined;
  private pmuEventPage = 0;
  private pmuMetricPage = 0;
  private pmuHostPage = 0;
  private pmuMetadataPage = 0;
  private selectedPmuHost: PmuHost | undefined;
  private pythonRuntimePage = 0;
  private pythonFunctionPage = 0;
  private pythonRangePage = 0;
  private selectedPythonRuntime: PythonRuntime | undefined;
  private pytorchSourcePage = 0;
  private pytorchMetadataPage = 0;
  private pytorchActivityPage = 0;
  private pytorchMemoryPage = 0;
  private selectedPytorchSource: PytorchSource | undefined;
  private theme: Theme = startingTheme;
  private colors = canvasThemes[startingTheme];

  constructor(report: Report, root: HTMLElement) {
    this.report = report;
    this.data = indexReport(report);
    this.collapsed = defaultTimelineCollapsed(report);
    this.root = root;
    // Only application-owned static markup is used here. All report strings enter via textContent or Canvas.
    root.innerHTML = `<header class="topbar"><div class="brand"><div class="mark">r/</div><div><strong>rprof</strong><small>OFFLINE TIMELINE</small></div></div><div class="header-actions"><div class="header-meta" id="header-meta"></div><button id="theme-toggle" type="button"></button></div></header>
      <section class="overview"><div class="overview-copy"><div class="eyebrow">CAPTURE OVERVIEW</div><h1 id="session-title">Session</h1><p id="overview-subtitle"></p></div><div class="stat-grid" id="stat-grid"></div></section>
      <div class="workspace"><section class="timeline-section"><div class="toolbar"><div class="toolbar-top"><div class="toolbar-heading"><h2>Timeline</h2><span id="domain-note">CLOCK DOMAINS ARE UNALIGNED</span></div><div class="zoom-controls"><button id="zoom-out" type="button" title="Zoom out" aria-label="Zoom out">−</button><button id="zoom-in" type="button" title="Zoom in" aria-label="Zoom in">+</button><button id="fit-view" type="button">Fit all</button></div></div><div class="toolbar-fields"><label class="field">Clock domain <select id="domain-select" aria-label="Clock domain"></select></label><label class="field search">Filter <input id="filter-input" type="search" placeholder="Name, category, rank…" aria-label="Filter tracks and events"></label><span id="range-label" class="range-label"></span></div><div class="io-toolbar" id="io-toolbar" hidden><button type="button" id="io-toggle" aria-label="Toggle I/O graph" aria-pressed="true">I/O ▾</button><label class="field">Filesystem <select id="io-fs-select" aria-label="Filesystem"></select></label><label class="field">Metric <select id="io-metric-select" aria-label="I/O metric"><option value="throughput">Throughput · MiB/s</option><option value="iops">Calls/s (IOPS)</option><option value="latency">Average latency · ms</option></select></label><span class="io-legend"><i class="io-read-key"></i> Read <i class="io-write-key"></i> Write</span><span id="io-coverage" class="io-coverage"></span></div></div><div class="ruler-wrap"><div class="track-header">TRACKS <span>▾ collapse</span></div><canvas id="time-ruler" aria-label="Timeline time axis"></canvas></div><div class="plot-scroll" id="timeline-scroll" tabindex="0" aria-label="Timeline tracks; scroll vertically, wheel over plot to zoom, drag to pan"><canvas id="timeline-canvas" aria-label="Interactive timeline spans and counters"></canvas><div id="timeline-spacer"></div></div><div class="timeline-footer"><span><i class="legend-swatch span-key"></i> span <i class="legend-swatch counter-key"></i> counter <i class="legend-swatch incomplete-key"></i> incomplete</span><span>Wheel: zoom · Drag: pan · Shift + wheel: scroll</span></div></section><aside class="sidebar"><section class="side-card" id="detail-panel"><div class="side-title">INSPECTOR</div><div id="detail-content" class="detail-content"></div></section><section class="side-card"><div class="side-title">CATEGORY SUMMARY <span>INCLUSIVE TIME</span></div><div class="category-wrap"><table id="category-table"><thead><tr><th>Category</th><th>Spans</th><th>Time</th></tr></thead><tbody></tbody></table></div><p class="footnote">Inclusive durations may overlap and double-count nested work. Counts cover the full capture, not just retained events.</p></section><section class="side-card"><div class="side-title">CAPTURE STATUS</div><div id="capture-status"></div></section></aside></div>`;
    this.domainSelect = this.get('domain-select') as HTMLSelectElement;
    if (report.viewer) this.root.querySelector('.brand small')!.textContent = 'INDEXED TIMELINE';
    this.get('fit-view').textContent = 'Reset';
    this.get('fit-view').title = 'Reset zoom to the full capture (Home)';
    this.get('fit-view').setAttribute('aria-label', 'Reset zoom');
    this.get('timeline-scroll').setAttribute('aria-label', 'Timeline tracks; wheel scrolls vertically, drag pans time, Shift-drag selects a range');
    this.root.querySelector('.timeline-footer > span:last-child')!.textContent = 'Wheel: scroll rows · Drag: pan · Shift-drag: select · + −: zoom · Home: reset · Esc: clear range';
    this.root.querySelector('.toolbar')!.insertAdjacentHTML('beforeend', `<div class="range-toolbar"><button id="select-range" type="button" aria-pressed="false" title="Drag to select; Shift-drag also selects in pan mode">Select range</button><button id="zoom-selection" type="button" disabled>Zoom to selection</button><button id="filter-selection" type="button" disabled title="Show timeline activity only within the selected interval">Filter by selection</button><button id="clear-selection" type="button" disabled>Clear range</button><span id="selection-range" role="status">Shift-drag to select a time range</span></div>`);
    // Static structure only: measured file names and values are always inserted as text nodes.
    root.insertAdjacentHTML('beforeend', `<section class="io-section" id="io-panel"><div class="io-heading"><div><h2>Per-file I/O</h2><p>Selected clock domain only · completed / requested bytes · observed-window bandwidth (bytes / window, not physical device bandwidth)</p></div><span id="io-count"></span></div><div class="io-wrap"><table id="io-table"><thead><tr><th>Host / file</th><th>Operations</th><th>Errors</th><th>Read (completed / requested)</th><th>Write (completed / requested)</th><th>Read bandwidth</th><th>Write bandwidth</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="io-pager"></div><div id="io-detail"></div></section>`);
    const perFile = this.get('io-panel');
    const perFileContent = node('details', 'io-per-file');
    perFileContent.id = 'io-per-file';
    const summary = node('summary', '', 'Per-file I/O details');
    for (const child of Array.from(perFile.children).filter(child => !child.classList.contains('io-heading'))) perFileContent.append(child);
    perFileContent.prepend(summary);
    perFile.append(perFileContent);
    if (report.viewer) perFile.hidden = true;
    if (!report.io_timeline) {
      if (!report.viewer) perFile.prepend(node('p', 'io-note', 'I/O time series unavailable in this older report; regenerate the report from the capture to view filesystem graphs. Per-file details remain available below.'));
    } else {
      perFile.hidden = !!report.viewer || !!report.io_timeline.threads?.length;
      this.get('io-toolbar').hidden = false;
      this.get('io-toggle').addEventListener('click', () => {
        this.ioVisible = !this.ioVisible;
        this.get('io-toggle').setAttribute('aria-pressed', String(this.ioVisible));
        this.get('io-toggle').textContent = this.ioVisible ? 'I/O ▾' : 'I/O ▸';
        this.buildRows(); this.schedule();
      });
      this.get('io-fs-select').addEventListener('change', () => {
        this.ioFilesystem = (this.get('io-fs-select') as HTMLSelectElement).value;
        this.selection = undefined; this.showDetails();
        this.buildRows(); this.schedule();
      });
    }
    if (report.tracks.some(track => track.kind === 'storage')) {
      this.get('io-toolbar').hidden = false;
      if (!report.io_timeline) {
        perFile.hidden = !(report.io?.length);
        (this.get('io-toggle') as HTMLButtonElement).hidden = true;
        this.get('io-fs-select').parentElement!.hidden = true;
        this.get('io-coverage').textContent = 'Storage is captured only while monitoring · host block-device, Lustre client bulk RPC and NFS client server payload are separate layers, not application-only or server physical I/O.';
      }
    }
    this.get('io-metric-select').addEventListener('change', () => {
      this.ioMetric = (this.get('io-metric-select') as HTMLSelectElement).value as IoMetric;
      this.schedule();
    });
    if (report.openmp) root.insertAdjacentHTML('beforeend', `<section class="io-section openmp-section" id="openmp-panel"><div class="io-heading"><div><h2>OpenMP</h2><p id="openmp-coverage"></p></div><span id="openmp-count"></span></div><p class="io-note" id="openmp-tasks"></p><div class="io-wrap"><table id="openmp-regions"><thead><tr><th>Region · process</th><th>Threads requested / observed</th><th>Region duration</th><th>Recorded work</th><th>Recorded wait</th><th>Observed work min / max / spread</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="openmp-region-pager"></div><div id="openmp-detail"></div><div id="gomp-panel" hidden><h3>GNU libgomp observed calls and outlined bodies</h3><p class="io-note" id="gomp-coverage"></p><p class="io-note" id="gomp-tasks"></p><h3>GOMP API calls <span id="gomp-api-count"></span></h3><div class="io-wrap"><table id="gomp-apis"><thead><tr><th>API · image / shard</th><th>Calls</th><th>Inclusive host time</th><th>Max host time</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="gomp-api-pager"></div><h3>Outlined body returns <span id="gomp-body-count"></span></h3><div class="io-wrap"><table id="gomp-bodies"><thead><tr><th>Kind · image / shard</th><th>Thread ID</th><th>Opaque code pointer (not source)</th><th>Body spans</th><th>Inclusive wall time</th><th>Max wall time</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="gomp-body-pager"></div></div><details id="openmp-runtimes"><summary>Runtime and capability details <span id="openmp-runtime-count"></span></summary><div id="openmp-runtime-list"></div><div class="io-pager" id="openmp-runtime-pager"></div><div id="openmp-callback-detail"></div></details></section>`);
    if (report.cuda) root.insertAdjacentHTML('beforeend', `<section class="io-section cuda-section" id="cuda-panel"><div class="io-heading"><div><h2>CUDA</h2><p>Selected clock domain · inclusive timed kernel/copy durations overlap across streams; not GPU utilization or elapsed application time. Host API time is not GPU execution time.</p></div><span id="cuda-count"></span></div><p class="io-note" id="cuda-health"></p><div class="io-wrap"><table id="cuda-devices"><thead><tr><th>GPU device · process</th><th>Visibility</th><th>Memory / compute CC</th><th>Timed kernels / copies / memsets</th><th>Inclusive kernel / copy time</th><th>Copy / memset bytes</th><th>Activity window</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="cuda-device-pager"></div><div id="cuda-device-detail"></div><h3>CPU CUDA API calls <span id="cuda-api-count"></span></h3><div class="io-wrap"><table id="cuda-apis"><thead><tr><th>Host API · image</th><th>Runtime / driver</th><th>Calls</th><th>Errors</th><th>Inclusive host time</th><th>Max host time</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="cuda-api-pager"></div><details id="cuda-diagnostics"><summary>Capabilities, errors and source health</summary><div id="cuda-runtime-list"></div><div class="io-pager" id="cuda-runtime-pager"></div><div id="cuda-runtime-detail"></div><h3>Capture sources</h3><div class="io-wrap"><table id="cuda-sources"><thead><tr><th>Shard</th><th>Images / finalized</th><th>Native received / attempted</th><th>Native ingress drops</th><th>CUPTI drops</th><th>Source errors</th><th>Invalid timing</th><th>Unattributed</th><th>Unresolved / ambiguous correlations</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="cuda-source-pager"></div></details></section>`);
    if (report.pmu) root.insertAdjacentHTML('beforeend', `<section class="io-section pmu-section" id="pmu-panel"><div class="io-heading"><div><h2>Linux perf counters (PMU)</h2><p>Selected clock domain · workload-tree aggregates or root-thread counts, not per-thread attribution. Hardware, software and raw perf events are distinguished below; software task-clock is not a hardware metric.</p></div><span id="pmu-count"></span></div><p class="io-note">Timeline pmu.EVENT.raw samples are cumulative raw counts, not rates. A missing or failed read is unknown, not zero. Scaled count = raw × enabled / running is an estimate under multiplexing, not an exact count. Events are independently scheduled: IPC and cache-miss ratios are estimates, not jointly scheduled measurements; only valid final pairs yield metrics.</p><div class="io-wrap"><table id="pmu-events"><thead><tr><th>Event / perf type</th><th>PID / shard</th><th>Scope / privilege</th><th>Status / error</th><th>Raw / unit</th><th>Enabled / running (exact ns)</th><th>Running %</th><th>Scaled estimate</th><th>Final read / samples</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="pmu-event-pager"></div><h3>Estimated ratios <span id="pmu-metric-count"></span></h3><div class="io-wrap"><table id="pmu-metrics"><thead><tr><th>Shard</th><th>IPC estimate (instructions / cycles)</th><th>Cache-miss estimate (%)</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="pmu-metric-pager"></div><details id="pmu-hosts"><summary>Host context at launch (not continuously sampled CPU frequency) <span id="pmu-host-count"></span></summary><p class="io-note">Kernel, CPUID model/vendor, affinity, CPU0 cache, NUMA and frequency context where available. Frequency metadata is launch-time context, not a continuous frequency trace.</p><div id="pmu-host-list"></div><div class="io-pager" id="pmu-host-pager"></div><div id="pmu-host-detail"></div></details></section>`);
    if (report.python) root.insertAdjacentHTML('beforeend', `<section class="io-section python-section" id="python-panel"><div class="io-heading"><div><h2>Python</h2><p>Selected clock domain · instrumented function execution segments, not whole generator/coroutine lifetimes. Yield/resume creates separate segments. Self time subtracts only selected child segments; it includes native calls, GIL waits and instrumentation, not CPU utilization. User ranges are elapsed time and can include suspension; self time is unavailable.</p></div><span id="python-count"></span></div><p class="io-note" id="python-health"></p><details open><summary>Runtime and source health</summary><div id="python-runtimes"></div><div class="io-pager" id="python-runtime-pager"></div><div id="python-runtime-detail"></div></details><h3>Instrumented functions</h3><div class="io-wrap"><table id="python-functions"><thead><tr><th>Function / source</th><th>File : line / kind / symbol</th><th>Segments</th><th>Inclusive</th><th>Selected-child self</th><th>Max</th><th>Yields / unwinds</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="python-function-pager"></div><h3>User ranges (elapsed)</h3><div class="io-wrap"><table id="python-ranges"><thead><tr><th>Range / source</th><th>File : line / kind / symbol</th><th>Segments</th><th>Inclusive elapsed</th><th>Self</th><th>Max</th><th>Yields / unwinds</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="python-range-pager"></div></section>`);
    if (report.pytorch) root.insertAdjacentHTML('beforeend', `<section class="io-section pytorch-section" id="pytorch-panel"><div class="io-heading"><div><h2>PyTorch import</h2><p>Selected clock domain · imported µs→ns relative clock, not aligned to native CLOCK_MONOTONIC. Source profiler loss and export coverage are unknown. CPU self is contained-CPU self time, not guaranteed to match torch.key_averages; unavailable for crossing/tied intervals or bounded-index overflow. GPU self time is not applicable.</p></div><span id="pytorch-count"></span></div><p class="io-note">Timeline marker-unit counter samples are trace instants, not execution spans. Memory rows are OBSERVED framework allocated/reserved total-byte counters, not physical GPU usage. Missing reserved samples are unknown, not zero.</p><details open><summary>Imported sources · metadata and diagnostics</summary><div id="pytorch-sources"></div><div class="io-pager" id="pytorch-source-pager"></div><div id="pytorch-source-detail"></div></details><h3>Activities</h3><div class="io-wrap"><table id="pytorch-activities"><thead><tr><th>Role / activity</th><th>Shard / device</th><th>Count</th><th>Inclusive</th><th>Contained-CPU self</th><th>Max</th><th>Bytes</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="pytorch-activity-pager"></div><h3>Observed framework memory</h3><div class="io-wrap"><table id="pytorch-memory"><thead><tr><th>Counter / track</th><th>Shard / device</th><th>Samples</th><th>Observed peak</th><th>Last observed</th></tr></thead><tbody></tbody></table></div><div class="io-pager" id="pytorch-memory-pager"></div></section>`);
    const legend = root.querySelector('.timeline-footer span')!;
    legend.append(node('i', 'legend-swatch mpi-key'), document.createTextNode(' MPI'));
    if (report.openmp) legend.append(node('i', 'legend-swatch openmp-key'), document.createTextNode(' OpenMP'));
    if (report.cuda) legend.append(node('i', 'legend-swatch cuda-key'), document.createTextNode(' CUDA'));
    if (report.python) legend.append(node('i', 'legend-swatch python-key'), document.createTextNode(' Python'));
    if (report.pytorch) legend.append(node('i', 'legend-swatch pytorch-key'), document.createTextNode(' PyTorch'));
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
    this.windowStatus = node('span', 'window-status');
    this.windowStatus.id = 'window-status';
    this.fileControls = node('div', 'file-controls');
    this.fileControls.id = 'file-controls';
    if (report.viewer) {
      this.get('range-label').after(this.windowStatus);
      this.get('io-toolbar').after(this.fileControls);
      this.windowStatus.textContent = 'Loading window…';
      const retry = node('button', '', 'Retry window');
      retry.id = 'retry-window'; retry.type = 'button'; retry.hidden = true;
      retry.addEventListener('click', () => {
        this.windowFailed = false; this.attemptedScopes = emptyScopes(); retry.hidden = true; this.schedule();
      });
      this.windowStatus.after(retry);
    }
    if (report.tracks.some(track => track.kind === 'system')) for (const [kind, label] of [['cpu', 'CPU ▸'], ['memory', 'Memory ▸']] as const) {
      const button = node('button', '', label);
      button.type = 'button'; button.setAttribute('aria-label', `Expand ${kind} system graphs`);
      button.addEventListener('click', () => {
        const key = `system:${this.domain?.id}:${kind}`;
        // Search temporarily reveals children: clicking while filtered clears
        // search and collapses the visible group rather than appearing to do nothing.
        if (this.filter.value) { this.filter.value = ''; this.collapsed.add(key); }
        else if (this.collapsed.has(key)) this.collapsed.delete(key); else this.collapsed.add(key);
        this.buildRows(); this.schedule();
      });
      this.systemButtons.set(kind, button);
      this.get('fit-view').before(button);
    }
    this.setupOverview();
    this.get('theme-toggle').addEventListener('click', () => {
      this.setTheme(this.theme === 'dark' ? 'light' : 'dark');
      rememberTheme(this.theme);
    });
    this.setTheme(this.theme);
    this.get('domain-note').textContent = domainClockLabel(report, report.domains[0]?.id);
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
    this.get('select-range').addEventListener('click', () => {
      this.selectRangeMode = !this.selectRangeMode;
      this.get('select-range').setAttribute('aria-pressed', String(this.selectRangeMode));
      this.canvas.style.cursor = this.selectRangeMode ? 'crosshair' : '';
    });
    this.get('zoom-selection').addEventListener('click', () => {
      if (!this.timeRange) return;
      this.nsPerPx = Math.max(1, Number(this.timeRange.end - this.timeRange.start) / this.plotWidth());
      this.start = this.clampStart(this.timeRange.start); this.schedule();
    });
    this.get('filter-selection').addEventListener('click', () => {
      if (!this.timeRange) return;
      this.timeFilter = { ...this.timeRange };
      this.selection = undefined; this.updateRelated(); this.showDetails();
      this.updateRangeControls(); this.schedule();
    });
    this.get('clear-selection').addEventListener('click', () => this.clearRange());
    this.scroll.addEventListener('scroll', () => { this.hideTooltip(); this.schedule(); });
    this.scroll.addEventListener('keydown', event => {
      if (event.key === '+' || event.key === '=') { this.zoom(0.5); event.preventDefault(); }
      else if (event.key === '-') { this.zoom(2); event.preventDefault(); }
      else if (event.key === 'Home') { this.fit(); event.preventDefault(); }
      else if (event.key === 'Escape') { this.clearRange(); event.preventDefault(); }
      else if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
        const step = BigInt(Math.round(this.plotWidth() * this.nsPerPx / 5));
        this.start = this.clampStart(this.start + (event.key === 'ArrowRight' ? step : -step));
        this.schedule(); event.preventDefault();
      }
    });
    // Let the browser handle wheel/trackpad scrolling and delta modes naturally.
    // Time zoom is explicit (buttons/keyboard), never an accidental wheel gesture.
    this.canvas.addEventListener('pointerdown', event => {
      if (event.button !== 0 && event.button !== 1) return;
      event.preventDefault(); this.scroll.focus({ preventScroll: true });
      this.canvas.setPointerCapture(event.pointerId);
      const inPlot = event.clientX - this.canvas.getBoundingClientRect().left >= LABEL_WIDTH;
      this.drag = { x: event.clientX, y: event.clientY, start: this.start, moved: false,
        selecting: inPlot && event.button === 0 && (event.shiftKey || this.selectRangeMode),
        anchor: this.pointerTime(event.clientX), previous: this.timeRange };
    });
    this.canvas.addEventListener('pointermove', event => {
      this.hover(event);
      if (!this.drag) return;
      const dx = event.clientX - this.drag.x;
      if (Math.abs(dx) + Math.abs(event.clientY - this.drag.y) > 4) this.drag.moved = true;
      if (this.drag.moved && this.drag.x - this.canvas.getBoundingClientRect().left >= LABEL_WIDTH) {
        if (this.drag.selecting) {
          const at = this.pointerTime(event.clientX), anchor = this.drag.anchor;
          this.timeRange = at === anchor ? undefined : { start: at < anchor ? at : anchor, end: at > anchor ? at : anchor };
          this.updateRangeControls(); this.hideTooltip();
        } else this.start = this.clampStart(this.drag.start - BigInt(Math.round(dx * this.nsPerPx)));
        this.schedule();
      }
    });
    this.canvas.addEventListener('pointerup', event => {
      if (!this.drag) return;
      const moved = this.drag.moved;
      this.drag = undefined;
      if (this.canvas.hasPointerCapture(event.pointerId)) this.canvas.releasePointerCapture(event.pointerId);
      if (!moved && event.button === 0) this.onClick(event.offsetX, event.offsetY);
    });
    const cancelDrag = () => {
      if (this.drag?.selecting) this.timeRange = this.drag.previous;
      this.drag = undefined; this.updateRangeControls(); this.schedule();
    };
    this.canvas.addEventListener('pointercancel', cancelDrag);
    this.canvas.addEventListener('lostpointercapture', cancelDrag);
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
    const cudaSources = this.report.cuda?.sources ?? [];
    if (cudaSources.length) {
      const sum = (field: 'received' | 'attempted' | 'ingress_dropped' | 'cupti_dropped' | 'source_errors' | 'invalid_timing' | 'unattributed' | 'unresolved_correlations' | 'ambiguous_correlations') =>
        cudaSources.reduce((total, source) => total + BigInt(source[field]), 0n).toString();
      const parts = [`${sum('received')}/${sum('attempted')} native received`, `${sum('ingress_dropped')} native ingress drops`,
        `${sum('cupti_dropped')} CUPTI drops`, `${sum('source_errors')} source errors`,
        `${sum('invalid_timing')} invalid timing`, `${sum('unattributed')} unattributed`,
        `${sum('unresolved_correlations')} unresolved / ${sum('ambiguous_correlations')} ambiguous correlations`];
      const unfinished = cudaSources.filter(source => !source.finalized).length;
      if (unfinished) parts.push(`${unfinished} not finalized · GPU activity may be missing`);
      put(dl, 'div', `CUDA · ${cudaSources.length} sources (capture-wide)`, parts.join(' · '));
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
    if (this.report.viewer) { this.activeFiles = undefined; this.fileControls.replaceChildren(); }
    this.get('domain-note').textContent = domainClockLabel(this.report, domain?.id);
    this.hideTooltip();
    this.ioPage = 0;
    this.selectedIo = undefined;
    if (this.report.io_timeline) this.renderIoProfile();
    this.renderIo();
    this.regionPage = 0;
    this.threadPage = 0;
    this.selectedRegion = undefined;
    this.runtimePage = 0;
    this.callbackPage = 0;
    this.gompApiPage = this.gompBodyPage = 0;
    this.selectedRuntime = undefined;
    if (this.report.openmp) this.renderOpenmp();
    this.cudaDevicePage = this.cudaActivityPage = this.cudaApiPage = this.cudaRuntimePage = this.cudaCapabilityPage = this.cudaErrorPage = this.cudaSourcePage = 0;
    this.selectedCudaDevice = undefined;
    this.selectedCudaRuntime = undefined;
    if (this.report.cuda) this.renderCuda();
    this.pmuEventPage = this.pmuMetricPage = this.pmuHostPage = this.pmuMetadataPage = 0;
    this.selectedPmuHost = undefined;
    if (this.report.pmu) this.renderPmu();
    this.pythonRuntimePage = this.pythonFunctionPage = this.pythonRangePage = 0;
    this.selectedPythonRuntime = undefined;
    if (this.report.python) this.renderPython();
    this.pytorchSourcePage = this.pytorchMetadataPage = this.pytorchActivityPage = this.pytorchMemoryPage = 0;
    this.selectedPytorchSource = undefined;
    if (this.report.pytorch) this.renderPytorch();
    this.selection = undefined;
    this.related.clear();
    this.showDetails();
    if (domain) this.domainSelect.value = domain.id;
    this.buildRows();
    this.fit();
    this.schedule();
  }

  private renderIo(): void {
    if (this.report.io_timeline?.threads?.length) return;
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

  private renderIoProfile(): void {
    this.ioFilesystem = 'all';
    const select = this.get('io-fs-select') as HTMLSelectElement;
    select.replaceChildren();
    const files = (this.report.io_timeline?.filesystems ?? []).filter(fs => fs.domain === this.domain?.id);
    const storage = files.filter(fs => fs.kind !== 'non_filesystem');
    const add = (value: string, label: string): void => { const option = node('option', '', label); option.value = value; select.append(option); };
    add('all', `All filesystem + unknown groups (${storage.length})`);
    for (const fs of files) add(fs.id, `${fs.host} · ${fs.mount_point || '(no mount)'} · ${fs.filesystem_type || 'unknown type'} · ${fs.device || 'unknown device'}${fs.kind === 'non_filesystem' ? ' · non-file I/O' : fs.kind === 'unknown' ? ' · unknown' : ''}`);
    const series = this.report.io_timeline!;
    this.get('io-coverage').textContent = `${this.report.viewer ? 'Indexed, selected-window' : 'Full-capture'} POSIX rates · ${series.omitted_events} filesystem / ${series.scope_omitted_events || '0'} ${this.report.viewer ? 'thread catalog omissions' : 'thread observations omitted'} · ${this.report.summary.dropped} capture drops${series.threads?.length ? ' · files appear only inside expanded threads' : this.report.viewer ? ' · no indexed thread catalog available' : ' · regenerate report for thread drill-down'} · no calls means latency unknown`;
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

  private tableRows(id: string, rows: string[][], columns: number, empty: string): void {
    const body = this.get(id).querySelector('tbody')!;
    body.replaceChildren();
    for (const cells of rows) {
      const row = node('tr');
      for (const cell of cells) row.append(node('td', '', cell));
      body.append(row);
    }
    if (!rows.length) this.emptyCudaRow(body, columns, empty);
  }

  private renderPython(): void {
    const python = this.report.python!;
    const domain = this.domain?.id;
    const runtimes = python.runtimes.filter(runtime => runtime.domain === domain);
    const functions = python.functions.filter(item => item.domain === domain && item.kind !== 'range');
    const ranges = python.functions.filter(item => item.domain === domain && item.kind === 'range');
    this.get('python-count').textContent = `${functions.length} functions · ${ranges.length} ranges · ${runtimes.length} runtimes`;
    this.get('python-health').textContent = `Capture-wide receiver drops: ${python.receiver_dropped} · malformed / out-of-order: ${python.malformed}. Runtime health below is selected-domain only; unknown metadata is shown as Unknown.`;
    const list = this.get('python-runtimes'); list.replaceChildren();
    for (const runtime of runtimes.slice(this.pythonRuntimePage * IO_PAGE_SIZE, (this.pythonRuntimePage + 1) * IO_PAGE_SIZE)) {
      const line = node('div', 'openmp-runtime-row');
      const button = node('button', 'io-file', `PID ${runtime.pid || 'Unknown'} · source ${runtime.source || 'Unknown'} · interpreter ${runtime.interpreter || 'Unknown'} · shard ${runtime.shard}`);
      button.type = 'button';
      button.addEventListener('click', () => { this.selectedPythonRuntime = runtime; this.renderPythonRuntimeDetail(); });
      line.append(button, node('span', '', `backend ${runtime.backend || 'Unknown'} · version ${runtime.version || 'Unknown'} · initialized ${runtime.initialized ? 'Yes' : 'No'} · finalized ${runtime.finalized ? 'Yes' : 'No · incomplete'} · received / attempted ${runtime.received} / ${runtime.attempted}`));
      list.append(line);
    }
    if (!runtimes.length) list.append(node('p', 'io-note', 'No Python runtime in this clock domain.'));
    this.renderPager(this.get('python-runtime-pager'), runtimes.length, this.pythonRuntimePage, page => { this.pythonRuntimePage = page; this.renderPython(); });
    this.renderPythonRuntimeDetail();
    for (const [items, tableId, pagerId, page, setPage, empty] of [
      [functions, 'python-functions', 'python-function-pager', this.pythonFunctionPage, (value: number) => { this.pythonFunctionPage = value; this.renderPython(); }, 'No instrumented function segments in this clock domain.'],
      [ranges, 'python-ranges', 'python-range-pager', this.pythonRangePage, (value: number) => { this.pythonRangePage = value; this.renderPython(); }, 'No user ranges in this clock domain.'],
    ] as const) {
      const rows = items.slice(page * IO_PAGE_SIZE, (page + 1) * IO_PAGE_SIZE).map(item => [
        `${item.name} · PID ${runtimes.find(runtime => runtime.shard === item.shard && runtime.source === item.source)?.pid || 'Unknown'} · source ${item.source || 'Unknown'} · shard ${item.shard}`,
        `${item.file || 'Unknown'}:${item.line || 'Unknown'} · ${item.execution_kind || 'Unknown'} · ${item.kind} · symbol ${item.symbol || 'Unknown'}`,
        item.segments, `${item.total_ns} ns`, item.self_ns === null ? 'N/A (elapsed range)' : `${item.self_ns} ns`, `${item.max_ns} ns`, `${item.yields} / ${item.unwinds}`,
      ]);
      this.tableRows(tableId, rows, 7, empty);
      this.renderPager(this.get(pagerId), items.length, page, setPage);
    }
  }

  private renderPythonRuntimeDetail(): void {
    const detail = this.get('python-runtime-detail'); detail.replaceChildren();
    const runtime = this.selectedPythonRuntime;
    if (!runtime || runtime.domain !== this.domain?.id) return;
    detail.append(node('h3', '', `Python source ${runtime.source || 'Unknown'} · PID ${runtime.pid || 'Unknown'} · interpreter ${runtime.interpreter || 'Unknown'}`));
    const lines: [string, string][] = [
      ['Process instance', runtime.instance || 'Unknown'], ['Backend / version', `${runtime.backend || 'Unknown'} / ${runtime.version || 'Unknown'}`],
      ['Clock / detail', `${runtime.clock || 'Unknown'} · ${runtime.detail || 'Unknown'}`], ['Initialized / finalized', `${runtime.initialized ? 'Yes' : 'No'} / ${runtime.finalized ? 'Yes' : 'No'}`],
      ['Attempted / received', `${runtime.attempted} / ${runtime.received}`], ['Source drops / omitted callbacks', `${runtime.source_dropped} / ${runtime.omitted_callbacks}`],
      ['Sequence gaps / unmatched', `${runtime.sequence_gaps} / ${runtime.unmatched}`], ['Abandoned / in flight', `${runtime.abandoned} / ${runtime.inflight}`],
      ['Source errors', runtime.source_errors],
    ];
    const info = node('dl', 'details-list');
    for (const [label, value] of lines) put(info, 'div', label, value);
    detail.append(info, node('h3', '', `Runtime errors · ${runtime.errors.length}`));
    for (const error of runtime.errors.slice(0, 32)) detail.append(node('p', 'io-note', error));
  }

  private renderPytorch(): void {
    const pytorch = this.report.pytorch!;
    const domain = this.domain?.id;
    const sources = pytorch.sources.filter(source => source.domain === domain);
    const activities = pytorch.activities.filter(activity => activity.domain === domain);
    const memory = pytorch.memory.filter(row => row.domain === domain);
    this.get('pytorch-count').textContent = `${sources.length} sources · ${activities.length} activities · ${memory.length} memory rows`;
    const list = this.get('pytorch-sources'); list.replaceChildren();
    for (const source of sources.slice(this.pytorchSourcePage * IO_PAGE_SIZE, (this.pytorchSourcePage + 1) * IO_PAGE_SIZE)) {
      const line = node('div', 'openmp-runtime-row');
      const button = node('button', 'io-file', `Shard ${source.shard}`); button.type = 'button';
      button.addEventListener('click', () => { this.selectedPytorchSource = source; this.pytorchMetadataPage = 0; this.renderPytorchSourceDetail(); });
      line.append(button, node('span', '', `Source loss / export coverage: Unknown · ${Object.keys(source.metadata).length} metadata · ${Object.keys(source.diagnostics).length} diagnostics`));
      list.append(line);
    }
    if (!sources.length) list.append(node('p', 'io-note', 'No imported PyTorch sources in this clock domain.'));
    this.renderPager(this.get('pytorch-source-pager'), sources.length, this.pytorchSourcePage, page => { this.pytorchSourcePage = page; this.renderPytorch(); });
    this.renderPytorchSourceDetail();
    this.tableRows('pytorch-activities', activities.slice(this.pytorchActivityPage * IO_PAGE_SIZE, (this.pytorchActivityPage + 1) * IO_PAGE_SIZE).map(activity => [
      `${activity.role} · ${activity.name}`, `${activity.shard} / ${activity.device ?? '—'}`, activity.count,
      `${activity.total_ns} ns`, activity.self_ns === null ? (['kernel', 'memcpy', 'memset', 'gpu_range'].includes(activity.role) ? 'N/A (GPU)' : 'Unknown') : `${activity.self_ns} ns`,
      `${activity.max_ns} ns`, activity.bytes === null ? 'Unknown' : `${activity.bytes} B`,
    ]), 7, 'No PyTorch activities in this clock domain.');
    this.renderPager(this.get('pytorch-activity-pager'), activities.length, this.pytorchActivityPage, page => { this.pytorchActivityPage = page; this.renderPytorch(); });
    this.tableRows('pytorch-memory', memory.slice(this.pytorchMemoryPage * IO_PAGE_SIZE, (this.pytorchMemoryPage + 1) * IO_PAGE_SIZE).map(row => [
      `${row.name} · track ${row.track}`, `${row.shard} / ${row.device ?? '—'}`, row.samples, `${row.peak_bytes} B`, `${row.last_bytes} B`,
    ]), 5, 'No observed framework allocated/reserved memory samples in this clock domain; missing counters are unknown.');
    this.renderPager(this.get('pytorch-memory-pager'), memory.length, this.pytorchMemoryPage, page => { this.pytorchMemoryPage = page; this.renderPytorch(); });
  }

  private renderPytorchSourceDetail(): void {
    const detail = this.get('pytorch-source-detail'); detail.replaceChildren();
    const source = this.selectedPytorchSource;
    if (!source || source.domain !== this.domain?.id) return;
    detail.append(node('h3', '', `Imported source · shard ${source.shard}`),
      node('p', 'io-note', 'Source loss / export coverage: Unknown (sourceLossKnown=false). Import diagnostics count skipped or ambiguous records; they do not measure source profiler coverage. Clock: relative imported microseconds converted to nanoseconds, not aligned with native time.'));
    const entries = [...Object.entries(source.metadata).map(([key, value]) => ['Metadata', key, value]),
      ...Object.entries(source.diagnostics).map(([key, value]) => ['Diagnostic', key, value])];
    const wrap = node('div', 'io-wrap'); const table = node('table'); table.id = 'pytorch-source-metadata';
    const header = node('tr'); for (const title of ['Type', 'Key', 'Value']) header.append(node('th', '', title));
    const head = node('thead'); head.append(header); table.append(head);
    const body = node('tbody');
    for (const entry of entries.slice(this.pytorchMetadataPage * IO_PAGE_SIZE, (this.pytorchMetadataPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr'); for (const value of entry) row.append(node('td', '', value)); body.append(row);
    }
    if (!entries.length) this.emptyCudaRow(body, 3, 'No source metadata or diagnostics recorded.');
    table.append(body); wrap.append(table); detail.append(wrap);
    const pager = node('div', 'io-pager'); pager.id = 'pytorch-metadata-pager'; detail.append(pager);
    this.renderPager(pager, entries.length, this.pytorchMetadataPage, page => { this.pytorchMetadataPage = page; this.renderPytorchSourceDetail(); });
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
    const gompRuntimes = runtimes.filter(runtime => runtime.backend === 'gomp');
    const omptRuntimes = runtimes.filter(runtime => runtime.backend !== 'gomp');
    const isGompRegion = (region: OpenmpRegion): boolean => runtimes.some(runtime => runtime.backend === 'gomp' &&
      runtime.shard === region.shard && runtime.instance === region.instance && runtime.pid === region.pid);
    this.get('openmp-count').textContent = `${regions.length} regions`;
    this.get('openmp-coverage').textContent = gompRuntimes.length ?
      'Selected clock domain · GNU libgomp observed joined parallel regions and outlined body wall time; GOMP wait / scheduled task time is unobserved (not zero). Work spread covers observed chunks only. Other tasks, pure waits, schedule events, dependence edges and inline static loop chunks are unavailable.' :
      'Selected clock domain · inclusive recorded work / wait (may overlap) · work spread is observed only';
    this.get('openmp-tasks').textContent = !summary.gomp || omptRuntimes.length ?
      `Capture-wide ${summary.gomp ? 'OMPT-only ' : ''}task totals: ${summary.tasks_created} created · ${summary.tasks_completed} completed · ${summary.execution_segments} execution segments (not filtered by domain)` :
      'OMPT task scheduling and completion are not observed by GNU libgomp; see GOMP task observations below.';
    const body = this.get('openmp-regions').querySelector('tbody')!;
    body.replaceChildren();
    for (const region of regions.slice(this.regionPage * IO_PAGE_SIZE, (this.regionPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr');
      const identity = node('td');
      const button = node('button', 'io-file', `Region ${region.parallel_id} · PID ${region.pid} · instance ${region.instance} · shard ${region.shard}`);
      button.type = 'button';
      button.addEventListener('click', () => { this.selectedRegion = region; this.threadPage = 0; this.renderOpenmpDetail(); });
      identity.append(button);
      const gomp = isGompRegion(region);
      row.append(identity, node('td', '', `${region.requested_threads} / ${region.observed_threads}`),
        node('td', '', optionalNs(region.duration_ns)), node('td', '', `${region.work_ns} ns`),
        node('td', '', gomp ? 'Unobserved (GOMP)' : `${region.wait_ns} ns`),
        node('td', '', `${optionalNs(region.work_min_ns)} / ${optionalNs(region.work_max_ns)} / ${optionalNs(region.work_spread_ns)}`));
      body.append(row);
    }
    if (!regions.length) {
      const row = node('tr'); const cell = node('td', 'empty-table', 'No OpenMP regions recorded in this clock domain');
      cell.colSpan = 6; row.append(cell); body.append(row);
    }
    this.renderPager(this.get('openmp-region-pager'), regions.length, this.regionPage, page => { this.regionPage = page; this.renderOpenmp(); });
    this.renderOpenmpDetail();
    this.renderGomp();
    this.get('openmp-runtime-count').textContent = `· ${runtimes.length} runtimes in this domain`;
    const runtimeList = this.get('openmp-runtime-list'); runtimeList.replaceChildren();
    for (const runtime of runtimes.slice(this.runtimePage * IO_PAGE_SIZE, (this.runtimePage + 1) * IO_PAGE_SIZE)) {
      const line = node('div', 'openmp-runtime-row');
      const select = node('button', 'io-file', `PID ${runtime.pid} · ${runtime.instance} · shard ${runtime.shard}`);
      select.type = 'button';
      select.addEventListener('click', () => { this.selectedRuntime = runtime; this.callbackPage = 0; this.renderOpenmpCallbacks(); });
      line.append(select, node('span', '', `${runtime.backend === 'gomp' ? 'GNU libgomp' : 'OMPT'} · Runtime ${runtime.version} · OpenMP ${runtime.omp_version} · initialized: ${runtime.initialized} · finalized: ${runtime.finalized} · sequence gaps: ${runtime.sequence_gaps} · unmatched endpoints: ${runtime.unmatched_endpoints} · abandoned intervals: ${runtime.abandoned_intervals}`));
      runtimeList.append(line);
    }
    if (!runtimes.length) runtimeList.append(node('p', 'io-note', 'No OpenMP runtime recorded in this clock domain; instrumentation may be unsupported or not initialized.'));
    this.renderPager(this.get('openmp-runtime-pager'), runtimes.length, this.runtimePage, page => { this.runtimePage = page; this.renderOpenmp(); });
    this.renderOpenmpCallbacks();
  }

  private renderGomp(): void {
    const gomp = this.report.openmp?.gomp;
    const panel = this.get('gomp-panel');
    panel.hidden = !gomp;
    if (!gomp) return;
    const domain = this.domain?.id;
    const runtimes = this.report.openmp!.runtimes.filter(runtime => runtime.domain === domain && runtime.backend === 'gomp');
    const apis = gomp.apis.filter(api => api.domain === domain);
    const bodies = gomp.bodies.filter(body => body.domain === domain);
    const selected = runtimes.map(runtime => {
      const mode = runtime.version.match(/detail=(regions|bodies|full)\b/)?.[1] ?? 'unknown';
      return `image ${runtime.instance} · shard ${runtime.shard}: detail ${mode}`;
    });
    this.get('gomp-coverage').textContent = `Selected-domain GNU libgomp runtimes: ${selected.length ? selected.join(' · ') : 'none'}. Detail selection: regions = joined parallel regions and outlined parallel bodies only; bodies adds tied ordinary task bodies; full adds reviewed GOMP API and observable loop chunks. Disabled or unsupported features are listed under runtime capabilities. GOMP calls can execute useful work from other tasks; API time is inclusive host time, not pure wait. Pure wait, OMPT schedule events, dependence edges and compiler-inlined static chunks are unavailable.`;
    this.get('gomp-tasks').textContent = `Capture-wide GOMP observations (not filtered by domain; do not sum with domain tables): ${gomp.task_creations} task creations · ${gomp.task_body_spans} task body spans · ${gomp.task_lifetimes_to_body_return} creation→body return lifetimes (not runtime task completion).`;
    this.get('gomp-api-count').textContent = `· ${apis.length} API rows in this domain`;
    this.tableRows('gomp-apis', apis.slice(this.gompApiPage * IO_PAGE_SIZE, (this.gompApiPage + 1) * IO_PAGE_SIZE).map(api => [
      `${api.name} · image ${api.instance} · shard ${api.shard}`, api.calls, `${api.total_ns} ns`, `${api.max_ns} ns`,
    ]), 4, 'No observed GOMP API calls in this clock domain (may be disabled by detail selection or unsupported; not measured zero).');
    this.renderPager(this.get('gomp-api-pager'), apis.length, this.gompApiPage, page => { this.gompApiPage = page; this.renderGomp(); });
    this.get('gomp-body-count').textContent = `· ${bodies.length} body rows in this domain`;
    this.tableRows('gomp-bodies', bodies.slice(this.gompBodyPage * IO_PAGE_SIZE, (this.gompBodyPage + 1) * IO_PAGE_SIZE).map(body => [
      `${body.kind} · image ${body.instance} · shard ${body.shard}`, body.tid, body.codeptr,
      body.count, `${body.total_ns} ns`, `${body.max_ns} ns`,
    ]), 6, 'No observed outlined body returns in this clock domain (not evidence of no work).');
    this.renderPager(this.get('gomp-body-pager'), bodies.length, this.gompBodyPage, page => { this.gompBodyPage = page; this.renderGomp(); });
  }

  private renderOpenmpDetail(): void {
    const detail = this.get('openmp-detail'); detail.replaceChildren();
    const region = this.selectedRegion;
    if (!region) return;
    const gomp = this.report.openmp?.runtimes.some(runtime => runtime.domain === region.domain && runtime.shard === region.shard &&
      runtime.instance === region.instance && runtime.pid === region.pid && runtime.backend === 'gomp');
    const threads = regionThreads(this.report, region);
    detail.append(node('h3', '', `Region ${region.parallel_id} · PID ${region.pid}`),
      node('p', 'io-note', gomp ?
        'GNU libgomp: outlined body inclusive wall time can include nested tasks, runtime work and waits; not OMPT scheduled execution or CPU utilization. Only observed chunks contribute to work. Wait, sync, scheduled intervals and execution segments are unobserved, not measured zero.' :
        'Recorded inclusive intervals; scheduled time includes logical task context (including waits), not CPU utilization or CPU cycles. Unknown durations are shown as —.'));
    const wrap = node('div', 'io-wrap'); const table = node('table'); table.id = 'openmp-threads';
    const head = node('thead'); const heading = node('tr');
    for (const label of ['Thread ID', 'Team index', 'Work', 'Wait', 'Sync', 'Scheduled interval', 'Execution segments']) heading.append(node('th', '', label));
    head.append(heading); table.append(head);
    const body = node('tbody');
    for (const thread of threads.slice(this.threadPage * IO_PAGE_SIZE, (this.threadPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr');
      row.append(node('td', '', thread.tid), node('td', '', thread.team_index ?? '—'), node('td', '', `${thread.work_ns} ns`),
        node('td', '', gomp ? 'Unobserved' : `${thread.wait_ns} ns`), node('td', '', gomp ? 'Unobserved' : `${thread.sync_ns} ns`),
        node('td', '', gomp ? 'Unobserved' : `${thread.scheduled_ns} ns`), node('td', '', gomp ? 'Unobserved' : thread.execution_segments));
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
    const gomp = runtime.backend === 'gomp';
    detail.append(node('h3', '', `${gomp ? 'GNU libgomp capabilities' : 'OMPT callbacks'} · PID ${runtime.pid} · ${runtime.instance}`),
      node('p', 'io-note', gomp ?
        'Capability status and detail describe enabled, disabled or unsupported instrumentation; registration alone is not evidence of an observed event. Unknown counts are not zero.' :
        'Registration status is not evidence that a callback was received. Unknown observed counts are not zero.'));
    const wrap = node('div', 'io-wrap'); const table = node('table'); table.id = 'openmp-callbacks';
    const head = node('thead'); const heading = node('tr');
    for (const label of [gomp ? 'Capability' : 'Callback', 'Registration status', 'Received callbacks', 'Observation complete', 'Detail / coverage']) heading.append(node('th', '', label));
    head.append(heading); table.append(head);
    const body = node('tbody');
    for (const callback of runtime.callbacks.slice(this.callbackPage * IO_PAGE_SIZE, (this.callbackPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr'); row.append(node('td', '', callback.name), node('td', '', callback.status),
        node('td', '', callback.observed ?? 'Unknown'), node('td', '', callback.observed_complete ? 'Yes' : 'No'), node('td', '', callback.detail || '—'));
      body.append(row);
    }
    if (!runtime.callbacks.length) {
      const row = node('tr'); const cell = node('td', 'empty-table', 'No callback registration data recorded');
      cell.colSpan = 5; row.append(cell); body.append(row);
    }
    table.append(body); wrap.append(table); detail.append(wrap);
    const pager = node('div', 'io-pager'); pager.id = 'openmp-callback-pager'; detail.append(pager);
    this.renderPager(pager, runtime.callbacks.length, this.callbackPage, page => { this.callbackPage = page; this.renderOpenmpCallbacks(); });
  }

  private renderCuda(): void {
    const cuda = this.report.cuda;
    if (!cuda) return;
    const domain = this.domain?.id;
    const activeDevices = new Set(cuda.activities.filter(activity => BigInt(activity.count) > 0n)
      .map(activity => JSON.stringify([activity.domain, activity.shard, activity.instance, activity.device])));
    const hasWork = (device: CudaDevice) => [device.kernels, device.copies, device.memsets].some(value => BigInt(value) > 0n) ||
      activeDevices.has(JSON.stringify([device.domain, device.shard, device.instance, device.device]));
    const devices = cuda.devices.filter(device => device.domain === domain && (device.visible !== false || hasWork(device)));
    const hidden = cuda.devices.filter(device => device.domain === domain && device.visible === false && !hasWork(device)).length;
    const apis = cuda.apis.filter(api => api.domain === domain);
    const runtimes = cuda.runtimes.filter(runtime => runtime.domain === domain);
    const sources = cuda.sources.filter(source => source.domain === domain);
    this.get('cuda-count').textContent = `${devices.length} devices${hidden ? ` · ${hidden} invisible unused omitted` : ''}`;
    const problems = sources.filter(source => !source.finalized || [source.ingress_dropped, source.cupti_dropped,
      source.source_errors, source.invalid_timing, source.unattributed, source.unresolved_correlations,
      source.ambiguous_correlations].some(value => BigInt(value) > 0n));
    this.get('cuda-health').textContent = !sources.length ? 'No CUDA source diagnostics recorded in this clock domain; device metadata alone does not establish measured GPU work.' :
      `${sources.length} source(s) · ${problems.length ? `${problems.length} with incomplete collection or diagnostics; see source health below.` : 'finalized without reported loss or timing issues.'}`;
    const body = this.get('cuda-devices').querySelector('tbody')!; body.replaceChildren();
    for (const device of devices.slice(this.cudaDevicePage * IO_PAGE_SIZE, (this.cudaDevicePage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr'); const identity = node('td');
      const select = node('button', 'io-file', `${device.name} · GPU ${device.device} · PID ${device.pid} · ${device.instance} · ${device.shard}`);
      select.type = 'button';
      select.addEventListener('click', () => { this.selectedCudaDevice = device; this.cudaActivityPage = 0; this.renderCudaActivities(); });
      identity.append(select, node('div', 'io-note', device.identity));
      const measured = hasWork(device);
      row.append(identity, node('td', '', `${device.visible === null ? 'Unknown' : device.visible ? 'Visible' : 'Invisible'} · ${measured ? 'timed activity' : 'metadata only / no timed work'}`),
        node('td', '', `${device.memory_bytes === null ? 'Unknown' : `${device.memory_bytes} B`} / ${device.compute_major === null || device.compute_minor === null ? 'Unknown' : `${device.compute_major}.${device.compute_minor}`} · ${device.multiprocessors ?? 'Unknown'} SMs`),
        node('td', '', `${device.kernels} / ${device.copies} / ${device.memsets}`),
        node('td', '', `${device.kernel_ns} / ${device.copy_ns} ns`),
        node('td', '', `${device.copy_bytes} / ${device.memset_bytes} B`), node('td', '', `${device.window_ns} ns`));
      body.append(row);
    }
    if (!devices.length) this.emptyCudaRow(body, 7, 'No visible CUDA devices recorded in this clock domain (or only invisible unused metadata).');
    this.renderPager(this.get('cuda-device-pager'), devices.length, this.cudaDevicePage, page => { this.cudaDevicePage = page; this.renderCuda(); });
    this.renderCudaActivities();
    this.get('cuda-api-count').textContent = `· ${apis.length} API rows`;
    const apiBody = this.get('cuda-apis').querySelector('tbody')!; apiBody.replaceChildren();
    for (const api of apis.slice(this.cudaApiPage * IO_PAGE_SIZE, (this.cudaApiPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr'); row.append(node('td', '', `${api.name} · ${api.instance} · ${api.shard}`),
        node('td', '', api.api_domain), node('td', '', api.calls), node('td', '', api.errors),
        node('td', '', `${api.total_ns} ns`), node('td', '', `${api.max_ns} ns`)); apiBody.append(row);
    }
    if (!apis.length) this.emptyCudaRow(apiBody, 6, 'No CUDA host API calls recorded in this clock domain.');
    this.renderPager(this.get('cuda-api-pager'), apis.length, this.cudaApiPage, page => { this.cudaApiPage = page; this.renderCuda(); });
    const list = this.get('cuda-runtime-list'); list.replaceChildren();
    for (const runtime of runtimes.slice(this.cudaRuntimePage * IO_PAGE_SIZE, (this.cudaRuntimePage + 1) * IO_PAGE_SIZE)) {
      const line = node('div', 'openmp-runtime-row');
      const select = node('button', 'io-file', `PID ${runtime.pid} · ${runtime.instance} · ${runtime.shard}`);
      select.type = 'button'; select.addEventListener('click', () => {
        this.selectedCudaRuntime = runtime; this.cudaCapabilityPage = this.cudaErrorPage = 0; this.renderCudaRuntimeDetail();
      });
      line.append(select, node('span', '', `initialized: ${runtime.initialized} · CUPTI ${runtime.cupti_version} · CUDA ${runtime.cuda_version} · clock: ${runtime.clock_method} · uncertainty: ${runtime.clock_uncertainty === null ? 'Unknown' : `${runtime.clock_uncertainty} ns`}`));
      list.append(line);
    }
    if (!runtimes.length) list.append(node('p', 'io-note', 'No CUDA runtime recorded in this clock domain.'));
    this.renderPager(this.get('cuda-runtime-pager'), runtimes.length, this.cudaRuntimePage, page => { this.cudaRuntimePage = page; this.renderCuda(); });
    this.renderCudaRuntimeDetail();
    const sourceBody = this.get('cuda-sources').querySelector('tbody')!; sourceBody.replaceChildren();
    for (const source of sources.slice(this.cudaSourcePage * IO_PAGE_SIZE, (this.cudaSourcePage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr'); row.append(node('td', '', source.shard),
        node('td', '', `${source.initialized_images} / ${source.finalized ? 'Yes' : 'No · GPU activity may be missing'}`),
        node('td', '', `${source.received} / ${source.attempted}`), node('td', '', source.ingress_dropped),
        node('td', '', source.cupti_dropped), node('td', '', source.source_errors),
        node('td', '', source.invalid_timing), node('td', '', source.unattributed),
        node('td', '', `${source.unresolved_correlations} / ${source.ambiguous_correlations}`)); sourceBody.append(row);
    }
    if (!sources.length) this.emptyCudaRow(sourceBody, 9, 'No CUDA source diagnostics recorded in this clock domain.');
    this.renderPager(this.get('cuda-source-pager'), sources.length, this.cudaSourcePage, page => { this.cudaSourcePage = page; this.renderCuda(); });
  }

  private emptyCudaRow(body: Element, columns: number, message: string): void {
    const row = node('tr'); const cell = node('td', 'empty-table', message); cell.colSpan = columns;
    row.append(cell); body.append(row);
  }

  private renderCudaActivities(): void {
    const detail = this.get('cuda-device-detail'); detail.replaceChildren();
    const device = this.selectedCudaDevice;
    if (!device) return;
    const activities = deviceActivities(this.report, device);
    detail.append(node('h3', '', `${device.name} · GPU ${device.device} · ${device.instance}`),
      node('p', 'io-note', 'Per-context/stream activity: inclusive durations overlap across streams. Counts and bytes are exact decimal values; no GPU utilization is inferred.'));
    const wrap = node('div', 'io-wrap'); const table = node('table'); table.id = 'cuda-activities';
    const head = node('thead'); const heading = node('tr');
    for (const label of ['Context / stream', 'Kind / name', 'Count', 'Inclusive time', 'Max time', 'Bytes']) heading.append(node('th', '', label));
    head.append(heading); table.append(head); const body = node('tbody');
    for (const activity of activities.slice(this.cudaActivityPage * IO_PAGE_SIZE, (this.cudaActivityPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr'); row.append(node('td', '', `${activity.context} / ${activity.stream}`),
        node('td', '', `${activity.kind} · ${activity.name}`), node('td', '', activity.count),
        node('td', '', `${activity.total_ns} ns`), node('td', '', `${activity.max_ns} ns`), node('td', '', `${activity.bytes} B`)); body.append(row);
    }
    if (!activities.length) this.emptyCudaRow(body, 6, 'No timed activity rows for this device; metadata alone is not measured GPU work.');
    table.append(body); wrap.append(table); detail.append(wrap);
    const pager = node('div', 'io-pager'); pager.id = 'cuda-activity-pager'; detail.append(pager);
    this.renderPager(pager, activities.length, this.cudaActivityPage, page => { this.cudaActivityPage = page; this.renderCudaActivities(); });
  }

  private renderCudaRuntimeDetail(): void {
    const detail = this.get('cuda-runtime-detail'); detail.replaceChildren();
    const runtime = this.selectedCudaRuntime;
    if (!runtime) return;
    detail.append(node('h3', '', `Capabilities · PID ${runtime.pid} · ${runtime.instance}`));
    const wrap = node('div', 'io-wrap'); const table = node('table'); table.id = 'cuda-capabilities';
    const head = node('thead'); const heading = node('tr');
    for (const label of ['Capability', 'Supported', 'Optional', 'Result', 'Detail']) heading.append(node('th', '', label));
    head.append(heading); table.append(head); const body = node('tbody');
    for (const capability of runtime.capabilities.slice(this.cudaCapabilityPage * IO_PAGE_SIZE, (this.cudaCapabilityPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr'); row.append(node('td', '', capability.name), node('td', '', capability.supported ? 'Yes' : 'No'),
        node('td', '', capability.optional ? 'Yes' : 'No'), node('td', '', capability.result), node('td', '', capability.detail)); body.append(row);
    }
    if (!runtime.capabilities.length) this.emptyCudaRow(body, 5, 'No capability information recorded.');
    table.append(body); wrap.append(table); detail.append(wrap);
    const pager = node('div', 'io-pager'); pager.id = 'cuda-capability-pager'; detail.append(pager);
    this.renderPager(pager, runtime.capabilities.length, this.cudaCapabilityPage, page => { this.cudaCapabilityPage = page; this.renderCudaRuntimeDetail(); });
    detail.append(node('h3', '', `Runtime errors · ${runtime.errors.length}`));
    const errors = node('div', 'cuda-errors');
    for (const error of runtime.errors.slice(this.cudaErrorPage * IO_PAGE_SIZE, (this.cudaErrorPage + 1) * IO_PAGE_SIZE)) errors.append(node('p', 'io-note', error));
    if (!runtime.errors.length) errors.append(node('p', 'io-note', 'No runtime errors reported.'));
    detail.append(errors);
    const errorPager = node('div', 'io-pager'); errorPager.id = 'cuda-error-pager'; detail.append(errorPager);
    this.renderPager(errorPager, runtime.errors.length, this.cudaErrorPage, page => { this.cudaErrorPage = page; this.renderCudaRuntimeDetail(); });
  }

  private renderPmu(): void {
    const pmu = this.report.pmu;
    if (!pmu) return;
    const domain = this.domain?.id;
    const events = pmu.events.filter(event => event.domain === domain);
    const metrics = pmu.metrics.filter(metric => metric.domain === domain);
    const hosts = pmu.hosts.filter(host => host.domain === domain);
    this.get('pmu-count').textContent = `${events.length} events`;
    const body = this.get('pmu-events').querySelector('tbody')!; body.replaceChildren();
    const typeName = (type: string): string => ({ '0': 'Hardware', '1': 'Software', '4': 'Raw hardware' })[type as '0' | '1' | '4'] ?? `Perf type ${type}`;
    for (const event of events.slice(this.pmuEventPage * IO_PAGE_SIZE, (this.pmuEventPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr');
      const identity = node('td'); identity.append(node('strong', '', event.name), node('div', 'io-note', `${typeName(event.event_type)} · config ${event.config}`));
      const valid = event.status === 'measured';
      const count = valid && event.raw !== null ? event.raw : 'Unavailable';
      row.append(identity, node('td', '', `PID ${event.pid} · ${event.shard}`),
        node('td', '', `${event.scope} · ${event.privilege}`),
        node('td', '', `${event.status}${event.error === null ? '' : ` · ${event.error}`}`),
        node('td', '', `${count} ${event.unit}`),
        node('td', '', `${optionalNs(event.time_enabled_ns)} / ${optionalNs(event.time_running_ns)}`),
        node('td', '', valid && event.running_percent !== null ? `${event.running_percent}%` : 'Unavailable'),
        node('td', '', valid && event.time_running_ns !== '0' && event.scaled_estimate !== null ? event.scaled_estimate : 'Unavailable'),
        node('td', '', `${event.final_read ? 'Yes' : 'No · checkpoint, not a workload total'} / ${event.samples}`));
      body.append(row);
    }
    if (!events.length) this.emptyCudaRow(body, 9, 'No PMU events recorded in this clock domain.');
    this.renderPager(this.get('pmu-event-pager'), events.length, this.pmuEventPage, page => { this.pmuEventPage = page; this.renderPmu(); });

    this.get('pmu-metric-count').textContent = `· ${metrics.length} shards`;
    const metricBody = this.get('pmu-metrics').querySelector('tbody')!; metricBody.replaceChildren();
    for (const metric of metrics.slice(this.pmuMetricPage * IO_PAGE_SIZE, (this.pmuMetricPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr'); row.append(node('td', '', metric.shard),
        node('td', '', metric.ipc_estimate ?? 'Unavailable — no valid final pair'),
        node('td', '', metric.cache_miss_percent_estimate === null ? 'Unavailable — no valid final pair' : `${metric.cache_miss_percent_estimate}%`));
      metricBody.append(row);
    }
    if (!metrics.length) this.emptyCudaRow(metricBody, 3, 'No final valid PMU metric pairs in this clock domain.');
    this.renderPager(this.get('pmu-metric-pager'), metrics.length, this.pmuMetricPage, page => { this.pmuMetricPage = page; this.renderPmu(); });

    this.get('pmu-host-count').textContent = `· ${hosts.length} hosts`;
    const list = this.get('pmu-host-list'); list.replaceChildren();
    for (const host of hosts.slice(this.pmuHostPage * IO_PAGE_SIZE, (this.pmuHostPage + 1) * IO_PAGE_SIZE)) {
      const button = node('button', 'io-file', `Shard ${host.shard}`); button.type = 'button';
      button.addEventListener('click', () => { this.selectedPmuHost = host; this.pmuMetadataPage = 0; this.renderPmuHostDetail(); });
      const line = node('div', 'openmp-runtime-row'); line.append(button, node('span', '', `${Object.keys(host.metadata).length} metadata entries`));
      list.append(line);
    }
    if (!hosts.length) list.append(node('p', 'io-note', 'No host metadata recorded in this clock domain.'));
    this.renderPager(this.get('pmu-host-pager'), hosts.length, this.pmuHostPage, page => { this.pmuHostPage = page; this.renderPmu(); });
    this.renderPmuHostDetail();
  }

  private renderPmuHostDetail(): void {
    const detail = this.get('pmu-host-detail'); detail.replaceChildren();
    const host = this.selectedPmuHost;
    if (!host) return;
    detail.append(node('h3', '', `Launch-time host metadata · shard ${host.shard}`));
    const entries = Object.entries(host.metadata);
    const wrap = node('div', 'io-wrap'); const table = node('table'); table.id = 'pmu-host-metadata';
    const head = node('thead'); const heading = node('tr');
    heading.append(node('th', '', 'Key'), node('th', '', 'Value')); head.append(heading); table.append(head);
    const body = node('tbody');
    for (const [key, value] of entries.slice(this.pmuMetadataPage * IO_PAGE_SIZE, (this.pmuMetadataPage + 1) * IO_PAGE_SIZE)) {
      const row = node('tr'); row.append(node('td', '', key), node('td', '', value)); body.append(row);
    }
    if (!entries.length) this.emptyCudaRow(body, 2, 'No host metadata entries recorded.');
    table.append(body); wrap.append(table); detail.append(wrap);
    const pager = node('div', 'io-pager'); pager.id = 'pmu-metadata-pager'; detail.append(pager);
    this.renderPager(pager, entries.length, this.pmuMetadataPage, page => { this.pmuMetadataPage = page; this.renderPmuHostDetail(); });
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
    const x = event.clientX - bounds.left, y = event.clientY - bounds.top;
    const hit = this.hitAt(x, y);
    const row = x < LABEL_WIDTH ? this.rows[lowerBound(this.rows, row => row.top + row.height > y + this.scroll.scrollTop)] : undefined;
    if (!hit && !row) { this.hideTooltip(); return; }
    const label = row ? row.label : hit!.selection.kind === 'span' ? hit!.selection.item.span.name : hit!.selection.kind === 'io' || hit!.selection.kind === 'storage' ? `${hit!.selection.graph.label} · ${this.ioMetric}` : hit!.selection.item.counter.name;
    this.tooltip.textContent = label;
    this.canvas.title = label;
    this.tooltip.hidden = false;
    this.tooltip.style.left = `${Math.max(8, Math.min(event.clientX + 12, window.innerWidth - this.tooltip.offsetWidth - 8))}px`;
    this.tooltip.style.top = `${Math.max(8, Math.min(event.clientY + 16, window.innerHeight - this.tooltip.offsetHeight - 8))}px`;
  }

  private fit(): void {
    this.clearRange();
    const extent = this.extent();
    // Numeric conversion is restricted to the view-relative domain extent.
    this.nsPerPx = Math.max(1, Number(extent) / this.plotWidth());
    this.start = 0n;
    this.schedule();
  }

  private pointerTime(clientX: number): bigint {
    const x = clamp(clientX - this.canvas.getBoundingClientRect().left - LABEL_WIDTH, 0, this.plotWidth());
    const at = xToTime(x, this.start, this.nsPerPx);
    return at < 0n ? 0n : at > this.extent() ? this.extent() : at;
  }

  private clearRange(): void {
    this.drag = undefined; this.timeRange = undefined; this.timeFilter = undefined;
    this.updateRangeControls(); this.schedule();
  }

  private updateRangeControls(): void {
    for (const id of ['zoom-selection', 'filter-selection']) (this.get(id) as HTMLButtonElement).disabled = !this.timeRange;
    (this.get('clear-selection') as HTMLButtonElement).disabled = !this.timeRange && !this.timeFilter;
    const text = (range: TimeRange) => `${formatNs(range.start)}–${formatNs(range.end)} (${formatNs(range.end - range.start)})`;
    const label = this.get('selection-range');
    label.textContent = `${this.timeRange ? `Selected ${text(this.timeRange)}` : 'Shift-drag to select a time range'}${this.timeFilter ? ` · Timeline filter ${text(this.timeFilter)} · summaries remain capture-wide` : ''}`;
    label.title = this.timeRange ? `${this.timeRange.start}–${this.timeRange.end} ns · domain-relative` : '';
    label.dataset.startNs = this.timeRange?.start.toString() ?? '';
    label.dataset.endNs = this.timeRange?.end.toString() ?? '';
    label.dataset.filtered = String(!!this.timeFilter);
  }

  private rangePixels(range: TimeRange): [number, number] {
    return [range.start, range.end].map(time => clamp(LABEL_WIDTH + timeToX(time, this.start, this.nsPerPx), LABEL_WIDTH + 1, this.scroll.clientWidth)) as [number, number];
  }

  private zoom(factor: number, cursorX = this.plotWidth() / 2): void {
    if (!this.domain) return;
    const anchor = xToTime(cursorX, this.start, this.nsPerPx);
    this.nsPerPx = clamp(this.nsPerPx * factor, 1, Math.max(1, Number(this.extent()) * 4 / this.plotWidth()));
    this.start = this.clampStart(anchor - BigInt(Math.round(cursorX * this.nsPerPx)));
    this.schedule();
  }

  private buildRows(): void {
    this.rows = timelineRows(this.report, this.data, this.domain?.id, this.filter.value, this.collapsed, this.ioFilesystem, this.ioVisible);
    for (const [kind, button] of this.systemButtons) {
      const key = `system:${this.domain?.id}:${kind}`;
      button.hidden = !this.rows.some(row => row.key === key);
      const expanded = this.rows.find(row => row.key === key)?.expanded ?? false;
      button.textContent = `${kind === 'cpu' ? 'CPU' : 'Memory'} ${expanded ? '▾' : '▸'}`;
      button.setAttribute('aria-expanded', String(expanded));
      button.setAttribute('aria-label', `${expanded ? 'Collapse' : 'Expand'} ${kind} system graphs`);
    }
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
    this.frame = requestAnimationFrame(() => { this.frame = 0; this.draw(); this.queueWindow(); });
  }

  private queueWindow(): void {
    const viewer = this.report.viewer;
    if (!viewer || !this.domain || !this.plotWidth()) return;
    const begin = lowerBound(this.rows, row => row.top + row.height > this.scroll.scrollTop);
    const visible = this.rows.slice(begin).filter(row => row.top < this.scroll.scrollTop + this.scroll.clientHeight);
    const rawTracks = [...new Set(visible.filter(row => row.kind === 'track')
      .map(row => row.track?.id).filter((id): id is string => !!id))];
    const rawScopes = [...new Set(visible.filter(row => row.io).map(row => row.io!.key))];
    const systemIds = new Set<string>();
    const shards = new Map(this.report.shards.map(shard => [shard.id, shard]));
    for (const row of visible.filter(row => row.kind === 'counter')) {
      if (!row.system && row.track) { systemIds.add(row.track.id); continue; }
      if (row.track && !row.track.id.startsWith('system:')) systemIds.add(row.track.id);
      else if (row.system) for (const track of this.report.tracks) if (track.kind === 'system' && !track.parent &&
        track.details.resource === row.system && shards.get(track.shard)?.domain === this.domain.id) systemIds.add(track.id);
    }
    for (const row of visible) if (row.storage) for (const leaf of row.storage.leaves) systemIds.add(leaf.id);
    const end = this.start + this.visibleDuration();
    const request = { snapshot: viewer.snapshot, domain: this.domain.id, start_ns: this.start.toString(),
      end_ns: (end < this.extent() ? end : this.extent()).toString(), pixels: clamp(Math.round(this.plotWidth()), 64, 4096),
      tracks: rawTracks, systems: [...systemIds], io_scopes: rawScopes, filesystem: this.ioFilesystem };
    if (BigInt(request.end_ns) <= this.start) return;
    const key = windowIdentity(request);
    if (key !== this.windowKey) {
      this.windowKey = key;
      this.windowAbort?.abort(); this.windowAbort = undefined;
      if (this.windowTimer) clearTimeout(this.windowTimer);
      this.windowTimer = undefined;
      this.windowCache = new WindowCache(); this.windowFailed = false;
      this.windowScopeLimited = false;
      this.visibleWindowKey = ''; this.get('retry-window').hidden = true;
    }
    const visibleKey = JSON.stringify([rawTracks, [...systemIds], rawScopes]);
    if (visibleKey !== this.visibleWindowKey) {
      this.visibleWindowKey = visibleKey; this.attemptedScopes = emptyScopes();
    }
    if (this.windowAbort || this.windowFailed) return;
    const missing = this.windowCache.missing(request);
    for (const kind of ['tracks', 'systems', 'io_scopes'] as const)
      missing[kind] = missing[kind].filter(id => !this.attemptedScopes[kind].includes(id));
    if (!hasScopes(missing)) {
      if (this.windowTimer) clearTimeout(this.windowTimer);
      this.windowTimer = undefined;
      const result = this.windowCache.merged();
      if (result) this.showWindowStatus(result);
      else {
        this.windowStatus.textContent = 'No visible rows to load'; this.windowStatus.dataset.state = 'ready';
      }
      return;
    }
    if (this.windowTimer) clearTimeout(this.windowTimer);
    const batch = { ...request, tracks: missing.tracks.slice(0, 64), systems: missing.systems.slice(0, 1024), io_scopes: missing.io_scopes.slice(0, 32) };
    this.windowStatus.textContent = this.loadedWindowKey === key ? 'Loading additional rows · reusing cached data' : 'Loading window · previous data may be stale';
    this.windowStatus.dataset.state = 'loading';
    this.windowTimer = setTimeout(() => { this.windowTimer = undefined; void this.loadWindow(key, batch); }, 120);
  }

  private async loadWindow(key: string, request: WindowRequest): Promise<void> {
    const viewer = this.report.viewer!;
    const path = localEndpoint(viewer.endpoint);
    if (!path) { this.windowStatus.textContent = 'Window unavailable: invalid local endpoint'; this.windowFailed = true; return; }
    const controller = new AbortController(); this.windowAbort = controller;
    try {
      const response = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(request), signal: controller.signal });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const result = await response.json() as WindowResponse;
      if (controller.signal.aborted || this.windowKey !== key) return;
      if (result.snapshot !== viewer.snapshot || result.domain !== request.domain || result.start_ns !== request.start_ns ||
        result.end_ns !== request.end_ns || !Array.isArray(result.spans) || !Array.isArray(result.counters) ||
        !Array.isArray(result.dependencies) || !result.io) throw new Error('Invalid window response');
      this.windowCache.add(request, result);
      for (const kind of ['tracks', 'systems', 'io_scopes'] as const) this.attemptedScopes[kind].push(...request[kind]);
      this.windowScopeLimited = this.windowCache.evicted;
      this.applyWindow(this.windowCache.merged()!);
    } catch (error) {
      if (!controller.signal.aborted && this.windowKey === key) {
        this.windowStatus.textContent = `Window failed (${error instanceof Error ? error.message : 'network error'}) · previous data may be stale`;
        this.windowStatus.dataset.state = 'error';
        this.windowFailed = true; this.get('retry-window').hidden = false;
      }
    } finally { if (this.windowAbort === controller) { this.windowAbort = undefined; this.schedule(); } }
  }

  private applyWindow(result: WindowResponse): void {
    this.loadedWindowKey = this.windowKey;
    this.hasWindowData = true;
    const old = this.selection;
    this.report.spans = result.spans;
    this.report.counters = result.counters;
    this.report.dependencies = result.dependencies;
    this.currentIoBins = result.io;
    setIoWindow(this.report, result.io);
    this.ioPlots = new WeakMap();
    this.storagePlots = new WeakMap();
    this.data = indexReport(this.report);
    if (old?.kind === 'span') {
      const span = old.item.span;
      const found = this.data.get(span.track)?.spans.items.find(item =>
        span.source_id ? item.span.source_id === span.source_id && item.span.track === span.track : item.span.id === span.id && item.span.track === span.track);
      this.selection = found ? { kind: 'span', item: found } : undefined;
    } else if (old?.kind === 'counter') {
      const counter = old.item.counter;
      const found = [...(this.data.get(counter.track)?.counters.values() ?? [])].flat().find(item =>
        item.counter.timestamp_ns === counter.timestamp_ns && item.counter.name === counter.name && item.counter.value === counter.value);
      this.selection = found ? { kind: 'counter', item: found } : undefined;
    } else if (old?.kind === 'io' || old?.kind === 'storage') this.selection = undefined;
    this.updateRelated(); this.showDetails();
    this.showWindowStatus(result);
    this.buildRows(); this.schedule();
  }

  private showWindowStatus(result: WindowResponse): void {
    this.windowStatus.dataset.state = result.limited || this.windowScopeLimited ? 'limited' : 'ready';
    const bins = Object.values(result.io).reduce((count, values) => count + values.length, 0);
    this.windowStatus.textContent = `Loaded ${formatNs(BigInt(result.start_ns))}–${formatNs(BigInt(result.end_ns))} · ${result.spans.length} spans · ${result.counters.length} counter samples · ${bins} I/O bins${this.windowScopeLimited ? ' · cache limit: oldest rows may reload' : ''}${result.limited ? ' · limited: zoom in / expand fewer tracks' : ''}${result.warnings?.length ? ` · ${result.warnings.join('; ')}` : ''}`;
  }

  private emptyWindowMessage(row: TimelineRow, loading: string, empty: string): string {
    if (!this.report.viewer) return empty;
    let covered = this.hasWindowData && this.loadedWindowKey === this.windowKey;
    if (row.io) covered &&= this.windowCache.covers('io_scopes', row.io.key);
    else if (row.storage) covered &&= row.storage.leaves.every(track => this.windowCache.covers('systems', track.id));
    else if (row.system && row.track?.id.startsWith('system:')) {
      const shards = new Set(this.report.shards.filter(shard => shard.domain === this.domain?.id).map(shard => shard.id));
      covered &&= this.report.tracks.filter(track => track.kind === 'system' && !track.parent &&
        track.details.resource === row.system && shards.has(track.shard)).every(track => this.windowCache.covers('systems', track.id));
    } else if (row.track) covered &&= this.windowCache.covers('systems', row.track.id);
    if (covered) return empty;
    if (this.windowFailed) return 'Window unavailable · Retry window';
    if (this.windowCache.evicted && this.windowStatus.dataset.state !== 'loading') return 'Cache limit · zoom in / expand fewer rows';
    return loading;
  }

  private showFileControls(thread: string): void {
    this.activeFiles = thread;
    const page = this.filePages.get(thread);
    const count = this.report.tracks.find(track => track.id === thread)?.details['viewer.file_count'] ?? page?.total ?? 0;
    this.fileControls.replaceChildren(node('span', '', `Files (${count}) · ${page ? `rows ${page.offset + 1}–${page.offset + page.tracks.length} of ${page.total}` : 'loading…'}`));
    if (!page) return;
    const previous = node('button', '', 'Previous files'); previous.type = 'button'; previous.disabled = page.offset === 0;
    previous.addEventListener('click', () => void this.loadFiles(thread, Math.max(0, page.offset - IO_PAGE_SIZE)));
    const next = node('button', '', 'Next files'); next.type = 'button'; next.disabled = page.offset + page.tracks.length >= page.total;
    next.addEventListener('click', () => void this.loadFiles(thread, page.offset + page.tracks.length));
    this.fileControls.append(previous, next);
  }

  private async loadFiles(thread: string, offset: number): Promise<void> {
    const viewer = this.report.viewer;
    if (!viewer) return;
    const path = localEndpoint(viewer.files_endpoint);
    if (!path) { this.fileControls.textContent = 'Files unavailable: invalid local endpoint'; return; }
    this.fileAbort.get(thread)?.abort();
    const controller = new AbortController(); this.fileAbort.set(thread, controller);
    this.activeFiles = thread;
    this.fileControls.textContent = 'Loading files…';
    try {
      const response = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ snapshot: viewer.snapshot, thread, offset, limit: IO_PAGE_SIZE }), signal: controller.signal });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const page = await response.json() as { snapshot: string; tracks: Track[]; total: number; offset: number; next_offset: number | null };
      if (controller.signal.aborted || page.snapshot !== viewer.snapshot || page.offset !== offset || !Array.isArray(page.tracks) || page.tracks.length > IO_PAGE_SIZE ||
        page.tracks.some(track => track.kind !== 'io' || track.parent !== thread)) throw new Error('Invalid files response');
      const old = this.filePages.get(thread);
      if (old) this.report.tracks = this.report.tracks.filter(track => !old.tracks.some(file => file.id === track.id));
      this.filePages.delete(thread);
      this.filePages.set(thread, { thread, offset, total: page.total, tracks: page.tracks });
      while (this.filePages.size > 8) {
        const oldest = this.filePages.keys().next().value!;
        this.report.tracks = this.report.tracks.filter(track => !this.filePages.get(oldest)!.tracks.some(file => file.id === track.id));
        this.filePages.delete(oldest);
        if (this.activeFiles === oldest) this.fileControls.replaceChildren();
      }
      this.report.tracks.push(...page.tracks);
      const retained = new Set(this.report.tracks.map(track => track.id));
      this.report.spans = this.report.spans.filter(span => retained.has(span.track));
      this.report.counters = this.report.counters.filter(counter => retained.has(counter.track));
      if (this.selection?.kind === 'span' && !retained.has(this.selection.item.span.track) ||
        this.selection?.kind === 'counter' && !retained.has(this.selection.item.counter.track)) {
        this.selection = undefined; this.showDetails();
      }
      this.data = indexReport(this.report);
      setIoWindow(this.report, this.currentIoBins);
      this.ioPlots = new WeakMap();
      this.storagePlots = new WeakMap();
      if (this.activeFiles === thread) this.showFileControls(thread);
      this.buildRows(); this.schedule();
    } catch (error) {
      if (!controller.signal.aborted) this.fileControls.textContent = `Files failed (${error instanceof Error ? error.message : 'network error'}) · previous page retained`;
    } finally { if (this.fileAbort.get(thread) === controller) this.fileAbort.delete(thread); }
  }

  private draw(): void {
    this.hideTooltip();
    const width = this.scroll.clientWidth;
    const height = this.scroll.clientHeight;
    if (!width || !height) return;
    const ctx = this.resize(this.canvas, width, height);
    this.hits.length = 0;
    ctx.fillStyle = this.colors.background; ctx.fillRect(0, 0, width, height);
    const ticks = this.drawRuler(width);
    const from = lowerBound(this.rows, row => row.top + row.height > this.scroll.scrollTop);
    const viewEnd = this.start + this.visibleDuration();
    const anchors = new Map<string, { x: number; y: number }>();
    for (let i = from; i < this.rows.length; i++) {
      const row = this.rows[i]!;
      const y = row.top - this.scroll.scrollTop;
      if (y >= height) break;
      ctx.fillStyle = row.kind === 'shard' ? this.colors.shard : row.kind === 'rank' ? this.colors.rank : i % 2 ? this.colors.odd : this.colors.even;
      ctx.fillRect(0, y, width, row.height);
      if (row.kind !== 'rank' && row.kind !== 'shard') {
        ctx.fillStyle = this.colors.labelBackground; ctx.fillRect(0, y, LABEL_WIDTH - 1, row.height);
      }
      ctx.strokeStyle = this.colors.border; ctx.beginPath(); ctx.moveTo(0, y + row.height - .5); ctx.lineTo(width, y + row.height - .5); ctx.stroke();
      ctx.save(); ctx.beginPath(); ctx.rect(LABEL_WIDTH + 1, y, width - LABEL_WIDTH - 1, row.height); ctx.clip();
      if (row.kind !== 'rank' && row.kind !== 'shard') {
        ctx.strokeStyle = this.colors.grid; ctx.lineWidth = 1; ctx.beginPath();
        for (const x of ticks) { ctx.moveTo(x + .5, y + 1); ctx.lineTo(x + .5, y + row.height - 1); }
        ctx.stroke();
      }
      if (this.timeFilter) {
        const [left, right] = this.rangePixels(this.timeFilter);
        ctx.beginPath(); ctx.rect(left, y, right - left, row.height); ctx.clip();
      }
      if (row.kind === 'track') this.drawSpans(ctx, row, y, viewEnd, anchors);
      if (row.kind === 'counter') this.drawCounter(ctx, row, y, viewEnd);
      if (row.kind === 'io') this.drawIoGraph(ctx, row, y, viewEnd);
      if (row.kind === 'storage') this.drawStorageGraph(ctx, row, y, viewEnd);
      ctx.restore();
      ctx.fillStyle = row.kind === 'shard' ? this.colors.heading : this.colors.label;
      ctx.font = row.kind === 'shard' || row.kind === 'rank' || (row.system || row.storage) && row.depth === 0 ? '600 12px system-ui' : '12px system-ui';
      const indent = Math.min(125, 15 + row.depth * 17);
      if (row.hasChildren) { ctx.fillStyle = this.colors.muted; ctx.fillText((row.expanded ?? !this.collapsed.has(row.key)) ? '▾' : '▸', indent, y + row.height / 2 + 4); }
      ctx.fillStyle = row.system ? this.colors[row.system] : row.storage ? this.colors.storage : row.kind === 'shard' ? this.colors.heading : this.colors.label;
      const labelX = indent + (row.hasChildren ? 16 : 4);
      const maxWidth = LABEL_WIDTH - labelX - 10;
      ctx.fillText(this.ellipsize(ctx, row.label, maxWidth), labelX, y + row.height / 2 + (row.system ? -3 : 4));
      if (row.system) {
        ctx.font = '10px system-ui'; ctx.fillStyle = this.colors.muted;
        const subtitle = row.system === 'cpu' ? 'CPU utilization · 0–100%' : 'Memory used · includes cache';
        ctx.fillText(this.ellipsize(ctx, subtitle, maxWidth), labelX, y + row.height / 2 + 13);
      }
      if (row.storage) {
        ctx.font = '10px system-ui'; ctx.fillStyle = this.colors.muted;
        const subtitle = row.storage.backend === 'block' ? 'Host block-device layer · not application-only' :
          row.storage.backend === 'lustre' ? 'Client bulk RPC · not server physical I/O' : 'Client server payload · not server physical I/O';
        ctx.fillText(this.ellipsize(ctx, subtitle, maxWidth), labelX, y + row.height / 2 + 14);
      }
      if (row.kind === 'track' && row.track && row.height >= 52) {
        ctx.font = '10px system-ui'; ctx.fillStyle = this.colors.muted;
        ctx.fillText(`${row.track.kind.replace('_', ' ')}${row.track.rank === null ? '' : ` · rank ${row.track.rank}`}`, labelX, y + row.height / 2 + 19, maxWidth);
      }
      if (row.kind === 'shard' && !row.shard.complete) {
        ctx.font = '10px system-ui'; ctx.fillStyle = this.colors.partial; ctx.fillText('PARTIAL', LABEL_WIDTH - 55, y + row.height / 2 + 4);
      }
    }
    ctx.save();
    if (this.timeFilter) {
      const [left, right] = this.rangePixels(this.timeFilter);
      ctx.beginPath(); ctx.rect(left, 0, right - left, height); ctx.clip();
      const visibleHits = this.hits.flatMap(hit => {
        const x = Math.max(left, hit.x), end = Math.min(right, hit.x + hit.width);
        return end > x ? [{ ...hit, x, width: end - x }] : [];
      });
      this.hits.splice(0, this.hits.length, ...visibleHits);
    }
    this.drawLinks(ctx, anchors); ctx.restore();
    const range = this.timeRange ?? this.timeFilter;
    if (range) {
      const [left, right] = this.rangePixels(range);
      if (right > left) {
        ctx.save(); ctx.fillStyle = this.colors.selected; ctx.globalAlpha = .13;
        ctx.fillRect(left, 0, right - left, height); ctx.globalAlpha = .85;
        ctx.strokeStyle = this.colors.selected; ctx.strokeRect(left, .5, right - left, height - 1); ctx.restore();
      }
    }
    ctx.fillStyle = this.colors.border; ctx.fillRect(LABEL_WIDTH - 1, 0, 1, height);
    if (!this.rows.length) {
      ctx.fillStyle = this.colors.muted; ctx.font = '14px system-ui';
      ctx.fillText(this.domain ? 'No tracks match this view. Try clearing the filter.' : 'No clock domains recorded. This capture has no timeline events.', 25, 50);
    }
    this.rangeLabel.textContent = this.domain ? `${formatNs(this.start)} – ${formatNs(this.start + this.visibleDuration())} · domain-relative${this.domain.calibration ? ` · estimated ±${this.domain.calibration.uncertainty_ns} ns` : this.report.domains.length > 1 ? ' · unaligned' : ''}` : 'No clock domain';
  }

  private ellipsize(ctx: CanvasRenderingContext2D, text: string, width: number): string {
    const key = `${ctx.font}:${width}:${text}`;
    const cached = this.labelCache.get(key);
    if (cached !== undefined) return cached;
    let result = text;
    if (ctx.measureText(text).width > width) {
      let low = 0, high = text.length;
      while (low < high) {
        const mid = Math.ceil((low + high) / 2);
        if (ctx.measureText(`${text.slice(0, mid)}…`).width <= width) low = mid; else high = mid - 1;
      }
      result = `${text.slice(0, low)}…`;
    }
    if (this.labelCache.size >= 2048) this.labelCache.clear();
    this.labelCache.set(key, result);
    return result;
  }

  private drawRuler(width: number): number[] {
    const ctx = this.resize(this.ruler, Math.max(1, width - LABEL_WIDTH), 35);
    const plotWidth = width - LABEL_WIDTH;
    ctx.fillStyle = this.colors.ruler; ctx.fillRect(0, 0, plotWidth, 35);
    ctx.font = '11px ui-monospace, monospace'; ctx.fillStyle = this.colors.muted; ctx.strokeStyle = this.colors.tick;
    const duration = this.nsPerPx * 115;
    const exponent = 10 ** Math.floor(Math.log10(Math.max(1, duration)));
    const step = BigInt(Math.max(1, Math.round(([1, 2, 5, 10].find(n => n * exponent >= duration) ?? 10) * exponent)));
    const first = (this.start / step) * step;
    const ticks: number[] = [];
    for (let tick = first, count = 0; count < 150 && tick <= this.start + this.visibleDuration(); tick += step, count++) {
      const x = timeToX(tick, this.start, this.nsPerPx);
      if (x < 0) continue;
      ticks.push(LABEL_WIDTH + x);
      ctx.beginPath(); ctx.moveTo(x + .5, 26); ctx.lineTo(x + .5, 35); ctx.stroke();
      ctx.fillText(formatNs(tick), x + 5, 20);
    }
    return ticks;
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
    const begin = this.timeFilter && this.timeFilter.start > this.start ? this.timeFilter.start : this.start;
    const finish = this.timeFilter && this.timeFilter.end < end ? this.timeFilter.end : end;
    const spans = finish > begin ? visibleSpans(data.spans, begin, finish, MAX_DRAWN_SPANS) : [];
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
    if (row.system) { this.drawSystemCounter(ctx, row, y, end); return; }
    const trackData = this.data.get(row.track!.id);
    const samples = trackData?.counters.get(row.series!) ?? [];
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

  private drawSystemCounter(ctx: CanvasRenderingContext2D, row: TimelineRow, y: number, end: bigint): void {
    const data = this.data.get(row.track!.id);
    const samples = data?.counters.get(row.series!) ?? [];
    const color = this.colors[row.system!];
    const top = y + 10, bottom = y + row.height - 22, height = bottom - top;
    ctx.font = '11px system-ui'; ctx.fillStyle = this.colors.muted;
    if (!samples.length) {
      ctx.fillText(this.emptyWindowMessage(row, 'Loading system samples…',
        row.depth === 0 ? 'No aligned samples; expand hosts' : this.report.viewer ? 'No observed samples in this window' : 'No retained samples'),
      LABEL_WIDTH + 8, y + row.height / 2);
      return;
    }
    // Include neighboring observations so a zoomed viewport between samples
    // still shows its continuous segment. Never extend beyond measured endpoints.
    const first = Math.max(0, lowerBound(samples, sample => sample.time >= this.start) - 1);
    const last = Math.min(samples.length, lowerBound(samples, sample => sample.time > end) + 1);
    const stride = Math.max(1, Math.ceil((last - first) / 3000));
    const indices: number[] = [];
    for (let i = first; i < last; i += stride) indices.push(i);
    if (last > first && indices.at(-1) !== last - 1) indices.push(last - 1);
    const values = indices.map(i => Number(samples[i]!.counter.value) / (row.system === 'memory' ? 2 ** 30 : 1));
    const max = row.system === 'cpu' ? 100 : row.capacity !== undefined ? Number(BigInt(row.capacity)) / 2 ** 30 : Math.max(0, ...values.filter(Number.isFinite));
    const range = max || 1;
    ctx.strokeStyle = this.colors.grid; ctx.lineWidth = 1;
    ctx.beginPath();
    for (const fraction of [0, .25, .5, .75, 1]) {
      const py = bottom - height * fraction;
      ctx.moveTo(LABEL_WIDTH + 1, py + .5); ctx.lineTo(this.scroll.clientWidth, py + .5);
    }
    ctx.stroke();
    type Point = { x: number; y: number; sample: IndexedCounter };
    const segments: Point[][] = [];
    let segment: Point[] | undefined;
    const gaps = data?.systemGaps?.get(row.series!);
    for (let i = 0; i < indices.length; i++) {
      const value = values[i]!;
      if (!Number.isFinite(value)) { segment = undefined; continue; }
      const continuous = i > 0 && gaps && systemPointsConnected(gaps, indices[i - 1]!, indices[i]!);
      if (!segment || !continuous) { segment = []; segments.push(segment); }
      const sample = samples[indices[i]!]!;
      segment.push({ x: LABEL_WIDTH + timeToX(sample.time, this.start, this.nsPerPx),
        y: bottom - height * clamp(value / range, 0, 1), sample });
    }
    const fill = ctx.createLinearGradient(0, top, 0, bottom);
    fill.addColorStop(0, `${color}${this.theme === 'dark' ? '70' : '50'}`);
    fill.addColorStop(1, `${color}${this.theme === 'dark' ? '28' : '20'}`);
    for (const segment of segments) {
      const firstPoint = segment[0]!, lastPoint = segment.at(-1)!;
      if (segment.length > 1) {
        ctx.beginPath(); ctx.moveTo(firstPoint.x, bottom);
        for (const point of segment) ctx.lineTo(point.x, point.y);
        ctx.lineTo(lastPoint.x, bottom); ctx.closePath(); ctx.fillStyle = fill; ctx.fill();
      }
      ctx.beginPath(); ctx.moveTo(firstPoint.x, firstPoint.y);
      for (const point of segment.slice(1)) ctx.lineTo(point.x, point.y);
      ctx.strokeStyle = color; ctx.lineWidth = 1.75; ctx.stroke();
      for (let i = 0; i < segment.length; i++) {
        const point = segment[i]!;
        const selected = this.selection?.kind === 'counter' && this.selection.item === point.sample;
        if (selected || segment.length === 1) {
          ctx.beginPath(); ctx.arc(point.x, point.y, selected ? 4 : 2, 0, Math.PI * 2);
          ctx.fillStyle = selected ? this.colors.selected : color; ctx.fill();
        }
        const left = Math.max(LABEL_WIDTH + 1, i ? (segment[i - 1]!.x + point.x) / 2 : point.x - (segment.length === 1 ? 4 : 0));
        const right = Math.min(this.scroll.clientWidth, i + 1 < segment.length ? (point.x + segment[i + 1]!.x) / 2 : point.x + (segment.length === 1 ? 4 : 0));
        if (right > left) this.hits.push({ x: left, y: top, width: right - left, height, selection: { kind: 'counter', item: point.sample } });
      }
    }
    ctx.lineWidth = 1; ctx.textAlign = 'right'; ctx.font = '600 10px system-ui';
    for (const fraction of [0, 1]) {
      const label = row.system === 'cpu' ? `${max * fraction}%` : `${(max * fraction).toLocaleString(undefined, { maximumSignificantDigits: 4 })} GiB`;
      const py = fraction ? top + 9 : bottom - 3;
      ctx.strokeStyle = this.colors.background; ctx.lineWidth = 3; ctx.strokeText(label, this.scroll.clientWidth - 8, py);
      ctx.fillStyle = this.colors.muted; ctx.fillText(label, this.scroll.clientWidth - 8, py);
    }
    ctx.lineWidth = 1; ctx.textAlign = 'left'; ctx.font = '10px system-ui';
    const source = samples[0]!.counter.attributes['system.report_samples'];
    const count = source && source !== String(samples.length) ? `${samples.length}/${source} full-run samples` : `${samples.length} samples`;
    const scope = row.track?.id.startsWith('system:') ? 'aligned host estimate' : 'host-wide, not application usage';
    ctx.fillText(`${count} · ${scope}${row.system === 'memory' && row.capacity === undefined ? ' · capacity unknown' : ''}`, LABEL_WIDTH + 8, y + row.height - 5);
  }

  private drawIoGraph(ctx: CanvasRenderingContext2D, row: TimelineRow, y: number, end: bigint): void {
    const graph = row.io!;
    let cache = this.ioPlots.get(graph);
    if (!cache) { cache = new Map(); this.ioPlots.set(graph, cache); }
    let plot = cache.get(this.ioMetric);
    if (!plot) {
      const bins = ioGraphBins(graph) ?? [];
      const values = bins.map(bin => ioMetricValues(bin, this.ioMetric));
      let max = 0;
      for (const pair of values) for (const value of pair) if (Number.isFinite(value)) max = Math.max(max, value);
      plot = { bins, values, starts: bins.map(bin => BigInt(bin.start_ns)), ends: bins.map(bin => BigInt(bin.start_ns) + BigInt(bin.duration_ns)), max: max || 1 }; cache.set(this.ioMetric, plot);
    }
    ctx.font = '10px system-ui'; ctx.fillStyle = this.colors.muted;
    if (!plot.bins.length) { ctx.fillText(this.emptyWindowMessage(row, 'Loading I/O window…', this.report.viewer ?
      'No observed I/O bins in this window' : 'No full-capture I/O bins available for this selection'), LABEL_WIDTH + 8, y + 32); return; }
    const first = lowerBound(plot.ends, time => time > this.start);
    const last = lowerBound(plot.starts, time => time >= end);
    const palette = this.theme === 'light' ? ['#176b61', '#af661c'] : ['#6bcdb0', '#e5b46d'];
    const top = y + 9, bottom = y + row.height - 24, height = bottom - top;
    const unit = this.ioMetric === 'throughput' ? 'MiB/s' : this.ioMetric === 'iops' ? 'calls/s' : 'ms';
    const geometry: { left: number; right: number }[] = [];
    for (let i = first; i < last; i++) geometry.push({
      left: Math.max(LABEL_WIDTH + 1, LABEL_WIDTH + timeToX(plot.starts[i]!, this.start, this.nsPerPx)),
      right: Math.min(this.scroll.clientWidth, LABEL_WIDTH + timeToX(plot.ends[i]!, this.start, this.nsPerPx)),
    });
    ctx.strokeStyle = this.colors.border; ctx.lineWidth = 1;
    for (const fraction of [0, .5, 1]) {
      const py = bottom - height * fraction;
      ctx.beginPath(); ctx.moveTo(LABEL_WIDTH + 1, py); ctx.lineTo(this.scroll.clientWidth, py); ctx.stroke();
    }
    for (let series = 0; series < 2; series++) {
      ctx.fillStyle = palette[series]!; ctx.strokeStyle = palette[series]!; ctx.lineWidth = 1.5;
      let previous: { x: number; y: number } | undefined;
      ctx.beginPath();
      for (let i = first; i < last; i++) {
        const value = plot.values[i]![series]!;
        if (!Number.isFinite(value)) { previous = undefined; continue; }
        const { left, right } = geometry[i - first]!;
        const py = bottom - height * value / plot.max;
        ctx.globalAlpha = .14; ctx.fillRect(left, py, right - left, bottom - py); ctx.globalAlpha = 1;
        if (previous && Math.abs(previous.x - left) < 1) { ctx.moveTo(left, previous.y); ctx.lineTo(left, py); }
        else ctx.moveTo(left, py);
        ctx.lineTo(right, py); previous = { x: right, y: py };
      }
      ctx.stroke();
    }
    for (let i = first; i < last; i++) {
      const bin = plot.bins[i]!;
      const { left, right } = geometry[i - first]!;
      if (right <= left) continue;
      this.hits.push({ x: left, y: top, width: right - left, height, selection: { kind: 'io', bin, graph } });
      if (this.selection?.kind === 'io' && this.selection.graph.key === graph.key && this.selection.bin === bin) {
        ctx.strokeStyle = this.colors.selected; ctx.strokeRect(left, top, right - left, height);
      }
    }
    ctx.font = '600 11px system-ui'; ctx.textAlign = 'right';
    for (const fraction of [0, .5, 1]) {
      const label = `${(plot.max * fraction).toLocaleString(undefined, { maximumSignificantDigits: 3 })} ${unit}`;
      const py = Math.max(top + 10, bottom - height * fraction - 3);
      ctx.lineWidth = 3; ctx.strokeStyle = this.colors.background; ctx.strokeText(label, this.scroll.clientWidth - 8, py);
      ctx.fillStyle = this.colors.heading; ctx.fillText(label, this.scroll.clientWidth - 8, py);
    }
    ctx.textAlign = 'left'; ctx.lineWidth = 1; ctx.fillStyle = this.colors.muted; ctx.font = '11px system-ui';
    const binWidth = plot.bins[0]?.duration_ns ?? '0';
    ctx.fillText(`Bin width ${formatNs(BigInt(binWidth))} · ${last - first} visible bins · each step is an average, not one operation`, LABEL_WIDTH + 8, y + row.height - 5);
  }

  private drawStorageGraph(ctx: CanvasRenderingContext2D, row: TimelineRow, y: number, end: bigint): void {
    const graph = row.storage!;
    let cache = this.storagePlots.get(graph);
    if (!cache) { cache = new Map(); this.storagePlots.set(graph, cache); }
    let bins = cache.get(this.ioMetric);
    if (!bins) { bins = storageBins(graph, this.data, this.ioMetric); cache.set(this.ioMetric, bins); }
    const top = y + 9, bottom = y + row.height - 24, height = bottom - top;
    ctx.font = '10px system-ui'; ctx.fillStyle = this.colors.muted;
    if (!bins.length) {
      ctx.fillText(this.emptyWindowMessage(row, 'Loading storage window…', 'No observed storage samples in this window (missing is not zero)'), LABEL_WIDTH + 8, y + 31);
      return;
    }
    const first = lowerBound(bins, bin => bin.start + bin.duration > this.start);
    const last = lowerBound(bins, bin => bin.start >= end);
    let max = 0;
    for (const bin of bins) for (const value of bin.values) if (Number.isFinite(value)) max = Math.max(max, value);
    max ||= 1;
    const palette = [this.colors.storageRead, this.colors.storageWrite];
    const unit = this.ioMetric === 'throughput' ? 'MiB/s' : this.ioMetric === 'iops' ? 'ops/s' : 'ms';
    for (const fraction of [0, .5, 1]) {
      const py = bottom - fraction * height;
      ctx.strokeStyle = this.colors.border; ctx.beginPath(); ctx.moveTo(LABEL_WIDTH + 1, py); ctx.lineTo(this.scroll.clientWidth, py); ctx.stroke();
    }
    for (let side = 0; side < 2; side++) {
      ctx.strokeStyle = palette[side]!; ctx.lineWidth = 1.5; ctx.beginPath();
      let previous: { x: number; y: number } | undefined;
      for (let i = first; i < last; i++) {
        const bin = bins[i]!, value = bin.values[side]!;
        if (!Number.isFinite(value)) { previous = undefined; continue; }
        const interval = bin.windows[side];
        if (!interval) { previous = undefined; continue; }
        const left = Math.max(LABEL_WIDTH + 1, LABEL_WIDTH + timeToX(interval.start, this.start, this.nsPerPx));
        const right = Math.min(this.scroll.clientWidth, LABEL_WIDTH + timeToX(interval.start + interval.duration, this.start, this.nsPerPx));
        if (right <= left) continue;
        const py = bottom - height * value / max;
        ctx.fillStyle = palette[side]!; ctx.globalAlpha = .12; ctx.fillRect(left, py, right - left, bottom - py); ctx.globalAlpha = 1;
        if (previous && !bin.gapBeforeSides[side] && Math.abs(previous.x - left) < 1) { ctx.moveTo(left, previous.y); ctx.lineTo(left, py); }
        else ctx.moveTo(left, py);
        ctx.lineTo(right, py); previous = { x: right, y: py };
      }
      ctx.stroke();
    }
    for (let i = first; i < last; i++) {
      const bin = bins[i]!;
      if (!bin.values.some(Number.isFinite)) continue;
      const left = Math.max(LABEL_WIDTH + 1, LABEL_WIDTH + timeToX(bin.start, this.start, this.nsPerPx));
      const right = Math.min(this.scroll.clientWidth, LABEL_WIDTH + timeToX(bin.start + bin.duration, this.start, this.nsPerPx));
      if (right <= left) continue;
      this.hits.push({ x: left, y: top, width: right - left, height, selection: { kind: 'storage', bin, graph } });
      if (this.selection?.kind === 'storage' && this.selection.graph.key === graph.key && this.selection.bin === bin) {
        ctx.strokeStyle = this.colors.selected; ctx.strokeRect(left, top, right - left, height);
      }
    }
    ctx.textAlign = 'right'; ctx.font = '600 10px system-ui';
    for (const fraction of [0, 1]) {
      const label = `${(max * fraction).toLocaleString(undefined, { maximumSignificantDigits: 3 })} ${unit}`;
      ctx.strokeStyle = this.colors.background; ctx.lineWidth = 3;
      ctx.strokeText(label, this.scroll.clientWidth - 8, fraction ? top + 9 : bottom - 3);
      ctx.fillStyle = this.colors.heading; ctx.fillText(label, this.scroll.clientWidth - 8, fraction ? top + 9 : bottom - 3);
    }
    ctx.lineWidth = 1; ctx.textAlign = 'left'; ctx.font = '10px system-ui'; ctx.fillStyle = this.colors.muted;
    ctx.fillText(`${bins.length} observed windows · ${graph.leaves.length} unique host resources · ${graph.leaves.length > 1 ? 'sampled aggregate estimate' : 'host resource'} · partial coverage in inspector`, LABEL_WIDTH + 8, y + row.height - 5);
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
        const opening = this.collapsed.has(row.key);
        if (this.report.viewer && (row.key.endsWith(':files') || row.key.startsWith('files:')) && row.io === undefined) {
          const thread = row.key.startsWith('files:') ? row.key.slice('files:'.length) :
            this.report.io_timeline?.threads?.find(series => `io:thread:${series.domain}:${series.thread_track}:files` === row.key)?.thread_track;
          if (thread && opening) {
            this.showFileControls(thread);
            if (!this.filePages.has(thread)) void this.loadFiles(thread, 0);
          } else if (thread && !opening && this.activeFiles === thread) { this.activeFiles = undefined; this.fileControls.replaceChildren(); }
        }
        if ((row.system || row.storage || (row.io && row.expanded && this.collapsed.has(row.key))) && this.filter.value) { this.filter.value = ''; this.collapsed.add(row.key); }
        else if (this.collapsed.has(row.key)) this.collapsed.delete(row.key); else this.collapsed.add(row.key);
        this.buildRows(); this.schedule();
      } else if (row?.kind === 'shard') this.showShard(row.shard);
      else if (row?.track) this.showTrack(row.track);
      else if (row?.storage) { this.selection = undefined; this.detail.replaceChildren(node('h3', '', row.label), node('p', 'muted', 'Selected backend only · expand to filesystem and host resource; no cross-layer physical total.')); }
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
      this.detail.append(node('div', 'empty-detail', 'Select a span or counter sample to inspect exact values, attributes, and correlations. Storage and I/O windows can also be inspected.'));
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
      if (span.source_id) put(dl, 'div', 'Source observation', span.source_id);
      const links = this.report.dependencies.filter(edge => edge.from === span.correlation || edge.to === span.correlation);
      for (const edge of links) put(dl, 'div', `Dependency · ${edge.relation}`, `${edge.from} → ${edge.to}`);
      if (span.correlation && !links.length) put(dl, 'div', 'Dependency', 'No linked endpoint retained');
      for (const [key, value] of Object.entries(span.attributes)) put(dl, 'div', key, value);
    } else if (this.selection.kind === 'io') {
      const { bin, graph } = this.selection;
      const rates = ioBinMetrics(bin);
      this.detail.append(node('h3', '', graph.label), node('p', 'muted', 'Full-capture POSIX I/O · calls assigned to completion bins; not physical disk rates.'));
      put(dl, 'div', 'Window', `${formatNs(BigInt(bin.start_ns))}–${formatNs(BigInt(bin.start_ns) + BigInt(bin.duration_ns))}`);
      put(dl, 'div', 'Bin duration', `${formatNs(BigInt(bin.duration_ns))} · completion-time average`);
      put(dl, 'div', 'Read bytes / calls', `${formatBytes(bin.read_bytes)} / ${bin.read_ops}`, `${bin.read_bytes} B / ${bin.read_ops} calls`);
      put(dl, 'div', 'Write bytes / calls', `${formatBytes(bin.write_bytes)} / ${bin.write_ops}`, `${bin.write_bytes} B / ${bin.write_ops} calls`);
      put(dl, 'div', 'Read throughput / IOPS', `${formatByteRate(bin.read_bytes, bin.duration_ns)} / ${rates.readOpsPerSec} calls/s`, `${rates.readBytesPerSec} B/s; ${bin.read_bytes} bytes in ${bin.duration_ns} ns`);
      put(dl, 'div', 'Write throughput / IOPS', `${formatByteRate(bin.write_bytes, bin.duration_ns)} / ${rates.writeOpsPerSec} calls/s`, `${rates.writeBytesPerSec} B/s; ${bin.write_bytes} bytes in ${bin.duration_ns} ns`);
      put(dl, 'div', 'Mean read / write latency', `${formatMeanLatency(bin.read_latency_ns, bin.read_ops)} / ${formatMeanLatency(bin.write_latency_ns, bin.write_ops)}`, `${rates.readLatencyNs} / ${rates.writeLatencyNs}`);
      put(dl, 'div', 'Weighted mean latency', formatMeanLatency((BigInt(bin.read_latency_ns) + BigInt(bin.write_latency_ns)).toString(), (BigInt(bin.read_ops) + BigInt(bin.write_ops)).toString()), rates.meanLatencyNs);
      put(dl, 'div', 'Other operations / errors', `${bin.other_ops} / ${bin.errors}`);
      put(dl, 'div', 'Max latency (all calls)', BigInt(bin.read_ops) + BigInt(bin.write_ops) + BigInt(bin.other_ops) > 0n ? formatMeanLatency(bin.max_latency_ns, '1') : '—', `${bin.max_latency_ns} ns`);
    } else if (this.selection.kind === 'storage') {
      const { bin, graph } = this.selection;
      this.detail.append(node('h3', '', graph.label), node('p', 'muted', graph.backend === 'block' ?
        'Host block-device layer I/O · virtual disks and shared/unmapped devices cannot be apportioned to an application or filesystem.' :
        graph.backend === 'lustre' ? 'Lustre client bulk RPC I/O · not server physical I/O or metadata RPC bandwidth.' :
          'NFS client server payload I/O · not server physical I/O or application-only bandwidth.'));
      put(dl, 'div', 'Observed window', `${formatNs(bin.start)}–${formatNs(bin.start + bin.duration)} · ${formatNs(bin.duration)} plotted extent`);
      put(dl, 'div', 'Read / write', bin.values.map(value => Number.isFinite(value) ? `${value.toLocaleString(undefined, { maximumSignificantDigits: 6 })} ${this.ioMetric === 'throughput' ? 'MiB/s' : this.ioMetric === 'iops' ? 'ops/s' : 'ms'}` : 'unavailable').join(' / '));
      put(dl, 'div', 'Fresh contributors read / write', bin.coverage.join(' / ') + ' · missing is not zero');
      put(dl, 'div', 'Measurement basis', `${graph.leaves.length > 1 || new Set(bin.members.map(item => item.timestamp_ns)).size > 1 ? 'Sampled aggregate estimate from fresh rates (not total bytes over the grid)' : 'Single-resource observed counter window'} · independent layers must not be summed; not application-only`);
      if (bin.gapBefore) put(dl, 'div', 'Continuity', 'Gap or missing source before this window; no interpolation');
      for (const counter of bin.members) {
        const track = this.report.tracks.find(track => track.id === counter.track);
        const group = node('details', 'storage-member'); group.append(node('summary', '', `${track?.name || counter.track} · ${counter.name} · ${counter.value} ${counter.unit}`));
        const attrs = node('dl', 'details-list');
        put(attrs, 'div', 'Sample end', `${formatNs(BigInt(counter.timestamp_ns))} (${counter.timestamp_ns} ns)`);
        const window = counter.attributes['storage.window_ns'];
        if (window) put(attrs, 'div', 'Measured interval', `${formatNs(BigInt(window))} ending at sample timestamp`, `${window} ns`);
        put(attrs, 'div', 'Observed value', counter.unit === 'B/s' ? `${(Number(counter.value) / 1048576).toLocaleString(undefined, { maximumSignificantDigits: 6 })} MiB/s` :
          counter.unit === 'ms' ? `${Number(counter.value).toLocaleString(undefined, { maximumSignificantDigits: 6 })} ms` : `${counter.value} ${counter.unit}`, `${counter.value} ${counter.unit}`);
        for (const [key, value] of Object.entries({ ...track?.details, ...counter.attributes })) {
          const formatted = /^storage\.(read_bytes|write_bytes)$/.test(key) && /^\d+$/.test(value) ? formatBytes(value) :
            /^storage\.(interval_ns|window_ns|read_latency_ns|write_latency_ns|busy_ns|weighted_io_ns|read_queue_ns|write_queue_ns|read_rtt_ns|write_rtt_ns|rpc_wait_ns|metadata_wait_ns|metadata_rpc_latency_ns)$/.test(key) && /^\d+$/.test(value) ? formatNs(BigInt(value)) : value;
          put(attrs, 'div', key, formatted, formatted === value ? undefined : value);
        }
        group.append(attrs); this.detail.append(group);
      }
    } else {
      const { counter, time } = this.selection.item;
      this.detail.append(node('h3', '', counter.name));
      put(dl, 'div', 'Timestamp', `${time} ns`);
      put(dl, 'div', counter.attributes['system.aggregate'] ? 'Sampled aggregate estimate' : 'Exact value', counter.value);
      put(dl, 'div', 'Type / unit', `${counter.value_type} · ${counter.unit}`);
      if (counter.unit === 'marker') put(dl, 'div', 'Interpretation', 'Trace instant marker sample; not an execution span or duration.');
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
    const data: Report | Projection = JSON.parse(payload.textContent);
    if (data.version !== 1) throw new Error(`Unsupported report version: ${String(data.version)}`);
    if ('kind' in data && data.kind === 'rprof-projection') new ProjectionViewer(data, root);
    else new Viewer(data as Report, root);
  } catch (error) {
    root.replaceChildren(node('main', 'load-error', `Unable to open report: ${error instanceof Error ? error.message : 'invalid data'}`));
  }
}
