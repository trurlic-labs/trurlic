import { Camera } from './renderer/camera';
import { Graph } from './state/graph';
import { ApiClient } from './state/api';
import { WsConnection } from './state/ws';
import type { WsState } from './state/ws';
import { ForceLayout } from './layout/force';
import { Panel } from './ui/panel';
import { Renderer } from './renderer/canvas';
import { LOD, computeLOD } from './renderer/lod';
import { search } from './ui/search';
import { CommandPalette } from './ui/command';
import type { PaletteAction } from './ui/command';
import { Breadcrumb } from './ui/breadcrumb';
import { Toolbar } from './ui/toolbar';
import { KeyboardDispatch, Keys } from './interaction/keyboard';
import type { AABB } from './renderer/culling';
import type { SearchResult } from './ui/search';
import type { FilterState } from './types';

import { UndoStack } from './app/undo';
import { Selection } from './app/selection';
import { DragState } from './app/drag';
import type { MinimapTransform } from './app/drag';
import { Navigation } from './app/navigation';
import { Filters } from './app/filters';
import { HoverTracker } from './app/hover';
import type { HoverEdge } from './app/hover';
import { pointBezierDistSq } from './renderer/geometry';
import { edgeCurveCP, buildEdgePairSet } from './renderer/edges';

// ── App ──────────────────────────────────────────────────────────────────

class App {
  // Infrastructure — immutable references, no domain state.
  private readonly graph = new Graph();
  private readonly camera = new Camera();
  private readonly renderer: Renderer;
  private readonly layout = new ForceLayout();
  private readonly panel: Panel;
  private readonly api: ApiClient;
  private readonly palette: CommandPalette;
  private readonly breadcrumb: Breadcrumb;
  private readonly toolbar: Toolbar;
  private readonly aria: HTMLElement;
  private readonly miniCtx: CanvasRenderingContext2D;
  private readonly canvas: HTMLCanvasElement;

  // Domain modules — own all mutable application state.
  private readonly undo = new UndoStack();
  private readonly selection = new Selection();
  private readonly drag = new DragState();
  private readonly nav: Navigation;
  private readonly filters = new Filters();
  private readonly hover = new HoverTracker();

  // Render-loop scheduling — derived per frame, not domain state.
  private needsRender = true;
  private lod: LOD = LOD.Overview;
  private visibleCount = 0;

  constructor() {
    const token = new URLSearchParams(location.search).get('token') ?? '';
    this.api = new ApiClient(token);

    const canvas = document.getElementById('canvas') as HTMLCanvasElement;
    this.canvas = canvas;
    this.renderer = new Renderer(canvas, this.camera);
    this.nav = new Navigation(this.camera);

    this.panel = new Panel(document.getElementById('panel')!);
    this.panel.init(this.api, {
      onNavigate: (name) => this.selectAndFocus(name),
      onMutated: () => this.reloadGraph(),
    });
    this.aria = document.getElementById('aria-live')!;

    this.palette = new CommandPalette();
    this.breadcrumb = new Breadcrumb({
      onProject: () => {
        this.selection.select(null);
        this.syncFocusClear();
        this.panel.showProject(this.graph);
        this.fitView();
        this.breadcrumb.update(this.graph.projectName, null);
      },
      onComponent: (name) => this.selectAndFocus(name),
    });

    this.toolbar = new Toolbar((state) => this.onFilterChange(state));

    const minimap = document.getElementById('minimap') as HTMLCanvasElement;
    const mctx = minimap.getContext('2d');
    if (!mctx) throw new Error('minimap context');
    this.miniCtx = mctx;

    this.setupCanvasEvents(canvas, minimap);
    this.setupSearch();
    this.installKeyboard();
    this.handleResize();
    window.addEventListener('resize', () => this.handleResize());

    this.api
      .fetchGraph()
      .then((snap) => {
        this.graph.loadSnapshot(snap);
        this.selection.setComponentNames([...this.graph.nodes.keys()].sort());
        this.layout.run(this.graph.nodes, this.graph.edges, 200);
        this.graph.rebuildQuadtree();
        this.fitView();
        this.updateLOD();
        this.panel.showProject(this.graph);
        this.breadcrumb.update(this.graph.projectName, null);
        this.toolbar.setAvailableTags(this.graph.allTags());
        this.needsRender = true;
        this.showFirstVisitHint();
      })
      .catch((e) => {
        console.error('Failed to load graph:', e);
        this.panel.showEmpty();
      });

    new WsConnection(
      token,
      (ev) => this.handleWsEvent(ev),
      (state) => this.handleWsState(state),
    );
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
    this.visibleCount = visible.size;
    this.lod = computeLOD(this.visibleCount, vp.w * vp.h);
  }

