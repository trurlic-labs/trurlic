import { Camera } from './camera';
import { Graph, ApiClient, WsConnection } from './graph';
import { ForceLayout } from './layout';
import { Panel } from './panel';
import { Renderer } from './renderer';
import { LOD, computeLOD } from './types';
import { search, neighborhood } from './search';
import type { AABB } from './quadtree';
import type { SearchResult } from './search';

class App {
  private graph = new Graph();
  private camera = new Camera();
  private renderer: Renderer;
  private layout = new ForceLayout();
  private panel: Panel;
  private miniCtx: CanvasRenderingContext2D;
  private api: ApiClient;
  private aria: HTMLElement;

  private selected: string | null = null;
  private dragging: string | null = null;
  private panning = false;
  private lastMouse = { x: 0, y: 0 };
  private needsRender = true;
  private layoutSaveTimer: number | null = null;
  private lod: LOD = LOD.Overview;
  private visibleCount = 0;

  // Search state.
  private searchOpen = false;
  private searchResults: SearchResult[] = [];
  private searchActiveIndex = -1;
  private focusSet: Set<string> | null = null;

  // Minimap state — set after each minimap render.
  private minimapTransform: {
    minX: number;
    minY: number;
    scale: number;
    ox: number;
    oy: number;
    mw: number;
    mh: number;
  } | null = null;
  private minimapDragging = false;

  // Component tab-cycling order.
  private componentNames: string[] = [];

  constructor() {
    const token = new URLSearchParams(location.search).get('token') ?? '';
    this.api = new ApiClient(token);

    const canvas = document.getElementById('canvas') as HTMLCanvasElement;
    this.renderer = new Renderer(canvas, this.camera);
    this.panel = new Panel(document.getElementById('panel')!);
    this.panel.init(this.api, {
      onNavigate: (name) => this.selectAndFocus(name),
      onMutated: () => this.reloadGraph(),
    });
    this.aria = document.getElementById('aria-live')!;

    const minimap = document.getElementById('minimap') as HTMLCanvasElement;
    const mctx = minimap.getContext('2d');
    if (!mctx) throw new Error('minimap context');
    this.miniCtx = mctx;

    this.setupEvents(canvas, minimap);
    this.setupSearch();
    this.handleResize();
    window.addEventListener('resize', () => this.handleResize());

    this.api
      .fetchGraph()
      .then((snap) => {
        this.graph.loadSnapshot(snap);
        this.componentNames = [...this.graph.nodes.keys()].sort();
        this.layout.run(this.graph.nodes, this.graph.edges, 200);
        this.graph.rebuildQuadtree();
        this.fitView();
        this.updateLOD();
        this.panel.showProject(this.graph);
        this.needsRender = true;
      })
      .catch((e) => {
        console.error('Failed to load graph:', e);
        this.panel.showEmpty();
      });

    new WsConnection(token, (ev) => this.handleWsEvent(ev));
    this.renderLoop();
  }

  // ── LOD ─────────────────────────────────────────────────────────────────

  private updateLOD(): void {
    const vp = this.camera.viewport();
    const vpAABB: AABB = {
      cx: vp.x + vp.w / 2,
      cy: vp.y + vp.h / 2,
      hw: vp.w / 2,
      hh: vp.h / 2,
    };
    const visible = this.graph.quadtree.queryViewport(vpAABB);
    this.visibleCount = new Set(visible).size;
    this.lod = computeLOD(this.visibleCount);
  }

  // ── Canvas events ───────────────────────────────────────────────────────

  private setupEvents(canvas: HTMLCanvasElement, minimap: HTMLCanvasElement): void {
    canvas.addEventListener('pointerdown', (e) => this.onPointerDown(e));
    canvas.addEventListener('pointermove', (e) => this.onPointerMove(e));
    canvas.addEventListener('pointerup', () => this.onPointerUp());
    canvas.addEventListener('pointerleave', () => this.onPointerUp());
    canvas.addEventListener('wheel', (e) => this.onWheel(e), { passive: false });

    // Minimap interaction.
    minimap.addEventListener('pointerdown', (e) => this.onMinimapDown(e));
    minimap.addEventListener('pointermove', (e) => this.onMinimapMove(e));
    minimap.addEventListener('pointerup', () => {
      this.minimapDragging = false;
    });
    minimap.addEventListener('pointerleave', () => {
      this.minimapDragging = false;
    });

    window.addEventListener('keydown', (e) => this.onKeyDown(e));
  }

