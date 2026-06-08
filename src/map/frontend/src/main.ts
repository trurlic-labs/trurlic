import { Camera } from './renderer/camera';
import { Graph } from './state/graph';
import { ApiClient } from './state/api';
import { WsConnection } from './state/ws';
import type { WsState } from './state/ws';
import { ForceLayout } from './layout/force';
import { Panel } from './ui/panel';
import { Renderer } from './renderer/canvas';
import { LOD, computeLOD } from './renderer/lod';
import { search, neighborhood } from './ui/search';
import { CommandPalette } from './ui/command';
import type { PaletteAction } from './ui/command';
import { Breadcrumb } from './ui/breadcrumb';
import { Toolbar } from './ui/toolbar';
import { KeyboardDispatch, Keys } from './interaction/keyboard';
import type { AABB } from './renderer/culling';
import type { SearchResult } from './ui/search';
import type { FilterState } from './types';

// ── Undo / Redo ──────────────────────────────────────────────────────────

interface UndoCommand {
  readonly description: string;
  undo(): Promise<void>;
  redo(): Promise<void>;
}

class UndoStack {
  private undos: UndoCommand[] = [];
  private redos: UndoCommand[] = [];
  private readonly limit = 50;

  push(cmd: UndoCommand): void {
    this.undos.push(cmd);
    if (this.undos.length > this.limit) this.undos.shift();
    this.redos.length = 0;
  }

  async undo(): Promise<string | null> {
    const cmd = this.undos.pop();
    if (!cmd) return null;
    try {
      await cmd.undo();
      this.redos.push(cmd);
      return cmd.description;
    } catch (e) {
      console.error('Undo failed:', e);
      return null;
    }
  }

  async redo(): Promise<string | null> {
    const cmd = this.redos.pop();
    if (!cmd) return null;
    try {
      await cmd.redo();
      this.undos.push(cmd);
      return cmd.description;
    } catch (e) {
      console.error('Redo failed:', e);
      return null;
    }
  }

  canUndo(): boolean {
    return this.undos.length > 0;
  }
  canRedo(): boolean {
    return this.redos.length > 0;
  }
}

// ── App ──────────────────────────────────────────────────────────────────

class App {
  private graph = new Graph();
  private camera = new Camera();
  private renderer: Renderer;
  private layout = new ForceLayout();
  private panel: Panel;
  private miniCtx: CanvasRenderingContext2D;
  private api: ApiClient;
  private undoStack = new UndoStack();
  private palette: CommandPalette;
  private breadcrumb: Breadcrumb;
  private toolbar: Toolbar;
  private aria: HTMLElement;
  private filters: FilterState | undefined;

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

