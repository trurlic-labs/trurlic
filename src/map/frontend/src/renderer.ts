import type { Camera } from './camera';
import type { Graph } from './graph';
import type { RenderNode } from './types';
import { LOD } from './types';
import type { AABB } from './quadtree';

// ── Colors (CSS variable-aware) ────────────────────────────────────────────

const C = {
  bg: () => css('--bg', '#0f1117'),
  surface: () => css('--surface', '#1a1d27'),
  surfaceHi: () => css('--surface-hi', '#252836'),
  border: () => css('--border', '#2e3244'),
  text: () => css('--text', '#e1e4ed'),
  textDim: () => css('--text-dim', '#8b90a0'),
  accent: () => css('--accent', '#6c8cff'),
  accentDim: () => css('--accent-dim', '#3a4f8f'),
  edge: () => css('--edge', '#3a3f52'),
  edgeDep: () => css('--edge-dep', '#5a7f5a'),
  edgeCon: () => css('--edge-con', '#8f6c3a'),
  selectRing: () => css('--select', '#6c8cff'),
  badge: () => css('--badge', '#4a5068'),
  minimap: () => css('--minimap-bg', '#13151d'),
  minimapVp: () => css('--minimap-vp', 'rgba(108,140,255,0.25)'),
};

function css(prop: string, fallback: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(prop).trim() || fallback;
}

// ── Edge dash patterns per kind ────────────────────────────────────────────

const EDGE_DASH: Record<string, number[]> = {
  depends_on: [6, 4],
  constrains: [2, 3],
  supersedes: [8, 3, 2, 3],
};

function edgeColor(kind: string): string {
  if (kind === 'depends_on') return C.edgeDep();
  if (kind === 'constrains') return C.edgeCon();
  return C.edge();
}

// ── Renderer ───────────────────────────────────────────────────────────────

export class Renderer {
  private ctx: CanvasRenderingContext2D;
  private cam: Camera;
  private dpr: number;

  constructor(canvas: HTMLCanvasElement, cam: Camera) {
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('Canvas 2D not supported');
    this.ctx = ctx;
    this.cam = cam;
    this.dpr = window.devicePixelRatio || 1;
  }

  resize(w: number, h: number): void {
    const canvas = this.ctx.canvas;
    this.dpr = window.devicePixelRatio || 1;
    canvas.width = w * this.dpr;
    canvas.height = h * this.dpr;
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    this.cam.screenW = w;
    this.cam.screenH = h;
  }

  /**
   * Main render pass. Uses the quadtree for viewport culling —
   * only visible nodes are drawn, giving O(k) cost where k is
   * the number of on-screen nodes, not the total graph size.
   */
  render(graph: Graph, selected: string | null, lod: LOD): void {
    const { ctx, cam, dpr } = this;

    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = C.bg();
    ctx.fillRect(0, 0, cam.screenW * dpr, cam.screenH * dpr);

    // Viewport in world coordinates for quadtree query.
    const vp = cam.viewport();
    const vpAABB: AABB = {
      cx: vp.x + vp.w / 2,
      cy: vp.y + vp.h / 2,
      hw: vp.w / 2,
      hh: vp.h / 2,
    };

    const visibleNames = new Set(graph.quadtree.queryViewport(vpAABB));

    // Apply camera transform.
    ctx.save();
    ctx.translate(cam.screenW / 2, cam.screenH / 2);
    ctx.scale(cam.zoom, cam.zoom);
    ctx.translate(-cam.cx, -cam.cy);

    this.drawEdges(graph, visibleNames, lod);
    this.drawNodes(graph, visibleNames, selected, lod);

    ctx.restore();
  }

  // ── Edges ──────────────────────────────────────────────────────────────

