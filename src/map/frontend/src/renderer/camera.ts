import type { Viewport } from '../types';

/** Cached reduced-motion preference. Updated on change. */
const motionQuery =
  typeof matchMedia !== 'undefined' ? matchMedia('(prefers-reduced-motion: reduce)') : null;
let reducedMotion = motionQuery?.matches ?? false;
motionQuery?.addEventListener?.('change', (e) => {
  reducedMotion = e.matches;
});

/**
 * Camera manages the world→screen coordinate transform.
 * World origin is (0,0) at graph center. Zoom > 1 means closer.
 */
export class Camera {
  /** Center of the viewport in world coordinates. */
  cx = 0;
  cy = 0;
  zoom = 1;
  /** Canvas pixel dimensions (set on resize). */
  screenW = 0;
  screenH = 0;

  private minZoom = 0.05;
  private maxZoom = 8;

  // ── Animation state ──────────────────────────────────────────────────

  private anim: {
    fromCx: number;
    fromCy: number;
    fromZoom: number;
    toCx: number;
    toCy: number;
    toZoom: number;
    start: number;
    duration: number;
  } | null = null;

  /** World → screen. */
  toScreenX(wx: number): number {
    return (wx - this.cx) * this.zoom + this.screenW / 2;
  }
  toScreenY(wy: number): number {
    return (wy - this.cy) * this.zoom + this.screenH / 2;
  }

  /** Screen → world. */
  toWorldX(sx: number): number {
    return (sx - this.screenW / 2) / this.zoom + this.cx;
  }
  toWorldY(sy: number): number {
    return (sy - this.screenH / 2) / this.zoom + this.cy;
  }

  /** Pan by screen-space delta. */
  pan(dsx: number, dsy: number): void {
    this.cx -= dsx / this.zoom;
    this.cy -= dsy / this.zoom;
    this.anim = null;
  }

  /** Zoom centered on a screen-space point. */
  zoomAt(sx: number, sy: number, factor: number): void {
    const wx = this.toWorldX(sx);
    const wy = this.toWorldY(sy);
    this.zoom = Math.max(this.minZoom, Math.min(this.maxZoom, this.zoom * factor));
    this.cx = wx - (sx - this.screenW / 2) / this.zoom;
    this.cy = wy - (sy - this.screenH / 2) / this.zoom;
    this.anim = null;
  }

  /** Animate to fit a bounding box with padding. */
  fitBounds(minX: number, minY: number, maxX: number, maxY: number, padding = 80): void {
    const bw = maxX - minX + padding * 2;
    const bh = maxY - minY + padding * 2;
    if (bw <= 0 || bh <= 0) return;
    const toCx = (minX + maxX) / 2;
    const toCy = (minY + maxY) / 2;
    let toZoom = Math.min(this.screenW / bw, this.screenH / bh, this.maxZoom);
    toZoom = Math.max(toZoom, this.minZoom);
    this.animateTo(toCx, toCy, toZoom);
  }

  /**
   * Smoothly animate to a target position/zoom.
   *
   * When the user prefers reduced motion, snaps instantly instead of
   * animating. The preference is read from a cached `matchMedia` query
   * that auto-updates on system changes.
   */
  animateTo(cx: number, cy: number, zoom: number, durationMs = 300): void {
    const clampedZoom = Math.max(this.minZoom, Math.min(this.maxZoom, zoom));

    if (reducedMotion) {
      this.cx = cx;
      this.cy = cy;
      this.zoom = clampedZoom;
      this.anim = null;
      return;
    }

    this.anim = {
      fromCx: this.cx,
      fromCy: this.cy,
      fromZoom: this.zoom,
      toCx: cx,
      toCy: cy,
      toZoom: clampedZoom,
      start: performance.now(),
      duration: durationMs,
    };
  }

  /**
   * Advance the animation by one frame. Returns `true` if the camera
   * moved (caller should re-render and re-check LOD).
   */
  tick(): boolean {
    if (this.anim === null) return false;

    const elapsed = performance.now() - this.anim.start;
    const t = Math.min(elapsed / this.anim.duration, 1);
    // Ease-out cubic.
    const e = 1 - (1 - t) * (1 - t) * (1 - t);

    this.cx = this.anim.fromCx + (this.anim.toCx - this.anim.fromCx) * e;
    this.cy = this.anim.fromCy + (this.anim.toCy - this.anim.fromCy) * e;
    this.zoom = this.anim.fromZoom + (this.anim.toZoom - this.anim.fromZoom) * e;

    if (t >= 1) this.anim = null;
    return true;
  }

  /** Current visible world-space rectangle. */
  viewport(): Viewport {
    const hw = this.screenW / (2 * this.zoom);
    const hh = this.screenH / (2 * this.zoom);
    return { x: this.cx - hw, y: this.cy - hh, w: hw * 2, h: hh * 2 };
  }
}
