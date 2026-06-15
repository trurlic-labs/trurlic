import type { FilterState } from '../types';
import { defaultFilterState } from '../types';
import { esc } from '../util';

const AGE_OPTIONS: { label: string; days: number | null }[] = [
  { label: 'All', days: null },
  { label: '1w', days: 7 },
  { label: '1m', days: 30 },
  { label: '3m', days: 90 },
  { label: '1y', days: 365 },
];

const EDGE_KINDS = ['connects_to', 'depends_on', 'constrains', 'supersedes'] as const;

/**
 * Toolbar controller (spec §Filtering and Visibility).
 *
 * Renders into `#toolbar` and calls `onChange` whenever the user
 * toggles an edge kind, selects a tag, changes the age range,
 * or toggles focus mode.
 */
export class Toolbar {
  private el: HTMLElement;
  private state: FilterState;
  private onChange: (state: FilterState) => void;
  private tags: string[] = [];

  constructor(onChange: (state: FilterState) => void) {
    this.el = document.getElementById('toolbar')!;
    this.state = defaultFilterState();
    this.onChange = onChange;
    this.render();
  }

  get filterState(): FilterState {
    return this.state;
  }

  /** Update the tag list when the graph changes. Re-renders tag pills. */
  setAvailableTags(tags: string[]): void {
    // Prune active tags that no longer exist.
    for (const t of this.state.activeTags) {
      if (!tags.includes(t)) this.state.activeTags.delete(t);
    }
    this.tags = tags;
    this.render();
  }

  /** Sync the focus pill visual without firing onChange (prevents loops). */
  setFocusActive(active: boolean): void {
    if (this.state.focusMode === active) return;
    this.state.focusMode = active;
    this.render();
  }

  private emit(): void {
    this.onChange(this.state);
  }

  private render(): void {
    const parts: string[] = [];

    // ── Edge kind toggles ──────────────────────────────────────────────
    parts.push('<div class="toolbar-group">');
    parts.push('<span class="toolbar-label">Edges</span>');
    for (const kind of EDGE_KINDS) {
      const on = this.state.edgeKinds.has(kind) ? ' on' : '';
      const label = kind.replace(/_/g, ' ');
      parts.push(`<button class="toolbar-pill${on}" data-edge="${kind}">${esc(label)}</button>`);
    }
    parts.push('</div>');

    // ── Tag pills (only when tags exist) ───────────────────────────────
    if (this.tags.length > 0) {
      parts.push('<div class="toolbar-group">');
      parts.push('<span class="toolbar-label">Tags</span>');
      for (const tag of this.tags) {
        const on = this.state.activeTags.has(tag) ? ' on' : '';
        parts.push(`<button class="toolbar-pill${on}" data-tag="${esc(tag)}">${esc(tag)}</button>`);
      }
      parts.push('</div>');
    }

    // ── Age dropdown ───────────────────────────────────────────────────
    parts.push('<div class="toolbar-group">');
    parts.push('<span class="toolbar-label">Age</span>');
    parts.push('<select class="toolbar-select" data-role="age">');
    for (const opt of AGE_OPTIONS) {
      const sel = this.state.maxAgeDays === opt.days ? ' selected' : '';
      const val = opt.days === null ? '' : String(opt.days);
      parts.push(`<option value="${val}"${sel}>${esc(opt.label)}</option>`);
    }
    parts.push('</select>');
    parts.push('</div>');

    // ── Focus toggle ───────────────────────────────────────────────────
    parts.push('<div class="toolbar-group">');
    const fOn = this.state.focusMode ? ' on' : '';
    parts.push(`<button class="toolbar-pill${fOn}" data-role="focus">Focus</button>`);
    parts.push('</div>');

    this.el.innerHTML = parts.join('');
    this.attachHandlers();
  }

  private attachHandlers(): void {
    // Edge kind toggles.
    for (const btn of this.el.querySelectorAll<HTMLButtonElement>('[data-edge]')) {
      btn.addEventListener('click', () => {
        const kind = btn.dataset.edge!;
        if (this.state.edgeKinds.has(kind)) {
          this.state.edgeKinds.delete(kind);
        } else {
          this.state.edgeKinds.add(kind);
        }
        this.render();
        this.emit();
      });
    }

    // Tag toggles.
    for (const btn of this.el.querySelectorAll<HTMLButtonElement>('[data-tag]')) {
      btn.addEventListener('click', () => {
        const tag = btn.dataset.tag!;
        if (this.state.activeTags.has(tag)) {
          this.state.activeTags.delete(tag);
        } else {
          this.state.activeTags.add(tag);
        }
        this.render();
        this.emit();
      });
    }

    // Age select.
    const ageSel = this.el.querySelector<HTMLSelectElement>('[data-role="age"]');
    if (ageSel) {
      ageSel.addEventListener('change', () => {
        this.state.maxAgeDays = ageSel.value === '' ? null : parseInt(ageSel.value, 10);
        this.emit();
      });
    }

    // Focus toggle.
    const focusBtn = this.el.querySelector<HTMLButtonElement>('[data-role="focus"]');
    if (focusBtn) {
      focusBtn.addEventListener('click', () => {
        this.state.focusMode = !this.state.focusMode;
        this.render();
        this.emit();
      });
    }
  }
}
