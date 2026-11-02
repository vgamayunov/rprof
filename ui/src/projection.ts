import { initialTheme, rememberTheme } from './theme.js';
import type { Theme } from './theme.js';

interface Provenance { kind: string; source: string; date: string; conditions: string }
interface Metric { unit: string; low: number; central: number; high: number; provenance: Provenance }
interface Profile { id: string; name: string; nodes: number; provenance: Provenance; placement: { ranks_per_node: number; threads_per_rank: number; gpus_per_rank: number }; resources: Record<string, { kind: string; description: string; slots_per_node: number; capacity_bytes_per_node: string | null; features: string[]; metrics: Record<string, Metric> }> }
interface Span { id: string; name: string; category: string; lane: string; start_ns: string; duration_ns: string; incomplete: boolean; enclosing: boolean; attributes: Record<string, string> }
interface Phase { id: string; span_id: string; name: string; resource: string | null; model: { kind: string; [key: string]: unknown }; after: string[]; node_scaling: { kind: string; exponent?: number }; assumption: string }
interface Workload { version: number; name: string; baseline_sha256: string; domain: string; window_start_ns: string; window_duration_ns: string; assumptions: string; unmodeled: string; unmodeled_assumption: string; allow_partial_evidence: boolean; scaling_assumption: string; placement_assumption: string; memory: Record<string, { bytes_per_source_node: string; distributed: boolean }>; required_features: Record<string, string[]>; phases: Phase[] }
interface Request { version: number; baseline: { capture_sha256: string; session_id: string; domain: string; duration_ns: string; complete: boolean; dropped: string; omitted: string; clock_uncertainty_ns: string | null; warnings: string[]; metadata: Record<string, string>; spans: Span[] }; source: Profile; targets: Profile[]; workload: Workload; nodes: number[]; scaling: string; validation: { target_id: string; nodes: number; observed_ns: string; workload_sha256: string; target_profile_sha256: string; provenance: Provenance }[] }
interface Task { id: string; span_id: string; name: string; resource: string | null; model: string; assumption: string; bottleneck: string; baseline_start_ns: string; baseline_duration_ns: string; start_ns: string; duration_ns: string; preserved_lag_ns: string; critical: boolean }
interface Scenario { id: string; target_id: string; target_name: string; nodes: number; scaling: string; status: string; issues: string[]; warnings: string[]; runtime: { optimistic_ns: string; central_ns: string; pessimistic_ns: string } | null; speedup: string | null; efficiency: string | null; tasks: Task[]; resources: { resource: string; slots: number; busy_ns: string; modeled_busy_percent: string }[]; critical_path: string[]; validation: { observed_ns: string; signed_error_percent: string; provenance: Provenance }[] }
export interface Projection { kind: 'rprof-projection'; version: 1; engine_version: string; hashes: Record<string, string>; request: Request; baseline_runtime_ns: string; covered_ns: string; coverage_percent: string; source_reconstructed_ns: string; assumptions: string[]; warnings: string[]; scenarios: Scenario[] }
type Editable = Pick<Request, 'source' | 'targets' | 'workload' | 'nodes' | 'scaling'>;

