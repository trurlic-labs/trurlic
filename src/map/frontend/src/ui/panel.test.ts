import { describe, it, expect } from 'vitest';
import type { DecisionNode, PatternNode } from '../types';
import type { Graph } from '../state/graph';

// Minimal DOM shim — esc() calls document.createElement('span') at module scope.
if (typeof document === 'undefined') {
  const escMap: Record<string, string> = { '&': '&amp;', '<': '&lt;', '>': '&gt;' };
  (globalThis as Record<string, unknown>).document = {
    createElement: () => {
      let text = '';
      return {
        set textContent(v: string) {
          text = v;
        },
        get innerHTML() {
          return text.replace(/[&<>]/g, (ch: string) => escMap[ch] ?? ch);
        },
      };
    },
  };
}

const {
  renderCodeRefs,
  renderAttribution,
  renderRevisionCount,
  decisionRow,
  patternList,
  recentDecisions,
} = await import('./panel');

function mkDecision(over: Partial<DecisionNode> = {}): DecisionNode {
  return {
    name: 'use-jwt',
    component: 'auth',
    choice: 'Use JWT',
    reason: 'Stateless',
    tags: [],
    created: '2026-01-01T00:00:00Z',
    alternatives: [],
    ...over,
  };
}

/** Minimal Graph stub — the list helpers only read `.decisions` / `.patterns`. */
function mkGraph(decisions: DecisionNode[], patterns: PatternNode[] = []): Graph {
  return {
    decisions: new Map(decisions.map((d) => [d.name, d])),
    patterns: new Map(patterns.map((p) => [p.name, p])),
  } as unknown as Graph;
}

describe('renderCodeRefs', () => {
  it('returns empty string for 0 refs (section absent)', () => {
    expect(renderCodeRefs([])).toBe('');
  });

  it('renders a ref with file only', () => {
    const html = renderCodeRefs([{ file: 'src/store/write.rs' }]);
    expect(html).toContain('Code references');
    expect(html).toContain('src/store/write.rs');
    expect(html).not.toContain('::');
  });

  it('renders a ref with file and symbol', () => {
    const html = renderCodeRefs([{ file: 'src/store/write.rs', symbol: 'commit_with_graph' }]);
    expect(html).toContain('Code references');
    expect(html).toContain('src/store/write.rs');
    expect(html).toContain('::');
    expect(html).toContain('commit_with_graph');
  });

  it('renders multiple refs', () => {
    const html = renderCodeRefs([{ file: 'src/a.rs', symbol: 'foo' }, { file: 'src/b.rs' }]);
    expect(html).toContain('src/a.rs');
    expect(html).toContain('foo');
    expect(html).toContain('src/b.rs');
  });

  it('escapes XSS in file name', () => {
    const html = renderCodeRefs([{ file: '<script>alert(1)</script>' }]);
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;');
  });

  it('escapes XSS in symbol', () => {
    const html = renderCodeRefs([{ file: 'a.rs', symbol: '"><img src=x onerror=alert(1)>' }]);
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;img');
  });
});

describe('renderAttribution', () => {
  it('renders an agent badge for agent attribution', () => {
    const html = renderAttribution('agent');
    expect(html).toContain('agent');
    expect(html).toContain('unreviewed');
    expect(html).toContain('attr-badge');
  });

  it('returns empty string for user attribution (neutral)', () => {
    expect(renderAttribution('user')).toBe('');
  });

  it('returns empty string when attribution is undefined', () => {
    expect(renderAttribution(undefined)).toBe('');
  });
});

describe('renderRevisionCount', () => {
  it('returns empty string for 0 revisions', () => {
    expect(renderRevisionCount(0)).toBe('');
  });

  it('renders count for 1 revision', () => {
    const html = renderRevisionCount(1);
    expect(html).toContain('Revised');
    expect(html).toContain('1');
    expect(html).toContain('×');
  });

  it('renders count for multiple revisions', () => {
    const html = renderRevisionCount(5);
    expect(html).toContain('Revised');
    expect(html).toContain('5');
    expect(html).toContain('×');
  });

  it('returns empty string for undefined', () => {
    expect(renderRevisionCount(undefined as unknown as number)).toBe('');
  });
});

describe('decisionRow', () => {
  it('renders choice, reason, and a navigable data-dec target', () => {
    const html = decisionRow(
      mkDecision({ name: 'db-pool', choice: 'Pool of 8', reason: 'Latency' }),
    );
    expect(html).toContain('data-dec="db-pool"');
    expect(html).toContain('dec-link');
    expect(html).toContain('Pool of 8');
    expect(html).toContain('Latency');
  });

  it('renders one chip per tag, none when empty', () => {
    expect(decisionRow(mkDecision({ tags: [] }))).not.toContain('chip tag');
    const html = decisionRow(mkDecision({ tags: ['security', 'auth'] }));
    expect(html).toContain('security');
    expect(html).toContain('auth');
    expect((html.match(/chip tag/g) ?? []).length).toBe(2);
  });

  it('escapes XSS in choice, reason, and name', () => {
    const html = decisionRow(
      mkDecision({
        name: 'x',
        choice: '<script>alert(1)</script>',
        reason: '"><img src=x onerror=alert(1)>',
      }),
    );
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;');
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;img');
  });
});

describe('patternList', () => {
  it('shows a placeholder when there are no patterns', () => {
    expect(patternList(mkGraph([]))).toContain('None yet');
  });

  it('renders each pattern with its component and decision counts', () => {
    const pattern: PatternNode = {
      name: 'fail-closed',
      description: 'Validate before write',
      decisions: ['a', 'b'],
      components: ['store'],
    };
    const html = patternList(mkGraph([], [pattern]));
    expect(html).toContain('data-pattern="fail-closed"');
    expect(html).toContain('Validate before write');
    expect(html).toContain('1 components · 2 decisions');
  });

  it('falls back to the pattern name when description is empty', () => {
    const pattern: PatternNode = {
      name: 'my-pattern',
      description: '',
      decisions: [],
      components: [],
    };
    expect(patternList(mkGraph([], [pattern]))).toContain('my-pattern');
  });
});

describe('recentDecisions', () => {
  it('shows a placeholder when there are no decisions', () => {
    expect(recentDecisions(mkGraph([]))).toContain('None yet');
  });

  it('sorts newest first and caps the list at five', () => {
    const decs = Array.from({ length: 7 }, (_, i) =>
      mkDecision({ name: `d${i}`, choice: `choice ${i}`, created: `2026-01-0${i + 1}T00:00:00Z` }),
    );
    const html = recentDecisions(mkGraph(decs));
    // Newest (d6) present, oldest two (d0, d1) dropped beyond the cap of 5.
    expect(html).toContain('data-dec="d6"');
    expect(html).not.toContain('data-dec="d1"');
    expect((html.match(/dec-card/g) ?? []).length).toBe(5);
  });

  it('escapes XSS in the component name', () => {
    const html = recentDecisions(mkGraph([mkDecision({ component: '<b>x</b>' })]));
    expect(html).not.toContain('<b>x</b>');
    expect(html).toContain('&lt;b&gt;');
  });
});
