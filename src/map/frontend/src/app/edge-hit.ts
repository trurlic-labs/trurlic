import type { Graph } from '../state/graph';
import type { FilterState } from '../types';
import type { HoverEdge } from './hover';
import { LOD } from '../renderer/lod';
import { pointBezierDistSq } from '../renderer/geometry';
import { edgeCurveCP } from '../renderer/edges';

/** Screen-pixel threshold for edge hover detection. */
const EDGE_HIT_PX = 8;

/**
 * Find the edge nearest to the cursor in world space.
 * Hit-tests against the quadratic Bezier curve (not the straight
 * chord) to match the rendered curvature. Only checks edges visible
 * at the current LOD and filter state.
 * Returns null if no edge is within EDGE_HIT_PX screen pixels.
 */
export function findHoveredEdge(
  graph: Graph,
  wx: number,
  wy: number,
  zoom: number,
  lod: LOD,
  filters: FilterState | undefined,
): HoverEdge | null {
  if (lod < LOD.Component) return null;

  const threshold = EDGE_HIT_PX / zoom;
  const threshSq = threshold * threshold;
  let bestDistSq = threshSq;
  let best: HoverEdge | null = null;

  for (const e of graph.edges) {
    if (e.kind === 'belongs_to') continue;
    if (lod === LOD.Overview && e.kind !== 'connects_to') continue;
    if (filters && !filters.edgeKinds.has(e.kind)) continue;

    const a = graph.nodes.get(e.from);
    const b = graph.nodes.get(e.to);
    if (!a || !b) continue;

    const { cpx, cpy } = edgeCurveCP(a.x, a.y, b.x, b.y, zoom, e.isReverse === true);

    const d = pointBezierDistSq(wx, wy, a.x, a.y, cpx, cpy, b.x, b.y);
    if (d < bestDistSq) {
      bestDistSq = d;
      best = { from: e.from, to: e.to, kind: e.kind };
    }
  }

  return best;
}
