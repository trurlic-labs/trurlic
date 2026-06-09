import type { ColorSnapshot, RenderEdge } from '../types';

/** Dash patterns per edge kind (empty array = solid). */
export const EDGE_DASH: Record<string, number[]> = {
  depends_on: [6, 4],
  constrains: [2, 3],
  supersedes: [8, 3, 2, 3],
};

/** Edge stroke color by kind, reading from the per-frame color snapshot. */
export function edgeColor(kind: string, c: ColorSnapshot): string {
  if (kind === 'depends_on') return c.edgeDep;
  if (kind === 'constrains') return c.edgeCon;
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

/**
 * Build a set of directed edge pair keys for O(1) bidirectional
 * look-up. Each entry is `"from\0to"` (NUL-separated).
 * Excludes `belongs_to` edges (not rendered).
 */
export function buildEdgePairSet(edges: readonly RenderEdge[]): Set<string> {
  const set = new Set<string>();
  for (const e of edges) {
    if (e.kind === 'belongs_to') continue;
    set.add(`${e.from}\0${e.to}`);
  }
  return set;
}
