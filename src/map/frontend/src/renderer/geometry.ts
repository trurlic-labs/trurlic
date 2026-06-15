// ── Types ──────────────────────────────────────────────────────────────

export interface Point {
  x: number;
  y: number;
}

// ── Convex Hull (Andrew's monotone chain) ──────────────────────────────

/**
 * 2D cross product of vectors OA and OB.
 * Positive → left turn, zero → collinear, negative → right turn
 * (in math coordinates; inverted in screen-space y-down).
 */
export function cross(o: Point, a: Point, b: Point): number {
  return (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
}

/**
 * Compute the convex hull of a set of points using Andrew's
 * monotone chain algorithm. O(n log n).
 *
 * Returns vertices in order (CCW in math coords). Degenerate
 * inputs (fewer than 3 unique points) return the input unchanged.
 */
export function convexHull(points: readonly Point[]): Point[] {
  const pts = points.slice().sort((a, b) => a.x - b.x || a.y - b.y);
  const n = pts.length;
  if (n <= 2) return pts;

  const lower: Point[] = [];
  for (const p of pts) {
    while (lower.length >= 2 && cross(lower[lower.length - 2], lower[lower.length - 1], p) <= 0) {
      lower.pop();
    }
    lower.push(p);
  }

  const upper: Point[] = [];
  for (let i = n - 1; i >= 0; i--) {
    const p = pts[i];
    while (upper.length >= 2 && cross(upper[upper.length - 2], upper[upper.length - 1], p) <= 0) {
      upper.pop();
    }
    upper.push(p);
  }

  // Remove last vertex of each half (it's the first of the other).
  lower.pop();
  upper.pop();
  return lower.concat(upper);
}

// ── Polygon expansion ──────────────────────────────────────────────────

/**
 * Outward unit normal of edge A→B, oriented away from centroid (cx, cy).
 * Works regardless of winding direction or coordinate convention.
 */
function edgeOutward(a: Point, b: Point, cx: number, cy: number): Point {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const len = Math.sqrt(dx * dx + dy * dy);
  if (len < 1e-10) return { x: 0, y: 0 };

  // Candidate normal (perpendicular to edge).
  let nx = -dy / len;
  let ny = dx / len;

  // Flip if pointing inward (toward centroid).
  const mx = (a.x + b.x) / 2 - cx;
  const my = (a.y + b.y) / 2 - cy;
  if (nx * mx + ny * my < 0) {
    nx = -nx;
    ny = -ny;
  }

  return { x: nx, y: ny };
}

/**
 * Expand a convex hull outward by `d` world units.
 *
 * Each vertex is moved along the bisector of its two adjacent edge
 * outward normals, scaled by `d / cos(halfAngle)` to achieve
 * uniform edge offset. The cos factor is clamped to avoid blow-up
 * at very acute angles.
 */
export function expandHull(hull: readonly Point[], d: number): Point[] {
  const n = hull.length;
  if (n < 3) return hull.slice();

  const cx = hull.reduce((s, p) => s + p.x, 0) / n;
  const cy = hull.reduce((s, p) => s + p.y, 0) / n;

  const result: Point[] = [];
  for (let i = 0; i < n; i++) {
    const prev = hull[(i - 1 + n) % n];
    const curr = hull[i];
    const next = hull[(i + 1) % n];

    const n1 = edgeOutward(prev, curr, cx, cy);
    const n2 = edgeOutward(curr, next, cx, cy);

    let bx = n1.x + n2.x;
    let by = n1.y + n2.y;
    const bLen = Math.sqrt(bx * bx + by * by);

    if (bLen < 1e-10) {
      result.push({ x: curr.x + n1.x * d, y: curr.y + n1.y * d });
    } else {
      bx /= bLen;
      by /= bLen;
      const cosHalf = n1.x * bx + n1.y * by;
      const scale = d / Math.max(cosHalf, 0.15);
      result.push({ x: curr.x + bx * scale, y: curr.y + by * scale });
    }
  }

  return result;
}

// ── Canvas path ────────────────────────────────────────────────────────

/**
 * Trace a rounded convex polygon path on the canvas context.
 * Each corner is smoothed with an arc of the given `radius`.
 * The path is NOT filled or stroked — caller decides.
 */
export function roundedHullPath(
  ctx: CanvasRenderingContext2D,
  hull: readonly Point[],
  radius: number,
): void {
  const n = hull.length;
  if (n < 3) return;

  ctx.beginPath();
  // Start at the midpoint of the last edge to avoid beginning on a corner.
  const last = hull[n - 1];
  const first = hull[0];
  ctx.moveTo((last.x + first.x) / 2, (last.y + first.y) / 2);

  for (let i = 0; i < n; i++) {
    const curr = hull[i];
    const next = hull[(i + 1) % n];
    ctx.arcTo(curr.x, curr.y, next.x, next.y, radius);
  }
  ctx.closePath();
}

// ── Ray–rectangle intersection ────────────────────────────────────────

/**
 * Intersection of a ray from (cx,cy) in direction (dx,dy) with an
 * axis-aligned rectangle centered at (cx,cy) with half-extents hw, hh.
 */
export function rayRectIntersect(
  cx: number,
  cy: number,
  hw: number,
  hh: number,
  dx: number,
  dy: number,
): Point {
  const absDx = Math.abs(dx);
  const absDy = Math.abs(dy);
  if (absDx < 1e-10 && absDy < 1e-10) return { x: cx, y: cy };

  const tx = absDx > 1e-10 ? hw / absDx : Infinity;
  const ty = absDy > 1e-10 ? hh / absDy : Infinity;
  const t = Math.min(tx, ty);

  return { x: cx + dx * t, y: cy + dy * t };
}

// ── Bounding-box helpers ───────────────────────────────────────────────

/**
 * Collect the four bounding-box corner points of a set of nodes.
 * Returns an empty array if no matching nodes are found.
 */
export function nodeCorners(
  names: readonly string[],
  nodes: ReadonlyMap<string, { x: number; y: number; w: number; h: number }>,
): Point[] {
  const pts: Point[] = [];
  for (const name of names) {
    const n = nodes.get(name);
    if (!n) continue;
    const hw = n.w / 2;
    const hh = n.h / 2;
    pts.push(
      { x: n.x - hw, y: n.y - hh },
      { x: n.x + hw, y: n.y - hh },
      { x: n.x + hw, y: n.y + hh },
      { x: n.x - hw, y: n.y + hh },
    );
  }
  return pts;
}

// ── Point-in-polygon ──────────────────────────────────────────────────

/**
 * Test whether point (px, py) is inside a convex polygon.
 * Uses the cross-product winding method. Polygon vertices
 * must be in consistent winding order (CW or CCW).
 */
export function pointInConvexPoly(px: number, py: number, poly: readonly Point[]): boolean {
  const n = poly.length;
  if (n < 3) return false;
  let positive = 0;
  let negative = 0;
  for (let i = 0; i < n; i++) {
    const a = poly[i];
    const b = poly[(i + 1) % n];
    const cp = (b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x);
    if (cp > 0) positive++;
    else if (cp < 0) negative++;
    if (positive > 0 && negative > 0) return false;
  }
  return true;
}

// ── Distance helpers ───────────────────────────────────────────────────

/**
 * Squared distance from point P to the line segment AB.
 * Pure geometry — no allocations.
 */
export function pointSegDistSq(
  px: number,
  py: number,
  ax: number,
  ay: number,
  bx: number,
  by: number,
): number {
  const dx = bx - ax;
  const dy = by - ay;
  const lenSq = dx * dx + dy * dy;
  if (lenSq < 1e-10) return (px - ax) ** 2 + (py - ay) ** 2;
  const t = Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / lenSq));
  const nx = ax + t * dx;
  const ny = ay + t * dy;
  return (px - nx) ** 2 + (py - ny) ** 2;
}

/**
 * Squared distance from point P to a quadratic Bézier curve A→CP→B.
 * Approximated by sampling the curve at {@link N} intervals and
 * checking distance to each resulting line segment.
 */
export function pointBezierDistSq(
  px: number,
  py: number,
  ax: number,
  ay: number,
  cpx: number,
  cpy: number,
  bx: number,
  by: number,
): number {
  const N = 5;
  let prevX = ax;
  let prevY = ay;
  let minDist = Infinity;

  for (let i = 1; i <= N; i++) {
    const t = i / N;
    const s = 1 - t;
    const x = s * s * ax + 2 * s * t * cpx + t * t * bx;
    const y = s * s * ay + 2 * s * t * cpy + t * t * by;

    const d = pointSegDistSq(px, py, prevX, prevY, x, y);
    if (d < minDist) minDist = d;

    prevX = x;
    prevY = y;
  }

  return minDist;
}
