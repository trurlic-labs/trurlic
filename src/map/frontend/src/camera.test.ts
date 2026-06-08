import { describe, it, expect } from 'vitest';
import { Camera } from './camera';

function cam(w = 1920, h = 1080): Camera {
  const c = new Camera();
  c.screenW = w;
  c.screenH = h;
  return c;
}

describe('Camera', () => {
  // ── Coordinate transforms ─────────────────────────────────────────

  it('world→screen→world round-trips', () => {
    const c = cam();
    c.cx = 200;
    c.cy = -100;
    c.zoom = 1.5;

    const wx = 350;
    const wy = 80;
    const sx = c.toScreenX(wx);
    const sy = c.toScreenY(wy);
    const wx2 = c.toWorldX(sx);
    const wy2 = c.toWorldY(sy);

    expect(wx2).toBeCloseTo(wx, 10);
    expect(wy2).toBeCloseTo(wy, 10);
  });

  it('screen center maps to world center', () => {
    const c = cam();
    c.cx = 42;
    c.cy = -17;
    // Screen center = (screenW/2, screenH/2).
    const wx = c.toWorldX(c.screenW / 2);
    const wy = c.toWorldY(c.screenH / 2);
    expect(wx).toBeCloseTo(42, 10);
    expect(wy).toBeCloseTo(-17, 10);
  });

  // ── Pan ───────────────────────────────────────────────────────────

  it('pan shifts the camera center', () => {
    const c = cam();
    const before = { cx: c.cx, cy: c.cy };
    c.pan(100, 0); // pan right → center moves left in world
    expect(c.cx).toBeLessThan(before.cx);
    expect(c.cy).toBeCloseTo(before.cy, 10);
  });

  it('pan by zero is a noop', () => {
    const c = cam();
    c.cx = 10;
    c.cy = 20;
    c.pan(0, 0);
    expect(c.cx).toBe(10);
    expect(c.cy).toBe(20);
  });

  // ── Zoom ──────────────────────────────────────────────────────────

  it('zoomAt preserves the world point under the cursor', () => {
    const c = cam();
    c.cx = 100;
    c.cy = 50;
    c.zoom = 1;

    // Pick an arbitrary screen point.
    const sx = 600;
    const sy = 400;
    const wxBefore = c.toWorldX(sx);
    const wyBefore = c.toWorldY(sy);

    c.zoomAt(sx, sy, 2.0);

    const wxAfter = c.toWorldX(sx);
    const wyAfter = c.toWorldY(sy);

    // THE critical invariant: the world point under the cursor
    // must not move after zoom.
    expect(wxAfter).toBeCloseTo(wxBefore, 8);
    expect(wyAfter).toBeCloseTo(wyBefore, 8);
  });

  it('zoom is clamped to [minZoom, maxZoom]', () => {
    const c = cam();
    c.zoomAt(0, 0, 0.001); // try to zoom way out
    expect(c.zoom).toBeGreaterThanOrEqual(0.05);

    c.zoom = 1;
    c.zoomAt(0, 0, 100); // try to zoom way in
    expect(c.zoom).toBeLessThanOrEqual(8);
  });

  // ── fitBounds ─────────────────────────────────────────────────────

  it('fitBounds centers the camera on the given rectangle', () => {
    const c = cam();
    c.fitBounds(-500, -300, 500, 300);
    // After animation completes, center should be at the midpoint.
    // fitBounds now animates, so run the animation to completion.
    while (c.tick()) {
      /* advance */
    }
    expect(c.cx).toBeCloseTo(0, 5);
    expect(c.cy).toBeCloseTo(0, 5);
  });

  it('fitBounds sets zoom so the bounds fit on screen', () => {
    const c = cam(1000, 1000);
    c.fitBounds(0, 0, 2000, 2000); // 2000x2000 box + padding
    while (c.tick()) {
      /* advance */
    }
    // The viewport should contain the bounds.
    const vp = c.viewport();
    expect(vp.x).toBeLessThanOrEqual(0);
    expect(vp.y).toBeLessThanOrEqual(0);
    expect(vp.x + vp.w).toBeGreaterThanOrEqual(2000);
    expect(vp.y + vp.h).toBeGreaterThanOrEqual(2000);
  });

  // ── Animation ─────────────────────────────────────────────────────

  it('animateTo reaches the target after completion', () => {
    const c = cam();
    c.cx = 0;
    c.cy = 0;
    c.zoom = 1;

    c.animateTo(500, 300, 2.0, 50);

    // Run to completion — tick() returns false when done.
    while (c.tick()) {
      /* advance */
    }

    expect(c.cx).toBeCloseTo(500, 5);
    expect(c.cy).toBeCloseTo(300, 5);
    expect(c.zoom).toBeCloseTo(2.0, 5);
  });

  it('tick returns false when no animation is running', () => {
    const c = cam();
    expect(c.tick()).toBe(false);
  });

  it('user pan cancels a running animation', () => {
    const c = cam();
    c.animateTo(999, 999, 3, 1000);
    // One tick to start.
    c.tick();
    // User pans — should cancel.
    c.pan(10, 10);
    expect(c.tick()).toBe(false); // animation gone
    // Camera should NOT be at the target.
    expect(c.cx).not.toBeCloseTo(999, 0);
  });

  // ── Viewport ──────────────────────────────────────────────────────

  it('viewport dimensions scale inversely with zoom', () => {
    const c = cam(1000, 1000);
    c.zoom = 1;
    const vp1 = c.viewport();

    c.zoom = 2;
    const vp2 = c.viewport();

    expect(vp2.w).toBeCloseTo(vp1.w / 2, 5);
    expect(vp2.h).toBeCloseTo(vp1.h / 2, 5);
  });
});
