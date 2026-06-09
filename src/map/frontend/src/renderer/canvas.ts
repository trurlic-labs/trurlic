import type { Camera } from './camera';
import type { Graph } from '../state/graph';
import type { RenderNode, FilterState, DecisionNode, ColorSnapshot } from '../types';
import { LOD } from './lod';
import type { AABB } from './culling';
import { EDGE_DASH, edgeColor } from './edges';
import { convexHull, expandHull, roundedHullPath, nodeCorners } from './geometry';
import type { HoverRenderState } from '../app/hover';

// ── Per-frame color snapshot ──────────────────────────────────────────────

function snapshotColors(): ColorSnapshot {
  const s = getComputedStyle(document.documentElement);
  const v = (prop: string, fb: string) => s.getPropertyValue(prop).trim() || fb;
  return {
    bg: v('--bg', '#0f1117'),
    surface: v('--surface', '#1a1d27'),
    surfaceHi: v('--surface-hi', '#252836'),
    border: v('--border', '#2e3244'),
    text: v('--text', '#e1e4ed'),
    textDim: v('--text-dim', '#8b90a0'),
    accent: v('--accent', '#6c8cff'),
    accentDim: v('--accent-dim', '#3a4f8f'),
    edge: v('--edge', '#3a3f52'),
    edgeDep: v('--edge-dep', '#5a7f5a'),
    edgeCon: v('--edge-con', '#8f6c3a'),
    selectRing: v('--select', '#6c8cff'),
    badge: v('--badge', '#4a5068'),
    minimap: v('--minimap-bg', '#13151d'),
    minimapVp: v('--minimap-vp', 'rgba(108,140,255,0.25)'),
  };
}

// ── Decision filtering ─────────────────────────────────────────────────────

const DAY_MS = 86_400_000;

function filterDecisions(
  decisions: readonly DecisionNode[],
  f: FilterState,
): readonly DecisionNode[] {
  if (f.activeTags.size === 0 && f.maxAgeDays === null) return decisions;
  const now = Date.now();
  return decisions.filter((d) => {
    if (f.activeTags.size > 0 && !d.tags.some((t) => f.activeTags.has(t))) return false;
    if (f.maxAgeDays !== null) {
      const age = (now - new Date(d.created).getTime()) / DAY_MS;
      if (age > f.maxAgeDays) return false;
    }
    return true;
  });
}

// ── Pattern region colors ──────────────────────────────────────────────────

/** Fixed hue palette for pattern regions (degrees). */
const PATTERN_HUES = [210, 150, 30, 330, 270, 90, 0, 60];

/** LOD label fade duration (ms). */
const LOD_FADE_MS = 150;

/** Pattern hull expansion (world units). */
const HULL_EXPAND = 30;

/** Pattern hull corner rounding radius (world units). */
const HULL_RADIUS = 12;

// ── Renderer ───────────────────────────────────────────────────────────────

export class Renderer {
  private ctx: CanvasRenderingContext2D;
  private cam: Camera;
  private dpr: number;
  /** Per-frame color snapshot — refreshed at the top of render(). */
  private c: ColorSnapshot;
  /** Per-frame hover state — set at the top of render(), read by draw methods. */
  private fh: HoverRenderState | null = null;

  // LOD transition fade state.
  private prevLod: LOD = LOD.Overview;
  private lodFadeAlpha = 1;
  private lodFadeStart = 0;