    this.palette = new CommandPalette();
    this.breadcrumb = new Breadcrumb({
      onProject: () => {
        this.selected = null;
        this.clearFocus();
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
        this.componentNames = [...this.graph.nodes.keys()].sort();
        this.layout.run(this.graph.nodes, this.graph.edges, 200);
        this.graph.rebuildQuadtree();
        this.fitView();
        this.updateLOD();
        this.panel.showProject(this.graph);
        this.breadcrumb.update(this.graph.projectName, null);
        this.toolbar.setAvailableTags(this.graph.allTags());
        this.needsRender = true;
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
    this.visibleCount = new Set(visible).size;
    this.lod = computeLOD(this.visibleCount, vp.w * vp.h);
  }

  // ── Canvas pointer events ───────────────────────────────────────────────

  private setupCanvasEvents(canvas: HTMLCanvasElement, minimap: HTMLCanvasElement): void {
    canvas.addEventListener('pointerdown', (e) => this.onPointerDown(e));
    canvas.addEventListener('pointermove', (e) => this.onPointerMove(e));
    canvas.addEventListener('pointerup', () => this.onPointerUp());
    canvas.addEventListener('pointerleave', () => this.onPointerUp());
    canvas.addEventListener('wheel', (e) => this.onWheel(e), { passive: false });

    minimap.addEventListener('pointerdown', (e) => this.onMinimapDown(e));
    minimap.addEventListener('pointermove', (e) => this.onMinimapMove(e));
    minimap.addEventListener('pointerup', () => {
      this.minimapDragging = false;
    });
    minimap.addEventListener('pointerleave', () => {
      this.minimapDragging = false;
    });
  }

  private onPointerDown(e: PointerEvent): void {
    if (this.searchOpen) this.closeSearch();
    if (this.palette.isOpen) this.palette.close();

    const canvas = e.target as HTMLCanvasElement;
    canvas.setPointerCapture(e.pointerId);
    const wx = this.camera.toWorldX(e.offsetX);
    const wy = this.camera.toWorldY(e.offsetY);
    const hit = this.graph.nodeAt(wx, wy);

    if (hit) {
      this.dragging = hit.name;
      this.selected = hit.name;
      this.clearFocus();
      this.panel.showComponent(hit, this.graph);
      this.announce(`Selected component: ${hit.name}`);
    } else {
      this.panning = true;
      this.selected = null;
      this.clearFocus();
      this.panel.showProject(this.graph);
    }
    this.breadcrumb.update(this.graph.projectName, this.selected);
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
      { match: () => this.palette.isOpen, run: () => {} }, // palette handles its own keys
      {
        match: Keys.search,
        run: (e) => {
          e.preventDefault();
          this.openSearch();
        },
      },
      { match: () => this.searchOpen, run: () => {} }, // search handles its own keys
      {
        match: Keys.undo,
        run: (e) => {
          e.preventDefault();
          this.undoStack.undo().then((d) => {
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
          this.undoStack.redo().then((d) => {
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
          if (this.focusSet) {
            this.clearFocus();
            this.needsRender = true;
          } else if (this.selected) {
            this.selected = null;
            this.clearFocus();
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
          if (this.componentNames.length === 0) return;
          e.preventDefault();
          const dir = e.shiftKey ? -1 : 1;
          const curIdx = this.selected ? this.componentNames.indexOf(this.selected) : -1;
          let next = curIdx + dir;
          if (next < 0) next = this.componentNames.length - 1;
          if (next >= this.componentNames.length) next = 0;
          this.selectAndFocus(this.componentNames[next]);
        },
      },
      {
        match: (e) => Keys.enter(e) && this.selected !== null,
        run: () => {
          this.setFocus(this.selected!);
          this.zoomToNode(this.selected!);
          this.needsRender = true;
        },
      },
      {
        match: (e) => {
          if (!Keys.del(e) || !this.selected) return false;
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

    if (this.undoStack.canUndo()) {
      actions.push({
        label: 'Undo',
        shortcut: 'Ctrl+Z',
        run: () => {
          this.undoStack.undo().then((d) => {
            if (d) this.reloadGraph();
          });
        },
      });
    }
    if (this.undoStack.canRedo()) {
      actions.push({
        label: 'Redo',
        shortcut: 'Ctrl+Shift+Z',
        run: () => {
          this.undoStack.redo().then((d) => {
            if (d) this.reloadGraph();
          });
        },
      });
    }

    for (const name of this.componentNames) {
      actions.push({
        label: `Focus: ${name}`,
        run: () => this.selectAndFocus(name),
      });
    }

    return actions;
  }

  // ── Filter state ─────────────────────────────────────────────────────────

  private onFilterChange(state: FilterState): void {
    this.filters = state;
    // Focus mode toggled on: activate neighborhood if a node is selected.
    if (state.focusMode && this.selected) {
      this.setFocus(this.selected);
    } else if (!state.focusMode) {
      this.clearFocus();
    }
    this.needsRender = true;
  }

  // ── Delete selected ─────────────────────────────────────────────────────

  private deleteSelected(): void {
    const name = this.selected;
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
          this.undoStack.push({
            description: `delete component ${name}`,
            undo: () => this.api.createComponent(name, desc),
            redo: () => this.api.deleteComponent(name),
          });
          this.selected = null;
          this.breadcrumb.update(this.graph.projectName, null);
          this.reloadGraph();
        })
        .catch((e) => alert(e.message));
    } else if (decision) {
      if (!confirm(`Delete decision "${name}"?`)) return;
      this.api
        .deleteDecision(name)
        .then(() => {
          this.undoStack.push({
            description: `delete decision ${name}`,
            undo: () =>
              Promise.reject(new Error('Decision deletion cannot be undone via the map API')),
            redo: () => this.api.deleteDecision(name),
          });
          this.selected = null;
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
      this.setFocus(result.name);
    } else if (result.kind === 'decision') {
      const dec = this.graph.decisions.get(result.name);
      if (dec) {
        this.selectAndFocus(dec.component);
        this.setFocus(dec.component);
      }
    } else if (result.kind === 'pattern') {
      const pat = this.graph.patterns.get(result.name);
      if (pat && pat.components.length > 0) {
        this.selectAndFocus(pat.components[0]);
        this.setFocus(pat.components[0]);
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

  /** Clear the focus set and sync the toolbar pill. */
  private clearFocus(): void {
    this.focusSet = null;
    this.toolbar.setFocusActive(false);
  }

  /** Set focus to a node's neighborhood and sync the toolbar pill. */
  private setFocus(name: string): void {
    this.focusSet = neighborhood(this.graph, name);
    this.toolbar.setFocusActive(true);
  }

  private selectAndFocus(name: string): void {
    const node = this.graph.nodes.get(name);
    if (!node) return;
    this.selected = name;
    this.panel.showComponent(node, this.graph);
    this.announce(`Selected component: ${name}`);
    this.breadcrumb.update(this.graph.projectName, name);
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

  private handleWsEvent(event: { type: string; [k: string]: unknown }): void {
    switch (event.type) {
      case 'node_removed': {
        const name = event.name as string | undefined;
        if (!name) break;
        this.graph.removeNode(name);
        if (this.selected === name) {
          this.selected = null;
          this.breadcrumb.update(this.graph.projectName, null);
          this.panel.showProject(this.graph);
        }
        this.componentNames = [...this.graph.nodes.keys()].sort();
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
        // node_added, node_updated, full_reload — event payload doesn't
        // carry enough data for client-side apply. Full fetch required.
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
        this.componentNames = [...this.graph.nodes.keys()].sort();
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
    if (!this.selected) {
      this.panel.showProject(this.graph);
      return;
    }
    const node = this.graph.nodes.get(this.selected);
    if (node) {
      this.panel.showComponent(node, this.graph);
    } else {
      this.selected = null;
      this.breadcrumb.update(this.graph.projectName, null);
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
    if (this.camera.tick()) {
      this.updateLOD();
      this.needsRender = true;
    }

    if (this.needsRender) {
      this.renderer.render(this.graph, this.selected, this.lod, this.focusSet, this.filters);
      this.minimapTransform = this.renderMinimap();
      this.needsRender = false;
    }
    requestAnimationFrame(this.renderLoop);
  };

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
    this.clearFocus();
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
