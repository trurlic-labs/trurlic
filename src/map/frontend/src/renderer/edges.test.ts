import { describe, it, expect } from 'vitest';
import { edgeColor, edgeCurveCP, buildEdgePairSet, EDGE_OPACITY } from './edges';
import type { ColorSnapshot, RenderEdge } from '../types';

/** Minimal color snapshot stub — only the fields edgeColor reads. */
const COLORS = {
  edge: '#3a3f52',
  edgeDep: '#5a7f5a',
  edgeCon: '#8f6c3a',
  edgeSup: '#7a5a7a',
} as ColorSnapshot;

describe('edgeColor', () => {
  it('returns edge-dep color for depends_on', () => {
    expect(edgeColor('depends_on', COLORS)).toBe('#5a7f5a');
  });

  it('returns edge-con color for constrains', () => {
    expect(edgeColor('constrains', COLORS)).toBe('#8f6c3a');
  });

  it('returns edge-sup color for supersedes', () => {
    expect(edgeColor('supersedes', COLORS)).toBe('#7a5a7a');
  });

  it('returns default edge color for connects_to and unknown kinds', () => {
    expect(edgeColor('connects_to', COLORS)).toBe('#3a3f52');
    expect(edgeColor('unknown_kind', COLORS)).toBe('#3a3f52');
  });
});

describe('EDGE_OPACITY', () => {
  it('primary connections are fully opaque', () => {
    expect(EDGE_OPACITY['connects_to']).toBe(1.0);
  });

  it('secondary connections are lower opacity', () => {
    expect(EDGE_OPACITY['depends_on']).toBeLessThan(1.0);
    expect(EDGE_OPACITY['constrains']).toBeLessThan(EDGE_OPACITY['depends_on']);
    expect(EDGE_OPACITY['supersedes']).toBeLessThan(EDGE_OPACITY['constrains']);
  });
});

describe('edgeCurveCP', () => {
  it('returns the midpoint for a horizontal edge with zero zoom offset', () => {
    // At infinite zoom the offset is negligible.
    const { cpx, cpy } = edgeCurveCP(0, 0, 100, 0, 10000, false);
    expect(cpx).toBeCloseTo(50, 0);
    expect(cpy).toBeCloseTo(0, 0);
  });

  it('offsets perpendicular to the edge direction', () => {
    // Horizontal edge: perpendicular is vertical.
    const { cpx, cpy } = edgeCurveCP(0, 0, 200, 0, 1, false);
    expect(cpx).toBeCloseTo(100, 0); // midpoint X
    expect(cpy).not.toBeCloseTo(0); // offset in Y
  });

  it('reverses the offset for bidirectional pairs', () => {
    const fwd = edgeCurveCP(0, 0, 200, 0, 1, false);
    const rev = edgeCurveCP(0, 0, 200, 0, 1, true);
    // Same X midpoint, opposite Y offsets.
    expect(fwd.cpx).toBeCloseTo(rev.cpx, 5);
    expect(Math.sign(fwd.cpy)).toBe(-Math.sign(rev.cpy));
  });

  it('handles degenerate zero-length edge gracefully', () => {
    const { cpx, cpy } = edgeCurveCP(50, 50, 50, 50, 1, false);
    expect(cpx).toBe(50);
    expect(cpy).toBe(50);
  });
});

describe('buildEdgePairSet', () => {
  it('includes rendered edge kinds', () => {
    const edges: RenderEdge[] = [
      { from: 'a', to: 'b', kind: 'connects_to' },
      { from: 'b', to: 'c', kind: 'depends_on' },
    ];
    const set = buildEdgePairSet(edges);
    expect(set.has('a\0b')).toBe(true);
    expect(set.has('b\0c')).toBe(true);
  });

  it('excludes belongs_to edges', () => {
    const edges: RenderEdge[] = [{ from: 'x', to: 'y', kind: 'belongs_to' }];
    const set = buildEdgePairSet(edges);
    expect(set.size).toBe(0);
  });

  it('enables O(1) bidirectional detection', () => {
    const edges: RenderEdge[] = [
      { from: 'a', to: 'b', kind: 'connects_to' },
      { from: 'b', to: 'a', kind: 'connects_to' },
    ];
    const set = buildEdgePairSet(edges);
    // For edge a→b, check if reverse b→a exists.
    expect(set.has('b\0a')).toBe(true);
  });
});
