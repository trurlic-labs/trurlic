import { describe, it, expect } from 'vitest';
import { Graph } from '../state/graph';
import type { GraphSnapshot } from '../types';
import { LOD } from '../renderer/lod';
import { findHoveredEdge } from './edge-hit';

function makeGraph(edges: GraphSnapshot['edges'] = []): Graph {
  const g = new Graph();
  g.loadSnapshot({
    project: { name: 'test', description: '' },
    components: [
      {
        name: 'auth',
        description: '',
        position: { x: 0, y: 0 },
        pinned: true,
        decision_count: 0,
        pattern_count: 0,
      },
      {
        name: 'api',
        description: '',
        position: { x: 300, y: 0 },
        pinned: true,
        decision_count: 0,
        pattern_count: 0,
      },
      {
        name: 'db',
        description: '',
        position: { x: 0, y: 300 },
        pinned: true,
        decision_count: 0,
        pattern_count: 0,
      },
    ],
    decisions: [],
    patterns: [],
    edges,
    layout_version: 1,
  });
  return g;
}

describe('findHoveredEdge', () => {
  it('returns null at LOD.Overview (below Component threshold)', () => {
    const g = makeGraph([{ from: 'auth', to: 'api', kind: 'connects_to' }]);
    const hit = findHoveredEdge(g, 150, 0, 1, LOD.Overview, undefined);
    expect(hit).toBeNull();
  });

  it('returns the edge when cursor is near the midpoint', () => {
    const g = makeGraph([{ from: 'auth', to: 'api', kind: 'connects_to' }]);
    const hit = findHoveredEdge(g, 150, 0, 1, LOD.Component, undefined);
    expect(hit).not.toBeNull();
    expect(hit!.from).toBe('auth');
    expect(hit!.to).toBe('api');
    expect(hit!.kind).toBe('connects_to');
  });

  it('returns null when cursor is far from any edge', () => {
    const g = makeGraph([{ from: 'auth', to: 'api', kind: 'connects_to' }]);
    const hit = findHoveredEdge(g, 9999, 9999, 1, LOD.Component, undefined);
    expect(hit).toBeNull();
  });

  it('skips belongs_to edges', () => {
    const g = makeGraph([{ from: 'auth', to: 'api', kind: 'belongs_to' }]);
    const hit = findHoveredEdge(g, 150, 0, 1, LOD.Component, undefined);
    expect(hit).toBeNull();
  });

  it('respects edge kind filter', () => {
    const g = makeGraph([{ from: 'auth', to: 'api', kind: 'depends_on' }]);
    const filters = {
      edgeKinds: new Set(['connects_to']),
      activeTags: new Set<string>(),
      focusMode: false,
      maxAgeDays: null,
    };
    const hit = findHoveredEdge(g, 150, 0, 1, LOD.Component, filters);
    expect(hit).toBeNull();
  });

  it('selects the nearest edge when multiple overlap', () => {
    const g = makeGraph([
      { from: 'auth', to: 'api', kind: 'connects_to' },
      { from: 'auth', to: 'db', kind: 'connects_to' },
    ]);
    // Point near the auth→api edge (y=0), far from auth→db (y=300).
    const hit = findHoveredEdge(g, 150, 0, 1, LOD.Component, undefined);
    expect(hit).not.toBeNull();
    expect(hit!.to).toBe('api');
  });

  it('skips edges with missing endpoint nodes', () => {
    const g = makeGraph([{ from: 'auth', to: 'missing', kind: 'connects_to' }]);
    const hit = findHoveredEdge(g, 150, 0, 1, LOD.Component, undefined);
    expect(hit).toBeNull();
  });
});
