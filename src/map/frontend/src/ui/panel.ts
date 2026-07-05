import type { RenderNode, DecisionNode, PatternNode, CodeRefData } from '../types';
import type { Graph } from '../state/graph';
import type { ApiClient } from '../state/api';
import { esc } from '../util';

// ── Callbacks ──────────────────────────────────────────────────────────────

export interface PanelCallbacks {
  onNavigate(componentName: string): void;
  onMutated(): void;
}

// ── Panel ──────────────────────────────────────────────────────────────────

const SAVE_DEBOUNCE = 1000;

/**
 * Right-side detail/inspector panel. Standard HTML — fully accessible,
 * serves as the primary content surface while the canvas provides
 * spatial context. Inline edits auto-save on blur with debounce.
 */
type PanelView =
  | { type: 'project' }
  | { type: 'component'; name: string }
  | { type: 'decision'; name: string }
  | { type: 'pattern'; name: string };

export class Panel {
  private el: HTMLElement;
  private api: ApiClient | null = null;
  private cb: PanelCallbacks | null = null;
  private saveTimer: ReturnType<typeof setTimeout> | null = null;
  private history: PanelView[] = [];
  private currentView: PanelView = { type: 'project' };

  constructor(el: HTMLElement) {
    this.el = el;
  }

  /** Wire up the API client and callbacks. Called once during app init. */
  init(api: ApiClient, cb: PanelCallbacks): void {
    this.api = api;
    this.cb = cb;
  }

  private clearPendingSave(): void {
    if (this.saveTimer) {
      clearTimeout(this.saveTimer);
      this.saveTimer = null;
    }
  }

  // ── Project view ────────────────────────────────────────────────────

  showProject(graph: Graph): void {
    this.clearPendingSave();
    this.currentView = { type: 'project' };
    this.history = [];
    const dc = graph.decisions.size;
    const cc = graph.nodes.size;
    const pc = graph.patterns.size;
    this.el.innerHTML = `
      <h2>${esc(graph.projectName)}</h2>
      ${graph.projectDescription ? `<p class="dim">${esc(graph.projectDescription)}</p>` : ''}
      <div class="stats">
        <div class="stat"><span class="stat-n">${cc}</span> components</div>
        <div class="stat"><span class="stat-n">${dc}</span> decisions</div>
        <div class="stat"><span class="stat-n">${pc}</span> patterns</div>
      </div>
      <h3>Recent decisions</h3>
      ${recentDecisions(graph)}
      <h3>Patterns <span class="dim">(${pc})</span></h3>
      ${patternList(graph)}
    `;
    this.bindDecisionLinks(graph);
    this.bindPatternLinks(graph);
  }

  // ── Component view ──────────────────────────────────────────────────

  showComponent(node: RenderNode, graph: Graph): void {
    this.clearPendingSave();
    this.currentView = { type: 'component', name: node.name };
    this.history = [];
    const decs = graph.decisionsFor(node.name);
    const allConns = graph.connectionsFor(node.name);

    const patterns: PatternNode[] = [];
    for (const [, p] of graph.patterns) {
      if (p.components.includes(node.name)) patterns.push(p);
    }

    this.el.innerHTML = `
      <div class="panel-kind">component</div>
      <h2 data-component="${esc(node.name)}">${esc(node.name)}</h2>
      ${node.description ? `<p class="dim">${esc(node.description)}</p>` : ''}
      ${
        allConns.length > 0
          ? `
        <h3>Connections</h3>
        <div class="chip-list">${allConns.map((c) => `<a class="chip nav-link" data-nav="${esc(c)}" href="#">${esc(c)}</a>`).join('')}</div>
      `
          : ''
      }
      ${
        patterns.length > 0
          ? `
        <h3>Patterns</h3>
        ${patterns.map((p) => `<div class="dec-card pattern-link" data-pattern="${esc(p.name)}"><div class="dec-choice">${esc(p.description)}</div></div>`).join('')}
      `
          : ''
      }
      <h3>Decisions <span class="dim">(${decs.length})</span></h3>
      ${decs.length === 0 ? '<p class="dim">None yet</p>' : decs.map((d) => decisionRow(d)).join('')}
    `;
    this.bindNavLinks();
    this.bindDecisionLinks(graph);
    this.bindPatternLinks(graph);
  }

  // ── Decision view ───────────────────────────────────────────────────

