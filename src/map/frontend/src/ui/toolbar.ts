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
  private popoverOpen = false;
  private tagFilter = '';
  private documentPointerHandler: ((e: PointerEvent) => void) | null = null;

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

    // ── Tag popover toggle (only when tags exist) ──────────────────────
    if (this.tags.length > 0) {
      const activeCount = this.state.activeTags.size;
      const label = activeCount > 0 ? `Tags (${activeCount})` : 'Tags';
      const on = activeCount > 0 ? ' on' : '';
      parts.push('<div class="toolbar-group has-popover">');
      parts.push('<span class="toolbar-label">Tags</span>');
      parts.push(`<button class="toolbar-pill${on}" data-role="tag-toggle">${esc(label)}</button>`);

      const hidden = this.popoverOpen ? '' : ' hidden';
      parts.push(`<div class="tag-popover${hidden}">`);
      parts.push('<input type="text" placeholder="Filter tags…" data-role="tag-filter">');
      parts.push('<div class="tag-popover-list">');
      const lowerFilter = this.tagFilter.toLowerCase();
      for (const tag of this.tags) {
        if (lowerFilter && !tag.toLowerCase().includes(lowerFilter)) continue;
        const checked = this.state.activeTags.has(tag) ? ' checked' : '';
        parts.push(
          `<label class="tag-popover-item" data-tag="${esc(tag)}">` +
            `<input type="checkbox"${checked} data-tag-check="${esc(tag)}">` +
            `${esc(tag)}</label>`,
        );
      }
      parts.push('</div>');
      parts.push('</div>');

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

    // Tag popover toggle.
    const tagToggle = this.el.querySelector<HTMLButtonElement>('[data-role="tag-toggle"]');
    if (tagToggle) {
      tagToggle.addEventListener('click', () => {
        if (this.popoverOpen) {
          this.closePopover();
        } else {
          this.openPopover();
        }
      });
    }

    // Tag filter input.
    const tagFilterInput = this.el.querySelector<HTMLInputElement>('[data-role="tag-filter"]');
    if (tagFilterInput) {
      tagFilterInput.value = this.tagFilter;
      tagFilterInput.addEventListener('input', () => {
        this.tagFilter = tagFilterInput.value;
        this.render();
        this.restorePopoverFocus();
      });
    }

    // Tag checkboxes.
    for (const cb of this.el.querySelectorAll<HTMLInputElement>('[data-tag-check]')) {
      cb.addEventListener('change', () => {
        const tag = cb.dataset.tagCheck!;
        if (this.state.activeTags.has(tag)) {
          this.state.activeTags.delete(tag);
        } else {
          this.state.activeTags.add(tag);
        }
        this.render();
        this.restorePopoverFocus();
        this.emit();
      });
    }

    // Close popover on outside click.
    this.removeDocumentHandler();
    if (this.popoverOpen) {
      this.documentPointerHandler = (e: PointerEvent) => {
        const popover = this.el.querySelector('.tag-popover');
        const toggle = this.el.querySelector('[data-role="tag-toggle"]');
        const target = e.target as Node;
        if (popover && !popover.contains(target) && toggle && !toggle.contains(target)) {
          this.closePopover();
        }
      };
      document.addEventListener('pointerdown', this.documentPointerHandler);
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

    // Escape closes popover.
    this.el.addEventListener('keydown', (e: KeyboardEvent) => {
      if (e.key === 'Escape' && this.popoverOpen) {
        this.closePopover();
      }
    });
  }

  private openPopover(): void {
    this.popoverOpen = true;
    this.tagFilter = '';
    this.render();
    this.restorePopoverFocus();
  }

  private closePopover(): void {
    this.popoverOpen = false;
    this.tagFilter = '';
    this.removeDocumentHandler();
    this.render();
  }

  private restorePopoverFocus(): void {
    const input = this.el.querySelector<HTMLInputElement>('[data-role="tag-filter"]');
    if (input) input.focus();
  }

  private removeDocumentHandler(): void {
    if (this.documentPointerHandler) {
      document.removeEventListener('pointerdown', this.documentPointerHandler);
      this.documentPointerHandler = null;
    }
  }
}