const PAGE = 100;
let noteListId = 0;
function el<K extends keyof HTMLElementTagNameMap>(tag: K, className = '', text?: string): HTMLElementTagNameMap[K] {
  const result = document.createElement(tag);
  result.className = className;
  if (text !== undefined) result.textContent = text;
  return result;
}
function line(parent: Element, label: string, value: string): void {
  const row = el('div', 'detail-row');
  row.append(el('dt', '', label), el('dd', '', value)); parent.append(row);
}
function details(parent: Element, heading: string, open = false): HTMLElement {
  const section = el('details', 'projection-details'); section.open = open;
  section.append(el('summary', '', heading)); const content = el('div', 'projection-detail-body');
  section.append(content); parent.append(section); return content;
}
function noteList(parent: Element, title: string, notes: string[]): void {
  if (!notes.length) return;
  parent.append(el('h3', '', `${title} (${notes.length})`));
  const list = el('div'); parent.append(list);
  paginate(parent, `projection-notes-${++noteListId}`, notes, (target, note) => target.append(el('p', 'projection-note', note)), list);
}
function table(parent: Element, id: string, headings: string[]): HTMLTableSectionElement {
  const wrap = el('div', 'io-wrap'); const t = el('table'); t.id = id;
  const tr = el('tr'); for (const heading of headings) tr.append(el('th', '', heading));
  const head = el('thead'); head.append(tr); const body = el('tbody'); t.append(head, body); wrap.append(t); parent.append(wrap);
  return body;
}
function row(body: Element, cells: string[]): void {
  const tr = el('tr'); for (const cell of cells) tr.append(el('td', '', cell)); body.append(tr);
}
function paginate<T>(parent: Element, id: string, values: readonly T[], render: (body: Element, item: T) => void, body: Element): void {
  let page = 0; const pager = el('div', 'io-pager'); pager.id = id; parent.append(pager);
  function draw(): void {
    body.replaceChildren(); pager.replaceChildren();
    for (const item of values.slice(page * PAGE, (page + 1) * PAGE)) render(body, item);
    if (values.length <= PAGE) return;
    const prev = el('button', '', 'Previous'); prev.type = 'button'; prev.disabled = page === 0;
    prev.addEventListener('click', () => { page--; draw(); });
    const next = el('button', '', 'Next'); next.type = 'button'; next.disabled = (page + 1) * PAGE >= values.length;
    next.addEventListener('click', () => { page++; draw(); });
    pager.append(prev, el('span', '', `Rows ${page * PAGE + 1}–${Math.min(values.length, (page + 1) * PAGE)} of ${values.length}`), next);
  }
  draw();
}
function ns(value: string | null): string { return value === null ? 'Unavailable' : `${value} ns`; }
// Only bounded, relative BigInt differences are converted to CSS coordinates. The original decimal strings stay exact.
function width(start: bigint, duration: bigint, extent: bigint): { left: string; width: string } {
  if (extent <= 0n) return { left: '0%', width: '0%' };
  const clamp = (n: bigint): bigint => n < 0n ? 0n : n > extent ? extent : n;
  const left = clamp(start) * 10000n / extent;
  const end = clamp(start + duration) * 10000n / extent;
  return { left: `${Number(left) / 100}%`, width: `${Number(end - left) / 100}%` };
}

