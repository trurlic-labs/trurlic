import type { RenderNode, RenderEdge } from '../types';

/**
 * Force-directed layout with AABB collision separation.
 *
 * Four passes per tick:
 *   1. Repulsion  — inverse-square Coulomb between all node pairs.
 *   2. Springs    — Hooke's law on connected edges.
 *   3. Gravity    — gentle pull toward the origin.
 *   4. Collision  — AABB overlap resolution (the key to no-overlap).
 *
 * Force accumulators use parallel arrays indexed by node position for
 * O(1) access in the O(n²) inner loops, avoiding Map overhead.
 */
export class ForceLayout {
  private readonly repulsion = 18_000;
  private readonly springK = 0.004;
  private readonly springLen = 400;
  private readonly gravity = 0.008;
  private readonly damping = 0.88;
  private readonly collisionPad = 24;

  private vx = new Map<string, number>();
  private vy = new Map<string, number>();

  run(nodes: Map<string, RenderNode>, edges: readonly RenderEdge[], iterations: number): void {
    // Prune stale velocity entries for removed nodes.
    for (const name of this.vx.keys()) {
      if (!nodes.has(name)) {
        this.vx.delete(name);
        this.vy.delete(name);
      }
    }
    // Initialize velocity for new nodes.
    for (const name of nodes.keys()) {
      if (!this.vx.has(name)) {
        this.vx.set(name, 0);
        this.vy.set(name, 0);
      }
    }

    // Hoist the values array — reused across all iterations.
    const arr = [...nodes.values()];

    for (let i = 0; i < iterations; i++) {
      this.tick(arr, edges);
    }
  }

  private tick(arr: RenderNode[], edges: readonly RenderEdge[]): void {
    const n = arr.length;

    // Indexed parallel arrays for force accumulation — avoids Map
    // lookups in the O(n²) repulsion loop.
    const fxArr = new Float64Array(n);
    const fyArr = new Float64Array(n);

    // ── 1. Repulsion ────────────────────────────────────────────────
    for (let i = 0; i < n; i++) {
      for (let j = i + 1; j < n; j++) {
        const a = arr[i];
        const b = arr[j];
        const ddx = b.x - a.x;
        const ddy = b.y - a.y;
        const dist = Math.sqrt(ddx * ddx + ddy * ddy) || 1;
        const force = this.repulsion / (dist * dist);
        const fx = (ddx / dist) * force;
        const fy = (ddy / dist) * force;
        fxArr[i] -= fx;
        fyArr[i] -= fy;
        fxArr[j] += fx;
        fyArr[j] += fy;
      }
    }

    // ── 2. Springs ──────────────────────────────────────────────────
    // Build a name→index lookup for edge endpoints.
    const idx = new Map<string, number>();
    for (let i = 0; i < n; i++) idx.set(arr[i].name, i);

    for (const e of edges) {
      const ai = idx.get(e.from);
      const bi = idx.get(e.to);
      if (ai === undefined || bi === undefined) continue;
      const a = arr[ai];
      const b = arr[bi];
      const ddx = b.x - a.x;
      const ddy = b.y - a.y;
      const dist = Math.sqrt(ddx * ddx + ddy * ddy) || 1;
      const force = this.springK * (dist - this.springLen);
      const fx = (ddx / dist) * force;
      const fy = (ddy / dist) * force;
      fxArr[ai] += fx;
      fyArr[ai] += fy;
      fxArr[bi] -= fx;
      fyArr[bi] -= fy;
    }

    // ── 3. Gravity ──────────────────────────────────────────────────
    for (let i = 0; i < n; i++) {
      fxArr[i] -= arr[i].x * this.gravity;
      fyArr[i] -= arr[i].y * this.gravity;
    }

    // ── Apply forces ────────────────────────────────────────────────
    for (let i = 0; i < n; i++) {
      const node = arr[i];
      if (node.pinned) continue;
      let nvx = ((this.vx.get(node.name) ?? 0) + fxArr[i]) * this.damping;
      let nvy = ((this.vy.get(node.name) ?? 0) + fyArr[i]) * this.damping;
      this.vx.set(node.name, nvx);
      this.vy.set(node.name, nvy);
      node.x += nvx;
      node.y += nvy;
    }

    // ── 4. Collision separation ──────────────────────────────────────
    this.separateOverlaps(arr);
  }

  /**
   * AABB overlap resolution. For each overlapping pair, push apart
   * along the axis of least overlap (shorter push = more stable).
   * Two passes per tick to handle transitive chains.
   */
  private separateOverlaps(nodes: RenderNode[]): void {
    const pad = this.collisionPad;
    const len = nodes.length;

    for (let pass = 0; pass < 2; pass++) {
      for (let i = 0; i < len; i++) {
        for (let j = i + 1; j < len; j++) {
          const a = nodes[i];
          const b = nodes[j];

          const dx = b.x - a.x;
          const dy = b.y - a.y;
          const overlapX = a.w / 2 + b.w / 2 + pad - Math.abs(dx);
          const overlapY = a.h / 2 + b.h / 2 + pad - Math.abs(dy);

          if (overlapX <= 0 || overlapY <= 0) continue;

          // Push along the axis with less overlap (more stable).
          // When one node is pinned, the other takes the full shift.
          const aPinned = a.pinned;
          const bPinned = b.pinned;
          if (aPinned && bPinned) continue;

          if (overlapX < overlapY) {
            const sign = dx >= 0 ? 1 : -1;
            if (aPinned) {
              b.x += sign * overlapX;
            } else if (bPinned) {
              a.x -= sign * overlapX;
            } else {
              const half = overlapX / 2;
              a.x -= sign * half;
              b.x += sign * half;
            }
          } else {
            const sign = dy >= 0 ? 1 : -1;
            if (aPinned) {
              b.y += sign * overlapY;
            } else if (bPinned) {
              a.y -= sign * overlapY;
            } else {
              const half = overlapY / 2;
              a.y -= sign * half;
              b.y += sign * half;
            }
          }
        }
      }
    }
  }
}
