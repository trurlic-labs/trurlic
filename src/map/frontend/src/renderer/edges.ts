import type { ColorSnapshot } from '../types';

/** Dash patterns per edge kind (empty array = solid). */
export const EDGE_DASH: Record<string, number[]> = {
  depends_on: [6, 4],
  constrains: [2, 3],
  supersedes: [8, 3, 2, 3],
};

/**
 * Base opacity per edge kind. Primary connections dominate visually
 * while secondary relationships recede.
 */
export const EDGE_OPACITY: Record<string, number> = {
  connects_to: 1.0,
  depends_on: 0.7,
  constrains: 0.55,
  supersedes: 0.4,
};

/** Edge stroke color by kind, reading from the per-frame color snapshot. */
export function edgeColor(kind: string, c: ColorSnapshot): string {
  if (kind === 'depends_on') return c.edgeDep;
  if (kind === 'constrains') return c.edgeCon;
  if (kind === 'supersedes') return c.edgeSup;
  return c.edge;
}

// ── Bezier curve helpers ───────────────────────────────────────────────

/** Screen-pixel perpendicular offset for edge curvature. */
const CURVE_OFFSET_PX = 15;

/**
 * Compute the quadratic Bézier control point for a curved edge.
 *
 * The control point is offset perpendicular to the edge direction
 * by {@link CURVE_OFFSET_PX} screen pixels (converted to world
 * units via zoom). For bidirectional pairs, `reverse` flips the
 * offset to the opposite side so the two edges form a visible pair.
 */
export function edgeCurveCP(
  ax: number,
  ay: number,
  bx: number,
  by: number,
  zoom: number,
  reverse: boolean,
): { cpx: number; cpy: number } {
  const mx = (ax + bx) / 2;
  const my = (ay + by) / 2;
  const dx = bx - ax;
  const dy = by - ay;
  const len = Math.sqrt(dx * dx + dy * dy);
  if (len < 1e-10) return { cpx: mx, cpy: my };

  // Perpendicular direction (left of A→B).
  const px = -dy / len;
  const py = dx / len;
  const offset = (reverse ? -CURVE_OFFSET_PX : CURVE_OFFSET_PX) / zoom;

  return {
    cpx: mx + px * offset,
    cpy: my + py * offset,
  };
}
