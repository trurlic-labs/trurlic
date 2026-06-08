import { describe, it, expect } from 'vitest';
import { ForceLayout } from './layout';
import type { RenderNode, RenderEdge } from './types';

function node(name: string, x: number, y: number, pinned = false): RenderNode {
  return { name, kind: 'component', x, y, w: 180, h: 60, pinned };
}

function dist(a: RenderNode, b: RenderNode): number {
  return Math.sqrt((a.x - b.x) ** 2 + (a.y - b.y) ** 2);
}

describe('ForceLayout', () => {
  it('pinned nodes do not move', () => {
    const layout = new ForceLayout();
    const nodes = new Map([
      ['fixed', node('fixed', 100, 200, true)],
      ['free', node('free', 100, 200, false)],
    ]);

    layout.run(nodes, [], 100);

    const fixed = nodes.get('fixed')!;
    expect(fixed.x).toBe(100);
    expect(fixed.y).toBe(200);
  });

  it('repulsion separates nearby nodes', () => {
    const layout = new ForceLayout();
    // Slightly offset — exact overlap produces zero direction vector.
    const nodes = new Map([
      ['a', node('a', 1, 0)],
      ['b', node('b', -1, 0)],
    ]);

    layout.run(nodes, [], 200);

    expect(dist(nodes.get('a')!, nodes.get('b')!)).toBeGreaterThan(50);
  });

  it('springs reduce distance between far-apart connected nodes', () => {
    const layout = new ForceLayout();
    const nodes = new Map([
      ['a', node('a', -400, 0)],
      ['b', node('b', 400, 0)],
    ]);
    const edges: RenderEdge[] = [{ from: 'a', to: 'b', kind: 'connects_to' }];

    const distBefore = dist(nodes.get('a')!, nodes.get('b')!); // 800

    layout.run(nodes, edges, 300);

    const distAfter = dist(nodes.get('a')!, nodes.get('b')!);
    // Spring rest length is 250. Starting at 800, the spring should
    // pull them closer. Gravity also pulls both toward center.
    expect(distAfter).toBeLessThan(distBefore);
  });

  it('gravity pulls nodes toward center', () => {
    const layout = new ForceLayout();
    const nodes = new Map([
      ['a', node('a', -500, 0)],
      ['b', node('b', 500, 0)],
    ]);

    layout.run(nodes, [], 300);

    const a = nodes.get('a')!;
    const b = nodes.get('b')!;
    expect(Math.abs(a.x)).toBeLessThan(500);
    expect(Math.abs(b.x)).toBeLessThan(500);
  });
});
