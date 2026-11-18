import { formatBytes, formatNs, sortSummaryRows } from './model.js';
import type { Report } from './model.js';
import { STACK_METRICS } from './stacks.js';
import type { StackGroup, StackMetric, StackNode, StackOrientation, StackQuery, StackTree } from './stacks.js';

declare const STACK_WORKER_SOURCE: string;
const ROW_HEIGHT = 30;
function element<K extends keyof HTMLElementTagNameMap>(tag: K, className = '', text = ''): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag); node.className = className; node.textContent = text; return node;
}
export class StackPanel {
  private readonly panel = element('section', 'function-panel');
  private readonly status = element('p', 'io-note');
  private readonly note = element('p', 'io-note');
  private readonly total = element('p', 'io-note');
  private readonly search = element('input');
  private readonly scroll = element('div', 'function-scroll');
  private readonly rows = element('div', 'function-rows');
  private readonly detail = element('pre', 'function-detail');
  private readonly retry = element('button', 'io-file', 'Retry functions');
  private metric: StackMetric = 'cpu';
  private orientation: StackOrientation = 'top-down';
  private group: StackGroup = 'combined';
  private scope: { domain: string; range?: { start: bigint; end: bigint }; filter: string } | undefined;
  private key = '';
  private request = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private abort: AbortController | undefined;
  private worker: Worker | undefined;
  private tree: StackTree | undefined;
  private expanded = new Set<number>();
  private children = new Map<number | null, StackNode[]>();
  private visible: { node: StackNode; depth: number }[] = [];
  private sort: 'total' | 'self_weight' | 'name' = 'total';
  private ascending = false;

