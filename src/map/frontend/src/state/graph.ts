import type { GraphSnapshot, DecisionNode, PatternNode, RenderNode, RenderEdge } from '../types';
import { Quadtree } from '../renderer/culling';
import { convexHull, expandHull, pointInConvexPoly } from '../renderer/geometry';
import type { Point } from '../renderer/geometry';

/** Precomputed hull metadata — avoids per-frame centroid/minY recomputation. */
export interface PatternHullMeta {
  hull: Point[];
  centroidX: number;
  minY: number;
}

/** Shared empty array — avoids allocation on `decisionsFor` misses. */
const NO_DECISIONS: readonly DecisionNode[] = Object.freeze([]);

/** Minimum node width (px). */
const MIN_NODE_W = 200;
/** Maximum node width (px). */
const MAX_NODE_W = 320;
/** Approximate character width at 14px system-ui (monospace). */
const CHAR_WIDTH_ESTIMATE = 8.8;
/** Horizontal padding inside the node box. */
const NODE_PAD_X = 40;

/** Default node height at LOD 0–1 (px). */
const BASE_NODE_H = 60;

/** Client-side graph model. */
export class Graph {
  nodes: Map<string, RenderNode> = new Map();
  /** All edges — connects_to rendered at LOD 0, others at LOD 1+. */
  edges: RenderEdge[] = [];
  decisions: Map<string, DecisionNode> = new Map();
  patterns: Map<string, PatternNode> = new Map();
  projectName = '';
  projectDescription = '';
  layoutVersion = 0;
  quadtree = new Quadtree();

  /** Pattern name -> expanded convex hull with precomputed label metadata. */
  patternHulls: Map<string, PatternHullMeta> = new Map();

  /** Edge pair set for bidirectional detection — rebuilt on snapshot load. */
  edgePairSet: Set<string> = new Set();

  /** Sorted pattern hulls for hit-testing (smallest first). Cached between layout changes. */
  private sortedPatternHulls: [string, PatternHullMeta][] = [];

  /** Component name → decisions index. O(1) lookup. */
  private byComponent = new Map<string, DecisionNode[]>();

  /** Adjacency index: component name → set of connected component names. */
  private adjacency = new Map<string, Set<string>>();

  loadSnapshot(snap: GraphSnapshot): void {
    this.nodes.clear();
    this.edges = [];
    this.decisions.clear();
    this.patterns.clear();
    this.byComponent.clear();
    this.adjacency.clear();
    this.projectName = snap.project.name;
    this.projectDescription = snap.project.description;
    this.layoutVersion = snap.layout_version;

    for (const c of snap.components) {
      this.nodes.set(c.name, {
        name: c.name,
        kind: 'component',
        x: c.position?.x ?? 0,
        y: c.position?.y ?? 0,
        w: nodeWidth(c.name),
        h: BASE_NODE_H,
        pinned: c.pinned,
        description: c.description,
        decisionCount: c.decision_count,
        patternCount: c.pattern_count,
      });
    }

    for (const d of snap.decisions) {
      this.decisions.set(d.name, d);

      const list = this.byComponent.get(d.component);
      if (list) list.push(d);
      else this.byComponent.set(d.component, [d]);
    }

    for (const p of snap.patterns) {
      this.patterns.set(p.name, p);
    }

    for (const e of snap.edges) {
      this.edges.push({ from: e.from, to: e.to, kind: e.kind });
    }

    this.assignMissingPositions();
    this.rebuildEdgePairSet();
    this.rebuildAdjacency();
    this.rebuildQuadtree();
    this.rebuildPatternHulls();
  }

  /** Rebuild the bidirectional edge pair set and per-edge flags. Call after edges change. */
  rebuildEdgePairSet(): void {
    this.edgePairSet.clear();
    for (const e of this.edges) {
      this.edgePairSet.add(`${e.from}\0${e.to}`);
    }
    for (const e of this.edges) {
      const bi = this.edgePairSet.has(`${e.to}\0${e.from}`);
      e.hasBi = bi;
      e.isReverse = bi && e.from > e.to;
    }
  }

  /** Rebuild the spatial index. Call after layout changes or drag. */
  rebuildQuadtree(): void {
    this.quadtree.build(this.nodes);
  }

