import type { Camera } from '../renderer/camera';
import type { Graph } from '../state/graph';
import type { RenderNode } from '../types';

// ── Types ──────────────────────────────────────────────────────────────

/** Cached minimap projection parameters set after each minimap render. */
export interface MinimapTransform {
  minX: number;
  minY: number;
  scale: number;
  ox: number;
  oy: number;
  mw: number;
  mh: number;
}

/** Result of a pointer event handler — tells App what happened. */
export interface DragResult {
  readonly needsRender: boolean;
  readonly nodePositionChanged: boolean;
}

// ── Constants ──────────────────────────────────────────────────────────

const LAYOUT_SAVE_DELAY = 500;

// ── DragState ──────────────────────────────────────────────────────────

/**
 * Node-drag, pan, and minimap-drag state machines.
 *
 * Pointer event handlers return a {@link DragResult} so the
 * coordinator (App) can decide whether to re-render and rebuild
 * spatial indices without inspecting internal state.
 */
export class DragState {
  private _dragging: string | null = null;
  private _panning = false;
  private lastMouse = { x: 0, y: 0 };
  private layoutSaveTimer: number | null = null;

  // Minimap
  private _minimapTransform: MinimapTransform | null = null;
  private _minimapDragging = false;

  // ── Accessors ──────────────────────────────────────────────────────

  get dragging(): string | null {
    return this._dragging;
  }

  get panning(): boolean {
    return this._panning;
  }

  get minimapDragging(): boolean {
    return this._minimapDragging;
  }

  // ── Canvas pointer events ──────────────────────────────────────────

  /**
   * Handle pointer down on the canvas.
   *
   * @param hit  The node under the cursor (from quadtree hit-test), or null.
   * @param sx   Screen-space X of the pointer.
   * @param sy   Screen-space Y of the pointer.
   * @returns    The name of the hit node, or null if panning started.
   */
  onPointerDown(hit: RenderNode | null, sx: number, sy: number): string | null {
    this.lastMouse = { x: sx, y: sy };
    if (hit) {
      this._dragging = hit.name;
      return hit.name;
    }
    this._panning = true;
    return null;
  }

  /**
   * Handle pointer move. Applies camera pan or node drag depending
   * on current state.
   */
  onPointerMove(sx: number, sy: number, camera: Camera, graph: Graph): DragResult {
    const dx = sx - this.lastMouse.x;
    const dy = sy - this.lastMouse.y;
    this.lastMouse = { x: sx, y: sy };

    if (this._panning) {
      camera.pan(dx, dy);
      return { needsRender: true, nodePositionChanged: false };
    }

    if (this._dragging) {
      const node = graph.nodes.get(this._dragging);
      if (node) {
        node.x += dx / camera.zoom;
        node.y += dy / camera.zoom;
        node.pinned = true;
        return { needsRender: true, nodePositionChanged: true };
      }
    }

    return { needsRender: false, nodePositionChanged: false };
  }

  /**
   * Handle pointer up. Returns whether a node was being dragged
   * (caller should rebuild the quadtree and save layout).
   */
  onPointerUp(): DragResult {
    const wasDragging = this._dragging !== null;
    this._dragging = null;
    this._panning = false;
    return { needsRender: false, nodePositionChanged: wasDragging };
  }

  /**
   * Debounced layout persistence. Resets the timer on each call
   * so rapid drags coalesce into a single save.
   */
  scheduleLayoutSave(saveFn: () => void): void {
    if (this.layoutSaveTimer != null) clearTimeout(this.layoutSaveTimer);
    this.layoutSaveTimer = window.setTimeout(saveFn, LAYOUT_SAVE_DELAY);
  }

  // ── Minimap ────────────────────────────────────────────────────────

  setMinimapTransform(t: MinimapTransform | null): void {
    this._minimapTransform = t;
  }

  onMinimapDown(pointerId: number, target: HTMLCanvasElement): void {
    target.setPointerCapture(pointerId);
    this._minimapDragging = true;
  }

  onMinimapUp(): void {
    this._minimapDragging = false;
  }

  /**
   * Convert minimap screen coordinates to world coordinates.
   * Returns null if no minimap transform has been set.
   */
  minimapToWorld(sx: number, sy: number): { wx: number; wy: number } | null {
    const t = this._minimapTransform;
    if (!t) return null;
    return {
      wx: t.minX + (sx - t.ox) / t.scale,
      wy: t.minY + (sy - t.oy) / t.scale,
    };
  }
}