  constructor(private readonly report: Report, host: HTMLElement,
    private readonly endpoint: (path: string) => string | undefined) {
    this.panel.id = 'functions-panel';
    this.panel.append(element('h2', '', 'Functions'));
    const toolbar = element('div', 'toolbar-fields');
    const select = <T extends string>(label: string, id: string, values: { value: T; label: string }[], change: (value: T) => void) => {
      const field = element('label', 'field', label);
      const input = element('select'); input.id = id; input.setAttribute('aria-label', label);
      for (const value of values) { const option = element('option', '', value.label); option.value = value.value; input.append(option); }
      input.addEventListener('change', () => {
        const selected = values.find(v => v.value === input.value);
        if (selected) { change(selected.value); this.refresh(); }
      });
      field.append(input); toolbar.append(field);
    };
    select<StackOrientation>('Call tree', 'functions-orientation', [
      { value: 'top-down', label: 'Top-down' }, { value: 'bottom-up', label: 'Bottom-up' },
    ], value => { this.orientation = value; });
    select<StackMetric>('Function metric', 'functions-metric', STACK_METRICS, value => { this.metric = value; });
    select<StackGroup>('Group functions', 'functions-group', [
      { value: 'combined', label: 'Combined' }, { value: 'rank', label: 'Rank / shard' }, { value: 'thread', label: 'Thread / stream' },
    ], value => { this.group = value; });
    const search = element('label', 'field', 'Find function');
    this.search.type = 'search'; this.search.id = 'functions-search';
    this.search.setAttribute('aria-label', 'Find function without changing totals');
    this.search.addEventListener('input', () => this.flatten());
    search.append(this.search); toolbar.append(search);
    const collapse = element('button', 'io-file', 'Collapse all');
    collapse.id = 'functions-collapse';
    collapse.addEventListener('click', () => { this.search.value = ''; this.expanded.clear(); this.flatten(); });
    toolbar.append(collapse);
    this.status.id = 'functions-status'; this.status.setAttribute('role', 'status');
    this.total.id = 'functions-total';
    this.retry.hidden = true; this.retry.addEventListener('click', () => {
      if (!this.worker && !this.report.viewer?.stacks_endpoint) this.startWorker();
      this.key = ''; this.refresh();
    });
    const columns = element('div', 'function-columns');
    for (const [key, label] of [['name', 'Function'], ['total', 'Total / attributed cost'], ['self_weight', 'Leaf / self cost']] as const) {
      const button = element('button', 'summary-sort', label);
      button.addEventListener('click', () => {
        this.ascending = this.sort === key ? !this.ascending : key === 'name'; this.sort = key; this.flatten();
      });
      columns.append(button);
    }
    columns.append(element('span', '', '% of scope'), element('span', '', 'Observations'));
    this.scroll.id = 'functions-scroll'; this.scroll.setAttribute('role', 'treegrid');
    this.scroll.setAttribute('aria-label', 'Function call tree'); this.scroll.setAttribute('aria-colcount', '5');
    this.scroll.addEventListener('scroll', () => this.renderRows());
    this.scroll.append(this.rows);
    const table = element('div', 'function-table'); table.append(columns, this.scroll);
    this.panel.append(toolbar, this.status, this.retry, this.note, this.total, table, this.detail);
    host.append(this.panel);
    if (!report.stacks) {
      this.status.textContent = 'Stacks were not collected. Use --collect stacks for native sampling, or --collect stacks,python for supported mixed Python/native stacks.';
      for (const control of Array.from(toolbar.querySelectorAll('input,select,button'))) control.setAttribute('disabled', '');
      this.scroll.hidden = true; columns.hidden = true;
    } else if (!report.viewer?.stacks_endpoint) {
      if (!report.stacks.complete) {
        this.fail(report.stacks.warnings.join(' ')); this.retry.hidden = true; return;
      }
      this.startWorker();
    }
  }
  private startWorker(): void {
    if (!this.report.stacks?.complete) return;
    try {
      const url = URL.createObjectURL(new Blob([STACK_WORKER_SOURCE], { type: 'text/javascript' }));
      this.worker = new Worker(url); URL.revokeObjectURL(url);
      this.worker.postMessage({ kind: 'init', data: this.report.stacks, tracks: this.report.tracks });
      this.worker.onmessage = (event: MessageEvent<{ id: number; tree?: StackTree; error?: string }>) => {
        if (event.data.id !== this.request) return;
        if (event.data.error) this.fail(event.data.error);
        else if (event.data.tree) this.accept(event.data.tree);
      };
      this.worker.onerror = event => {
        this.worker?.terminate(); this.worker = undefined;
        this.fail(`Offline function worker failed: ${event.message}`);
      };
    } catch (error) {
      this.fail(`Offline function worker unavailable: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  setScope(domain: string | undefined, range: { start: bigint; end: bigint } | undefined, filter: string): void {
    if (!domain || !this.report.stacks) return;
    this.scope = { domain, range, filter }; this.refresh();
  }
  private refresh(): void {
    if (!this.scope || !this.report.stacks || (!this.worker && !this.report.viewer?.stacks_endpoint)) return;
    const domain = this.report.domains.find(d => d.id === this.scope!.domain);
    if (!domain) return;
    const query: StackQuery = {
      snapshot: this.report.viewer?.snapshot ?? 'offline', domain: domain.id,
      start_ns: (this.scope.range?.start ?? 0n).toString(),
      end_ns: (this.scope.range?.end ?? BigInt(domain.end_ns) + 1n).toString(),
      filter: this.scope.filter, metric: this.metric, orientation: this.orientation, group: this.group,
    };
    const key = JSON.stringify(query); if (key === this.key) return;
    this.key = key; const id = ++this.request;
    clearTimeout(this.timer); this.abort?.abort();
    this.status.textContent = 'Loading functions · previous results may be stale';
    this.status.dataset.state = 'loading'; this.panel.setAttribute('aria-busy', 'true'); this.retry.hidden = true;
    this.timer = setTimeout(() => { void this.load(query, id); }, 120);
  }
  private async load(query: StackQuery, id: number): Promise<void> {
    if (this.worker) { this.worker.postMessage({ kind: 'query', query, id }); return; }
    const path = this.endpoint(this.report.viewer?.stacks_endpoint ?? '');
    if (!path) { this.fail('Invalid local functions endpoint'); return; }
    const abort = new AbortController(); this.abort = abort;
    try {
      const response = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(query), signal: abort.signal });
      if (!response.ok) {
        const body: { error?: string } = await response.json();
        throw new Error(body.error ?? `HTTP ${response.status}`);
      }
      const tree: StackTree = await response.json();
      if (id === this.request && !abort.signal.aborted) this.accept(tree);
    } catch (error) {
      if (id === this.request && !abort.signal.aborted) this.fail(error instanceof Error ? error.message : String(error));
    }
  }
  private accept(tree: StackTree): void {
    try {
      if (tree.domain !== this.scope?.domain || tree.metric !== this.metric ||
          tree.snapshot !== (this.report.viewer?.snapshot ?? 'offline') || !Array.isArray(tree.nodes) || tree.nodes.length > 20_000)
        throw new Error('Invalid or stale functions response');
      BigInt(tree.total); BigInt(tree.unattributed); BigInt(tree.observations);
      for (const [id, node] of tree.nodes.entries()) {
        if (node.id !== id || (node.parent !== null && (!Number.isInteger(node.parent) || node.parent < 0 || node.parent >= id)) ||
            typeof node.frame.name !== 'string' || typeof node.frame.key !== 'string')
          throw new Error('Invalid function tree node');
        BigInt(node.total); BigInt(node.self_weight); BigInt(node.observations);
      }
      this.tree = tree; this.expanded.clear(); this.children.clear(); this.detail.textContent = '';
      for (const node of tree.nodes) {
        const children = this.children.get(node.parent) ?? []; children.push(node); this.children.set(node.parent, children);
      }
      let roots = this.children.get(null) ?? [];
      for (let depth = 0; depth < 8 && roots.length === 1; depth++) {
        this.expanded.add(roots[0]!.id); roots = this.children.get(roots[0]!.id) ?? [];
      }
      const range = this.scope?.range;
      this.status.textContent = `${range ? `${formatNs(range.start)}–${formatNs(range.end)}` : 'Full clock domain'}${this.scope?.filter ? ` · Filter: ${this.scope.filter}` : ''} · ${tree.nodes.length} function/path nodes`;
      this.status.dataset.state = 'ready'; this.panel.setAttribute('aria-busy', 'false'); this.retry.hidden = true;
      this.note.textContent = `${STACK_METRICS.find(m => m.value === this.metric)!.note} ${tree.warnings.join(' ')}`;
      this.total.textContent = `Scoped total: ${this.cost(tree.total)} · ${tree.observations} ${this.metric === 'cpu' ? 'samples' : 'completed observations'} · Unattributed: ${this.cost(tree.unattributed)}. Find function only changes displayed rows.`;
      this.scroll.scrollTop = 0; this.flatten();
    } catch (error) { this.fail(error instanceof Error ? error.message : String(error)); }
  }
  private fail(message: string): void {
    this.status.textContent = `Functions unavailable: ${message}${this.tree ? ' · previous results may be stale' : ''}`;
    this.status.dataset.state = 'error'; this.panel.setAttribute('aria-busy', 'false'); this.retry.hidden = false;
  }
  private cost(value: string): string {
    return this.metric.endsWith('bytes') ? formatBytes(value) : formatNs(BigInt(value));
  }
  private flatten(): void {
    if (!this.tree) return;
    const allowed = new Set<number>(), search = this.search.value.trim().toLowerCase();
    if (search) for (const node of this.tree.nodes) if (`${node.frame.name} ${node.frame.module}`.toLowerCase().includes(search)) {
      let current: StackNode | undefined = node;
      while (current && !allowed.has(current.id)) {
        allowed.add(current.id); current = current.parent === null ? undefined : this.tree.nodes[current.parent];
      }
    }
    this.visible = [];
    const visit = (parent: number | null, depth: number) => {
      const children = sortSummaryRows(this.children.get(parent) ?? [],
        node => this.sort === 'name' ? node.frame.name : BigInt(node[this.sort]),
        this.ascending ? 'ascending' : 'descending');
      for (const node of children) {
        if (search && !allowed.has(node.id)) continue;
        this.visible.push({ node, depth });
        if (search || this.expanded.has(node.id)) visit(node.id, depth + 1);
      }
    };
    visit(null, 0);
    this.scroll.setAttribute('aria-rowcount', String(this.visible.length));
    this.rows.style.height = `${Math.max(1, this.visible.length) * ROW_HEIGHT}px`;
    this.renderRows();
  }
  private toggle(node: StackNode): void {
    if (this.search.value || this.expanded.has(node.id)) this.expanded.delete(node.id); else this.expanded.add(node.id);
    this.search.value = ''; this.flatten(); this.focus(node.id);
  }
  private focus(id: number): void {
    const index = this.visible.findIndex(row => row.node.id === id); if (index < 0) return;
    if (index * ROW_HEIGHT < this.scroll.scrollTop) this.scroll.scrollTop = index * ROW_HEIGHT;
    if ((index + 1) * ROW_HEIGHT > this.scroll.scrollTop + this.scroll.clientHeight)
      this.scroll.scrollTop = (index + 1) * ROW_HEIGHT - this.scroll.clientHeight;
    this.renderRows();
    this.rows.querySelector<HTMLButtonElement>(`[data-function-id="${id}"] button`)?.focus({ preventScroll: true });
  }
  private renderRows(): void {
    const active = document.activeElement;
    const focused = active instanceof HTMLButtonElement && this.rows.contains(active)
      ? { id: active.closest<HTMLElement>('[data-function-id]')?.dataset.functionId,
        label: active.classList.contains('function-label') } : undefined;
    this.rows.replaceChildren(); if (!this.tree) return;
    if (!this.visible.length) { this.rows.append(element('p', 'io-note', 'No function observations match this scope/search.')); return; }
    const from = Math.max(0, Math.floor(this.scroll.scrollTop / ROW_HEIGHT) - 5);
    const to = Math.min(this.visible.length, from + Math.ceil((this.scroll.clientHeight || 360) / ROW_HEIGHT) + 12);
    const total = BigInt(this.tree.total);
    for (let i = from; i < to; i++) {
      const { node, depth } = this.visible[i]!;
      const row = element('div', 'function-row');
      row.dataset.functionId = String(node.id); row.setAttribute('role', 'row'); row.setAttribute('aria-level', String(depth + 1));
      row.setAttribute('aria-rowindex', String(i + 1)); row.style.top = `${i * ROW_HEIGHT}px`;
      const hasChildren = !!this.children.get(node.id)?.length;
      const expanded = !!this.search.value || this.expanded.has(node.id);
      if (hasChildren) row.setAttribute('aria-expanded', String(expanded));
      const cell = element('div', 'function-name'); cell.style.paddingLeft = `${Math.min(depth, 8) * 14}px`;
      const toggle = element('button', 'io-file', hasChildren ? (expanded ? '▾' : '▸') : '·');
      toggle.setAttribute('aria-label', `${hasChildren ? (expanded ? 'Collapse' : 'Expand') : 'Inspect'} ${node.frame.name}`);
      if (hasChildren) toggle.addEventListener('click', () => this.toggle(node));
      const label = element('button', 'io-file function-label', node.frame.name);
      label.title = `${node.frame.name}\n${node.frame.module}\n${node.frame.language}`;
      const inspect = () => {
        const path: string[] = []; let current: StackNode | undefined = node;
        while (current) { path.unshift(current.frame.name); current = current.parent === null ? undefined : this.tree!.nodes[current.parent]; }
        this.detail.textContent = `${path.join('\n  → ')}\n\n${node.frame.module}\nRepresentative address: ${node.frame.address || 'n/a'}\nTotal: ${node.total}${this.metric.endsWith('bytes') ? ' bytes' : ' ns'}\nLeaf/self: ${node.self_weight}\nAggregated call path, not a single invocation.`;
      };
      label.addEventListener('click', inspect); if (!hasChildren) toggle.addEventListener('click', inspect);
      cell.append(toggle, label); row.append(cell, element('span', '', this.cost(node.total)), element('span', '', this.cost(node.self_weight)),
        element('span', '', total ? `${Number(BigInt(node.total) * 10000n / total) / 100}%` : '—'), element('span', '', node.observations));
      for (const cell of Array.from(row.children)) cell.setAttribute('role', 'gridcell');
      row.addEventListener('keydown', event => {
        if (event.key === 'ArrowRight' && hasChildren && !expanded) { event.preventDefault(); this.toggle(node); }
        else if (event.key === 'ArrowLeft') {
          event.preventDefault();
          if (hasChildren && expanded) this.toggle(node); else if (node.parent !== null) this.focus(node.parent);
        } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
          event.preventDefault(); const next = this.visible[i + (event.key === 'ArrowDown' ? 1 : -1)];
          if (next) this.focus(next.node.id);
        }
      });
      this.rows.append(row);
    }
    if (focused?.id) this.rows.querySelector<HTMLButtonElement>(
      `[data-function-id="${focused.id}"] ${focused.label ? '.function-label' : 'button'}`)?.focus({ preventScroll: true });
  }
}
