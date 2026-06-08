import type { GraphSnapshot, DecisionNode, PatternNode, RenderNode, RenderEdge } from '../types';
import { Quadtree } from '../renderer/culling';

/** Shared empty array — avoids allocation on `decisionsFor` misses. */
const NO_DECISIONS: readonly DecisionNode[] = Object.freeze([]);

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

  /** Component name → decisions index. O(1) lookup. */
  private byComponent = new Map<string, DecisionNode[]>();

  loadSnapshot(snap: GraphSnapshot): void {
    this.nodes.clear();
    this.edges = [];
    this.decisions.clear();
    this.patterns.clear();
    this.byComponent.clear();
    this.projectName = snap.project.name;
    this.projectDescription = snap.project.description;
    this.layoutVersion = snap.layout_version;

    for (const c of snap.components) {
      this.nodes.set(c.name, {
        name: c.name,
        kind: 'component',
        x: c.position?.x ?? 0,
        y: c.position?.y ?? 0,
        w: 180,
        h: 60,
        pinned: c.pinned,
        description: c.description,
        decisionCount: c.decision_count,
        patternCount: c.pattern_count,
      });
    }

    for (const d of snap.decisions) {
      this.decisions.set(d.name, d);

      // Build component → decisions index.
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
    this.rebuildQuadtree();
  }

  /** Rebuild the spatial index. Call after layout changes or drag. */
  rebuildQuadtree(): void {
    this.quadtree.build(this.nodes);
  }

  private assignMissingPositions(): void {
    let i = 0;
    const count = this.nodes.size;
    for (const node of this.nodes.values()) {
      if (node.x === 0 && node.y === 0 && !node.pinned) {
        const angle = (2 * Math.PI * i) / Math.max(count, 1);
        const radius = 200 + count * 20;
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
}