  private drawEdges(graph: Graph, visible: Set<string>, lod: LOD): void {
    const { ctx, cam } = this;
    const baseWidth = 1.5 / cam.zoom;

    for (const e of graph.edges) {
      // LOD 0: only connects_to (architecture skeleton).
      if (lod === LOD.Overview && e.kind !== 'connects_to') continue;
      // Skip belongs_to edges — structural, not visual.
      if (e.kind === 'belongs_to') continue;

      const a = graph.nodes.get(e.from);
      const b = graph.nodes.get(e.to);
      if (!a || !b) continue;
      // Draw if either endpoint is visible (edge may cross viewport).
      if (!visible.has(e.from) && !visible.has(e.to)) continue;

      ctx.strokeStyle = edgeColor(e.kind);
      ctx.lineWidth = baseWidth;
      ctx.setLineDash((EDGE_DASH[e.kind] ?? []).map((v) => v / cam.zoom));

      ctx.beginPath();
      ctx.moveTo(a.x, a.y);
      ctx.lineTo(b.x, b.y);
      ctx.stroke();

      // Arrowhead.
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const len = Math.sqrt(dx * dx + dy * dy);
      if (len < 1) continue;
      const ux = dx / len;
      const uy = dy / len;
      const headLen = 10 / cam.zoom;
      const tipX = b.x - ux * (b.w / 2 + 2);
      const tipY = b.y - uy * (b.h / 2 + 2);

      ctx.fillStyle = edgeColor(e.kind);
      ctx.setLineDash([]);
      ctx.beginPath();
      ctx.moveTo(tipX, tipY);
      ctx.lineTo(
        tipX - ux * headLen - uy * headLen * 0.4,
        tipY - uy * headLen + ux * headLen * 0.4,
      );
      ctx.lineTo(
        tipX - ux * headLen + uy * headLen * 0.4,
        tipY - uy * headLen - ux * headLen * 0.4,
      );
      ctx.fill();

      // Edge kind label at LOD 1+.
      if (lod >= LOD.Component && e.kind !== 'connects_to') {
        const mx = (a.x + b.x) / 2;
        const my = (a.y + b.y) / 2;
        const labelSize = 9 / cam.zoom;
        ctx.font = `400 ${labelSize}px system-ui, sans-serif`;
        ctx.fillStyle = C.textDim();
        ctx.textAlign = 'center';
        ctx.textBaseline = 'bottom';
        ctx.fillText(e.kind.replace(/_/g, ' '), mx, my - 3 / cam.zoom);
      }
    }

    ctx.setLineDash([]);
  }

  // ── Nodes ──────────────────────────────────────────────────────────────

  private drawNodes(graph: Graph, visible: Set<string>, selected: string | null, lod: LOD): void {
    for (const name of visible) {
      const node = graph.nodes.get(name);
      if (!node) continue;
      const isSelected = name === selected;

      switch (lod) {
        case LOD.Overview:
          this.drawNodeLOD0(node, isSelected, graph);
          break;
        case LOD.Component:
          this.drawNodeLOD1(node, isSelected, graph);
          break;
        case LOD.Decision:
          this.drawNodeLOD2(node, isSelected, graph);
          break;
      }
    }
  }

  /** LOD 0 — System Overview: labeled box + decision count badge. */
  private drawNodeLOD0(node: RenderNode, selected: boolean, _graph: Graph): void {
    const { ctx, cam } = this;

    if (selected) this.drawSelectRing(node);

    ctx.fillStyle = selected ? C.surfaceHi() : C.surface();
    this.roundRect(node.x - node.w / 2, node.y - node.h / 2, node.w, node.h, 8);
    ctx.fill();
    ctx.strokeStyle = C.border();
    ctx.lineWidth = 1 / cam.zoom;
    ctx.stroke();

    const fontSize = Math.max(12, 14 / Math.max(cam.zoom, 0.5));
    ctx.font = `600 ${fontSize}px system-ui, -apple-system, sans-serif`;
    ctx.fillStyle = C.text();
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(node.name, node.x, node.y - 4, node.w - 16);

    // Decision count badge.
    if (node.decisionCount != null && node.decisionCount > 0) {
      const badge = `${node.decisionCount}`;
      const badgeFontSize = fontSize * 0.7;
      ctx.font = `500 ${badgeFontSize}px system-ui, sans-serif`;
      ctx.fillStyle = C.badge();
      const bw = ctx.measureText(badge).width + 10;
      this.roundRect(node.x - bw / 2, node.y + 8, bw, badgeFontSize + 6, 4);
      ctx.fill();
      ctx.fillStyle = C.textDim();
      ctx.fillText(badge, node.x, node.y + 8 + (badgeFontSize + 6) / 2, bw);
    }
  }