  // ── Canvas pointer events ───────────────────────────────────────────────

  private setupCanvasEvents(canvas: HTMLCanvasElement, minimap: HTMLCanvasElement): void {
    canvas.addEventListener('pointerdown', (e) => this.onPointerDown(e));
    canvas.addEventListener('pointermove', (e) => this.onPointerMove(e));
    canvas.addEventListener('pointerup', () => this.onPointerUp());
    canvas.addEventListener('pointerleave', () => {
      this.onPointerUp();
      this.hover.clear();
      this.canvas.style.cursor = '';
      this.needsRender = true;
    });
    canvas.addEventListener('wheel', (e) => this.onWheel(e), { passive: false });

    minimap.addEventListener('pointerdown', (e) => this.onMinimapDown(e));
    minimap.addEventListener('pointermove', (e) => this.onMinimapMove(e));
    minimap.addEventListener('pointerup', () => this.drag.onMinimapUp());
    minimap.addEventListener('pointerleave', () => this.drag.onMinimapUp());
  }

  private onPointerDown(e: PointerEvent): void {
    if (this.selection.searchOpen) this.closeSearch();
    if (this.palette.isOpen) this.palette.close();

    // Clear hover — no hover effects during drag/pan.
    this.hover.clear();
    this.canvas.style.cursor = '';

    const canvas = e.target as HTMLCanvasElement;
    canvas.setPointerCapture(e.pointerId);
    const wx = this.camera.toWorldX(e.offsetX);
    const wy = this.camera.toWorldY(e.offsetY);
    const hit = this.graph.nodeAt(wx, wy);

    const hitName = this.drag.onPointerDown(hit, e.offsetX, e.offsetY);

    if (hitName && hit) {
      this.selection.select(hitName);
      this.syncFocusClear();
      this.panel.showComponent(hit, this.graph);
      this.announce(`Selected component: ${hitName}`);
    } else {
      this.selection.select(null);
      this.syncFocusClear();
      this.panel.showProject(this.graph);
    }
    this.breadcrumb.update(this.graph.projectName, this.selection.selected);
    this.needsRender = true;
  }

  private onPointerMove(e: PointerEvent): void {
    // During drag/pan: delegate to drag module, no hover tracking.
    if (this.drag.dragging || this.drag.panning) {
      const result = this.drag.onPointerMove(e.offsetX, e.offsetY, this.camera, this.graph);
      if (result.needsRender) {
        if (this.drag.panning) this.updateLOD();
        this.needsRender = true;
      }
      return;
    }

    // Idle: track hover for visual feedback.
    const wx = this.camera.toWorldX(e.offsetX);
    const wy = this.camera.toWorldY(e.offsetY);
    const hit = this.graph.nodeAt(wx, wy);
    const hitName = hit?.name ?? null;
    const hitDesc = hit?.description ?? '';
    const hitEdge = hitName
      ? null
      : findHoveredEdge(this.graph, wx, wy, this.camera.zoom, this.lod, this.filters.state);

    const now = performance.now();
    if (this.hover.update(hitName, hitDesc, hitEdge, e.offsetX, e.offsetY, now)) {
      this.needsRender = true;
    }

    // Cursor: pointer over interactive elements, otherwise default (CSS grab).
    this.canvas.style.cursor = hitName || hitEdge ? 'pointer' : '';
  }

  private onPointerUp(): void {
    const result = this.drag.onPointerUp();
    if (result.nodePositionChanged) {
      this.graph.rebuildQuadtree();
      this.updateLOD();
      this.drag.scheduleLayoutSave(() => this.saveLayout());
    }
  }