  private onPointerDown(e: PointerEvent): void {
    if (this.searchOpen) {
      this.closeSearch();
    }
    const canvas = e.target as HTMLCanvasElement;
    canvas.setPointerCapture(e.pointerId);
    const wx = this.camera.toWorldX(e.offsetX);
    const wy = this.camera.toWorldY(e.offsetY);
    const hit = this.graph.nodeAt(wx, wy);

    if (hit) {
      this.dragging = hit.name;
      this.selected = hit.name;
      this.focusSet = null;
      this.panel.showComponent(hit, this.graph);
      this.announce(`Selected component: ${hit.name}`);
    } else {
      this.panning = true;
      this.selected = null;
      this.focusSet = null;
      this.panel.showProject(this.graph);
    }
    this.lastMouse = { x: e.offsetX, y: e.offsetY };
    this.needsRender = true;
  }

  private onPointerMove(e: PointerEvent): void {
    const dx = e.offsetX - this.lastMouse.x;
    const dy = e.offsetY - this.lastMouse.y;
    this.lastMouse = { x: e.offsetX, y: e.offsetY };

    if (this.panning) {
      this.camera.pan(dx, dy);
      this.updateLOD();
      this.needsRender = true;
    } else if (this.dragging) {
      const node = this.graph.nodes.get(this.dragging);
      if (node) {
        node.x += dx / this.camera.zoom;
        node.y += dy / this.camera.zoom;
        node.pinned = true;
        this.needsRender = true;
      }
    }
  }

  private onPointerUp(): void {
    if (this.dragging) {
      this.graph.rebuildQuadtree();
      this.updateLOD();
      this.scheduleLayoutSave();
    }
    this.dragging = null;
    this.panning = false;
  }

  private onWheel(e: WheelEvent): void {
    e.preventDefault();
    const factor = e.deltaY > 0 ? 0.9 : 1.1;
    this.camera.zoomAt(e.offsetX, e.offsetY, factor);
    this.updateLOD();
    this.needsRender = true;
  }

  // ── Keyboard ────────────────────────────────────────────────────────────

  private onKeyDown(e: KeyboardEvent): void {
    // Search: Ctrl+F / Cmd+F / `/`
    if (e.key === '/' || ((e.ctrlKey || e.metaKey) && e.key === 'f')) {
      e.preventDefault();
      this.openSearch();
      return;
    }

    // If search is open, let its own handlers take over.
    if (this.searchOpen) return;

    // Escape: clear focus → clear selection → noop.
    if (e.key === 'Escape') {
      if (this.focusSet) {
        this.focusSet = null;
        this.needsRender = true;
        return;
      }
      this.selected = null;
      this.panel.showProject(this.graph);
      this.announce('Selection cleared');
      this.needsRender = true;
      return;
    }

    // Zoom to fit: Ctrl+0 / Cmd+0
    if ((e.ctrlKey || e.metaKey) && e.key === '0') {
      e.preventDefault();
      this.fitView();
      return;
    }

    // Zoom +/-
    if (e.key === '=' || e.key === '+') {
      this.camera.zoomAt(this.camera.screenW / 2, this.camera.screenH / 2, 1.15);
      this.updateLOD();
      this.needsRender = true;
      return;
    }
    if (e.key === '-') {
      this.camera.zoomAt(this.camera.screenW / 2, this.camera.screenH / 2, 0.87);
      this.updateLOD();
      this.needsRender = true;
      return;
    }

    // Arrow key pan (40px per press).
    const PAN = 40;
    if (e.key === 'ArrowLeft') {
      this.camera.pan(PAN, 0);
      this.updateLOD();
      this.needsRender = true;
      return;
    }
    if (e.key === 'ArrowRight') {
      this.camera.pan(-PAN, 0);
      this.updateLOD();
      this.needsRender = true;
      return;
    }
    if (e.key === 'ArrowUp') {
      this.camera.pan(0, PAN);
      this.updateLOD();
      this.needsRender = true;
      return;
    }
    if (e.key === 'ArrowDown') {
      this.camera.pan(0, -PAN);
      this.updateLOD();
      this.needsRender = true;
      return;
    }

    // Tab: cycle through components.
    if (e.key === 'Tab' && this.componentNames.length > 0) {
      e.preventDefault();
      const dir = e.shiftKey ? -1 : 1;
      const curIdx = this.selected ? this.componentNames.indexOf(this.selected) : -1;
      let next = curIdx + dir;
      if (next < 0) next = this.componentNames.length - 1;
      if (next >= this.componentNames.length) next = 0;
      const name = this.componentNames[next];
      this.selectAndFocus(name);
      return;
    }

    // Enter: zoom to selected component neighborhood.
    if (e.key === 'Enter' && this.selected) {
      this.zoomToNode(this.selected);
      return;
    }
  }

