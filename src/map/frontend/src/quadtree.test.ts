import { describe, it, expect } from 'vitest';
import { Quadtree } from './quadtree';
import type { RenderNode } from './types';

function node(name: string, x: number, y: number, w = 180, h = 60): RenderNode {
  return { name, kind: 'component', x, y, w, h, pinned: false };
}

describe('Quadtree', () => {
  // ── Build + viewport query ────────────────────────────────────────

  it('returns nothing from an empty tree', () => {
    const qt = new Quadtree();
    qt.build(new Map());
    expect(qt.queryViewport({ cx: 0, cy: 0, hw: 1000, hh: 1000 })).toEqual([]);
  });

  it('returns a node inside the viewport', () => {
    const qt = new Quadtree();
    const nodes = new Map([['auth', node('auth', 100, 100)]]);
    qt.build(nodes);

    const results = qt.queryViewport({ cx: 100, cy: 100, hw: 200, hh: 200 });
    expect(results).toContain('auth');
  });

  it('excludes a node outside the viewport', () => {
    const qt = new Quadtree();
    const nodes = new Map([
      ['near', node('near', 0, 0)],
      ['far', node('far', 5000, 5000)],
    ]);
    qt.build(nodes);

    // Small viewport around origin — should only see 'near'.
    const results = qt.queryViewport({ cx: 0, cy: 0, hw: 200, hh: 200 });
    expect(results).toContain('near');
    expect(results).not.toContain('far');
  });

  it('includes a node partially overlapping the viewport edge', () => {
    const qt = new Quadtree();
    // Node at (190, 0) with half-width 90 → left edge at 100.
    // Viewport right edge at 105. Overlap = 5px.
    const nodes = new Map([['edge', node('edge', 190, 0)]]);
    qt.build(nodes);

    const results = qt.queryViewport({ cx: 0, cy: 0, hw: 105, hh: 100 });
    expect(results).toContain('edge');
  });

  it('deduplicates nodes near cell boundaries', () => {
    const qt = new Quadtree();
    // Place a node at the origin — it will sit near the center of the
    // root cell and might end up in multiple children after subdivision.
    const nodes = new Map<string, RenderNode>();
    // Add enough nodes to force subdivision, with one at the boundary.
    for (let i = 0; i < 20; i++) {
      nodes.set(`n${i}`, node(`n${i}`, i * 50, i * 50));
    }
    qt.build(nodes);

    // Query the entire space.
    const results = qt.queryViewport({ cx: 500, cy: 500, hw: 2000, hh: 2000 });
    // Every name must appear exactly once.
    const unique = new Set(results);
    expect(unique.size).toBe(results.length);
    expect(unique.size).toBe(20);
  });

  // ── Hit detection ─────────────────────────────────────────────────

  it('hit test returns the node at a point', () => {
    const qt = new Quadtree();
    const nodes = new Map([
      ['auth', node('auth', 0, 0)],
      ['db', node('db', 400, 400)],
    ]);
    qt.build(nodes);

    expect(qt.hitTest(0, 0)).toBe('auth');
    expect(qt.hitTest(400, 400)).toBe('db');
  });

  it('hit test returns null for empty space', () => {
    const qt = new Quadtree();
    const nodes = new Map([['auth', node('auth', 0, 0)]]);
    qt.build(nodes);

    // Point far from any node.
    expect(qt.hitTest(9999, 9999)).toBeNull();
  });

  it('hit test returns null for empty tree', () => {
    const qt = new Quadtree();
    qt.build(new Map());
    expect(qt.hitTest(0, 0)).toBeNull();
  });

  it('hit test respects node bounds, not just center', () => {
    const qt = new Quadtree();
    // Node at (0,0) with w=180, h=60 → bounds [-90,-30] to [90,30].
    const nodes = new Map([['auth', node('auth', 0, 0, 180, 60)]]);
    qt.build(nodes);

    // Inside bounds but not at center.
    expect(qt.hitTest(80, 25)).toBe('auth');
    // Outside bounds.
    expect(qt.hitTest(100, 0)).toBeNull();
  });

  // ── Scale ─────────────────────────────────────────────────────────

  it('handles 1000 nodes without error', () => {
    const qt = new Quadtree();
    const nodes = new Map<string, RenderNode>();
    for (let i = 0; i < 1000; i++) {
      const x = (i % 50) * 200;
      const y = Math.floor(i / 50) * 200;
      nodes.set(`n${i}`, node(`n${i}`, x, y));
    }
    qt.build(nodes);

    // Viewport covering roughly a quarter of the grid.
    const results = qt.queryViewport({ cx: 2500, cy: 1000, hw: 2500, hh: 1000 });
    // Should return a subset, not all 1000.
    expect(results.length).toBeGreaterThan(0);
    expect(results.length).toBeLessThan(1000);
  });

  // ── Rebuild ───────────────────────────────────────────────────────

  it('rebuild reflects moved nodes', () => {
    const qt = new Quadtree();
    const auth = node('auth', 0, 0);
    const nodes = new Map([['auth', auth]]);
    qt.build(nodes);

    expect(qt.hitTest(0, 0)).toBe('auth');

    // Move the node and rebuild.
    auth.x = 500;
    auth.y = 500;
    qt.build(nodes);

    expect(qt.hitTest(0, 0)).toBeNull();
    expect(qt.hitTest(500, 500)).toBe('auth');
  });
});
