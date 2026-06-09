import type { Graph } from '../state/graph';
import type { SearchResult } from '../ui/search';
import { neighborhood } from '../ui/search';

/**
 * Selection, focus, and search state.
 *
 * Owns the "what is the user looking at" state: which node is
 * selected, which nodes are in the focus neighborhood, what the
 * search results are, and the tab-cycling order. The App reads
 * these via accessors and passes them to the renderer each frame.
 *
 * Does NOT touch the DOM — the App handles search bar visibility
 * and panel updates as coordination side-effects.
 */
export class Selection {
  private _selected: string | null = null;
  private _focusSet: Set<string> | null = null;
  private _componentNames: string[] = [];

  // Search state.
  private _searchOpen = false;
  private _searchResults: SearchResult[] = [];
  private _searchActiveIndex = -1;

  // ── Accessors ──────────────────────────────────────────────────────

  get selected(): string | null {
    return this._selected;
  }

  get focusSet(): Set<string> | null {
    return this._focusSet;
  }

  get componentNames(): readonly string[] {
    return this._componentNames;
  }

  get searchOpen(): boolean {
    return this._searchOpen;
  }

  get searchResults(): readonly SearchResult[] {
    return this._searchResults;
  }

  get searchActiveIndex(): number {
    return this._searchActiveIndex;
  }

  // ── Selection ──────────────────────────────────────────────────────

  select(name: string | null): void {
    this._selected = name;
  }

  // ── Focus ──────────────────────────────────────────────────────────

  clearFocus(): void {
    this._focusSet = null;
  }

  /** Compute and set the 1-hop neighborhood for `name`. */
  setFocus(name: string, graph: Graph): void {
    this._focusSet = neighborhood(graph, name);
  }

  // ── Component cycling ──────────────────────────────────────────────

  setComponentNames(names: string[]): void {
    this._componentNames = names;
  }

  /**
   * Cycle to the next (direction=1) or previous (direction=-1)
   * component in sorted order. Returns the target name, or null
   * if no components exist.
   */
  cycleComponent(direction: 1 | -1): string | null {
    const names = this._componentNames;
    if (names.length === 0) return null;
    const curIdx = this._selected ? names.indexOf(this._selected) : -1;
    let next = curIdx + direction;
    if (next < 0) next = names.length - 1;
    if (next >= names.length) next = 0;
    return names[next];
  }

  // ── Search ─────────────────────────────────────────────────────────

  openSearch(): void {
    this._searchOpen = true;
    this._searchResults = [];
    this._searchActiveIndex = -1;
  }

  closeSearch(): void {
    this._searchOpen = false;
    this._searchResults = [];
    this._searchActiveIndex = -1;
  }

  setSearchResults(results: SearchResult[]): void {
    this._searchResults = results;
    this._searchActiveIndex = results.length > 0 ? 0 : -1;
  }

  nextSearchResult(): void {
    if (this._searchActiveIndex < this._searchResults.length - 1) {
      this._searchActiveIndex++;
    }
  }

  prevSearchResult(): void {
    if (this._searchActiveIndex > 0) {
      this._searchActiveIndex--;
    }
  }

  /** Return the currently highlighted search result, or null. */
  activeSearchResult(): SearchResult | null {
    const idx = this._searchActiveIndex;
    if (idx >= 0 && idx < this._searchResults.length) {
      return this._searchResults[idx];
    }
    return null;
  }
}
