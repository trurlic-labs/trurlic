import { describe, it, expect } from 'vitest';
import {
  convexHull,
  expandHull,
  cross,
  nodeCorners,
  rayRectIntersect,
  pointInConvexPoly,
  pointBezierDistSq,
  pointSegDistSq,
} from './geometry';
import type { Point } from './geometry';

describe('cross', () => {
  it('returns positive for left turn', () => {
    expect(cross({ x: 0, y: 0 }, { x: 1, y: 0 }, { x: 1, y: 1 })).toBeGreaterThan(0);
  });

  it('returns negative for right turn', () => {
    expect(cross({ x: 0, y: 0 }, { x: 1, y: 0 }, { x: 1, y: -1 })).toBeLessThan(0);
  });

  it('returns 0 for collinear points', () => {
    expect(cross({ x: 0, y: 0 }, { x: 1, y: 1 }, { x: 2, y: 2 })).toBe(0);
  });
});

describe('convexHull', () => {
  it('returns a triangle for 3 non-collinear points', () => {
    const pts: Point[] = [
      { x: 0, y: 0 },
      { x: 4, y: 0 },
      { x: 2, y: 3 },
    ];
    const hull = convexHull(pts);
    expect(hull).toHaveLength(3);
  });

  it('excludes interior points', () => {
    const pts: Point[] = [
      { x: 0, y: 0 },
      { x: 4, y: 0 },
      { x: 4, y: 4 },
      { x: 0, y: 4 },
      { x: 2, y: 2 }, // interior
    ];
    const hull = convexHull(pts);
    expect(hull).toHaveLength(4);
    const names = hull.map((p) => `${p.x},${p.y}`);
    expect(names).not.toContain('2,2');
  });

  it('returns points for fewer than 3 inputs', () => {
    expect(convexHull([{ x: 0, y: 0 }])).toHaveLength(1);
    expect(
      convexHull([
        { x: 0, y: 0 },
        { x: 1, y: 1 },
      ]),
    ).toHaveLength(2);
  });

  it('handles collinear points', () => {
    const pts: Point[] = [
      { x: 0, y: 0 },
      { x: 1, y: 0 },
      { x: 2, y: 0 },
      { x: 3, y: 0 },
    ];
    const hull = convexHull(pts);
    expect(hull).toHaveLength(2);
  });

  it('handles duplicate points', () => {
    const pts: Point[] = [
      { x: 0, y: 0 },
      { x: 0, y: 0 },
      { x: 1, y: 0 },
      { x: 0, y: 1 },
    ];
    const hull = convexHull(pts);
    expect(hull.length).toBeGreaterThanOrEqual(3);
  });
});

describe('expandHull', () => {
  function squareHull(): Point[] {
    return convexHull([
      { x: 0, y: 0 },
      { x: 100, y: 0 },
      { x: 100, y: 100 },
      { x: 0, y: 100 },
    ]);
  }

  it('expands a square outward', () => {
    const hull = squareHull();
    const exp = expandHull(hull, 10);
    expect(exp).toHaveLength(4);

    // Every expanded vertex should be farther from the centroid.
    const cx = 50;
    const cy = 50;
    for (let i = 0; i < hull.length; i++) {
      const origDist = Math.hypot(hull[i].x - cx, hull[i].y - cy);
      const expDist = Math.hypot(exp[i].x - cx, exp[i].y - cy);
      expect(expDist).toBeGreaterThan(origDist);
    }
  });

  it('preserves vertex count', () => {
    const hull = squareHull();
    const exp = expandHull(hull, 30);
    expect(exp).toHaveLength(hull.length);
  });

  it('approximate uniform offset for square', () => {
    const hull = squareHull();
    const exp = expandHull(hull, 20);

    // For a 90° corner, the bisector offset distance is d / cos(45°)
    // ≈ 28.3, but each coordinate component is 28.3 × cos(45°) = 20.
    // So the bottom-left vertex (0,0) moves to (-20, -20).
    const bl = exp.find((p) => p.x < 50 && p.y < 50);
    expect(bl).toBeDefined();
    expect(bl!.x).toBeCloseTo(-20, 0);
    expect(bl!.y).toBeCloseTo(-20, 0);
  });

  it('returns copy for fewer than 3 points', () => {
    const pts = [
      { x: 0, y: 0 },
      { x: 1, y: 1 },
    ];
    const exp = expandHull(pts, 10);
    expect(exp).toHaveLength(2);
    expect(exp).not.toBe(pts); // should be a new array
  });
});