  private onWheel(e: WheelEvent): void {
    e.preventDefault();
    const factor = e.deltaY > 0 ? 0.9 : 1.1;
    this.camera.zoomAt(e.offsetX, e.offsetY, factor);
    this.updateLOD();
    this.needsRender = true;
  }

  // ── Minimap ─────────────────────────────────────────────────────────────

  private onMinimapDown(e: PointerEvent): void {
    this.drag.onMinimapDown(e.pointerId, e.target as HTMLCanvasElement);
    this.jumpToMinimapPoint(e.offsetX, e.offsetY);
  }

  private onMinimapMove(e: PointerEvent): void {
    if (!this.drag.minimapDragging) return;
    this.jumpToMinimapPoint(e.offsetX, e.offsetY);
  }

  private jumpToMinimapPoint(sx: number, sy: number): void {
    const world = this.drag.minimapToWorld(sx, sy);
    if (!world) return;
    this.camera.cx = world.wx;
    this.camera.cy = world.wy;
    this.updateLOD();
    this.needsRender = true;
  }

  // ── Keyboard ────────────────────────────────────────────────────────────

  private installKeyboard(): void {
    const PAN = 40;
    const zoomCenter = (f: number) => {
      this.camera.zoomAt(this.camera.screenW / 2, this.camera.screenH / 2, f);
      this.updateLOD();
      this.needsRender = true;
    };
    const pan = (dx: number, dy: number) => {
      this.camera.pan(dx, dy);
      this.updateLOD();
      this.needsRender = true;
    };

    new KeyboardDispatch([
      {
        match: Keys.cmdK,
        run: (e) => {
          e.preventDefault();
          if (this.palette.isOpen) this.palette.close();
          else this.palette.open(this.buildPaletteActions());
        },
      },
      { match: () => this.palette.isOpen, run: () => {} },
      {
        match: Keys.search,
        run: (e) => {
          e.preventDefault();
          this.openSearch();
        },
      },
      { match: () => this.selection.searchOpen, run: () => {} },
      {
        match: Keys.undo,
        run: (e) => {
          e.preventDefault();
          this.undo.undo().then((d) => {
            if (d) {
              this.announce(`Undo: ${d}`);
              this.reloadGraph();
            }
          });
        },
      },
      {
        match: Keys.redo,
        run: (e) => {
          e.preventDefault();
          this.undo.redo().then((d) => {
            if (d) {
              this.announce(`Redo: ${d}`);
              this.reloadGraph();
            }
          });
        },
      },
      {
        match: Keys.escape,
        run: () => {
          if (this.selection.focusSet) {
            this.syncFocusClear();
            this.needsRender = true;
          } else if (this.selection.selected) {
            this.selection.select(null);
            this.syncFocusClear();
            this.panel.showProject(this.graph);
            this.announce('Selection cleared');
            this.breadcrumb.update(this.graph.projectName, null);
            this.needsRender = true;
          }
        },
      },
      {
        match: Keys.zoomFit,
        run: (e) => {
          e.preventDefault();
          this.fitView();
        },
      },
      { match: Keys.zoomIn, run: () => zoomCenter(1.15) },
      { match: Keys.zoomOut, run: () => zoomCenter(0.87) },
      { match: Keys.arrowLeft, run: () => pan(PAN, 0) },
      { match: Keys.arrowRight, run: () => pan(-PAN, 0) },
      { match: Keys.arrowUp, run: () => pan(0, PAN) },
      { match: Keys.arrowDown, run: () => pan(0, -PAN) },
      {
        match: Keys.tab,
        run: (e) => {
          const next = this.selection.cycleComponent(e.shiftKey ? -1 : 1);
          if (next === null) return;
          e.preventDefault();
          this.selectAndFocus(next);
        },
      },
      {
        match: (e) => Keys.enter(e) && this.selection.selected !== null,
        run: () => {
          this.syncFocusSet(this.selection.selected!);
          this.nav.focusNode(this.selection.selected!, this.graph);
          this.updateLOD();
          this.needsRender = true;
        },
      },
      {
        match: (e) => {
          if (!Keys.del(e) || !this.selection.selected) return false;
          const tag = (document.activeElement as HTMLElement)?.tagName;
          if (tag === 'INPUT' || tag === 'TEXTAREA') return false;
          if ((document.activeElement as HTMLElement)?.isContentEditable) return false;
          return true;
        },
        run: (e) => {
          e.preventDefault();
          this.deleteSelected();
        },
      },
    ]).attach();
  }