  /** LOD 1 — Component Detail: name, description, and decision list inside box. */
  private drawNodeLOD1(node: RenderNode, selected: boolean, graph: Graph): void {
    const { ctx, cam } = this;
    const decisions = graph.decisionsFor(node.name);
    const lineH = 16 / cam.zoom;
    const expandedH = Math.max(node.h, 40 + decisions.length * lineH);

    if (selected) {
      this.drawSelectRing(node, expandedH);
    }

    ctx.fillStyle = selected ? C.surfaceHi() : C.surface();
    this.roundRect(node.x - node.w / 2, node.y - expandedH / 2, node.w, expandedH, 8);
    ctx.fill();
    ctx.strokeStyle = C.border();
    ctx.lineWidth = 1 / cam.zoom;
    ctx.stroke();

    const fontSize = 14 / cam.zoom;
    let cursorY = node.y - expandedH / 2 + fontSize + 6 / cam.zoom;

    // Name.
    ctx.font = `600 ${fontSize}px system-ui, -apple-system, sans-serif`;
    ctx.fillStyle = C.text();
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(node.name, node.x, cursorY, node.w - 16);
    cursorY += fontSize * 0.6;

    // Description.
    if (node.description) {
      const descSize = fontSize * 0.75;
      ctx.font = `400 ${descSize}px system-ui, sans-serif`;
      ctx.fillStyle = C.textDim();
      const desc =
        node.description.length > 50 ? node.description.slice(0, 47) + '…' : node.description;
      ctx.fillText(desc, node.x, cursorY + descSize, node.w - 16);
      cursorY += descSize + 4 / cam.zoom;
    }

    // Decision rows.
    if (decisions.length > 0) {
      cursorY += 6 / cam.zoom;
      const rowSize = fontSize * 0.7;
      ctx.font = `400 ${rowSize}px system-ui, sans-serif`;
      ctx.textAlign = 'left';
      const leftX = node.x - node.w / 2 + 10 / cam.zoom;
      const maxW = node.w - 20 / cam.zoom;

      for (const d of decisions) {
        cursorY += lineH;
        ctx.fillStyle = C.accent();
        ctx.fillText('•', leftX, cursorY);
        ctx.fillStyle = C.text();
        const label = d.choice.length > 35 ? d.choice.slice(0, 32) + '…' : d.choice;
        ctx.fillText(label, leftX + 10 / cam.zoom, cursorY, maxW - 10 / cam.zoom);
      }
    }
  }

  /** LOD 2 — Decision Detail: full cards with choice, reason, tags, timestamp. */
  private drawNodeLOD2(node: RenderNode, selected: boolean, graph: Graph): void {
    const { ctx, cam } = this;
    const decisions = graph.decisionsFor(node.name);
    const cardH = 50 / cam.zoom;
    const gap = 6 / cam.zoom;
    const expandedH = Math.max(node.h, 50 + decisions.length * (cardH + gap));

    if (selected) {
      this.drawSelectRing(node, expandedH);
    }

    ctx.fillStyle = selected ? C.surfaceHi() : C.surface();
    this.roundRect(node.x - node.w / 2, node.y - expandedH / 2, node.w, expandedH, 8);
    ctx.fill();
    ctx.strokeStyle = C.border();
    ctx.lineWidth = 1 / cam.zoom;
    ctx.stroke();

    const fontSize = 14 / cam.zoom;
    let cursorY = node.y - expandedH / 2 + fontSize + 6 / cam.zoom;

    // Name.
    ctx.font = `600 ${fontSize}px system-ui, -apple-system, sans-serif`;
    ctx.fillStyle = C.text();
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(node.name, node.x, cursorY, node.w - 16);
    cursorY += fontSize * 0.5;

    // Description.
    if (node.description) {
      const descSize = fontSize * 0.8;
      ctx.font = `400 ${descSize}px system-ui, sans-serif`;
      ctx.fillStyle = C.textDim();
      ctx.fillText(node.description, node.x, cursorY + descSize, node.w - 16);
      cursorY += descSize + 6 / cam.zoom;
    }

    // Decision cards.
    if (decisions.length > 0) {
      cursorY += 4 / cam.zoom;
      const leftX = node.x - node.w / 2 + 8 / cam.zoom;
      const cardW = node.w - 16 / cam.zoom;

      for (const d of decisions) {
        cursorY += gap;
        // Card background.
        ctx.fillStyle = C.bg();
        this.roundRect(leftX, cursorY, cardW, cardH, 4);
        ctx.fill();

        const cSize = fontSize * 0.72;
        const rSize = fontSize * 0.6;
        const pad = 6 / cam.zoom;

        // Choice.
        ctx.font = `600 ${cSize}px system-ui, sans-serif`;
        ctx.fillStyle = C.text();
        ctx.textAlign = 'left';
        ctx.textBaseline = 'top';
        const choiceLabel = d.choice.length > 45 ? d.choice.slice(0, 42) + '…' : d.choice;
        ctx.fillText(choiceLabel, leftX + pad, cursorY + pad, cardW - pad * 2);

        // Reason.
        ctx.font = `400 ${rSize}px system-ui, sans-serif`;
        ctx.fillStyle = C.textDim();
        const reasonLabel = d.reason.length > 60 ? d.reason.slice(0, 57) + '…' : d.reason;
        ctx.fillText(
          reasonLabel,
          leftX + pad,
          cursorY + pad + cSize + 2 / cam.zoom,
          cardW - pad * 2,
        );

        // Tags (small chips).
        if (d.tags.length > 0) {
          const tagSize = fontSize * 0.5;
          ctx.font = `500 ${tagSize}px system-ui, sans-serif`;
          let tagX = leftX + pad;
          const tagY = cursorY + cardH - tagSize - pad;
          for (const tag of d.tags.slice(0, 4)) {
            const tw = ctx.measureText(tag).width + 6 / cam.zoom;
            ctx.fillStyle = C.accentDim();
            this.roundRect(tagX, tagY, tw, tagSize + 3 / cam.zoom, 2);
            ctx.fill();
            ctx.fillStyle = C.text();
            ctx.textBaseline = 'middle';
            ctx.fillText(tag, tagX + 3 / cam.zoom, tagY + (tagSize + 3 / cam.zoom) / 2);
            tagX += tw + 3 / cam.zoom;
          }
        }

        cursorY += cardH;
      }
    }
  }

