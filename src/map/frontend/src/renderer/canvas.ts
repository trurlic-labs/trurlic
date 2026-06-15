import type { Camera } from './camera';
import type { Graph } from '../state/graph';
import type { RenderNode, FilterState, DecisionNode, ColorSnapshot } from '../types';
import { LOD } from './lod';
import type { AABB } from './culling';
import { EDGE_DASH, EDGE_OPACITY, edgeColor, edgeCurveCP, buildEdgePairSet } from './edges';
import { convexHull, expandHull, roundedHullPath, nodeCorners, rayRectIntersect } from './geometry';
import type { HoverRenderState } from '../app/hover';

// ── Per-frame color snapshot ──────────────────────────────────────────────

/** Cached snapshot — only refreshed when the color scheme changes. */
let cachedColors: ColorSnapshot | null = null;

function invalidateColors(): void {
  cachedColors = null;
}

// Listen for system theme changes.
if (typeof matchMedia !== 'undefined') {
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', invalidateColors);
  matchMedia('(prefers-color-scheme: light)').addEventListener('change', invalidateColors);
  matchMedia('(prefers-contrast: more)').addEventListener('change', invalidateColors);
}

function snapshotColors(): ColorSnapshot {
  if (cachedColors !== null) return cachedColors;
  const s = getComputedStyle(document.documentElement);
  const v = (prop: string, fb: string) => s.getPropertyValue(prop).trim() || fb;
  cachedColors = {
    bg: v('--bg', '#1c1c26'),
    surface: v('--surface', '#282832'),
    surfaceHi: v('--surface-hi', '#32323c'),
    border: v('--border', '#3d3d48'),
    text: v('--text', '#e4e4ec'),
    textDim: v('--text-dim', '#8b8b9a'),
    accent: v('--accent', '#e8993a'),
    accentDim: v('--accent-dim', '#a06828'),
    edge: v('--edge', '#4a4a56'),
    edgeDep: v('--edge-dep', '#7aad6a'),
    edgeCon: v('--edge-con', '#c09040'),
    edgeSup: v('--edge-sup', '#a07890'),
    selectRing: v('--select', '#e8993a'),
    badge: v('--badge', '#4a4a56'),
    minimap: v('--minimap-bg', '#1c1c26'),
    minimapVp: v('--minimap-vp', 'rgba(232,153,58,0.25)'),
    gridDot: v('--grid-dot', '#28283230'),
    shadow: v('--shadow', 'rgba(0,0,0,0.25)'),
  };
  return cachedColors;
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

/** Diverse palette for pattern regions (hue degrees). */
const PATTERN_HUES = [30, 200, 150, 340, 60, 270, 100, 310];

/** LOD label fade duration (ms). */
const LOD_FADE_MS = 150;

/** Pattern hull expansion (world units). */
const HULL_EXPAND = 50;

/** Pattern hull corner rounding radius (world units). */
const HULL_RADIUS = 20;

/** Node card corner radius (canvas units, scaled by 1/zoom in world space). */
const NODE_RADIUS = 8;

// ── Renderer ───────────────────────────────────────────────────────────────

export class Renderer {
  private ctx: CanvasRenderingContext2D;
  private cam: Camera;
  private dpr: number;
  /** Per-frame color snapshot — refreshed at the top of render(). */
  private c: ColorSnapshot;
  /** Per-frame hover state — set at the top of render(), read by draw methods. */
  private fh: HoverRenderState | null = null;
  /** Cached edge pair set — rebuilt per render frame. */
  private edgePairSet: Set<string> = new Set();

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

  get cachedEdgePairSet(): Set<string> {
    return this.edgePairSet;
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
    this.edgePairSet = buildEdgePairSet(graph.edges);
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
    ctx.fillRect(0, 0, cam.screenW, cam.screenH);

    const vp = cam.viewport();
    const vpAABB: AABB = {
      cx: vp.x + vp.w / 2,
      cy: vp.y + vp.h / 2,
      hw: vp.w / 2,
      hh: vp.h / 2,
    };

    // Dot grid — fades out below zoom 0.25 for performance and clarity.
    if (cam.zoom > 0.15) {
      this.drawGrid(vp, c);
    }

    const visibleNames = graph.quadtree.queryViewport(vpAABB);

    ctx.save();
    ctx.translate(cam.screenW / 2, cam.screenH / 2);
    ctx.scale(cam.zoom, cam.zoom);
    ctx.translate(-cam.cx, -cam.cy);

    if (lod <= LOD.Component) {
      this.drawPatternRegions(graph, visibleNames, focus, lod, filters);
    }
    this.drawEdges(graph, visibleNames, lod, focus, filters);
    this.drawNodes(graph, visibleNames, selected, lod, focus, filters);

    ctx.restore();

    // Tooltip: rendered in screen space, LOD 0 only.
    if (hover?.tooltipVisible && hover.tooltipText && lod === LOD.Overview) {
      this.drawTooltip(hover.tooltipText, hover.tooltipX, hover.tooltipY);
    }

    // Edge tooltip: screen-space, immediate (no dwell delay).
    if (hover?.edge && hover.edgeTooltipText && !hover?.tooltipVisible) {
      this.drawTooltip(hover.edgeTooltipText, hover.tooltipX, hover.tooltipY);
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
    lod: LOD,
    filters?: FilterState,
  ): void {
    if (graph.patterns.size === 0) return;
    const { ctx, cam, c } = this;
    const prefersLight =
      typeof matchMedia !== 'undefined'
        ? matchMedia('(prefers-color-scheme: light)').matches
        : false;
    const lightness = prefersLight ? 48 : 55;
    const saturation = prefersLight ? 40 : 45;
    const baseFill = prefersLight ? 0.10 : 0.14;
    const baseStroke = 0.45;
    const dimFill = 0.03;
    const labelSize = 13 / cam.zoom;

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
      ctx.fillStyle = `hsla(${hue}, ${saturation}%, ${lightness}%, ${fillAlpha})`;
      roundedHullPath(ctx, expanded, HULL_RADIUS);
      ctx.fill();

      // Stroke.
      ctx.strokeStyle = `hsla(${hue}, ${saturation}%, ${lightness}%, ${baseStroke})`;
      ctx.lineWidth = 2.0 / cam.zoom;
      ctx.stroke();

      // Label: shown at all LOD levels.
      // Overview: shortened name (max 20 chars), bold, larger font.
      // Component: truncated description with background pill.
      // Decision: full description.
      {
        const cx = expanded.reduce((s, p) => s + p.x, 0) / expanded.length;
        const cy = expanded.reduce((s, p) => s + p.y, 0) / expanded.length;

        const isOverview = lod < LOD.Component;
        const rawLabel = isOverview ? pat.name : (pat.description || pat.name);
        const maxLen = isOverview ? 20 : lod >= LOD.Decision ? 80 : 30;
        const label = rawLabel.length > maxLen ? rawLabel.slice(0, maxLen - 1) + '…' : rawLabel;
        const size = isOverview ? 14 / cam.zoom : labelSize;
        const weight = isOverview ? 600 : 400;

        ctx.font = `${weight} ${size}px system-ui, sans-serif`;
        const tw = ctx.measureText(label).width;

        const px = 6 / cam.zoom;
        const py = 3 / cam.zoom;
        ctx.fillStyle = c.bg;
        ctx.globalAlpha = (dimmedByFocus ? 0.15 : 1) * 0.88;
        this.roundRect(
          cx - tw / 2 - px,
          cy - size / 2 - py,
          tw + px * 2,
          size + py * 2,
          4 / cam.zoom,
        );
        ctx.fill();

        ctx.globalAlpha = dimmedByFocus ? 0.15 : 1;
        ctx.fillStyle = c.textDim;
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText(label, cx, cy);
      }

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

    const pairSet = this.edgePairSet;

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

      const kindOpacity = EDGE_OPACITY[e.kind] ?? 0.6;
      ctx.globalAlpha = dimmed ? 0.15 : kindOpacity;

      const color = edgeColor(e.kind, c);
      ctx.strokeStyle = isHovered ? c.accent : color;
      ctx.lineWidth = isHovered ? baseWidth * 2.5 : baseWidth;
      ctx.setLineDash((EDGE_DASH[e.kind] ?? []).map((v) => v / cam.zoom));

      // Bezier control point — reverse offset for bidirectional pairs.
      const hasBi = pairSet.has(`${e.to}\0${e.from}`);
      const reverse = hasBi && e.from > e.to;
      const { cpx, cpy } = edgeCurveCP(a.x, a.y, b.x, b.y, cam.zoom, reverse);

      ctx.beginPath();
      ctx.moveTo(a.x, a.y);
      ctx.quadraticCurveTo(cpx, cpy, b.x, b.y);
      ctx.stroke();

      // Arrowhead — aligned with the curve tangent at B.
      const tdx = b.x - cpx;
      const tdy = b.y - cpy;
      const tlen = Math.sqrt(tdx * tdx + tdy * tdy);
      if (tlen < 1e-10) continue;
      const ux = tdx / tlen;
      const uy = tdy / tlen;
      const headLen = 10 / cam.zoom;
      const margin = 3 / cam.zoom;
      const inter = rayRectIntersect(b.x, b.y, b.w / 2 + margin, b.h / 2 + margin, -ux, -uy);
      const tipX = inter.x;
      const tipY = inter.y;

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
      // connects_to: hover only (already gated). Others: always at LOD Component+.
      if (lod >= LOD.Component && (e.kind !== 'connects_to' || isHovered)) {
        const lx = 0.25 * a.x + 0.5 * cpx + 0.25 * b.x;
        const ly = 0.25 * a.y + 0.5 * cpy + 0.25 * b.y;
        const labelSize = 9 / cam.zoom;
        const label = e.kind.replace(/_/g, ' ');
        ctx.font = `400 ${labelSize}px system-ui, sans-serif`;
        const tw = ctx.measureText(label).width;

        // Background pill for legibility.
        const px = 4 / cam.zoom;
        const py = 2 / cam.zoom;
        const savedAlpha = ctx.globalAlpha;
        ctx.globalAlpha = savedAlpha * 0.75;
        ctx.fillStyle = c.bg;
        this.roundRect(
          lx - tw / 2 - px,
          ly - labelSize - py * 2,
          tw + px * 2,
          labelSize + py * 2,
          3 / cam.zoom,
        );
        ctx.fill();
        ctx.globalAlpha = savedAlpha;

        ctx.fillStyle = isHovered ? c.text : c.textDim;
        ctx.textAlign = 'center';
        ctx.textBaseline = 'bottom';
        ctx.fillText(label, lx, ly - 3 / cam.zoom);
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

      this.drawNodeCompact(node, isSelected, lod >= LOD.Component, graph, filters);
    }
    this.ctx.globalAlpha = 1;
  }

  /**
   * Compact node card — used at every LOD level.
   *
   * The canvas shows architecture (components + edges). Decision detail
   * lives in the panel. At LOD ≥ Component, a one-line description is
   * shown under the name for context while zoomed in.
   */
  private drawNodeCompact(
    node: RenderNode,
    selected: boolean,
    showDescription: boolean,
    graph: Graph,
    filters?: FilterState,
  ): void {
    const { ctx, cam, c } = this;

    if (selected) this.drawSelectRing(node);
    this.drawShadow(node);

    ctx.fillStyle = selected ? c.surfaceHi : c.surface;
    this.roundRect(node.x - node.w / 2, node.y - node.h / 2, node.w, node.h, NODE_RADIUS);
    ctx.fill();

    this.drawNodeBorder(node);

    const fontSize = Math.max(12, 14 / Math.max(cam.zoom, 0.5));
    const hasDesc = showDescription && !!node.description;

    // Vertical layout: name sits higher when description is present.
    const nameY = hasDesc ? node.y - 10 : node.y - 4;

    // Name — monospace, component identifier.
    ctx.font = `600 ${fontSize}px ui-monospace, 'SF Mono', 'Cascadia Code', 'Consolas', monospace`;
    ctx.fillStyle = c.text;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    ctx.fillText(node.name, node.x, nameY, node.w - 16);

    // Description — shown at LOD ≥ Component for context while zoomed in.
    if (hasDesc) {
      const descSize = fontSize * 0.72;
      ctx.font = `400 ${descSize}px system-ui, sans-serif`;
      ctx.fillStyle = c.textDim;
      const desc =
        node.description!.length > 45 ? node.description!.slice(0, 42) + '…' : node.description!;
      ctx.fillText(desc, node.x, node.y + 4, node.w - 16);
    }

    // Decision count badge — reflects active filters.
    const rawCount = node.decisionCount ?? 0;
    const count =
      filters && rawCount > 0
        ? filterDecisions(graph.decisionsFor(node.name), filters).length
        : rawCount;
    if (count > 0) {
      const badge = `${count}`;
      const badgeFontSize = fontSize * 0.7;
      const badgeY = hasDesc ? node.y + 18 : node.y + 8;
      ctx.font = `500 ${badgeFontSize}px system-ui, sans-serif`;
      ctx.fillStyle = c.badge;
      const bw = ctx.measureText(badge).width + 10;
      this.roundRect(node.x - bw / 2, badgeY, bw, badgeFontSize + 6, 4);
      ctx.fill();
      ctx.fillStyle = c.textDim;
      ctx.fillText(badge, node.x, badgeY + (badgeFontSize + 6) / 2, bw);
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
    ctx.fillStyle = 'rgba(17, 15, 13, 0.92)';
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

  // ── Dot grid ──────────────────────────────────────────────────────────

  /** Subtle dot grid for spatial grounding. Drawn in camera space. */
  private drawGrid(vp: { x: number; y: number; w: number; h: number }, c: ColorSnapshot): void {
    const { ctx, cam, dpr } = this;
    const spacing = 60;

    // Fade grid when zoomed out.
    const alpha = Math.min(1, (cam.zoom - 0.15) / 0.35);
    if (alpha <= 0) return;

    ctx.save();
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.translate(cam.screenW / 2, cam.screenH / 2);
    ctx.scale(cam.zoom, cam.zoom);
    ctx.translate(-cam.cx, -cam.cy);

    ctx.fillStyle = c.gridDot;
    ctx.globalAlpha = alpha;

    const startX = Math.floor(vp.x / spacing) * spacing;
    const startY = Math.floor(vp.y / spacing) * spacing;
    const endX = vp.x + vp.w;
    const endY = vp.y + vp.h;
    const halfDot = 0.6 / cam.zoom;
    const dotSize = halfDot * 2;

    // Batch all dots into a single path — one fill() call.
    ctx.beginPath();
    for (let x = startX; x <= endX; x += spacing) {
      for (let y = startY; y <= endY; y += spacing) {
        ctx.rect(x - halfDot, y - halfDot, dotSize, dotSize);
      }
    }
    ctx.fill();

    ctx.globalAlpha = 1;
    ctx.restore();
  }

  // ── Drop shadow ─────────────────────────────────────────────────────

  /** Soft shadow behind node cards for depth. */
  private drawShadow(node: RenderNode, overrideH?: number): void {
    const { ctx, cam, c } = this;
    const h = overrideH ?? node.h;
    const offset = 3 / cam.zoom;
    ctx.fillStyle = c.shadow;
    this.roundRect(node.x - node.w / 2 + offset, node.y - h / 2 + offset, node.w, h, NODE_RADIUS);
    ctx.fill();
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