  // ── Command palette actions ─────────────────────────────────────────────

  private buildPaletteActions(): PaletteAction[] {
    const actions: PaletteAction[] = [
      { label: 'Zoom to fit', shortcut: 'Ctrl+0', run: () => this.fitView() },
      { label: 'Search', shortcut: 'Ctrl+F', run: () => this.openSearch() },
      {
        label: 'Reset layout',
        run: () => {
          if (!confirm('Unpin all nodes and recompute layout? Pinned positions will be lost.'))
            return;
          this.api
            .resetLayout()
            .then((v) => {
              this.graph.layoutVersion = v;
              for (const n of this.graph.nodes.values()) n.pinned = false;
              this.layout.run(this.graph.nodes, this.graph.edges, 200);
              this.graph.rebuildQuadtree();
              this.fitView();
            })
            .catch((e) => console.error('Reset layout failed:', e));
        },
      },
    ];

    if (this.undo.canUndo()) {
      actions.push({
        label: 'Undo',
        shortcut: 'Ctrl+Z',
        run: () => {
          this.undo.undo().then((d) => {
            if (d) this.reloadGraph();
          });
        },
      });
    }
    if (this.undo.canRedo()) {
      actions.push({
        label: 'Redo',
        shortcut: 'Ctrl+Shift+Z',
        run: () => {
          this.undo.redo().then((d) => {
            if (d) this.reloadGraph();
          });
        },
      });
    }

    for (const name of this.selection.componentNames) {
      actions.push({
        label: `Focus: ${name}`,
        run: () => this.selectAndFocus(name),
      });
    }

    return actions;
  }

  // ── Filter state ─────────────────────────────────────────────────────────

  private onFilterChange(state: FilterState): void {
    this.filters.update(state);
    if (state.focusMode && this.selection.selected) {
      this.syncFocusSet(this.selection.selected);
    } else if (!state.focusMode) {
      this.syncFocusClear();
    }
    this.needsRender = true;
  }

  // ── Delete selected ─────────────────────────────────────────────────────

  private deleteSelected(): void {
    const name = this.selection.selected;
    if (!name) return;
    const node = this.graph.nodes.get(name);
    const decision = this.graph.decisions.get(name);

    if (node) {
      const desc = node.description ?? '';
      if (
        !confirm(
          `Delete component "${name}"?\n\nThis cannot be undone if cascade rules block re-creation.`,
        )
      ) {
        return;
      }
      this.api
        .deleteComponent(name)
        .then(() => {
          this.undo.push({
            description: `delete component ${name}`,
            undo: () => this.api.createComponent(name, desc),
            redo: () => this.api.deleteComponent(name),
          });
          this.selection.select(null);
          this.breadcrumb.update(this.graph.projectName, null);
          this.reloadGraph();
        })
        .catch((e) => alert(e.message));
    } else if (decision) {
      if (!confirm(`Delete decision "${name}"?`)) return;
      this.api
        .deleteDecision(name)
        .then(() => {
          this.undo.push({
            description: `delete decision ${name}`,
            undo: () =>
              Promise.reject(new Error('Decision deletion cannot be undone via the map API')),
            redo: () => this.api.deleteDecision(name),
          });
          this.selection.select(null);
          this.breadcrumb.update(this.graph.projectName, null);
          this.reloadGraph();
        })
        .catch((e) => alert(e.message));
    }
  }

  // ── Search ──────────────────────────────────────────────────────────────

  private setupSearch(): void {
    const input = document.getElementById('search-input') as HTMLInputElement;
    const results = document.getElementById('search-results')!;

    input.addEventListener('input', () => {
      this.selection.setSearchResults(search(this.graph, input.value));
      this.renderSearchResults(results);
    });

    input.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        this.closeSearch();
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        this.selection.nextSearchResult();
        this.renderSearchResults(results);
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        this.selection.prevSearchResult();
        this.renderSearchResults(results);
        return;
      }
      if (e.key === 'Enter') {
        e.preventDefault();
        const result = this.selection.activeSearchResult();
        if (result) this.selectSearchResult(result);
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
    this.selection.openSearch();
    document.getElementById('search-results')!.innerHTML = '';
  }

