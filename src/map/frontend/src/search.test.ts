import { describe, it, expect } from 'vitest';
import { search, neighborhood } from './search';
import { Graph } from './graph';
import type { GraphSnapshot } from './types';

/** A realistic graph snapshot: 3 components, 3 decisions, 1 pattern. */
function testSnapshot(): GraphSnapshot {
  return {
    project: { name: 'test-project', description: 'A test' },
    components: [
      {
        name: 'auth',
        description: 'JWT authentication',
        position: null,
        pinned: false,
        decision_count: 2,
        pattern_count: 1,
      },
      {
        name: 'database',
        description: 'PostgreSQL storage',
        position: null,
        pinned: false,
        decision_count: 1,
        pattern_count: 0,
      },
      {
        name: 'rate-limiter',
        description: 'Per-key rate limiting',
        position: null,
        pinned: false,
        decision_count: 0,
        pattern_count: 0,
      },
    ],
    decisions: [
      {
        name: 'use-jwt',
        component: 'auth',
        choice: 'JWT with DPoP binding',
        reason: 'Stateless, no session store',
        tags: ['security', 'auth'],
        created: '2025-01-15T10:00:00Z',
        alternatives: ['Session cookies'],
      },
      {
        name: 'token-expiry',
        component: 'auth',
        choice: '15 minute token expiry',
        reason: 'Short-lived reduces theft window',
        tags: ['security'],
        created: '2025-01-15T11:00:00Z',
        alternatives: [],
      },
      {
        name: 'db-pool',
        component: 'database',
        choice: 'Connection pool via deadpool',
        reason: 'Async-native, configurable',
        tags: ['performance'],
        created: '2025-01-16T09:00:00Z',
        alternatives: ['r2d2'],
      },
    ],
    patterns: [
      {
        name: 'stateless-auth',
        description: 'All auth is stateless via JWT tokens',
        decisions: ['use-jwt', 'token-expiry'],
        components: ['auth'],
      },
    ],
    edges: [
      { from: 'auth', to: 'database', kind: 'connects_to' },
      { from: 'rate-limiter', to: 'database', kind: 'connects_to' },
      { from: 'use-jwt', to: 'auth', kind: 'belongs_to' },
      { from: 'token-expiry', to: 'auth', kind: 'belongs_to' },
      { from: 'db-pool', to: 'database', kind: 'belongs_to' },
      { from: 'token-expiry', to: 'use-jwt', kind: 'depends_on' },
    ],
    layout_version: 1,
  };
}

function testGraph(): Graph {
  const g = new Graph();
  g.loadSnapshot(testSnapshot());
  return g;
}

describe('search', () => {
  it('finds a component by name', () => {
    const results = search(testGraph(), 'auth');
    expect(results.length).toBeGreaterThan(0);
    expect(results.some((r) => r.name === 'auth' && r.kind === 'component')).toBe(true);
  });

  it('finds a decision by choice text', () => {
    const results = search(testGraph(), 'JWT DPoP');
    expect(results.some((r) => r.name === 'use-jwt' && r.kind === 'decision')).toBe(true);
  });

  it('finds a decision by tag', () => {
    const results = search(testGraph(), 'security');
    const decisionResults = results.filter((r) => r.kind === 'decision');
    expect(decisionResults.length).toBeGreaterThanOrEqual(2); // use-jwt + token-expiry
  });

  it('finds a pattern by description', () => {
    const results = search(testGraph(), 'stateless tokens');
    expect(results.some((r) => r.kind === 'pattern')).toBe(true);
  });

  it('is case insensitive', () => {
    const lower = search(testGraph(), 'jwt');
    const upper = search(testGraph(), 'JWT');
    expect(lower.length).toBe(upper.length);
    expect(lower.map((r) => r.name).sort()).toEqual(upper.map((r) => r.name).sort());
  });

  it('ranks multi-token matches higher', () => {
    const results = search(testGraph(), 'JWT stateless session');
    // use-jwt matches all three tokens in its choice+reason; db-pool matches none.
    const jwtIdx = results.findIndex((r) => r.name === 'use-jwt');
    const dbIdx = results.findIndex((r) => r.name === 'db-pool');
    if (dbIdx >= 0) {
      expect(jwtIdx).toBeLessThan(dbIdx);
    }
  });

  it('returns at most 10 results', () => {
    // Even with a broad query that matches everything.
    const results = search(testGraph(), 'auth database pool JWT token rate');
    expect(results.length).toBeLessThanOrEqual(10);
  });

  it('returns empty for blank query', () => {
    expect(search(testGraph(), '')).toEqual([]);
    expect(search(testGraph(), '   ')).toEqual([]);
  });

  it('returns empty for very short tokens', () => {
    // Single-char tokens are filtered (min token length = 2).
    expect(search(testGraph(), 'a b c')).toEqual([]);
  });

  it('returns empty when nothing matches', () => {
    expect(search(testGraph(), 'zzzzz xylophone')).toEqual([]);
  });
});

describe('neighborhood', () => {
  it('includes the center node', () => {
    const g = testGraph();
    const n = neighborhood(g, 'auth');
    expect(n.has('auth')).toBe(true);
  });

  it('includes directly connected components', () => {
    const g = testGraph();
    // auth connects_to database.
    const n = neighborhood(g, 'auth');
    expect(n.has('database')).toBe(true);
  });

  it('includes reverse-connected components', () => {
    const g = testGraph();
    // rate-limiter connects_to database, so database's neighborhood
    // includes rate-limiter.
    const n = neighborhood(g, 'database');
    expect(n.has('rate-limiter')).toBe(true);
    expect(n.has('auth')).toBe(true);
  });

  it('includes decisions of neighboring components', () => {
    const g = testGraph();
    const n = neighborhood(g, 'auth');
    // auth's own decisions.
    expect(n.has('use-jwt')).toBe(true);
    expect(n.has('token-expiry')).toBe(true);
    // database is a neighbor — its decisions should be included.
    expect(n.has('db-pool')).toBe(true);
  });

  it('does not include unconnected components', () => {
    const g = testGraph();
    // rate-limiter connects to database but not directly to auth.
    // However, auth → database → rate-limiter, so rate-limiter IS
    // a neighbor of database, and database IS a neighbor of auth.
    // rate-limiter should NOT appear in auth's 1-hop neighborhood.
    const n = neighborhood(g, 'auth');
    expect(n.has('rate-limiter')).toBe(false);
  });

  it('works for a decision name (includes parent component)', () => {
    const g = testGraph();
    const n = neighborhood(g, 'use-jwt');
    expect(n.has('use-jwt')).toBe(true);
    expect(n.has('auth')).toBe(true); // parent component
  });
});