describe('rayRectIntersect', () => {
  const cx = 100;
  const cy = 50;
  const hw = 60;
  const hh = 30;

  it('ray pointing right hits the right edge', () => {
    const p = rayRectIntersect(cx, cy, hw, hh, 1, 0);
    expect(p.x).toBeCloseTo(cx + hw);
    expect(p.y).toBeCloseTo(cy);
  });

  it('ray pointing left hits the left edge', () => {
    const p = rayRectIntersect(cx, cy, hw, hh, -1, 0);
    expect(p.x).toBeCloseTo(cx - hw);
    expect(p.y).toBeCloseTo(cy);
  });

  it('ray pointing down hits the bottom edge', () => {
    const p = rayRectIntersect(cx, cy, hw, hh, 0, 1);
    expect(p.x).toBeCloseTo(cx);
    expect(p.y).toBeCloseTo(cy + hh);
  });

  it('ray pointing up hits the top edge', () => {
    const p = rayRectIntersect(cx, cy, hw, hh, 0, -1);
    expect(p.x).toBeCloseTo(cx);
    expect(p.y).toBeCloseTo(cy - hh);
  });

  it('45° diagonal on a square hits the corner', () => {
    const p = rayRectIntersect(0, 0, 50, 50, 1, 1);
    expect(p.x).toBeCloseTo(50);
    expect(p.y).toBeCloseTo(50);
  });

  it('45° diagonal on a rectangle hits the shorter edge', () => {
    const p = rayRectIntersect(cx, cy, hw, hh, 1, 1);
    // hh < hw, so the horizontal edge (top/bottom) is hit first
    expect(p.y).toBeCloseTo(cy + hh);
    expect(p.x).toBeCloseTo(cx + hh); // dx == dy, so x offset == y offset
  });

  it('steep diagonal hits the top/bottom edge', () => {
    const p = rayRectIntersect(cx, cy, hw, hh, 0.5, 2);
    // ty = hh / 2 = 15, tx = hw / 0.5 = 120 → hits horizontal edge
    expect(p.y).toBeCloseTo(cy + hh);
    expect(p.x).toBeCloseTo(cx + 0.5 * (hh / 2));
  });

  it('shallow diagonal hits the left/right edge', () => {
    const p = rayRectIntersect(cx, cy, hw, hh, 2, 0.5);
    // tx = hw / 2 = 30, ty = hh / 0.5 = 60 → hits vertical edge
    expect(p.x).toBeCloseTo(cx + hw);
    expect(p.y).toBeCloseTo(cy + 0.5 * (hw / 2));
  });

  it('zero direction returns the center', () => {
    const p = rayRectIntersect(cx, cy, hw, hh, 0, 0);
    expect(p.x).toBe(cx);
    expect(p.y).toBe(cy);
  });

  it('negative diagonal hits the opposite corner region', () => {
    const p = rayRectIntersect(0, 0, 50, 50, -1, -1);
    expect(p.x).toBeCloseTo(-50);
    expect(p.y).toBeCloseTo(-50);
  });
});