  showDecision(name: string, graph: Graph): void {
    this.clearPendingSave();
    const dec = graph.decisions.get(name);
    if (!dec) {
      this.showProject(graph);
      return;
    }

    this.pushCurrentView();
    this.currentView = { type: 'decision', name };
    const backHtml = this.history.length > 0 ? '<a href="#" class="panel-back">← back</a>' : '';

    const attrHtml = renderAttribution(dec.attribution);
    const revHtml = renderRevisionCount(dec.revision_count);
    const metaSuffix = [attrHtml, revHtml].filter(Boolean).join(' ');

    this.el.innerHTML = `
      ${backHtml}
      <div class="panel-kind">decision</div>
      <h2 class="editable-heading" data-field="choice" data-dec="${esc(name)}" data-placeholder="Click to edit">${esc(dec.choice)}</h2>
      <p class="dim">
        <a class="nav-link" data-nav="${esc(dec.component)}" href="#">${esc(dec.component)}</a>
        · ${new Date(dec.created).toLocaleDateString()}${metaSuffix ? ` · ${metaSuffix}` : ''}
      </p>
      <h3>Reason</h3>
      <div class="editable-block" data-field="reason" data-dec="${esc(name)}" data-placeholder="Click to add reason">${esc(dec.reason)}</div>
      ${
        dec.tags.length > 0
          ? `
        <h3>Tags</h3>
        <div class="chip-list">${dec.tags.map((t) => `<span class="chip tag">${esc(t)}</span>`).join('')}</div>
      `
          : ''
      }
      ${renderCodeRefs(dec.code_refs ?? [])}
      ${
        dec.alternatives.length > 0
          ? `
        <h3>Alternatives considered</h3>
        <ul class="alt-list">${dec.alternatives.map((a) => `<li class="dim">${esc(a)}</li>`).join('')}</ul>
      `
          : ''
      }
      <div class="panel-actions">
        <button class="btn btn-danger" data-delete-dec="${esc(name)}">Delete decision</button>
      </div>
    `;
    this.bindBackLink(graph);
    this.bindNavLinks();
    this.bindEditableFields(name);
    this.bindDeleteDecision(name);
  }

  // ── Pattern view ────────────────────────────────────────────────────

  showPattern(name: string, graph: Graph): void {
    this.clearPendingSave();
    const pat = graph.patterns.get(name);
    if (!pat) {
      this.showProject(graph);
      return;
    }

    this.pushCurrentView();
    this.currentView = { type: 'pattern', name };
    const backHtml = this.history.length > 0 ? '<a href="#" class="panel-back">← back</a>' : '';

    const memberDecs = pat.decisions
      .map((d) => graph.decisions.get(d))
      .filter((d): d is DecisionNode => d != null);

    this.el.innerHTML = `
      ${backHtml}
      <div class="panel-kind">pattern</div>
      <h2>${esc(pat.description)}</h2>
      <p class="dim">${esc(name)}</p>
      ${
        pat.components.length > 0
          ? `
        <h3>Applies to</h3>
        <div class="chip-list">${pat.components.map((c) => `<a class="chip nav-link" data-nav="${esc(c)}" href="#">${esc(c)}</a>`).join('')}</div>
      `
          : ''
      }
      <h3>Decisions <span class="dim">(${memberDecs.length})</span></h3>
      ${memberDecs.map((d) => decisionRow(d)).join('')}
    `;
    this.bindBackLink(graph);
    this.bindNavLinks();
    this.bindDecisionLinks(graph);
  }

  showEmpty(): void {
    this.clearPendingSave();
    this.el.innerHTML = `<p class="dim" style="padding:24px">Click a component to inspect</p>`;
  }

  showLoading(): void {
    this.clearPendingSave();
    this.el.innerHTML = '<p class="dim" style="padding:24px">Loading…</p>';
  }

  showLoadError(retry: () => void): void {
    this.el.innerHTML = `
      <div style="padding: 24px;">
        <p style="margin-bottom: 12px; color: var(--text);">Could not load the architecture graph.</p>
        <button class="btn" id="retry-btn">Retry</button>
      </div>
    `;
    this.el.querySelector('#retry-btn')?.addEventListener('click', retry);
  }

  // ── Navigation history ───────────────────────────────────────────────

  private pushCurrentView(): void {
    this.history.push(this.currentView);
  }

  private bindBackLink(graph: Graph): void {
    const link = this.el.querySelector<HTMLElement>('.panel-back');
    if (!link) return;
    link.addEventListener('click', (e) => {
      e.preventDefault();
      const prev = this.history.pop();
      if (!prev) return;
      switch (prev.type) {
        case 'project':
          this.showProject(graph);
          break;
        case 'component': {
          const node = graph.nodes.get(prev.name);
          if (node) this.showComponent(node, graph);
          else this.showProject(graph);
          break;
        }
        case 'decision':
          this.showDecision(prev.name, graph);
          break;
        case 'pattern':
          this.showPattern(prev.name, graph);
          break;
      }
    });
  }

  // ── Event binding ───────────────────────────────────────────────────

  private bindNavLinks(): void {
    for (const link of this.el.querySelectorAll<HTMLElement>('.nav-link')) {
      link.addEventListener('click', (e) => {
        e.preventDefault();
        const target = link.dataset.nav;
        if (target) this.cb?.onNavigate(target);
      });
    }
  }

  private bindDecisionLinks(graph: Graph): void {
    for (const card of this.el.querySelectorAll<HTMLElement>('.dec-link')) {
      card.addEventListener('click', () => {
        const name = card.dataset.dec;
        if (name) this.showDecision(name, graph);
      });
    }
  }