  private closeSearch(): void {
    document.getElementById('search-bar')!.classList.add('hidden');
    this.selection.closeSearch();
  }

  private renderSearchResults(el: HTMLElement): void {
    const results = this.selection.searchResults;
    const activeIndex = this.selection.searchActiveIndex;

    if (results.length === 0) {
      el.innerHTML = '';
      return;
    }
    el.innerHTML = results
      .map((r, i) => {
        const active = i === activeIndex ? ' active' : '';
        const kind = `<span class="search-result-kind">${esc(r.kind)}</span>`;
        return `<div class="search-result${active}" data-idx="${i}">${kind}${esc(r.label)}</div>`;
      })
      .join('');

    for (const child of el.children) {
      child.addEventListener('click', () => {
        const idx = parseInt((child as HTMLElement).dataset.idx ?? '-1', 10);
        if (idx >= 0 && idx < results.length) {
          this.selectSearchResult(results[idx]);
        }
      });
    }
  }

  private selectSearchResult(result: SearchResult): void {
    this.closeSearch();

    if (result.kind === 'component') {
      this.selectAndFocus(result.name);
      this.syncFocusSet(result.name);
    } else if (result.kind === 'decision') {
      const dec = this.graph.decisions.get(result.name);
      if (dec) {
        this.selectAndFocus(dec.component);
        this.syncFocusSet(dec.component);
      }
    } else if (result.kind === 'pattern') {
      const pat = this.graph.patterns.get(result.name);
      if (pat && pat.components.length > 0) {
        this.selectAndFocus(pat.components[0]);
        this.syncFocusSet(pat.components[0]);
      }
    }

    this.needsRender = true;
  }

  // ── Coordination helpers ────────────────────────────────────────────────

  private selectAndFocus(name: string): void {
    const node = this.graph.nodes.get(name);
    if (!node) return;
    this.selection.select(name);
    this.panel.showComponent(node, this.graph);
    this.announce(`Selected component: ${name}`);
    this.breadcrumb.update(this.graph.projectName, name);
    this.nav.focusNode(name, this.graph);
    this.updateLOD();
    this.needsRender = true;
  }

  private syncFocusClear(): void {
    this.selection.clearFocus();
    this.toolbar.setFocusActive(false);
  }

  private syncFocusSet(name: string): void {
    this.selection.setFocus(name, this.graph);
    this.toolbar.setFocusActive(true);
  }

  private announce(text: string): void {
    this.aria.textContent = text;
  }

  // ── First-visit hint ────────────────────────────────────────────────────

  private showFirstVisitHint(): void {
    const autoLayout = ![...this.graph.nodes.values()].some((n) => n.pinned);
    if (!autoLayout) return;
    try {
      if (sessionStorage.getItem('trurlic-hint-shown')) return;
      sessionStorage.setItem('trurlic-hint-shown', '1');
    } catch {
      return; // sessionStorage unavailable (privacy mode).
    }

    const hint = document.getElementById('hint-overlay');
    if (!hint) return;
    hint.classList.remove('hidden');
    setTimeout(() => {
      hint.classList.add('fade-out');
      setTimeout(() => hint.classList.add('hidden'), 600);
    }, 4000);
  }

  // ── WebSocket ───────────────────────────────────────────────────────────

  private handleWsEvent(event: { type: string; [k: string]: unknown }): void {
    switch (event.type) {
      case 'node_removed': {
        const name = event.name as string | undefined;
        if (!name) break;
        this.graph.removeNode(name);
        if (this.selection.selected === name) {
          this.selection.select(null);
          this.breadcrumb.update(this.graph.projectName, null);
          this.panel.showProject(this.graph);
        }
        this.selection.setComponentNames([...this.graph.nodes.keys()].sort());
        this.toolbar.setAvailableTags(this.graph.allTags());
        this.updateLOD();
        this.needsRender = true;
        return;
      }
      case 'edge_added': {
        const edge = event.edge as { from: string; to: string; kind: string } | undefined;
        if (edge) {
          this.graph.addEdge(edge.from, edge.to, edge.kind);
          this.needsRender = true;
        }
        return;
      }
      case 'edge_removed': {
        const from = event.from as string | undefined;
        const to = event.to as string | undefined;
        const kind = event.kind as string | undefined;
        if (from && to && kind) {
          this.graph.removeEdge(from, to, kind);
          this.needsRender = true;
        }
        return;
      }
      default:
        this.reloadGraph();
        return;
    }
  }

