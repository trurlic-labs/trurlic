import { describe, it, expect } from 'vitest';
import { HoverTracker } from './hover';
import { pointSegDistSq } from '../renderer/geometry';

describe('HoverTracker', () => {
  it('starts with nothing hovered', () => {
    const h = new HoverTracker();
    expect(h.node).toBeNull();
    expect(h.edge).toBeNull();
    expect(h.borderAlpha).toBe(0);
    expect(h.tooltipVisible).toBe(false);
  });

  it('update with a node returns changed', () => {
    const h = new HoverTracker();
    const changed = h.update('auth', 'JWT authentication', null, 100, 200, 1000);
    expect(changed).toBe(true);
    expect(h.node).toBe('auth');
    expect(h.tooltipText).toBe('JWT authentication');
  });

  it('update with the same node returns unchanged', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', null, 100, 200, 1000);
    const changed = h.update('auth', 'desc', null, 110, 210, 1010);
    expect(changed).toBe(false);
  });

  it('update with null clears node state', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', null, 100, 200, 1000);
    const changed = h.update(null, '', null, 150, 250, 1050);
    expect(changed).toBe(true);
    expect(h.node).toBeNull();
    expect(h.tooltipText).toBe('');
  });

  it('update truncates long descriptions', () => {
    const h = new HoverTracker();
    const long = 'A'.repeat(120);
    h.update('auth', long, null, 0, 0, 0);
    expect(h.tooltipText.length).toBe(80);
    expect(h.tooltipText.endsWith('…')).toBe(true);
  });

  // ── Border alpha ramp ─────────────────────────────────────────────

  it('tick ramps borderAlpha over 100ms', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', null, 0, 0, 1000);

    h.tick(1000); // t=0 → alpha=0
    expect(h.borderAlpha).toBe(0);

    h.tick(1050); // t=50 → alpha=0.5
    expect(h.borderAlpha).toBeCloseTo(0.5);

    h.tick(1100); // t=100 → alpha=1
    expect(h.borderAlpha).toBe(1);

    // Stays at 1 beyond 100ms.
    h.tick(1200);
    expect(h.borderAlpha).toBe(1);
  });

  // ── Tooltip dwell ─────────────────────────────────────────────────

  it('tooltip becomes visible after 400ms dwell', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', null, 0, 0, 1000);

    h.tick(1200);
    expect(h.tooltipVisible).toBe(false);

    h.tick(1400);
    expect(h.tooltipVisible).toBe(true);
  });

  it('tooltip resets when hovering a new node', () => {
    const h = new HoverTracker();
    h.update('auth', 'a desc', null, 0, 0, 1000);
    h.tick(1500); // tooltip visible
    expect(h.tooltipVisible).toBe(true);

    h.update('database', 'b desc', null, 50, 50, 1500);
    expect(h.tooltipVisible).toBe(false);
    expect(h.borderAlpha).toBe(0);
  });

  // ── Tick with no hover ────────────────────────────────────────────

  it('tick with no hover is a noop', () => {
    const h = new HoverTracker();
    const changed = h.tick(1000);
    expect(changed).toBe(false);
  });

  it('update(null) eagerly clears borderAlpha', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', null, 0, 0, 1000);
    h.tick(1100); // alpha=1
    expect(h.borderAlpha).toBe(1);

    h.update(null, '', null, 0, 0, 1200);
    // update() resets alpha immediately — no stale frame.
    expect(h.borderAlpha).toBe(0);

    // Subsequent tick is a no-op (already clean).
    const changed = h.tick(1200);
    expect(changed).toBe(false);
  });

  // ── Edge hover ────────────────────────────────────────────────────

  it('edge hover activates when no node is hovered', () => {
    const h = new HoverTracker();
    const edge = { from: 'auth', to: 'db', kind: 'connects_to' };
    const changed = h.update(null, '', edge, 0, 0, 0);
    expect(changed).toBe(true);
    expect(h.edge).toEqual(edge);
    expect(h.edgeTooltipText).toBe('auth → db');
  });

  it('edge tooltip text clears when edge is deselected', () => {
    const h = new HoverTracker();
    const edge = { from: 'auth', to: 'db', kind: 'connects_to' };
    h.update(null, '', edge, 0, 0, 0);
    expect(h.edgeTooltipText).toBe('auth → db');
    h.update(null, '', null, 0, 0, 0);
    expect(h.edgeTooltipText).toBe('');
  });

  it('edge is suppressed when a node is hovered', () => {
    const h = new HoverTracker();
    const edge = { from: 'auth', to: 'db', kind: 'connects_to' };
    h.update('auth', 'desc', edge, 0, 0, 0);
    expect(h.edge).toBeNull();
    expect(h.edgeTooltipText).toBe('');
  });

  // ── Clear ─────────────────────────────────────────────────────────

  it('clear resets all state', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', { from: 'a', to: 'b', kind: 'c' }, 100, 200, 1000);
    h.tick(1500);
    h.clear();
    expect(h.node).toBeNull();
    expect(h.edge).toBeNull();
    expect(h.borderAlpha).toBe(0);
    expect(h.tooltipVisible).toBe(false);
  });
});

describe('pointSegDistSq', () => {
  it('returns 0 for a point on the segment', () => {
    expect(pointSegDistSq(5, 5, 0, 0, 10, 10)).toBeCloseTo(0);
  });

  it('returns squared distance for a point off the segment', () => {
    // Point (0, 5), segment from (0,0) to (10,0).
    // Nearest point is (0,0)... wait no, nearest point on segment is (0,0).
    // Actually, perpendicular from (0,5) to horizontal line y=0 hits (0,0).
    // Distance = 5, squared = 25.
    expect(pointSegDistSq(0, 5, 0, 0, 10, 0)).toBeCloseTo(25);
  });

  it('clamps to segment endpoints', () => {
    // Point far beyond endpoint B.
    // Segment (0,0)→(10,0), point (20,0). Nearest = (10,0), dist = 10.
    expect(pointSegDistSq(20, 0, 0, 0, 10, 0)).toBeCloseTo(100);
  });

  it('handles zero-length segment', () => {
    // Degenerate segment: both endpoints at (5,5). Distance to (8,5) = 3.
    expect(pointSegDistSq(8, 5, 5, 5, 5, 5)).toBeCloseTo(9);
  });

  it('midpoint perpendicular distance', () => {
    // Segment (0,0)→(10,0), point (5,3). Nearest = (5,0), dist = 3.
    expect(pointSegDistSq(5, 3, 0, 0, 10, 0)).toBeCloseTo(9);
  });
});
