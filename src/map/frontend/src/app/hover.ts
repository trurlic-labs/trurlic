// ── Types ──────────────────────────────────────────────────────────────

/** Edge that is currently hovered. */
export interface HoverEdge {
  readonly from: string;
  readonly to: string;
  readonly kind: string;
}

/**
 * Read-only hover state consumed by the Renderer each frame.
 * {@link HoverTracker} satisfies this interface via getters.
 */
export interface HoverRenderState {
  readonly node: string | null;
  readonly borderAlpha: number;
  readonly tooltipVisible: boolean;
  readonly tooltipText: string;
  readonly tooltipX: number;
  readonly tooltipY: number;
  readonly edge: HoverEdge | null;
}

// ── Constants ──────────────────────────────────────────────────────────

/** Time (ms) for the hover border to ramp from 0 to full opacity. */
const BORDER_RAMP_MS = 100;

/** Dwell time (ms) before the tooltip appears. */
const TOOLTIP_DWELL_MS = 400;

/** Maximum tooltip text length. */
const TOOLTIP_MAX_CHARS = 80;

// ── HoverTracker ───────────────────────────────────────────────────────

/**
 * Tracks canvas hover state: which node/edge the cursor is over,
 * the animated border highlight, and the tooltip delay timer.
 *
 * Called from `onPointerMove` (idle only — not during drag/pan)
 * and ticked every render frame for animation.
 */
export class HoverTracker {
  // Node hover.
  private _node: string | null = null;
  private _borderAlpha = 0;
  private _tooltipVisible = false;
  private _tooltipText = '';
  private enterTime = 0;

  // Edge hover.
  private _edge: HoverEdge | null = null;

  // Cursor position (screen-space).
  private _tooltipX = 0;
  private _tooltipY = 0;

  // ── Getters (satisfy HoverRenderState) ─────────────────────────────

  get node(): string | null {
    return this._node;
  }
  get borderAlpha(): number {
    return this._borderAlpha;
  }
  get tooltipVisible(): boolean {
    return this._tooltipVisible;
  }
  get tooltipText(): string {
    return this._tooltipText;
  }
  get tooltipX(): number {
    return this._tooltipX;
  }
  get tooltipY(): number {
    return this._tooltipY;
  }
  get edge(): HoverEdge | null {
    return this._edge;
  }

  // ── Update (called on pointer move) ────────────────────────────────

  /**
   * Update hover targets based on hit-test results.
   *
   * @param nodeName  Hit-tested node name, or null.
   * @param nodeDesc  Node description (for tooltip text).
   * @param hitEdge   Hit-tested edge, or null (only when no node hit).
   * @param sx        Screen-space cursor X.
   * @param sy        Screen-space cursor Y.
   * @param now       Current timestamp (performance.now()).
   * @returns         True if the hover target changed (caller should re-render).
   */
  update(
    nodeName: string | null,
    nodeDesc: string,
    hitEdge: HoverEdge | null,
    sx: number,
    sy: number,
    now: number,
  ): boolean {
    this._tooltipX = sx;
    this._tooltipY = sy;

    let changed = false;

    // Edge activates only when no node is hovered.
    const effectiveEdge = nodeName ? null : hitEdge;
    if (!sameEdge(effectiveEdge, this._edge)) {
      this._edge = effectiveEdge;
      changed = true;
    }

    if (nodeName !== this._node) {
      this._node = nodeName;
      this._tooltipText = nodeName ? truncate(nodeDesc, TOOLTIP_MAX_CHARS) : '';
      this._borderAlpha = 0;
      this._tooltipVisible = false;
      this.enterTime = nodeName ? now : 0;
      changed = true;
    }

    return changed;
  }

  // ── Tick (called every render frame) ───────────────────────────────

  /**
   * Advance hover animations. Returns true if any visual state changed
   * (caller should set needsRender).
   */
  tick(now: number): boolean {
    if (!this._node) {
      if (this._borderAlpha > 0) {
        this._borderAlpha = 0;
        return true;
      }
      return false;
    }

    const elapsed = now - this.enterTime;
    let changed = false;

    // Border alpha: linear ramp over BORDER_RAMP_MS.
    const targetAlpha = Math.min(1, elapsed / BORDER_RAMP_MS);
    if (targetAlpha !== this._borderAlpha) {
      this._borderAlpha = targetAlpha;
      changed = true;
    }

    // Tooltip: visible after TOOLTIP_DWELL_MS.
    const shouldShow = elapsed >= TOOLTIP_DWELL_MS;
    if (shouldShow !== this._tooltipVisible) {
      this._tooltipVisible = shouldShow;
      changed = true;
    }

    return changed;
  }

  // ── Clear (called on pointer down / drag start) ────────────────────

  clear(): void {
    this._node = null;
    this._edge = null;
    this._borderAlpha = 0;
    this._tooltipVisible = false;
    this._tooltipText = '';
    this.enterTime = 0;
  }
}

// ── Helpers ────────────────────────────────────────────────────────────

function truncate(s: string, max: number): string {
  return s.length > max ? s.slice(0, max - 1) + '…' : s;
}

function sameEdge(a: HoverEdge | null, b: HoverEdge | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return a.from === b.from && a.to === b.to && a.kind === b.kind;
}

// ── Re-export for backward compatibility ───────────────────────────────

export { pointSegDistSq } from '../renderer/geometry';
