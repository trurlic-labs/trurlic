import type { RenderNode } from './types';

// ── AABB helpers ──────────────────────────────────────────────────────────

export interface AABB {
  cx: number;
  cy: number;
  hw: number; // half-width
  hh: number; // half-height
}

function intersects(a: AABB, b: AABB): boolean {
  return Math.abs(a.cx - b.cx) < a.hw + b.hw && Math.abs(a.cy - b.cy) < a.hh + b.hh;
}

function containsPoint(box: AABB, px: number, py: number): boolean {
  return Math.abs(px - box.cx) <= box.hw && Math.abs(py - box.cy) <= box.hh;
}

// ── Quadtree ──────────────────────────────────────────────────────────────

const MAX_DEPTH = 8;
const CELL_CAPACITY = 8;

interface Entry {
  name: string;
  bounds: AABB;
}

class QTNode {
  bounds: AABB;
  entries: Entry[] = [];
  children: QTNode[] | null = null;
  depth: number;

  constructor(bounds: AABB, depth: number) {
    this.bounds = bounds;
    this.depth = depth;
  }

  insert(entry: Entry): void {
    if (!intersects(this.bounds, entry.bounds)) return;

    if (this.children === null) {
      this.entries.push(entry);
      if (this.entries.length > CELL_CAPACITY && this.depth < MAX_DEPTH) {
        this.subdivide();
      }
      return;
    }

    for (const child of this.children) {
      child.insert(entry);
    }
  }

  query(range: AABB, results: string[]): void {
    if (!intersects(this.bounds, range)) return;

    for (const e of this.entries) {
      if (intersects(e.bounds, range)) {
        results.push(e.name);
      }
    }

    if (this.children !== null) {
      for (const child of this.children) {
        child.query(range, results);
      }
    }
  }

  /** Point query — returns the top-most (last-inserted) hit, or null. */
  queryPoint(px: number, py: number): string | null {
    if (!containsPoint(this.bounds, px, py)) return null;

    // Check children first (depth-first) for tighter results.
    if (this.children !== null) {
      for (let i = this.children.length - 1; i >= 0; i--) {
        const hit = this.children[i].queryPoint(px, py);
        if (hit !== null) return hit;
      }
    }

    // Check own entries in reverse insertion order.
    for (let i = this.entries.length - 1; i >= 0; i--) {
      if (containsPoint(this.entries[i].bounds, px, py)) {
        return this.entries[i].name;
      }
    }

    return null;
  }

  private subdivide(): void {
    const { cx, cy, hw, hh } = this.bounds;
    const qw = hw / 2;
    const qh = hh / 2;
    const d = this.depth + 1;

    this.children = [
      new QTNode({ cx: cx - qw, cy: cy - qh, hw: qw, hh: qh }, d), // NW
      new QTNode({ cx: cx + qw, cy: cy - qh, hw: qw, hh: qh }, d), // NE
      new QTNode({ cx: cx - qw, cy: cy + qh, hw: qw, hh: qh }, d), // SW
      new QTNode({ cx: cx + qw, cy: cy + qh, hw: qw, hh: qh }, d), // SE
    ];

    // Re-insert existing entries into children.
    const entries = this.entries;
    this.entries = [];
    for (const entry of entries) {
      for (const child of this.children) {
        child.insert(entry);
      }
    }
  }
}

/**
 * Spatial index for viewport culling and hit detection.
 *
 * Rebuilt when the graph changes or nodes move. Query cost is O(log n)
 * for point queries and O(k + log n) for range queries where k is the
 * number of results.
 */
export class Quadtree {
  private root: QTNode | null = null;

  /** Rebuild the tree from the current node positions. */
  build(nodes: Map<string, RenderNode>): void {
    if (nodes.size === 0) {
      this.root = null;
      return;
    }

    // Compute world bounds with padding.
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const n of nodes.values()) {
      minX = Math.min(minX, n.x - n.w / 2);
      minY = Math.min(minY, n.y - n.h / 2);
      maxX = Math.max(maxX, n.x + n.w / 2);
      maxY = Math.max(maxY, n.y + n.h / 2);
    }
    const pad = 100;
    const cx = (minX + maxX) / 2;
    const cy = (minY + maxY) / 2;
    const hw = (maxX - minX) / 2 + pad;
    const hh = (maxY - minY) / 2 + pad;

    this.root = new QTNode({ cx, cy, hw, hh }, 0);

    for (const n of nodes.values()) {
      this.root.insert({
        name: n.name,
        bounds: { cx: n.x, cy: n.y, hw: n.w / 2, hh: n.h / 2 },
      });
    }
  }

  /** Return names of all nodes whose bounds intersect the viewport. */
  queryViewport(viewport: AABB): string[] {
    if (this.root === null) return [];
    const results: string[] = [];
    this.root.query(viewport, results);
    // Deduplicate — a node near a cell boundary may appear in multiple cells.
    return [...new Set(results)];
  }

  /** Return the name of the node at world-space point (wx, wy), or null. */
  hitTest(wx: number, wy: number): string | null {
    if (this.root === null) return null;
    return this.root.queryPoint(wx, wy);
  }
}