  private bindPatternLinks(graph: Graph): void {
    for (const card of this.el.querySelectorAll<HTMLElement>('.pattern-link')) {
      card.addEventListener('click', () => {
        const name = card.dataset.pattern;
        if (name) this.showPattern(name, graph);
      });
    }
  }

  private bindEditableFields(decName: string): void {
    for (const el of this.el.querySelectorAll<HTMLElement>('.editable-heading, .editable-block')) {
      el.contentEditable = 'true';
      el.addEventListener('blur', () => {
        const field = el.dataset.field as 'choice' | 'reason';
        const value = el.textContent?.trim() ?? '';
        if (!value) return; // Don't save empty.
        this.debouncedSave(decName, { [field]: value });
      });
      el.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' && !e.shiftKey) {
          e.preventDefault();
          el.blur();
        }
      });
    }
  }

  private bindDeleteDecision(name: string): void {
    const btn = this.el.querySelector<HTMLElement>(`[data-delete-dec="${CSS.escape(name)}"]`);
    if (!btn) return;
    btn.addEventListener('click', () => {
      if (!confirm(`Delete decision "${name}"? This cannot be undone.`)) return;
      this.api
        ?.deleteDecision(name)
        .then(() => {
          this.cb?.onMutated();
        })
        .catch((e) => this.showError(e.message));
    });
  }

  private debouncedSave(decName: string, body: { choice?: string; reason?: string }): void {
    if (this.saveTimer) clearTimeout(this.saveTimer);
    this.saveTimer = setTimeout(() => {
      this.api
        ?.updateDecision(decName, body)
        .then(() => {
          this.showSaveIndicator();
          this.cb?.onMutated();
        })
        .catch((e) => this.showError(e.message));
    }, SAVE_DEBOUNCE);
  }

  private showSaveIndicator(): void {
    let indicator = this.el.querySelector('.save-indicator');
    if (!indicator) {
      indicator = document.createElement('div');
      indicator.className = 'save-indicator';
      this.el.prepend(indicator);
    }
    indicator.textContent = 'Saved';
    setTimeout(() => indicator?.remove(), 1500);
  }

  private showError(msg: string): void {
    let errEl = this.el.querySelector('.panel-error');
    if (!errEl) {
      errEl = document.createElement('div');
      errEl.className = 'panel-error';
      this.el.prepend(errEl);
    }
    errEl.textContent = msg;
    setTimeout(() => errEl?.remove(), 5000);
  }
}

// ── Exported helpers (tested in panel.test.ts) ──────────────────────────

export function renderCodeRefs(refs: CodeRefData[]): string {
  if (refs.length === 0) return '';
  const items = refs
    .map((r) => {
      const display = r.symbol ? `${esc(r.file)}::${esc(r.symbol)}` : esc(r.file);
      return `<span class="chip code-ref">${display}</span>`;
    })
    .join('');
  return `<h3>Code references</h3>\n        <div class="chip-list">${items}</div>`;
}

export function renderAttribution(attribution: string | undefined): string {
  if (attribution === 'agent') {
    return '<span class="chip attr-badge agent">agent · unreviewed</span>';
  }
  return '';
}

export function renderRevisionCount(count: number | undefined): string {
  if (!count) return '';
  return `<span class="dim revision-count">Revised ${count}×</span>`;
}

// ── Helpers ──────────────────────────────────────────────────────────────

function decisionRow(d: DecisionNode): string {
  return `
    <div class="dec-card dec-link" data-dec="${esc(d.name)}">
      <div class="dec-choice">${esc(d.choice)}</div>
      <div class="dec-reason dim">${esc(d.reason)}</div>
      ${d.tags.length > 0 ? `<div class="chip-list">${d.tags.map((t) => `<span class="chip tag">${esc(t)}</span>`).join('')}</div>` : ''}
    </div>
  `;
}

function patternList(graph: Graph): string {
  const patterns = [...graph.patterns.values()];
  if (patterns.length === 0) return '<p class="dim">None yet</p>';
  return patterns
    .map(
      (p) => `
    <div class="dec-card pattern-link" data-pattern="${esc(p.name)}" style="cursor:pointer">
      <div class="dec-choice">${esc(p.description || p.name)}</div>
      <div class="dim" style="font-size:12px">${p.components.length} components · ${p.decisions.length} decisions</div>
    </div>
  `,
    )
    .join('');
}

function recentDecisions(graph: Graph): string {
  const sorted = [...graph.decisions.values()]
    .sort((a, b) => b.created.localeCompare(a.created))
    .slice(0, 5);
  if (sorted.length === 0) return '<p class="dim">None yet</p>';
  return sorted
    .map(
      (d) => `
    <div class="dec-card dec-link" data-dec="${esc(d.name)}">
      <div class="dec-choice">${esc(d.choice)}</div>
      <div class="dim" style="font-size:12px">${esc(d.component)} · ${new Date(d.created).toLocaleDateString()}</div>
    </div>
  `,
    )
    .join('');
}
