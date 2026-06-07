import { Camera } from './camera';
import { Graph, ApiClient, WsConnection } from './graph';
import { ForceLayout } from './layout';
import { Panel } from './panel';
import { Renderer } from './renderer';

class App {
  private graph = new Graph();
  private camera = new Camera();
  private renderer: Renderer;
  private layout = new ForceLayout();
  private panel: Panel;
  private miniCtx: CanvasRenderingContext2D;
  private api: ApiClient;
  private selected: string | null = null;
  private dragging: string | null = null;
  private panning = false;
  private lastMouse = { x: 0, y: 0 };
  private needsRender = true;
  private layoutSaveTimer: number | null = null;

  constructor() {
    const token = new URLSearchParams(location.search).get('token') ?? '';
    this.api = new ApiClient(token);

    const canvas = document.getElementById('canvas') as HTMLCanvasElement;
    this.renderer = new Renderer(canvas, this.camera);
    this.panel = new Panel(document.getElementById('panel')!);

    const minimap = document.getElementById('minimap') as HTMLCanvasElement;
    const mctx = minimap.getContext('2d');
    if (!mctx) throw new Error('minimap context');
    this.miniCtx = mctx;

    this.setupEvents(canvas);
    this.handleResize();
    window.addEventListener('resize', () => this.handleResize());

    // Load graph then start render loop.
    this.api
      .fetchGraph()
      .then((snap) => {
        this.graph.loadSnapshot(snap);
        this.layout.run(this.graph.nodes, this.graph.edges, 200);
        this.fitView();
        this.panel.showProject(this.graph);
        this.needsRender = true;
      })
      .catch((e) => {
        console.error('Failed to load graph:', e);
        this.panel.showEmpty();
      });

    // WebSocket for live updates.
    new WsConnection(token, (event) => this.handleWsEvent(event));

    this.renderLoop();
  }

  // ── Events ─────────────────────────────────────────────────────────────

  private setupEvents(canvas: HTMLCanvasElement): void {
    canvas.addEventListener('pointerdown', (e) => this.onPointerDown(e));
    canvas.addEventListener('pointermove', (e) => this.onPointerMove(e));
    canvas.addEventListener('pointerup', () => this.onPointerUp());
    canvas.addEventListener('pointerleave', () => this.onPointerUp());
    canvas.addEventListener('wheel', (e) => this.onWheel(e), { passive: false });

    // Keyboard.
    window.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        this.selected = null;
        this.panel.showProject(this.graph);
        this.needsRender = true;
      }
      // Ctrl+0 / Cmd+0: zoom to fit.
      if ((e.ctrlKey || e.metaKey) && e.key === '0') {
        e.preventDefault();
        this.fitView();
      }
    });
  }

  private onPointerDown(e: PointerEvent): void {
    const canvas = e.target as HTMLCanvasElement;
    canvas.setPointerCapture(e.pointerId);
    const wx = this.camera.toWorldX(e.offsetX);
    const wy = this.camera.toWorldY(e.offsetY);
    const hit = this.graph.nodeAt(wx, wy);

    if (hit) {
      this.dragging = hit.name;
      this.selected = hit.name;
      this.panel.showComponent(hit, this.graph);
    } else {
      this.panning = true;
      this.selected = null;
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
      this.scheduleLayoutSave();
    }
    this.dragging = null;
    this.panning = false;
  }

  private onWheel(e: WheelEvent): void {
    e.preventDefault();
    const factor = e.deltaY > 0 ? 0.9 : 1.1;
    this.camera.zoomAt(e.offsetX, e.offsetY, factor);
    this.needsRender = true;
  }

  // ── WebSocket ──────────────────────────────────────────────────────────

  private handleWsEvent(event: { type: string; [k: string]: unknown }): void {
    if (event.type === 'full_reload') {
      this.api
        .fetchGraph()
        .then((snap) => {
          this.graph.loadSnapshot(snap);
          this.layout.run(this.graph.nodes, this.graph.edges, 50);
          this.needsRender = true;
          if (this.selected) this.refreshPanel();
        })
        .catch((e) => console.error('Reload failed:', e));
    } else {
      // For granular events, just do a full refresh for now.
      // Granular client-side patching is a future optimization.
      this.api
        .fetchGraph()
        .then((snap) => {
          this.graph.loadSnapshot(snap);
          this.needsRender = true;
          if (this.selected) this.refreshPanel();
        })
        .catch(() => {});
    }
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

  // ── Layout persistence ─────────────────────────────────────────────────

  private scheduleLayoutSave(): void {
    if (this.layoutSaveTimer != null) clearTimeout(this.layoutSaveTimer);
    this.layoutSaveTimer = window.setTimeout(() => this.saveLayout(), 500);
  }

  private saveLayout(): void {
    const positions: Record<string, { x: number; y: number; pinned: boolean }> = {};
    for (const [name, n] of this.graph.nodes) {
      if (n.pinned) {
        positions[name] = { x: n.x, y: n.y, pinned: true };
      }
    }
    this.api
      .saveLayout(positions, this.graph.layoutVersion)
      .then((v) => {
        this.graph.layoutVersion = v;
      })
      .catch((e) => console.error('Layout save failed:', e));
  }

  // ── Render loop ────────────────────────────────────────────────────────

  private renderLoop = (): void => {
    if (this.needsRender) {
      this.renderer.render(this.graph, this.selected);
      this.renderer.renderMinimap(this.miniCtx, 180, 120, this.graph);
      this.needsRender = false;
    }
    requestAnimationFrame(this.renderLoop);
  };

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
    this.needsRender = true;
  }

  private handleResize(): void {
    const panel = document.getElementById('panel')!;
    const w = window.innerWidth - panel.offsetWidth;
    const h = window.innerHeight;
    this.renderer.resize(w, h);

    // Minimap.
    const minimap = document.getElementById('minimap') as HTMLCanvasElement;
    const dpr = window.devicePixelRatio || 1;
    minimap.width = 180 * dpr;
    minimap.height = 120 * dpr;

    this.needsRender = true;
  }
}

// Boot.
new App();