  // ── Search ──────────────────────────────────────────────────────────────

  private setupSearch(): void {
    const input = document.getElementById('search-input') as HTMLInputElement;
    const results = document.getElementById('search-results')!;

    input.addEventListener('input', () => {
      this.searchResults = search(this.graph, input.value);
      this.searchActiveIndex = this.searchResults.length > 0 ? 0 : -1;
      this.renderSearchResults(results);
    });

    input.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        this.closeSearch();
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (this.searchActiveIndex < this.searchResults.length - 1) {
          this.searchActiveIndex++;
          this.renderSearchResults(results);
        }
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (this.searchActiveIndex > 0) {
          this.searchActiveIndex--;
          this.renderSearchResults(results);
        }
        return;
      }
      if (e.key === 'Enter') {
        e.preventDefault();
        if (this.searchActiveIndex >= 0 && this.searchActiveIndex < this.searchResults.length) {
          this.selectSearchResult(this.searchResults[this.searchActiveIndex]);
        }
        return;
      }
    });
  }

  private openSearch(): void {
    const bar = document.getElementById('search-bar')!;
    const input = document.getElementById('search-input') as HTMLInputElement;
    bar.classList.remove('hidden');
    input.value = '';
    input.focus();
    this.searchOpen = true;
    this.searchResults = [];
    this.searchActiveIndex = -1;
    document.getElementById('search-results')!.innerHTML = '';
  }

  private closeSearch(): void {
    document.getElementById('search-bar')!.classList.add('hidden');
    this.searchOpen = false;
    this.searchResults = [];
    this.searchActiveIndex = -1;
  }

  private renderSearchResults(el: HTMLElement): void {
    if (this.searchResults.length === 0) {
      el.innerHTML = '';
      return;
    }
    el.innerHTML = this.searchResults
      .map((r, i) => {
        const active = i === this.searchActiveIndex ? ' active' : '';
        const kind = `<span class="search-result-kind">${esc(r.kind)}</span>`;
        return `<div class="search-result${active}" data-idx="${i}">${kind}${esc(r.label)}</div>`;
      })
      .join('');

    // Click handler on results.
    for (const child of el.children) {
      child.addEventListener('click', () => {
        const idx = parseInt((child as HTMLElement).dataset.idx ?? '-1', 10);
        if (idx >= 0 && idx < this.searchResults.length) {
          this.selectSearchResult(this.searchResults[idx]);
        }
      });
    }
  }

  private selectSearchResult(result: SearchResult): void {
    this.closeSearch();

    if (result.kind === 'component') {
      this.selectAndFocus(result.name);
      this.focusSet = neighborhood(this.graph, result.name);
    } else if (result.kind === 'decision') {
      // Focus the parent component.
      const dec = this.graph.decisions.get(result.name);
      if (dec) {
        this.selectAndFocus(dec.component);
        this.focusSet = neighborhood(this.graph, dec.component);
      }
    } else if (result.kind === 'pattern') {
      // Focus the first applied component.
      const pat = this.graph.patterns.get(result.name);
      if (pat && pat.components.length > 0) {
        this.selectAndFocus(pat.components[0]);
        this.focusSet = neighborhood(this.graph, pat.components[0]);
      }
    }

    this.needsRender = true;
  }

  // ── Minimap ─────────────────────────────────────────────────────────────

  private onMinimapDown(e: PointerEvent): void {
    (e.target as HTMLCanvasElement).setPointerCapture(e.pointerId);
    this.minimapDragging = true;
    this.jumpToMinimapPoint(e.offsetX, e.offsetY);
  }

  private onMinimapMove(e: PointerEvent): void {
    if (!this.minimapDragging) return;
    this.jumpToMinimapPoint(e.offsetX, e.offsetY);
  }

  private jumpToMinimapPoint(sx: number, sy: number): void {
    const t = this.minimapTransform;
    if (!t) return;
    const wx = t.minX + (sx - t.ox) / t.scale;
    const wy = t.minY + (sy - t.oy) / t.scale;
    this.camera.cx = wx;
    this.camera.cy = wy;
    this.updateLOD();
    this.needsRender = true;
  }

  // ── Helpers ─────────────────────────────────────────────────────────────

  private selectAndFocus(name: string): void {
    const node = this.graph.nodes.get(name);
    if (!node) return;
    this.selected = name;
    this.panel.showComponent(node, this.graph);
    this.announce(`Selected component: ${name}`);
    this.zoomToNode(name);
  }

  private zoomToNode(name: string): void {
    const node = this.graph.nodes.get(name);
    if (!node) return;
    const pad = 300;
    this.camera.fitBounds(node.x - pad, node.y - pad, node.x + pad, node.y + pad);
    this.updateLOD();
    this.needsRender = true;
  }

  private announce(text: string): void {
    this.aria.textContent = text;
  }

  // ── WebSocket ───────────────────────────────────────────────────────────

  private handleWsEvent(_event: { type: string; [k: string]: unknown }): void {
    this.reloadGraph();
  }

  /** Reload the full graph from the server. Called on WebSocket events
   *  and after panel mutations (edit, delete). */
  private reloadGraph(): void {
    this.api
      .fetchGraph()
      .then((snap) => {
        this.graph.loadSnapshot(snap);
        this.componentNames = [...this.graph.nodes.keys()].sort();
        this.layout.run(this.graph.nodes, this.graph.edges, 50);
        this.graph.rebuildQuadtree();
        this.updateLOD();
        this.needsRender = true;
        this.refreshPanel();
      })
      .catch((e) => console.error('Reload failed:', e));
  }

  private refreshPanel(): void {
    if (!this.selected) {
      this.panel.showProject(this.graph);
      return;
    }
    const node = this.graph.nodes.get(this.selected);
    if (node) {
      this.panel.showComponent(node, this.graph);
    } else {
      this.selected = null;
      this.panel.showProject(this.graph);
    }
  }

  // ── Layout persistence ──────────────────────────────────────────────────

  private scheduleLayoutSave(): void {
    if (this.layoutSaveTimer != null) clearTimeout(this.layoutSaveTimer);
    this.layoutSaveTimer = window.setTimeout(() => this.saveLayout(), 500);
  }

  private saveLayout(): void {
    const positions: Record<string, { x: number; y: number; pinned: boolean }> = {};
    for (const [name, n] of this.graph.nodes) {
      if (n.pinned) positions[name] = { x: n.x, y: n.y, pinned: true };
    }
    this.api
      .saveLayout(positions, this.graph.layoutVersion)
      .then((v) => {
        this.graph.layoutVersion = v;
      })
      .catch((e) => console.error('Layout save failed:', e));
  }

  // ── Render loop ─────────────────────────────────────────────────────────

  private renderLoop = (): void => {
    // Advance camera animation — forces a render if the camera moved.
    if (this.camera.tick()) {
      this.updateLOD();
      this.needsRender = true;
    }

    if (this.needsRender) {
      this.renderer.render(this.graph, this.selected, this.lod, this.focusSet);
      this.minimapTransform = this.renderMinimap();
      this.needsRender = false;
    }
    requestAnimationFrame(this.renderLoop);
  };

  /**
   * Render the minimap and return the transform used, so minimap
   * click/drag can convert screen→world coordinates.
   */
  private renderMinimap(): typeof this.minimapTransform {
    const mw = 180;
    const mh = 120;
    this.renderer.renderMinimap(this.miniCtx, mw, mh, this.graph);

    if (this.graph.nodes.size === 0) return null;
    let minX = Infinity,
      minY = Infinity,
      maxX = -Infinity,
      maxY = -Infinity;
    for (const n of this.graph.nodes.values()) {
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
    return { minX, minY, scale, ox, oy, mw, mh };
  }

  private fitView(): void {
    let minX = Infinity,
      minY = Infinity,
      maxX = -Infinity,
      maxY = -Infinity;
    for (const n of this.graph.nodes.values()) {
      minX = Math.min(minX, n.x - n.w / 2);
      minY = Math.min(minY, n.y - n.h / 2);
      maxX = Math.max(maxX, n.x + n.w / 2);
      maxY = Math.max(maxY, n.y + n.h / 2);
    }
    if (this.graph.nodes.size > 0) {
      this.camera.fitBounds(minX, minY, maxX, maxY);
    }
    this.focusSet = null;
    this.updateLOD();
    this.needsRender = true;
  }

  private handleResize(): void {
    const panel = document.getElementById('panel')!;
    const w = window.innerWidth - panel.offsetWidth;
    const h = window.innerHeight;
    this.renderer.resize(w, h);

    const minimap = document.getElementById('minimap') as HTMLCanvasElement;
    const dpr = window.devicePixelRatio || 1;
    minimap.width = 180 * dpr;
    minimap.height = 120 * dpr;

    this.updateLOD();
    this.needsRender = true;
  }
}

function esc(s: string): string {
  const el = document.createElement('span');
  el.textContent = s;
  return el.innerHTML;
}

new App();