describe('pointInConvexPoly', () => {
  const triangle: Point[] = [
    { x: 0, y: 0 },
    { x: 4, y: 0 },
    { x: 2, y: 3 },
  ];

  const square: Point[] = [
    { x: 0, y: 0 },
    { x: 10, y: 0 },
    { x: 10, y: 10 },
    { x: 0, y: 10 },
  ];

  it('detects point inside a triangle', () => {
    expect(pointInConvexPoly(2, 1, triangle)).toBe(true);
  });

  it('detects point outside a triangle', () => {
    expect(pointInConvexPoly(5, 5, triangle)).toBe(false);
  });

  it('detects point inside a square', () => {
    expect(pointInConvexPoly(5, 5, square)).toBe(true);
  });

  it('treats point on edge as inside', () => {
    expect(pointInConvexPoly(2, 0, triangle)).toBe(true);
  });

  it('detects point far away', () => {
    expect(pointInConvexPoly(1000, 1000, triangle)).toBe(false);
  });

  it('returns false for degenerate polygon with fewer than 3 points', () => {
    expect(pointInConvexPoly(0, 0, [])).toBe(false);
    expect(pointInConvexPoly(0, 0, [{ x: 0, y: 0 }])).toBe(false);
    expect(
      pointInConvexPoly(0, 0, [
        { x: 0, y: 0 },
        { x: 1, y: 1 },
      ]),
    ).toBe(false);
  });
});

describe('nodeCorners', () => {
  it('collects 4 corners per node with correct coordinates', () => {
    const nodes = new Map([
      ['a', { x: 0, y: 0, w: 100, h: 60 }],
      ['b', { x: 200, y: 100, w: 100, h: 60 }],
    ]);
    const pts = nodeCorners(['a', 'b'], nodes);
    expect(pts).toHaveLength(8);
    expect(pts).toContainEqual({ x: -50, y: -30 });
    expect(pts).toContainEqual({ x: 50, y: -30 });
    expect(pts).toContainEqual({ x: 50, y: 30 });
    expect(pts).toContainEqual({ x: -50, y: 30 });
  });

  it('skips missing nodes', () => {
    const nodes = new Map([['a', { x: 0, y: 0, w: 100, h: 60 }]]);
    const pts = nodeCorners(['a', 'missing'], nodes);
    expect(pts).toHaveLength(4);
  });

  it('returns empty for no matches', () => {
    const pts = nodeCorners(['x'], new Map());
    expect(pts).toHaveLength(0);
  });
});

describe('convexHull edge cases', () => {
  it('returns degenerate hull for all-identical points', () => {
    const pts: Point[] = [
      { x: 5, y: 5 },
      { x: 5, y: 5 },
      { x: 5, y: 5 },
    ];
    const hull = convexHull(pts);
    expect(hull.length).toBeLessThanOrEqual(2);
    for (const p of hull) {
      expect(p.x).toBe(5);
      expect(p.y).toBe(5);
    }
  });
});

describe('pointSegDistSq', () => {
  it('returns 0 for point on segment start', () => {
    expect(pointSegDistSq(0, 0, 0, 0, 10, 0)).toBe(0);
  });

  it('returns 0 for point on segment end', () => {
    expect(pointSegDistSq(10, 0, 0, 0, 10, 0)).toBe(0);
  });

  it('returns squared perpendicular distance for midpoint offset', () => {
    expect(pointSegDistSq(5, 3, 0, 0, 10, 0)).toBeCloseTo(9);
  });

  it('returns distance to nearest endpoint when past segment', () => {
    expect(pointSegDistSq(15, 0, 0, 0, 10, 0)).toBeCloseTo(25);
  });

  it('handles degenerate zero-length segment', () => {
    expect(pointSegDistSq(3, 4, 0, 0, 0, 0)).toBeCloseTo(25);
  });
});

describe('pointBezierDistSq', () => {
  it('returns near-zero for point on start of curve', () => {
    const d = pointBezierDistSq(0, 0, 0, 0, 5, 5, 10, 0);
    expect(d).toBeLessThan(1);
  });

  it('returns near-zero for point on end of curve', () => {
    const d = pointBezierDistSq(10, 0, 0, 0, 5, 5, 10, 0);
    expect(d).toBeLessThan(1);
  });

  it('matches pointSegDistSq for a straight-line Bezier', () => {
    const d = pointBezierDistSq(5, 3, 0, 0, 5, 0, 10, 0);
    const seg = pointSegDistSq(5, 3, 0, 0, 10, 0);
    expect(d).toBeCloseTo(seg, 0);
  });

  it('returns large distance for point far from curve', () => {
    const d = pointBezierDistSq(1000, 1000, 0, 0, 5, 5, 10, 0);
    expect(d).toBeGreaterThan(1e6);
  });
});
