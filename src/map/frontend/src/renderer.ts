import type { Camera } from './camera';
import type { Graph } from './graph';

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
  selectRing: () => css('--select', '#6c8cff'),
  badge: () => css('--badge', '#4a5068'),
  minimap: () => css('--minimap-bg', '#13151d'),
  minimapVp: () => css('--minimap-vp', 'rgba(108,140,255,0.25)'),
};

function css(prop: string, fallback: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(prop).trim() || fallback;
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

  render(graph: Graph, selected: string | null): void {
    const { ctx, cam, dpr } = this;
    const W = cam.screenW * dpr;
    const H = cam.screenH * dpr;

    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = C.bg();
    ctx.fillRect(0, 0, W, H);

    // Apply camera transform.
    ctx.save();
    ctx.translate(cam.screenW / 2, cam.screenH / 2);
    ctx.scale(cam.zoom, cam.zoom);
    ctx.translate(-cam.cx, -cam.cy);

    this.drawEdges(graph);
    this.drawNodes(graph, selected);

    ctx.restore();
  }

  // ── Edges ──────────────────────────────────────────────────────────────

  private drawEdges(graph: Graph): void {
    const { ctx, cam } = this;
    ctx.strokeStyle = C.edge();
    ctx.lineWidth = 1.5 / cam.zoom;
    ctx.beginPath();

    for (const e of graph.edges) {
      const a = graph.nodes.get(e.from);
      const b = graph.nodes.get(e.to);
      if (!a || !b) continue;
      ctx.moveTo(a.x, a.y);
      ctx.lineTo(b.x, b.y);
    }
    ctx.stroke();

    // Arrowheads.
    const headLen = 10 / cam.zoom;
    ctx.fillStyle = C.edge();
    for (const e of graph.edges) {
      const a = graph.nodes.get(e.from);
      const b = graph.nodes.get(e.to);
      if (!a || !b) continue;
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const len = Math.sqrt(dx * dx + dy * dy);
      if (len < 1) continue;
      const ux = dx / len;
      const uy = dy / len;
      // Arrow tip at edge of target node.
      const tipX = b.x - ux * (b.w / 2 + 2);
      const tipY = b.y - uy * (b.h / 2 + 2);
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
    }
  }

  // ── Nodes ──────────────────────────────────────────────────────────────

  private drawNodes(graph: Graph, selected: string | null): void {
    const { ctx, cam } = this;
    const lod = cam.zoom > 0.6 ? 1 : 0;

    for (const node of graph.nodes.values()) {
      const isSelected = node.name === selected;

      // Selection ring.
      if (isSelected) {
        ctx.strokeStyle = C.selectRing();
        ctx.lineWidth = 3 / cam.zoom;
        this.roundRect(
          node.x - node.w / 2 - 4,
          node.y - node.h / 2 - 4,
          node.w + 8,
          node.h + 8,
          12,
        );
        ctx.stroke();
      }

      // Background.
      ctx.fillStyle = isSelected ? C.surfaceHi() : C.surface();
      this.roundRect(node.x - node.w / 2, node.y - node.h / 2, node.w, node.h, 8);
      ctx.fill();
      ctx.strokeStyle = C.border();
      ctx.lineWidth = 1 / cam.zoom;
      ctx.stroke();

      // Label.
      const fontSize = Math.max(12, 14 / Math.max(cam.zoom, 0.5));
      ctx.font = `600 ${fontSize}px system-ui, -apple-system, sans-serif`;
      ctx.fillStyle = C.text();
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';

      if (lod === 0) {
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
      } else {
        // LOD 1: show name and brief description.
        ctx.fillText(node.name, node.x, node.y - node.h / 2 + fontSize + 4, node.w - 16);
        if (node.description) {
          const descFontSize = fontSize * 0.75;
          ctx.font = `400 ${descFontSize}px system-ui, sans-serif`;
          ctx.fillStyle = C.textDim();
          const desc =
            node.description.length > 40 ? node.description.slice(0, 37) + '…' : node.description;
          ctx.fillText(desc, node.x, node.y, node.w - 16);
        }
      }
    }
  }

  // ── Minimap ────────────────────────────────────────────────────────────

  renderMinimap(miniCtx: CanvasRenderingContext2D, mw: number, mh: number, graph: Graph): void {
    const dpr = this.dpr;
    miniCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
    miniCtx.fillStyle = C.minimap();
    miniCtx.fillRect(0, 0, mw, mh);

    if (graph.nodes.size === 0) return;

    // Compute bounds.
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

    // Draw edges.
    miniCtx.strokeStyle = C.edge();
    miniCtx.lineWidth = 0.5;
    miniCtx.beginPath();
    for (const e of graph.edges) {
      const a = graph.nodes.get(e.from);
      const b = graph.nodes.get(e.to);
      if (!a || !b) continue;
      miniCtx.moveTo(ox + (a.x - minX) * scale, oy + (a.y - minY) * scale);
      miniCtx.lineTo(ox + (b.x - minX) * scale, oy + (b.y - minY) * scale);
    }
    miniCtx.stroke();

    // Draw nodes as dots.
    miniCtx.fillStyle = C.accent();
    for (const n of graph.nodes.values()) {
      const sx = ox + (n.x - minX) * scale;
      const sy = oy + (n.y - minY) * scale;
      miniCtx.beginPath();
      miniCtx.arc(sx, sy, 3, 0, Math.PI * 2);
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