  /** Rebuild expanded convex hulls for all patterns. Call after layout. */
  rebuildPatternHulls(): void {
    this.patternHulls.clear();
    for (const [name, pat] of this.patterns) {
      const corners: Point[] = [];
      for (const cName of pat.components) {
        const n = this.nodes.get(cName);
        if (!n) continue;
        const hw = n.w / 2;
        const hh = n.h / 2;
        corners.push(
          { x: n.x - hw, y: n.y - hh },
          { x: n.x + hw, y: n.y - hh },
          { x: n.x + hw, y: n.y + hh },
          { x: n.x - hw, y: n.y + hh },
        );
      }
      if (corners.length < 3) continue;
      const hull = convexHull(corners);
      if (hull.length < 3) continue;
      const expanded = expandHull(hull, 50);
      let cx = 0;
      let minY = Infinity;
      for (const p of expanded) {
        cx += p.x;
        if (p.y < minY) minY = p.y;
      }
      cx /= expanded.length;
      this.patternHulls.set(name, { hull: expanded, centroidX: cx, minY });
    }
    this.sortedPatternHulls = [...this.patternHulls.entries()].sort(
      (a, b) => a[1].hull.length - b[1].hull.length,
    );
  }

  /** Hit-test pattern regions. Returns the pattern name if (wx, wy) is inside any hull. Smaller patterns (fewer components) win over broad ones. */
  patternAt(wx: number, wy: number): string | null {
    for (const [name, meta] of this.sortedPatternHulls) {
      if (pointInConvexPoly(wx, wy, meta.hull)) return name;
    }
    return null;
  }

  private assignMissingPositions(): void {
    let i = 0;
    const count = this.nodes.size;
    for (const node of this.nodes.values()) {
      if (node.x === 0 && node.y === 0 && !node.pinned) {
        const angle = (2 * Math.PI * i) / Math.max(count, 1);
        const radius = 350 + count * 45;
        node.x = Math.cos(angle) * radius;
        node.y = Math.sin(angle) * radius;
      }
      i++;
    }
  }

  /** Hit test using quadtree — O(log n) instead of linear scan. */
  nodeAt(wx: number, wy: number): RenderNode | null {
    const name = this.quadtree.hitTest(wx, wy);
    return name ? (this.nodes.get(name) ?? null) : null;
  }

  /** O(1) lookup via pre-built index. Returns frozen empty array on miss. */
  decisionsFor(component: string): readonly DecisionNode[] {
    return this.byComponent.get(component) ?? NO_DECISIONS;
  }

  /** All unique tags across every decision. Sorted alphabetically. */
  allTags(): string[] {
    const tags = new Set<string>();
    for (const d of this.decisions.values()) {
      for (const t of d.tags) tags.add(t);
    }
    return [...tags].sort();
  }

  // ── Incremental updates (WS diff processing) ─────────────────────────

  /**
   * Remove a node and all its related data. Used for `node_removed` WS
   * events to avoid a full graph refetch.
   */
  removeNode(name: string): void {
    this.nodes.delete(name);
    this.byComponent.delete(name);
    const toRemove: string[] = [];
    for (const [dName, d] of this.decisions) {
      if (d.component === name) toRemove.push(dName);
    }
    for (const dName of toRemove) this.decisions.delete(dName);
    this.edges = this.edges.filter((e) => e.from !== name && e.to !== name);
    this.rebuildEdgePairSet();
    this.rebuildAdjacency();
    this.rebuildQuadtree();
    this.rebuildPatternHulls();
  }

  /** Add an edge. Used for `edge_added` WS events. */
  addEdge(from: string, to: string, kind: string): void {
    this.edges.push({ from, to, kind });
  }

  /** Remove a specific edge. Used for `edge_removed` WS events. */
  removeEdge(from: string, to: string, kind: string): void {
    const idx = this.edges.findIndex((e) => e.from === from && e.to === to && e.kind === kind);
    if (idx !== -1) this.edges.splice(idx, 1);
  }

  /** Rebuild adjacency index from connects_to edges. */
  rebuildAdjacency(): void {
    this.adjacency.clear();
    for (const e of this.edges) {
      if (e.kind !== 'connects_to') continue;
      let fromSet = this.adjacency.get(e.from);
      if (!fromSet) {
        fromSet = new Set();
        this.adjacency.set(e.from, fromSet);
      }
      fromSet.add(e.to);
      let toSet = this.adjacency.get(e.to);
      if (!toSet) {
        toSet = new Set();
        this.adjacency.set(e.to, toSet);
      }
      toSet.add(e.from);
    }
  }

  /** O(1) lookup of connected component names via adjacency index. */
  connectionsFor(name: string): string[] {
    const set = this.adjacency.get(name);
    return set ? [...set] : [];
  }
}

/**
 * Compute node width from the component name length.
 * Avoids truncated labels while preventing excessively wide boxes.
 */
function nodeWidth(name: string): number {
  const textWidth = name.length * CHAR_WIDTH_ESTIMATE + NODE_PAD_X;
  return Math.max(MIN_NODE_W, Math.min(MAX_NODE_W, textWidth));
}
