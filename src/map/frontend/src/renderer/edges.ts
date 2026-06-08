import type { ColorSnapshot } from '../types';

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