interface WasmExports { memory: WebAssembly.Memory; projection_alloc(len: number): number; projection_run(ptr: number, len: number): number; projection_output(): number }
async function engine(): Promise<WasmExports> {
  const script = document.getElementById('rprof-project-wasm');
  const base64 = script?.textContent?.replace(/\s/g, '');
  if (!base64) throw new Error('Projection engine is not embedded in this HTML file. Re-export it with projection support.');
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  const instantiated = await WebAssembly.instantiate(bytes, {});
  const exports = instantiated.instance.exports as unknown as Partial<WasmExports>;
  if (!(exports.memory instanceof WebAssembly.Memory) || typeof exports.projection_alloc !== 'function' || typeof exports.projection_run !== 'function' || typeof exports.projection_output !== 'function') throw new Error('Invalid projection engine ABI.');
  return exports as WasmExports;
}
function calculate(wasm: WasmExports, request: Request): Projection {
  const input = new TextEncoder().encode(JSON.stringify(request));
  const ptr = wasm.projection_alloc(input.length);
  if (!ptr) throw new Error('Projection request exceeds the engine size limit.');
  new Uint8Array(wasm.memory.buffer, ptr, input.length).set(input);
  const len = wasm.projection_run(ptr, input.length); // consumes the allocation exactly once
  if (!len) throw new Error('Projection engine returned an empty response.');
  const output = wasm.projection_output();
  // run may grow linear memory: reacquire memory.buffer *after* the call.
  const response: unknown = JSON.parse(new TextDecoder().decode(new Uint8Array(wasm.memory.buffer, output, len)));
  if (typeof response !== 'object' || response === null) throw new Error('Invalid projection engine response.');
  if ('error' in response) throw new Error(String(response.error));
  if (!('kind' in response) || response.kind !== 'rprof-projection' || !('version' in response) || response.version !== 1) throw new Error('Unexpected projection engine result.');
  return response as Projection;
}
function download(filename: string, value: unknown): void {
  const url = URL.createObjectURL(new Blob([JSON.stringify(value, null, 2) + '\n'], { type: 'application/json' }));
  const link = el('a'); link.href = url; link.download = filename;
  document.body.append(link); link.click(); link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

export class ProjectionViewer {
  private readonly root: HTMLElement;
  private readonly original: Projection;
  private artifact: Projection;
  private selected = 0;
  private scenarioPage = 0;
  private theme: Theme = initialTheme();
  private wasm: Promise<WasmExports> | undefined;
  private readonly editor: HTMLTextAreaElement;
  private readonly error: HTMLElement;
  private readonly result: HTMLElement;

  constructor(artifact: Projection, root: HTMLElement) {
    this.original = artifact; this.artifact = artifact; this.root = root;
    document.documentElement.dataset.theme = this.theme;
    // Static, application-owned markup only. All artifact and editor strings use textContent.
    root.innerHTML = `<header class="topbar"><div class="brand"><div class="mark">r/</div><div><strong>rprof</strong><small>OFFLINE PROJECTION</small></div></div><div class="header-actions"><div class="header-meta"><span class="badge amber">MODELED ESTIMATE</span></div><button id="theme-toggle" type="button"></button></div></header><main class="projection-main"><section class="overview projection-overview"><div class="overview-copy"><div class="eyebrow">WORKLOAD PROJECTION · NOT A MEASUREMENT</div><h1 id="projection-title"></h1><p id="projection-subtitle"></p></div><div class="stat-grid" id="projection-stats"></div></section><section class="io-section projection-card" id="projection-result"></section><section class="io-section projection-card"><h2>Edit model and scenarios</h2><p class="io-note">Edit source, targets, workload phases/DAG, nodes and scaling as JSON. Baseline evidence and measured validation are immutable here. Recalculate uses the same Rust engine in this offline page; no requests are sent. Export request for <code>rprof project BASELINE --request downloaded.json -o projection.json</code>.</p><label for="projection-editor">Editable request fields</label><textarea id="projection-editor" spellcheck="false"></textarea><div class="projection-actions"><button id="projection-recalculate" type="button">Recalculate</button><button id="projection-restore" type="button">Restore original</button><button id="projection-request-download" type="button">Download request JSON</button><button id="projection-artifact-download" type="button">Download artifact JSON</button></div><p id="projection-error" class="projection-error" role="alert" hidden></p></section></main>`;
    this.editor = root.querySelector<HTMLTextAreaElement>('#projection-editor')!;
    this.error = root.querySelector<HTMLElement>('#projection-error')!;
    this.result = root.querySelector<HTMLElement>('#projection-result')!;
    this.editor.value = JSON.stringify(this.editable(artifact.request), null, 2);
    this.setTheme(this.theme);
    root.querySelector('#theme-toggle')!.addEventListener('click', () => { this.setTheme(this.theme === 'dark' ? 'light' : 'dark'); rememberTheme(this.theme); });
    root.querySelector('#projection-restore')!.addEventListener('click', () => { this.editor.value = JSON.stringify(this.editable(this.original.request), null, 2); this.error.hidden = true; });
    root.querySelector('#projection-recalculate')!.addEventListener('click', () => { void this.recalculate(); });
    root.querySelector('#projection-request-download')!.addEventListener('click', () => download('downloaded.json', this.artifact.request));
    root.querySelector('#projection-artifact-download')!.addEventListener('click', () => download('projection.json', this.artifact));
    this.render();
  }
  private editable(request: Request): Editable { return { source: request.source, targets: request.targets, workload: request.workload, nodes: request.nodes, scaling: request.scaling }; }
  private setTheme(theme: Theme): void {
    this.theme = theme; document.documentElement.dataset.theme = theme;
    const target = theme === 'dark' ? 'light' : 'dark';
    const button = this.root.querySelector<HTMLButtonElement>('#theme-toggle')!;
    button.textContent = target === 'light' ? 'Light mode' : 'Dark mode';
    button.setAttribute('aria-label', `Switch to ${target} theme`);
  }
  private async recalculate(): Promise<void> {
    const button = this.root.querySelector<HTMLButtonElement>('#projection-recalculate')!;
    button.disabled = true; this.error.hidden = true;
    try {
      const editable: unknown = JSON.parse(this.editor.value);
      if (!editable || typeof editable !== 'object' || Array.isArray(editable)) throw new Error('Editor must contain a JSON object.');
      const fields = Object.keys(editable);
      if (fields.length !== 5 || fields.some(key => !['source', 'targets', 'workload', 'nodes', 'scaling'].includes(key))) throw new Error('Edit exactly source, targets, workload, nodes and scaling; baseline and validation cannot be edited.');
      const { source, targets, workload, nodes, scaling } = editable as Editable;
      const request: Request = { version: this.original.request.version, baseline: this.original.request.baseline, validation: this.original.request.validation, source, targets, workload, nodes, scaling };
      this.wasm ??= engine().catch(error => { this.wasm = undefined; throw error; });
      const result = calculate(await this.wasm, request);
      if (result.engine_version !== this.original.engine_version || !this.original.hashes.engine_source || result.hashes.engine_source !== this.original.hashes.engine_source) {
        throw new Error('Embedded projection engine does not match this artifact; rebuild the WASM bundle and regenerate the HTML.');
      }
      this.artifact = result; this.selected = 0; this.scenarioPage = 0; this.render();
    } catch (error) {
      this.error.textContent = error instanceof Error ? error.message : String(error);
      this.error.hidden = false;
    } finally { button.disabled = false; }
  }
  private render(): void {
    const a = this.artifact, r = a.request, b = r.baseline;
    this.root.querySelector('#projection-title')!.textContent = r.workload.name;
    this.root.querySelector('#projection-subtitle')!.textContent = `Observed baseline ${b.session_id} · domain ${b.domain} · source ${r.source.name} → ${r.targets.length} target profiles · ${a.engine_version}`;
    const meta = this.root.querySelector('.header-meta')!;
    meta.querySelector('.projection-provenance')?.remove();
    const source = el('span', `badge projection-provenance ${b.metadata.provenance === 'synthetic' ? 'amber' : 'green'}`,
      `${b.metadata.provenance ?? 'unspecified'} baseline`.toUpperCase());
    meta.append(source);
    const stats = this.root.querySelector('#projection-stats')!; stats.replaceChildren();
    for (const [label, value] of [['OBSERVED ROI', ns(a.baseline_runtime_ns)], ['MODELED COVERAGE', `${a.coverage_percent}% · ${ns(a.covered_ns)}`], ['SOURCE RECONSTRUCTED', ns(a.source_reconstructed_ns)], ['SCENARIOS', String(a.scenarios.length)]]) {
      const card = el('div', 'stat'); card.append(el('strong', '', value), el('span', '', label)); stats.append(card);
    }
    this.result.replaceChildren();
    this.result.append(el('h2', '', 'Scenario comparison'), el('p', 'io-note', 'Optimistic / central / pessimistic runtimes are an envelope of evaluated parameter cases, not guaranteed extrema across mixed inputs, modeling errors, or statistical confidence intervals. Speedup and efficiency are model estimates, not measured performance. Unavailable estimates are never zero.'));
    const body = table(this.result, 'projection-scenarios', ['Target / scenario', 'Nodes · scaling', 'Status / reasons', 'Optimistic / central / pessimistic (ns)', 'Speedup', 'Efficiency']);
    // Rebuild this table only when the scenario page changes; selection keeps its page.
    const pager = el('div', 'io-pager'); pager.id = 'projection-scenario-pager'; this.result.append(pager);
    const draw = (): void => {
      body.replaceChildren(); pager.replaceChildren();
      for (let i = this.scenarioPage * PAGE; i < Math.min(a.scenarios.length, (this.scenarioPage + 1) * PAGE); i++) {
        const scenario = a.scenarios[i]!; const tr = el('tr');
        const select = el('button', 'io-file', `${scenario.target_name} · ${scenario.id}`); select.type = 'button';
        select.setAttribute('aria-label', `Inspect scenario ${scenario.id}`);
        select.addEventListener('click', () => { this.selected = i; this.renderSelection(); });
        const first = el('td'); first.append(select);
        tr.append(first, el('td', '', `${scenario.nodes} · ${scenario.scaling}`), el('td', '', `${scenario.status}${scenario.issues.length ? ` · ${scenario.issues.join('; ')}` : ''}`),
          el('td', '', scenario.runtime ? `${scenario.runtime.optimistic_ns} / ${scenario.runtime.central_ns} / ${scenario.runtime.pessimistic_ns}` : 'Unavailable'),
          el('td', '', scenario.speedup ?? 'Unavailable'), el('td', '', scenario.efficiency ?? 'Unavailable'));
        if (i === this.selected) tr.className = 'projection-selected';
        body.append(tr);
      }
      if (a.scenarios.length > PAGE) {
        const prev = el('button', '', 'Previous'); prev.type = 'button'; prev.disabled = this.scenarioPage === 0;
        prev.addEventListener('click', () => { this.scenarioPage--; draw(); });
        const next = el('button', '', 'Next'); next.type = 'button'; next.disabled = (this.scenarioPage + 1) * PAGE >= a.scenarios.length;
        next.addEventListener('click', () => { this.scenarioPage++; draw(); });
        pager.append(prev, el('span', '', `Rows ${this.scenarioPage * PAGE + 1}–${Math.min(a.scenarios.length, (this.scenarioPage + 1) * PAGE)} of ${a.scenarios.length}`), next);
      }
    };
    draw();
    const selection = el('section'); selection.id = 'projection-selection'; this.result.append(selection);
    const assumptions = details(this.result, `Model assumptions, warnings and hashes (${a.assumptions.length} assumptions · ${a.warnings.length} warnings)`);
    noteList(assumptions, 'Assumptions', a.assumptions); noteList(assumptions, 'Warnings', a.warnings);
    line(assumptions, 'Workload', r.workload.assumptions);
    for (const [label, value] of [['Scaling', r.workload.scaling_assumption], ['Placement', r.workload.placement_assumption], ['Unmodeled phases', `${r.workload.unmodeled} · ${r.workload.unmodeled_assumption}`], ['Partial evidence allowed', String(r.workload.allow_partial_evidence)]]) line(assumptions, label!, value!);
    const hashBody = table(assumptions, 'projection-hashes', ['Hash', 'SHA-256']);
    paginate(assumptions, 'projection-hash-pager', Object.entries(a.hashes), (target, [key, value]) => row(target, [key, value]), hashBody);
    const evidence = details(this.result, 'Observed baseline, source hardware and provenance');
    for (const [key, value] of [['Session', b.session_id], ['Capture SHA-256', b.capture_sha256], ['Domain', b.domain], ['ROI start (domain ns)', r.workload.window_start_ns], ['ROI duration', ns(r.workload.window_duration_ns)], ['Observed baseline duration', ns(b.duration_ns)], ['Complete', String(b.complete)], ['Dropped', b.dropped], ['Omitted', b.omitted], ['Clock uncertainty', ns(b.clock_uncertainty_ns)]]) line(evidence, key!, value!);
    noteList(evidence, 'Evidence warnings', b.warnings);
    const metadata = table(evidence, 'projection-metadata', ['Metadata key', 'Value']);
    paginate(evidence, 'projection-metadata-pager', Object.entries(b.metadata), (target, [key, value]) => row(target, [key, value]), metadata);
    const profiles = [r.source, ...r.targets];
    const profileBody = table(evidence, 'projection-profiles', ['Profile · nodes / placement', 'Profile provenance', 'Resources and constraints']);
    paginate(evidence, 'projection-profile-pager', profiles, (target, profile) => row(target,
      [`${profile.name} (${profile.id}) · ${profile.nodes} nodes · ${profile.placement.ranks_per_node} ranks/node · ${profile.placement.threads_per_rank} threads/rank · ${profile.placement.gpus_per_rank} GPUs/rank`,
        `${profile.provenance.kind} · ${profile.provenance.source} · ${profile.provenance.date} · ${profile.provenance.conditions}`,
        Object.entries(profile.resources).map(([id, resource]) => `${id}: ${resource.description}; ${resource.slots_per_node} slots/node; capacity ${resource.capacity_bytes_per_node ?? 'unknown'} B; features ${resource.features.join(', ') || 'none'}`).join(' · ') || 'No resources']), profileBody);
    const profileRows = profiles.flatMap(profile => Object.entries(profile.resources).flatMap(([resource, spec]) => Object.entries(spec.metrics).map(([metric, value]) => ({ profile, resource, spec, metric, value }))));
    const metrics = table(evidence, 'projection-metrics', ['Profile · resource', 'Metric', 'Low / central / high', 'Provenance']);
    paginate(evidence, 'projection-metric-pager', profileRows, (target, entry) => row(target, [`${entry.profile.name} · ${entry.resource} (${entry.spec.kind}, ${entry.spec.slots_per_node} slots/node)`, entry.metric, `${entry.value.low} / ${entry.value.central} / ${entry.value.high} ${entry.value.unit}`, `${entry.value.provenance.kind} · ${entry.value.provenance.source} · ${entry.value.provenance.date} · ${entry.value.provenance.conditions}`]), metrics);
    const observed = table(evidence, 'projection-observed', ['Observed span · lane', 'Start / duration (ns)', 'Status']);
    paginate(evidence, 'projection-observed-pager', b.spans, (target, span) => row(target, [`${span.name} · ${span.lane} · ${span.id}`, `${span.start_ns} / ${span.duration_ns}`, `${span.category}${span.incomplete ? ' · incomplete' : ''}${span.enclosing ? ' · enclosing' : ''}`]), observed);
    this.renderSelection();
  }
  private renderSelection(): void {
    const parent = this.result.querySelector('#projection-selection')!; parent.replaceChildren();
    this.result.querySelectorAll('#projection-scenarios tbody tr').forEach((row, i) => row.classList.toggle('projection-selected', this.scenarioPage * PAGE + i === this.selected));
    const s = this.artifact.scenarios[this.selected]; if (!s) return;
    parent.append(el('h2', '', `${s.target_name} · ${s.nodes} nodes · ${s.scaling}`), el('p', 'io-note', `${s.status} · ${s.runtime ? `central ${ns(s.runtime.central_ns)}` : 'runtime unavailable'} · ${s.tasks.length} projected tasks`));
    noteList(parent, 'Unsupported / infeasible reasons', s.issues); noteList(parent, 'Scenario warnings', s.warnings);
    const workload = this.artifact.request.workload;
    const models = details(parent, `Modeled phases and explicit assumptions (${workload.phases.length})`);
    const phaseBody = table(models, 'projection-phases', ['Phase · observed span', 'Resource · model', 'DAG predecessors', 'Node scaling', 'Assumption']);
    paginate(models, 'projection-phase-pager', workload.phases, (target, phase) => row(target, [`${phase.name} · ${phase.id} / ${phase.span_id}`, `${phase.resource ?? 'none'} · ${JSON.stringify(phase.model)}`, phase.after.join(', ') || '—', `${phase.node_scaling.kind}${phase.node_scaling.exponent === undefined ? '' : ` exponent ${phase.node_scaling.exponent}`}`, phase.assumption]), phaseBody);
    const requirements = table(models, 'projection-requirements', ['Resource', 'Memory / required features']);
    const keys = [...new Set([...Object.keys(workload.memory), ...Object.keys(workload.required_features)])];
    paginate(models, 'projection-requirements-pager', keys, (target, id) => row(target, [id,
      `${workload.memory[id] ? `${workload.memory[id].bytes_per_source_node} bytes/source node · ${workload.memory[id].distributed ? 'distributed' : 'fixed'}; ` : ''}features: ${workload.required_features[id]?.join(', ') || 'none'}`]), requirements);
    const taskSection = details(parent, `Observed source phases vs projected task timeline (${s.tasks.length})`, true);
    taskSection.append(el('p', 'io-note', 'Starts are relative to the selected observed ROI. Source and target task durations are shown separately; critical-path markings and projected starts are modeled, not measured.'));
    const duration = s.runtime?.central_ns ?? this.artifact.baseline_runtime_ns;
    let extent = BigInt(duration); const baseline = BigInt(this.artifact.baseline_runtime_ns);
    if (baseline > extent) extent = baseline;
    const taskBody = table(taskSection, 'projection-tasks', ['Task / model · resource', 'Observed start / duration (ns)', 'Projected start / duration (ns)', 'Source vs projected timeline', 'Critical / lag / bottleneck / assumption']);
    paginate(taskSection, 'projection-task-pager', s.tasks, (target, task) => {
      const tr = el('tr');
      const plot = el('td'); const graphic = el('div', 'projection-plot');
      for (const [kind, start, duration] of [['observed', task.baseline_start_ns, task.baseline_duration_ns], ['projected', task.start_ns, task.duration_ns]]) {
        const lane = el('div', 'projection-lane'); const bar = el('span', `projection-bar ${kind}${task.critical && kind === 'projected' ? ' critical' : ''}`);
        const position = width(BigInt(start!), BigInt(duration!), extent);
        bar.style.left = position.left; bar.style.width = position.width;
        // Zero-duration points are shown with a dot; a zero extent never divides by zero.
        if (BigInt(duration!) === 0n) bar.classList.add('point');
        lane.append(bar); graphic.append(lane);
      }
      plot.append(graphic);
      tr.append(el('td', '', `${task.name} · ${task.id} / ${task.span_id} · ${task.model} · ${task.resource ?? 'none'}`),
        el('td', '', `${task.baseline_start_ns} / ${task.baseline_duration_ns}`), el('td', '', `${task.start_ns} / ${task.duration_ns}`), plot,
        el('td', '', `${task.critical ? 'Critical path' : 'Noncritical'} · lag ${task.preserved_lag_ns} ns · ${task.bottleneck} · ${task.assumption}`)); target.append(tr);
    }, taskBody);
    parent.append(el('h3', '', 'Modeled resource load'));
    parent.append(el('p', 'io-note', 'Busy % is modeled service-slot occupancy, NOT CPU/GPU/network/storage hardware utilization.'));
    const loads = table(parent, 'projection-resources', ['Resource', 'Slots', 'Modeled busy (ns)', 'Modeled slot busy %']);
    paginate(parent, 'projection-resource-pager', s.resources, (target, load) => row(target, [load.resource, String(load.slots), load.busy_ns, `${load.modeled_busy_percent}%`]), loads);
    const path = details(parent, `Critical path task IDs (${s.critical_path.length})`);
    const pathBody = table(path, 'projection-critical-path', ['Task ID']);
    paginate(path, 'projection-critical-pager', s.critical_path, (target, id) => row(target, [id]), pathBody);
    const validation = details(parent, `Measured validation observations (${s.validation.length})`);
    validation.append(el('p', 'io-note', 'Observed validation durations and signed errors are separate measurements against the central estimate; they are not uncertainty ranges or confidence intervals.'));
    const valBody = table(validation, 'projection-validation', ['Observed duration', 'Signed error', 'Measurement provenance']);
    paginate(validation, 'projection-validation-pager', s.validation, (target, result) => row(target, [ns(result.observed_ns), `${result.signed_error_percent}%`, `${result.provenance.kind} · ${result.provenance.source} · ${result.provenance.date} · ${result.provenance.conditions}`]), valBody);
    const inputBody = table(validation, 'projection-validation-input', ['Target · nodes', 'Observed duration', 'Workload SHA-256 / profile SHA-256', 'Provenance']);
    paginate(validation, 'projection-validation-input-pager', this.artifact.request.validation, (target, observation) => row(target, [`${observation.target_id} · ${observation.nodes}`, ns(observation.observed_ns), `${observation.workload_sha256} / ${observation.target_profile_sha256}`, `${observation.provenance.kind} · ${observation.provenance.source} · ${observation.provenance.date} · ${observation.provenance.conditions}`]), inputBody);
  }
}
