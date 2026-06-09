import { describe, it, expect } from 'vitest';
import { Selection } from './selection';
import type { SearchResult } from '../ui/search';

function makeResults(n: number): SearchResult[] {
  return Array.from({ length: n }, (_, i) => ({
    name: `result-${i}`,
    kind: 'component' as const,
    label: `Result ${i}`,
    score: n - i,
  }));
}

describe('Selection', () => {
  it('starts with nothing selected', () => {
    const s = new Selection();
    expect(s.selected).toBeNull();
    expect(s.focusSet).toBeNull();
    expect(s.searchOpen).toBe(false);
    expect(s.searchActiveIndex).toBe(-1);
  });

  it('select and clear', () => {
    const s = new Selection();
    s.select('auth');
    expect(s.selected).toBe('auth');
    s.select(null);
    expect(s.selected).toBeNull();
  });

  describe('cycleComponent', () => {
    it('returns null with no components', () => {
      const s = new Selection();
      expect(s.cycleComponent(1)).toBeNull();
      expect(s.cycleComponent(-1)).toBeNull();
    });

    it('cycles forward from no selection', () => {
      const s = new Selection();
      s.setComponentNames(['a', 'b', 'c']);
      // curIdx = -1, next = -1+1 = 0 → 'a'
      expect(s.cycleComponent(1)).toBe('a');
    });

    it('cycles forward from a selection', () => {
      const s = new Selection();
      s.setComponentNames(['a', 'b', 'c']);
      s.select('a');
      expect(s.cycleComponent(1)).toBe('b');
    });

    it('wraps forward past the end', () => {
      const s = new Selection();
      s.setComponentNames(['a', 'b', 'c']);
      s.select('c');
      expect(s.cycleComponent(1)).toBe('a');
    });

    it('wraps backward past the start', () => {
      const s = new Selection();
      s.setComponentNames(['a', 'b', 'c']);
      s.select('a');
      expect(s.cycleComponent(-1)).toBe('c');
    });

    it('cycles backward from no selection', () => {
      const s = new Selection();
      s.setComponentNames(['a', 'b', 'c']);
      // curIdx = -1, next = -1-1 = -2 → wraps to 2 → 'c'
      expect(s.cycleComponent(-1)).toBe('c');
    });

    it('handles single-element list', () => {
      const s = new Selection();
      s.setComponentNames(['only']);
      s.select('only');
      expect(s.cycleComponent(1)).toBe('only');
      expect(s.cycleComponent(-1)).toBe('only');
    });
  });

  describe('search', () => {
    it('openSearch resets state', () => {
      const s = new Selection();
      s.setSearchResults(makeResults(5));
      s.openSearch();
      expect(s.searchOpen).toBe(true);
      expect(s.searchResults).toHaveLength(0);
      expect(s.searchActiveIndex).toBe(-1);
    });

    it('closeSearch resets state', () => {
      const s = new Selection();
      s.openSearch();
      s.setSearchResults(makeResults(3));
      s.closeSearch();
      expect(s.searchOpen).toBe(false);
      expect(s.searchResults).toHaveLength(0);
      expect(s.searchActiveIndex).toBe(-1);
    });

    it('setSearchResults activates the first result', () => {
      const s = new Selection();
      s.setSearchResults(makeResults(3));
      expect(s.searchActiveIndex).toBe(0);
    });

    it('setSearchResults with empty list sets index to -1', () => {
      const s = new Selection();
      s.setSearchResults([]);
      expect(s.searchActiveIndex).toBe(-1);
    });

    it('nextSearchResult advances', () => {
      const s = new Selection();
      s.setSearchResults(makeResults(3));
      s.nextSearchResult();
      expect(s.searchActiveIndex).toBe(1);
      s.nextSearchResult();
      expect(s.searchActiveIndex).toBe(2);
    });

    it('nextSearchResult clamps at end', () => {
      const s = new Selection();
      s.setSearchResults(makeResults(2));
      s.nextSearchResult(); // 0 → 1
      s.nextSearchResult(); // stays at 1
      s.nextSearchResult(); // stays at 1
      expect(s.searchActiveIndex).toBe(1);
    });

    it('prevSearchResult clamps at start', () => {
      const s = new Selection();
      s.setSearchResults(makeResults(3));
      s.prevSearchResult(); // stays at 0
      expect(s.searchActiveIndex).toBe(0);
    });

    it('prev after next returns to original', () => {
      const s = new Selection();
      s.setSearchResults(makeResults(5));
      s.nextSearchResult(); // 1
      s.nextSearchResult(); // 2
      s.prevSearchResult(); // 1
      expect(s.searchActiveIndex).toBe(1);
    });

    it('activeSearchResult returns highlighted item', () => {
      const s = new Selection();
      const results = makeResults(3);
      s.setSearchResults(results);
      expect(s.activeSearchResult()).toBe(results[0]);
      s.nextSearchResult();
      expect(s.activeSearchResult()).toBe(results[1]);
    });

    it('activeSearchResult returns null when empty', () => {
      const s = new Selection();
      expect(s.activeSearchResult()).toBeNull();
    });

    it('activeSearchResult returns null after close', () => {
      const s = new Selection();
      s.setSearchResults(makeResults(3));
      s.closeSearch();
      expect(s.activeSearchResult()).toBeNull();
    });
  });
});