  // ── Selection ring ────────────────────────────────────────────────────

  private drawSelectRing(node: RenderNode, overrideH?: number): void {
    const { ctx, cam } = this;
    const h = overrideH ?? node.h;
    ctx.strokeStyle = C.selectRing();
    ctx.lineWidth = 3 / cam.zoom;
    this.roundRect(node.x - node.w / 2 - 4, node.y - h / 2 - 4, node.w + 8, h + 8, 12);
    ctx.stroke();
  }

  // ── Minimap ────────────────────────────────────────────────────────────

  renderMinimap(miniCtx: CanvasRenderingContext2D, mw: number, mh: number, graph: Graph): void {
    const dpr = this.dpr;
    miniCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
    miniCtx.fillStyle = C.minimap();
    miniCtx.fillRect(0, 0, mw, mh);

    if (graph.nodes.size === 0) return;

    let minX = Infinity,
      minY = Infinity,
      maxX = -Infinity,
      maxY = -Infinity;
    for (const n of graph.nodes.values()) {
      minX = Math.min(minX, n.x - n.w / 2);
      minY = Math.min(minY, n.y - n.h / 2);
      maxX = Math.max(maxX, n.x + n.w / 2);
      maxY = Math.max(maxY, n.y + n.h / 2);
    }
    const pad = 40;
    minX -= pad;
    minY -= pad;
    maxX += pad;
    maxY += pad;
    const bw = maxX - minX;
    const bh = maxY - minY;
    const scale = Math.min(mw / bw, mh / bh);
    const ox = (mw - bw * scale) / 2;
    const oy = (mh - bh * scale) / 2;

    // Edges as hairlines.
    miniCtx.strokeStyle = C.edge();
    miniCtx.lineWidth = 0.5;
    miniCtx.beginPath();
    for (const e of graph.edges) {
      if (e.kind !== 'connects_to') continue;
      const a = graph.nodes.get(e.from);
      const b = graph.nodes.get(e.to);
      if (!a || !b) continue;
      miniCtx.moveTo(ox + (a.x - minX) * scale, oy + (a.y - minY) * scale);
      miniCtx.lineTo(ox + (b.x - minX) * scale, oy + (b.y - minY) * scale);
    }
    miniCtx.stroke();

    // Nodes as dots.
    miniCtx.fillStyle = C.accent();
    for (const n of graph.nodes.values()) {
      miniCtx.beginPath();
      miniCtx.arc(ox + (n.x - minX) * scale, oy + (n.y - minY) * scale, 3, 0, Math.PI * 2);
      miniCtx.fill();
    }

    // Viewport rectangle.
    const vp = this.cam.viewport();
    const vx = ox + (vp.x - minX) * scale;
    const vy = oy + (vp.y - minY) * scale;
    const vw = vp.w * scale;
    const vh = vp.h * scale;
    miniCtx.strokeStyle = C.selectRing();
    miniCtx.lineWidth = 1.5;
    miniCtx.strokeRect(vx, vy, vw, vh);
    miniCtx.fillStyle = C.minimapVp();
    miniCtx.fillRect(vx, vy, vw, vh);
  }

  // ── Helpers ────────────────────────────────────────────────────────────

  private roundRect(x: number, y: number, w: number, h: number, r: number): void {
    const ctx = this.ctx;
    ctx.beginPath();
    ctx.moveTo(x + r, y);
    ctx.lineTo(x + w - r, y);
    ctx.quadraticCurveTo(x + w, y, x + w, y + r);
    ctx.lineTo(x + w, y + h - r);
    ctx.quadraticCurveTo(x + w, y + h, x + w - r, y + h);
    ctx.lineTo(x + r, y + h);
    ctx.quadraticCurveTo(x, y + h, x, y + h - r);
    ctx.lineTo(x, y + r);
    ctx.quadraticCurveTo(x, y, x + r, y);
    ctx.closePath();
  }
}