  constructor(canvas: HTMLCanvasElement, cam: Camera) {
    const ctx = canvas.getContext('2d');
    if (!ctx) throw new Error('Canvas 2D not supported');
    this.ctx = ctx;
    this.cam = cam;
    this.dpr = window.devicePixelRatio || 1;
    this.c = snapshotColors();
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
   * Main render pass. Snapshots CSS colors once, then uses the snapshot
   * for all draw calls — zero getComputedStyle overhead in the hot path.
   *
   * Returns `true` if a LOD transition fade is in progress (caller
   * should keep re-rendering).
   */
  render(
    graph: Graph,
    selected: string | null,
    lod: LOD,
    focus: Set<string> | null = null,
    filters?: FilterState,
    hover?: HoverRenderState,
  ): boolean {
    this.c = snapshotColors();
    this.fh = hover ?? null;
    const { ctx, cam, dpr, c } = this;

    // LOD transition fade.
    const now = performance.now();
    if (lod !== this.prevLod) {
      this.lodFadeAlpha = 0;
      this.lodFadeStart = now;
      this.prevLod = lod;
    }
    if (this.lodFadeAlpha < 1) {
      this.lodFadeAlpha = Math.min(1, (now - this.lodFadeStart) / LOD_FADE_MS);
    }

    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = c.bg;
    ctx.fillRect(0, 0, cam.screenW * dpr, cam.screenH * dpr);

    const vp = cam.viewport();
    const vpAABB: AABB = {
      cx: vp.x + vp.w / 2,
      cy: vp.y + vp.h / 2,
      hw: vp.w / 2,
      hh: vp.h / 2,
    };

    const visibleNames = new Set(graph.quadtree.queryViewport(vpAABB));

    ctx.save();
    ctx.translate(cam.screenW / 2, cam.screenH / 2);
    ctx.scale(cam.zoom, cam.zoom);
    ctx.translate(-cam.cx, -cam.cy);

    if (lod <= LOD.Component) {
      this.drawPatternRegions(graph, visibleNames, focus, filters);
    }
    this.drawEdges(graph, visibleNames, lod, focus, filters);
    this.drawNodes(graph, visibleNames, selected, lod, focus, filters);

    ctx.restore();

    // Tooltip: rendered in screen space, LOD 0 only.
    if (hover?.tooltipVisible && hover.tooltipText && lod === LOD.Overview) {
      this.drawTooltip(hover.tooltipText, hover.tooltipX, hover.tooltipY);
    }

    this.fh = null;
    return this.lodFadeAlpha < 1;
  }

  // ── Pattern regions ─────────────────────────────────────────────────────

  /**
   * Draw semi-transparent pattern regions behind nodes/edges.
   * Skipped at LOD 2 (regions would fill the entire viewport).
   */
  private drawPatternRegions(
    graph: Graph,
    visible: Set<string>,
    focus: Set<string> | null,
    filters?: FilterState,
  ): void {
    if (graph.patterns.size === 0) return;
    const { ctx, cam, c } = this;
    const prefersLight =
      typeof matchMedia !== 'undefined'
        ? matchMedia('(prefers-color-scheme: light)').matches
        : false;
    const lightness = prefersLight ? 45 : 55;
    const baseFill = prefersLight ? 0.06 : 0.08;
    const baseStroke = 0.25;
    const dimFill = 0.03;
    const labelSize = 11 / cam.zoom;

    let patIdx = 0;
    for (const [, pat] of graph.patterns) {
      // Skip patterns with no visible components.
      const memberNames = pat.components.filter((name) => visible.has(name));
      if (memberNames.length === 0) {
        patIdx++;
        continue;
      }

      // Collect bounding-box corners of member components.
      const corners = nodeCorners(pat.components, graph.nodes);
      if (corners.length < 3) {
        patIdx++;
        continue;
      }

      const hull = convexHull(corners);
      if (hull.length < 3) {
        patIdx++;
        continue;
      }

      const expanded = expandHull(hull, HULL_EXPAND);

      // Focus dimming.
      const dimmedByFocus = focus !== null && !pat.components.some((n) => focus.has(n));

      // Filter dimming: when tag filter is active and no decisions in
      // this pattern match the active tags, dim to 3%.
      let dimmedByFilter = false;
      if (filters && filters.activeTags.size > 0) {
        dimmedByFilter = !pat.decisions.some((dName) => {
          const dec = graph.decisions.get(dName);
          return dec && dec.tags.some((t) => filters.activeTags.has(t));
        });
      }

      const fillAlpha = dimmedByFilter ? dimFill : baseFill;
      const hue = PATTERN_HUES[patIdx % PATTERN_HUES.length];

      ctx.globalAlpha = dimmedByFocus ? 0.15 : 1;

      // Fill.
      ctx.fillStyle = `hsla(${hue}, 60%, ${lightness}%, ${fillAlpha})`;
      roundedHullPath(ctx, expanded, HULL_RADIUS);
      ctx.fill();

      // Stroke.
      ctx.strokeStyle = `hsla(${hue}, 60%, ${lightness}%, ${baseStroke})`;
      ctx.lineWidth = 1.5 / cam.zoom;
      ctx.stroke();

      // Label: centered in region.
      const cx = expanded.reduce((s, p) => s + p.x, 0) / expanded.length;
      const cy = expanded.reduce((s, p) => s + p.y, 0) / expanded.length;
      ctx.font = `400 ${labelSize}px system-ui, sans-serif`;
      ctx.fillStyle = c.textDim;
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText(pat.name, cx, cy);

      patIdx++;
    }
    ctx.globalAlpha = 1;
  }

  // ── Edges ──────────────────────────────────────────────────────────────

  private drawEdges(
    graph: Graph,
    visible: Set<string>,
    lod: LOD,
    focus: Set<string> | null,
    filters?: FilterState,
  ): void {
    const { ctx, cam, c, fh } = this;
    const baseWidth = 1.5 / cam.zoom;

    for (const e of graph.edges) {
      if (lod === LOD.Overview && e.kind !== 'connects_to') continue;
      if (e.kind === 'belongs_to') continue;
      if (filters && !filters.edgeKinds.has(e.kind)) continue;

      const a = graph.nodes.get(e.from);
      const b = graph.nodes.get(e.to);
      if (!a || !b) continue;
      if (!visible.has(e.from) && !visible.has(e.to)) continue;

      const dimmed = focus !== null && !focus.has(e.from) && !focus.has(e.to);
      const isHovered =
        fh?.edge !== null &&
        fh?.edge !== undefined &&
        fh.edge.from === e.from &&
        fh.edge.to === e.to &&
        fh.edge.kind === e.kind;

      ctx.globalAlpha = dimmed ? 0.15 : 1;

      const color = edgeColor(e.kind, c);
      ctx.strokeStyle = isHovered ? c.accent : color;
      ctx.lineWidth = isHovered ? baseWidth * 2.5 : baseWidth;
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

      ctx.fillStyle = isHovered ? c.accent : color;
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
      // Always show for non-connects_to; show for connects_to only when hovered.
      if (lod >= LOD.Component && (e.kind !== 'connects_to' || isHovered)) {
        const mx = (a.x + b.x) / 2;
        const my = (a.y + b.y) / 2;
        const labelSize = 9 / cam.zoom;
        ctx.font = `400 ${labelSize}px system-ui, sans-serif`;
        ctx.fillStyle = isHovered ? c.text : c.textDim;
        ctx.textAlign = 'center';
        ctx.textBaseline = 'bottom';
        ctx.fillText(e.kind.replace(/_/g, ' '), mx, my - 3 / cam.zoom);
      }
    }

    ctx.setLineDash([]);
    ctx.globalAlpha = 1;
  }

  // ── Nodes ──────────────────────────────────────────────────────────────

  private drawNodes(
    graph: Graph,
    visible: Set<string>,
    selected: string | null,
    lod: LOD,
    focus: Set<string> | null,
    filters?: FilterState,
  ): void {
    for (const name of visible) {
      const node = graph.nodes.get(name);
      if (!node) continue;
      const isSelected = name === selected;

      const dimmed = focus !== null && !focus.has(name);
      this.ctx.globalAlpha = dimmed ? 0.3 : 1;

      switch (lod) {
        case LOD.Overview:
          this.drawNodeLOD0(node, isSelected, graph, filters);
          break;
        case LOD.Component:
          this.drawNodeLOD1(node, isSelected, graph, filters);
          break;
        case LOD.Decision:
          this.drawNodeLOD2(node, isSelected, graph, filters);
          break;
      }
    }
    this.ctx.globalAlpha = 1;
  }

  /** LOD 0 — System Overview: labeled box + decision count badge. */
  private drawNodeLOD0(
    node: RenderNode,
    selected: boolean,
    graph: Graph,
    filters?: FilterState,
  ): void {
    const { ctx, cam, c } = this;

    if (selected) this.drawSelectRing(node);

    ctx.fillStyle = selected ? c.surfaceHi : c.surface;
    this.roundRect(node.x - node.w / 2, node.y - node.h / 2, node.w, node.h, 8);
    ctx.fill();

    // Border — brightens to accent-dim on hover.
    this.drawNodeBorder(node);

    const fontSize = Math.max(12, 14 / Math.max(cam.zoom, 0.5));
    ctx.font = `600 ${fontSize}px system-ui, -apple-system, sans-serif`;
    ctx.fillStyle = c.text;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(node.name, node.x, node.y - 4, node.w - 16);

    // Decision count badge — reflects active filters.
    const rawCount = node.decisionCount ?? 0;
    const count =
      filters && rawCount > 0
        ? filterDecisions(graph.decisionsFor(node.name), filters).length
        : rawCount;
    if (count > 0) {
      const badge = `${count}`;
      const badgeFontSize = fontSize * 0.7;
      ctx.font = `500 ${badgeFontSize}px system-ui, sans-serif`;
      ctx.fillStyle = c.badge;
      const bw = ctx.measureText(badge).width + 10;
      this.roundRect(node.x - bw / 2, node.y + 8, bw, badgeFontSize + 6, 4);
      ctx.fill();
      ctx.fillStyle = c.textDim;
      ctx.fillText(badge, node.x, node.y + 8 + (badgeFontSize + 6) / 2, bw);
    }
  }

  /** LOD 1 — Component Detail: name, description, and decision list inside box. */
  private drawNodeLOD1(
    node: RenderNode,
    selected: boolean,
    graph: Graph,
    filters?: FilterState,
  ): void {
    const { ctx, cam, c } = this;
    const rawDecisions = graph.decisionsFor(node.name);
    const decisions = filters ? filterDecisions(rawDecisions, filters) : rawDecisions;
    const lineH = 16 / cam.zoom;
    const expandedH = Math.max(node.h, 40 + decisions.length * lineH);

    if (selected) {
      this.drawSelectRing(node, expandedH);
    }

    ctx.fillStyle = selected ? c.surfaceHi : c.surface;
    this.roundRect(node.x - node.w / 2, node.y - expandedH / 2, node.w, expandedH, 8);
    ctx.fill();

    // Border — brightens on hover.
    this.drawNodeBorder(node, expandedH);

    const fontSize = 14 / cam.zoom;
    let cursorY = node.y - expandedH / 2 + fontSize + 6 / cam.zoom;

    // Name.
    ctx.font = `600 ${fontSize}px system-ui, -apple-system, sans-serif`;
    ctx.fillStyle = c.text;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(node.name, node.x, cursorY, node.w - 16);
    cursorY += fontSize * 0.6;

    // Description.
    if (node.description) {
      const descSize = fontSize * 0.75;
      ctx.font = `400 ${descSize}px system-ui, sans-serif`;
      ctx.fillStyle = c.textDim;
      const desc =
        node.description.length > 50 ? node.description.slice(0, 47) + '…' : node.description;
      ctx.fillText(desc, node.x, cursorY + descSize, node.w - 16);
      cursorY += descSize + 4 / cam.zoom;
    }

    // Decision rows — faded in during LOD transition.
    if (decisions.length > 0) {
      const prevAlpha = ctx.globalAlpha;
      ctx.globalAlpha = prevAlpha * this.lodFadeAlpha;
      cursorY += 6 / cam.zoom;
      const rowSize = fontSize * 0.7;
      ctx.font = `400 ${rowSize}px system-ui, sans-serif`;
      ctx.textAlign = 'left';
      const leftX = node.x - node.w / 2 + 10 / cam.zoom;
      const maxW = node.w - 20 / cam.zoom;

      for (const d of decisions) {
        cursorY += lineH;
        ctx.fillStyle = c.accent;
        ctx.fillText('•', leftX, cursorY);
        ctx.fillStyle = c.text;
        const label = d.choice.length > 35 ? d.choice.slice(0, 32) + '…' : d.choice;
        ctx.fillText(label, leftX + 10 / cam.zoom, cursorY, maxW - 10 / cam.zoom);
      }
      ctx.globalAlpha = prevAlpha;
    }
  }

  /** LOD 2 — Decision Detail: full cards with choice, reason, tags, timestamp. */
  private drawNodeLOD2(
    node: RenderNode,
    selected: boolean,
    graph: Graph,
    filters?: FilterState,
  ): void {
    const { ctx, cam, c } = this;
    const rawDecisions = graph.decisionsFor(node.name);
    const decisions = filters ? filterDecisions(rawDecisions, filters) : rawDecisions;
    const cardH = 50 / cam.zoom;
    const gap = 6 / cam.zoom;
    const expandedH = Math.max(node.h, 50 + decisions.length * (cardH + gap));

    if (selected) {
      this.drawSelectRing(node, expandedH);
    }

    ctx.fillStyle = selected ? c.surfaceHi : c.surface;
    this.roundRect(node.x - node.w / 2, node.y - expandedH / 2, node.w, expandedH, 8);
    ctx.fill();

    // Border — brightens on hover.
    this.drawNodeBorder(node, expandedH);

    const fontSize = 14 / cam.zoom;
    let cursorY = node.y - expandedH / 2 + fontSize + 6 / cam.zoom;

    // Name.
    ctx.font = `600 ${fontSize}px system-ui, -apple-system, sans-serif`;
    ctx.fillStyle = c.text;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(node.name, node.x, cursorY, node.w - 16);
    cursorY += fontSize * 0.5;

    // Description.
    if (node.description) {
      const descSize = fontSize * 0.8;
      ctx.font = `400 ${descSize}px system-ui, sans-serif`;
      ctx.fillStyle = c.textDim;
      ctx.fillText(node.description, node.x, cursorY + descSize, node.w - 16);
      cursorY += descSize + 6 / cam.zoom;
    }

    // Decision cards — faded in during LOD transition.
    if (decisions.length > 0) {
      const prevAlpha = ctx.globalAlpha;
      ctx.globalAlpha = prevAlpha * this.lodFadeAlpha;
      cursorY += 4 / cam.zoom;
      const leftX = node.x - node.w / 2 + 8 / cam.zoom;
      const cardW = node.w - 16 / cam.zoom;

      for (const d of decisions) {
        cursorY += gap;
        // Card background.
        ctx.fillStyle = c.bg;
        this.roundRect(leftX, cursorY, cardW, cardH, 4);
        ctx.fill();

        const cSize = fontSize * 0.72;
        const rSize = fontSize * 0.6;
        const pad = 6 / cam.zoom;

        // Choice.
        ctx.font = `600 ${cSize}px system-ui, sans-serif`;
        ctx.fillStyle = c.text;
        ctx.textAlign = 'left';
        ctx.textBaseline = 'top';
        const choiceLabel = d.choice.length > 45 ? d.choice.slice(0, 42) + '…' : d.choice;
        ctx.fillText(choiceLabel, leftX + pad, cursorY + pad, cardW - pad * 2);

        // Reason.
        ctx.font = `400 ${rSize}px system-ui, sans-serif`;
        ctx.fillStyle = c.textDim;
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
            ctx.fillStyle = c.accentDim;
            this.roundRect(tagX, tagY, tw, tagSize + 3 / cam.zoom, 2);
            ctx.fill();
            ctx.fillStyle = c.text;
            ctx.textBaseline = 'middle';
            ctx.fillText(tag, tagX + 3 / cam.zoom, tagY + (tagSize + 3 / cam.zoom) / 2);
            tagX += tw + 3 / cam.zoom;
          }
        }

        cursorY += cardH;
      }
      ctx.globalAlpha = prevAlpha;
    }
  }

  // ── Node border (with hover highlight) ────────────────────────────────

  /**
   * Draw the node border. When the node is hovered, blends toward
   * --accent-dim with 1px extra width.
   */
  private drawNodeBorder(node: RenderNode, overrideH?: number): void {
    const { ctx, cam, c, fh } = this;
    const h = overrideH ?? node.h;
    const isHovered = fh !== null && fh.node === node.name;
    const alpha = isHovered ? fh.borderAlpha : 0;

    ctx.strokeStyle = alpha > 0 ? c.accentDim : c.border;
    ctx.lineWidth = (1 + alpha) / cam.zoom;
    this.roundRect(node.x - node.w / 2, node.y - h / 2, node.w, h, 8);
    ctx.stroke();
  }

  // ── Selection ring ────────────────────────────────────────────────────

  private drawSelectRing(node: RenderNode, overrideH?: number): void {
    const { ctx, cam, c } = this;
    const h = overrideH ?? node.h;
    ctx.strokeStyle = c.selectRing;
    ctx.lineWidth = 3 / cam.zoom;
    this.roundRect(node.x - node.w / 2 - 4, node.y - h / 2 - 4, node.w + 8, h + 8, 12);
    ctx.stroke();
  }

  // ── Tooltip ───────────────────────────────────────────────────────────

  /** Canvas-rendered tooltip in screen space. */
  private drawTooltip(text: string, sx: number, sy: number): void {
    const { ctx, c } = this;
    const fontSize = 12;
    const padding = 8;
    const offsetY = 20;
    const radius = 6;

    ctx.font = `400 ${fontSize}px system-ui, sans-serif`;
    const tw = ctx.measureText(text).width;
    const boxW = tw + padding * 2;
    const boxH = fontSize + padding * 2;

    // Position: centered below cursor, clamped to canvas bounds.
    let x = sx - boxW / 2;
    let y = sy + offsetY;
    const maxX = this.cam.screenW - boxW - 4;
    const maxY = this.cam.screenH - boxH - 4;
    if (x < 4) x = 4;
    if (x > maxX) x = maxX;
    if (y > maxY) y = sy - offsetY - boxH; // flip above cursor

    // Background.
    ctx.fillStyle = 'rgba(20, 22, 30, 0.92)';
    this.roundRect(x, y, boxW, boxH, radius);
    ctx.fill();

    // Text.
    ctx.fillStyle = c.text;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(text, x + boxW / 2, y + boxH / 2, boxW - padding * 2);
  }

  // ── Minimap ────────────────────────────────────────────────────────────

  renderMinimap(miniCtx: CanvasRenderingContext2D, mw: number, mh: number, graph: Graph): void {
    const { dpr, c } = this;
    miniCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
    miniCtx.fillStyle = c.minimap;
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
    miniCtx.strokeStyle = c.edge;
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
    miniCtx.fillStyle = c.accent;
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
    miniCtx.strokeStyle = c.selectRing;
    miniCtx.lineWidth = 1.5;
    miniCtx.strokeRect(vx, vy, vw, vh);
    miniCtx.fillStyle = c.minimapVp;
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
