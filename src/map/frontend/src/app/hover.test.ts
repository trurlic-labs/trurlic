import { describe, it, expect } from 'vitest';
import { HoverTracker } from './hover';

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
    const changed = h.update('auth', 'JWT authentication', null, '', null, 100, 200, 1000);
    expect(changed).toBe(true);
    expect(h.node).toBe('auth');
    expect(h.tooltipText).toBe('JWT authentication');
  });

  it('update with the same node returns unchanged', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', null, '', null, 100, 200, 1000);
    const changed = h.update('auth', 'desc', null, '', null, 110, 210, 1010);
    expect(changed).toBe(false);
  });

  it('update with null clears node state', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', null, '', null, 100, 200, 1000);
    const changed = h.update(null, '', null, '', null, 150, 250, 1050);
    expect(changed).toBe(true);
    expect(h.node).toBeNull();
    expect(h.tooltipText).toBe('');
  });

  it('update truncates long descriptions', () => {
    const h = new HoverTracker();
    const long = 'A'.repeat(120);
    h.update('auth', long, null, '', null, 0, 0, 0);
    expect(h.tooltipText.length).toBe(80);
    expect(h.tooltipText.endsWith('…')).toBe(true);
  });

  // ── Border alpha ramp ─────────────────────────────────────────────

  it('tick ramps borderAlpha over 100ms', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', null, '', null, 0, 0, 1000);

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
    h.update('auth', 'desc', null, '', null, 0, 0, 1000);

    h.tick(1200);
    expect(h.tooltipVisible).toBe(false);

    h.tick(1400);
    expect(h.tooltipVisible).toBe(true);
  });

  it('tooltip resets when hovering a new node', () => {
    const h = new HoverTracker();
    h.update('auth', 'a desc', null, '', null, 0, 0, 1000);
    h.tick(1500); // tooltip visible
    expect(h.tooltipVisible).toBe(true);

    h.update('database', 'b desc', null, '', null, 50, 50, 1500);
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
    h.update('auth', 'desc', null, '', null, 0, 0, 1000);
    h.tick(1100); // alpha=1
    expect(h.borderAlpha).toBe(1);

    h.update(null, '', null, '', null, 0, 0, 1200);
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
    const changed = h.update(null, '', null, '', edge, 0, 0, 0);
    expect(changed).toBe(true);
    expect(h.edge).toEqual(edge);
    expect(h.edgeTooltipText).toBe('auth → db');
  });

  it('edge tooltip text clears when edge is deselected', () => {
    const h = new HoverTracker();
    const edge = { from: 'auth', to: 'db', kind: 'connects_to' };
    h.update(null, '', null, '', edge, 0, 0, 0);
    expect(h.edgeTooltipText).toBe('auth → db');
    h.update(null, '', null, '', null, 0, 0, 0);
    expect(h.edgeTooltipText).toBe('');
  });

  it('edge is suppressed when a node is hovered', () => {
    const h = new HoverTracker();
    const edge = { from: 'auth', to: 'db', kind: 'connects_to' };
    h.update('auth', 'desc', null, '', edge, 0, 0, 0);
    expect(h.edge).toBeNull();
    expect(h.edgeTooltipText).toBe('');
  });

  it('edge is suppressed when a pattern is hovered', () => {
    const h = new HoverTracker();
    const edge = { from: 'auth', to: 'db', kind: 'connects_to' };
    h.update(null, '', 'fail-closed', 'All mutations validate', edge, 0, 0, 0);
    expect(h.edge).toBeNull();
    expect(h.pattern).toBe('fail-closed');
  });

  // ── Pattern hover ─────────────────────────────────────────────────

  it('pattern hover activates when no node is hovered', () => {
    const h = new HoverTracker();
    const changed = h.update(null, '', 'fail-closed', 'All mutations validate', null, 0, 0, 0);
    expect(changed).toBe(true);
    expect(h.pattern).toBe('fail-closed');
    expect(h.patternDesc).toBe('All mutations validate');
  });

  it('pattern is suppressed when a node is hovered', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', 'fail-closed', 'All mutations validate', null, 0, 0, 0);
    expect(h.pattern).toBeNull();
    expect(h.node).toBe('auth');
  });

  it('pattern tooltip appears after dwell', () => {
    const h = new HoverTracker();
    h.update(null, '', 'fail-closed', 'All mutations validate', null, 0, 0, 1000);
    h.tick(1200);
    expect(h.tooltipVisible).toBe(false);
    h.tick(1400);
    expect(h.tooltipVisible).toBe(true);
  });

  it('tick with pattern only: borderAlpha stays 0, tooltip appears after dwell', () => {
    const h = new HoverTracker();
    h.update(null, '', 'fail-closed', 'All mutations validate', null, 0, 0, 1000);
    h.tick(1050);
    expect(h.borderAlpha).toBe(0);
    expect(h.tooltipVisible).toBe(false);
    h.tick(1400);
    expect(h.borderAlpha).toBe(0);
    expect(h.tooltipVisible).toBe(true);
  });

  // ── Clear ─────────────────────────────────────────────────────────

  it('clear resets all state', () => {
    const h = new HoverTracker();
    h.update('auth', 'desc', null, '', { from: 'a', to: 'b', kind: 'c' }, 100, 200, 1000);
    h.tick(1500);
    h.clear();
    expect(h.node).toBeNull();
    expect(h.pattern).toBeNull();
    expect(h.edge).toBeNull();
    expect(h.borderAlpha).toBe(0);
    expect(h.tooltipVisible).toBe(false);
  });
});