  private handleWsState(state: WsState): void {
    const el = document.getElementById('ws-status')!;
    if (state === 'reconnecting') {
      el.classList.remove('hidden');
    } else {
      el.classList.add('hidden');
      this.reloadGraph();
    }
  }

  private reloadGraph(): void {
    this.api
      .fetchGraph()
      .then((snap) => {
        this.graph.loadSnapshot(snap);
        this.selection.setComponentNames([...this.graph.nodes.keys()].sort());
        this.layout.run(this.graph.nodes, this.graph.edges, 50);
        this.graph.rebuildQuadtree();
        this.updateLOD();
        this.needsRender = true;
        this.toolbar.setAvailableTags(this.graph.allTags());
        this.refreshPanel();
      })
      .catch((e) => console.error('Reload failed:', e));
  }

  private refreshPanel(): void {
    const name = this.selection.selected;
    if (!name) {
      this.panel.showProject(this.graph);
      return;
    }
    const node = this.graph.nodes.get(name);
    if (node) {
      this.panel.showComponent(node, this.graph);
    } else {
      this.selection.select(null);
      this.breadcrumb.update(this.graph.projectName, null);
      this.panel.showProject(this.graph);
    }
  }

  // ── Layout persistence ──────────────────────────────────────────────────

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
    const now = performance.now();
    if (this.camera.tick()) {
      this.updateLOD();
      this.needsRender = true;
    }
    if (this.hover.tick(now)) {
      this.needsRender = true;
    }

    if (this.needsRender) {
      const fading = this.renderer.render(
        this.graph,
        this.selection.selected,
        this.lod,
        this.selection.focusSet,
        this.filters.state,
        this.hover,
      );
      this.drag.setMinimapTransform(this.renderMinimap());
      this.needsRender = fading;
    }
    requestAnimationFrame(this.renderLoop);
  };

  private renderMinimap(): MinimapTransform | null {
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
    this.nav.fitAll(this.graph);
    this.syncFocusClear();
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

// ── Edge hit-testing ──────────────────────────────────────────────────────

/** Screen-pixel threshold for edge hover detection. */
const EDGE_HIT_PX = 8;

/**
 * Find the edge nearest to the cursor in world space.
 * Hit-tests against the quadratic Bézier curve (not the straight
 * chord) to match the rendered curvature. Only checks edges visible
 * at the current LOD and filter state.
 * Returns null if no edge is within EDGE_HIT_PX screen pixels.
 */
function findHoveredEdge(
  graph: Graph,
  wx: number,
  wy: number,
  zoom: number,
  lod: LOD,
  filters: FilterState | undefined,
): HoverEdge | null {
  if (lod < LOD.Component) return null;

  const threshold = EDGE_HIT_PX / zoom;
  const threshSq = threshold * threshold;
  let bestDistSq = threshSq;
  let best: HoverEdge | null = null;

  const pairSet = buildEdgePairSet(graph.edges);

  for (const e of graph.edges) {
    if (e.kind === 'belongs_to') continue;
    if (lod === LOD.Overview && e.kind !== 'connects_to') continue;
    if (filters && !filters.edgeKinds.has(e.kind)) continue;

    const a = graph.nodes.get(e.from);
    const b = graph.nodes.get(e.to);
    if (!a || !b) continue;

    const hasBi = pairSet.has(`${e.to}\0${e.from}`);
    const reverse = hasBi && e.from > e.to;
    const { cpx, cpy } = edgeCurveCP(a.x, a.y, b.x, b.y, zoom, reverse);

    const d = pointBezierDistSq(wx, wy, a.x, a.y, cpx, cpy, b.x, b.y);
    if (d < bestDistSq) {
      bestDistSq = d;
      best = { from: e.from, to: e.to, kind: e.kind };
    }
  }

  return best;
}

function esc(s: string): string {
  const el = document.createElement('span');
  el.textContent = s;
  return el.innerHTML;
}

new App();
