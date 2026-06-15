import { esc } from '../util';

/** A single action in the command palette. */
export interface PaletteAction {
  label: string;
  shortcut?: string;
  run: () => void;
}

/**
 * Command palette controller (Ctrl+K / Cmd+K).
 *
 * Owns the DOM interaction for the palette overlay. The host provides
 * a list of actions via `open(actions)` and the palette handles
 * fuzzy-filter, keyboard navigation, and selection.
 */
export class CommandPalette {
  private backdrop: HTMLElement;
  private input: HTMLInputElement;
  private results: HTMLElement;

  private actions: PaletteAction[] = [];
  private filtered: PaletteAction[] = [];
  private activeIndex = -1;
  private _open = false;

  get isOpen(): boolean {
    return this._open;
  }

  constructor() {
    this.backdrop = document.getElementById('palette-backdrop')!;
    this.input = document.getElementById('palette-input') as HTMLInputElement;
    this.results = document.getElementById('palette-results')!;

    this.backdrop.addEventListener('pointerdown', (e) => {
      if (e.target === this.backdrop) this.close();
    });

    this.input.addEventListener('input', () => {
      this.filter(this.input.value);
      this.render();
    });

    this.input.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        this.close();
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (this.activeIndex < this.filtered.length - 1) {
          this.activeIndex++;
          this.render();
        }
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (this.activeIndex > 0) {
          this.activeIndex--;
          this.render();
        }
        return;
      }
      if (e.key === 'Enter') {
        e.preventDefault();
        if (this.activeIndex >= 0 && this.activeIndex < this.filtered.length) {
          const action = this.filtered[this.activeIndex];
          this.close();
          action.run();
        }
        return;
      }
    });
  }

  open(actions: PaletteAction[]): void {
    this.actions = actions;
    this.filtered = actions;
    this.activeIndex = actions.length > 0 ? 0 : -1;
    this._open = true;

    this.backdrop.classList.remove('hidden');
    this.input.value = '';
    this.input.focus();
    this.render();
  }

  close(): void {
    this.backdrop.classList.add('hidden');
    this._open = false;
    this.filtered = [];
    this.activeIndex = -1;
  }

  private filter(query: string): void {
    const q = query.toLowerCase().trim();
    if (q.length === 0) {
      this.filtered = this.actions;
    } else {
      this.filtered = this.actions.filter((a) => a.label.toLowerCase().includes(q));
    }
    this.activeIndex = this.filtered.length > 0 ? 0 : -1;
  }

  private render(): void {
    const el = this.results;
    if (this.filtered.length === 0) {
      el.innerHTML =
        '<div class="palette-action" style="color:var(--text-dim)">No matching commands</div>';
      return;
    }
    el.innerHTML = this.filtered
      .map((a, i) => {
        const active = i === this.activeIndex ? ' active' : '';
        const shortcut = a.shortcut
          ? `<span class="palette-shortcut">${esc(a.shortcut)}</span>`
          : '';
        return `<div class="palette-action${active}" data-idx="${i}">${esc(a.label)}${shortcut}</div>`;
      })
      .join('');

    for (const child of el.children) {
      child.addEventListener('click', () => {
        const idx = parseInt((child as HTMLElement).dataset.idx ?? '-1', 10);
        if (idx >= 0 && idx < this.filtered.length) {
          this.close();
          this.filtered[idx].run();
        }
      });
    }

    const active = el.querySelector('.active') as HTMLElement | null;
    if (active) active.scrollIntoView({ block: 'nearest' });
  }
}
