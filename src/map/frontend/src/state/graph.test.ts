import { describe, it, expect } from 'vitest';
import { Graph } from './graph';
import type { GraphSnapshot } from '../types';

function makeSnapshot(overrides?: Partial<GraphSnapshot>): GraphSnapshot {
  return {
    project: { name: 'test', description: '' },
    components: [
      {
        name: 'auth',
        description: 'Auth service',
        position: { x: 0, y: 0 },
        pinned: false,
        decision_count: 1,
        pattern_count: 0,
      },
      {
        name: 'api',
        description: 'API gateway',
        position: { x: 100, y: 0 },
        pinned: false,
        decision_count: 0,
        pattern_count: 0,
      },
    ],
    decisions: [
      {
        name: 'use-jwt',
        component: 'auth',
        choice: 'JWT tokens',
        reason: 'Stateless',
        tags: ['security'],
        created: '2025-01-01T00:00:00Z',
        alternatives: [],
      },
    ],
    patterns: [],
    edges: [
      { from: 'api', to: 'auth', kind: 'connects_to' },
      { from: 'api', to: 'auth', kind: 'depends_on' },
    ],
    layout_version: 1,
    ...overrides,
  };
}

describe('Graph', () => {
  describe('decisionsFor', () => {
    it('returns decisions indexed by component in O(1)', () => {
      const g = new Graph();
      g.loadSnapshot(makeSnapshot());
      const decs = g.decisionsFor('auth');
      expect(decs).toHaveLength(1);
      expect(decs[0].name).toBe('use-jwt');
    });

    it('returns frozen empty array for unknown component', () => {
      const g = new Graph();
      g.loadSnapshot(makeSnapshot());
      const decs = g.decisionsFor('nonexistent');
      expect(decs).toHaveLength(0);
      expect(Object.isFrozen(decs)).toBe(true);
    });
  });

  describe('removeNode', () => {
    it('removes component and its edges', () => {
      const g = new Graph();
      g.loadSnapshot(makeSnapshot());
      expect(g.nodes.has('auth')).toBe(true);
      expect(g.edges).toHaveLength(2);

      g.removeNode('auth');

      expect(g.nodes.has('auth')).toBe(false);
      expect(g.edges).toHaveLength(0); // both edges referenced 'auth'
    });

    it('removes decisions belonging to the deleted component', () => {
      const g = new Graph();
      g.loadSnapshot(makeSnapshot());
      expect(g.decisions.has('use-jwt')).toBe(true);

      g.removeNode('auth');

      expect(g.decisions.has('use-jwt')).toBe(false);
      expect(g.decisionsFor('auth')).toHaveLength(0);
    });

    it('preserves unrelated nodes and edges', () => {
      const g = new Graph();
      g.loadSnapshot(
        makeSnapshot({
          components: [
            {
              name: 'auth',
              description: '',
              position: null,
              pinned: false,
              decision_count: 0,
              pattern_count: 0,
            },
            {
              name: 'api',
              description: '',
              position: null,
              pinned: false,
              decision_count: 0,
              pattern_count: 0,
            },
            {
              name: 'db',
              description: '',
              position: null,
              pinned: false,
              decision_count: 0,
              pattern_count: 0,
            },
          ],
          decisions: [],
          edges: [
            { from: 'api', to: 'auth', kind: 'connects_to' },
            { from: 'api', to: 'db', kind: 'connects_to' },
          ],
        }),
      );

      g.removeNode('auth');

      expect(g.nodes.has('api')).toBe(true);
      expect(g.nodes.has('db')).toBe(true);
      expect(g.edges).toHaveLength(1);
      expect(g.edges[0].to).toBe('db');
    });

    it('is a no-op for unknown node', () => {
      const g = new Graph();
      g.loadSnapshot(makeSnapshot());
      const edgesBefore = g.edges.length;

      g.removeNode('nonexistent');

      expect(g.edges).toHaveLength(edgesBefore);
      expect(g.nodes.size).toBe(2);
    });
  });

  describe('addEdge', () => {
    it('appends a new edge to the edge list', () => {
      const g = new Graph();
      g.loadSnapshot(makeSnapshot({ edges: [] }));
      expect(g.edges).toHaveLength(0);

      g.addEdge('auth', 'api', 'connects_to');

      expect(g.edges).toHaveLength(1);
      expect(g.edges[0]).toEqual({ from: 'auth', to: 'api', kind: 'connects_to' });
    });
  });

  describe('removeEdge', () => {
    it('removes the matching edge', () => {
      const g = new Graph();
      g.loadSnapshot(makeSnapshot());
      expect(g.edges).toHaveLength(2);

      g.removeEdge('api', 'auth', 'depends_on');

      expect(g.edges).toHaveLength(1);
      expect(g.edges[0].kind).toBe('connects_to');
    });

    it('is a no-op when edge does not exist', () => {
      const g = new Graph();
      g.loadSnapshot(makeSnapshot());
      const before = g.edges.length;

      g.removeEdge('auth', 'api', 'nonexistent_kind');

      expect(g.edges).toHaveLength(before);
    });

    it('removes only the first match (handles duplicates gracefully)', () => {
      const g = new Graph();
      g.loadSnapshot(
        makeSnapshot({
          edges: [
            { from: 'a', to: 'b', kind: 'connects_to' },
            { from: 'a', to: 'b', kind: 'connects_to' },
          ],
        }),
      );

      g.removeEdge('a', 'b', 'connects_to');

      expect(g.edges).toHaveLength(1);
    });
  });

  describe('allTags', () => {
    it('returns sorted unique tags', () => {
      const g = new Graph();
      g.loadSnapshot(
        makeSnapshot({
          decisions: [
            {
              name: 'd1',
              component: 'auth',
              choice: '',
              reason: '',
              tags: ['beta', 'alpha'],
              created: '',
              alternatives: [],
            },
            {
              name: 'd2',
              component: 'auth',
              choice: '',
              reason: '',
              tags: ['beta', 'gamma'],
              created: '',
              alternatives: [],
            },
          ],
        }),
      );

      expect(g.allTags()).toEqual(['alpha', 'beta', 'gamma']);
    });
  });
});
