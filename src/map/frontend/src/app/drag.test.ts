import { describe, it, expect } from 'vitest';
import { DragState } from './drag';
import { Camera } from '../renderer/camera';
import { Graph } from '../state/graph';
import type { GraphSnapshot, RenderNode } from '../types';

function makeNode(name: string, x = 0, y = 0): RenderNode {
  return { name, kind: 'component', x, y, w: 180, h: 60, pinned: false };
}

function makeGraph(...names: string[]): Graph {
  const snap: GraphSnapshot = {
    project: { name: 'test', description: '' },
    components: names.map((n) => ({
      name: n,
      description: '',
      position: null,
      pinned: false,
      decision_count: 0,
      pattern_count: 0,
    })),
    decisions: [],
    patterns: [],
    edges: [],
    layout_version: 1,
  };
  const g = new Graph();
  g.loadSnapshot(snap);
  return g;
}

function cam(w = 1920, h = 1080): Camera {
  const c = new Camera();
  c.screenW = w;
  c.screenH = h;
  return c;
}

describe('DragState', () => {
  it('starts idle', () => {
    const d = new DragState();
    expect(d.dragging).toBeNull();
    expect(d.panning).toBe(false);
    expect(d.minimapDragging).toBe(false);
  });

  // ── Canvas pointer state machine ──────────────────────────────────

  it('pointerDown on a node starts dragging', () => {
    const d = new DragState();
    const node = makeNode('auth');
    const hit = d.onPointerDown(node, 100, 200);
    expect(hit).toBe('auth');
    expect(d.dragging).toBe('auth');
    expect(d.panning).toBe(false);
  });

  it('pointerDown on empty starts panning', () => {
    const d = new DragState();
    const hit = d.onPointerDown(null, 100, 200);
    expect(hit).toBeNull();
    expect(d.dragging).toBeNull();
    expect(d.panning).toBe(true);
  });

  it('pointerMove while panning moves the camera', () => {
    const d = new DragState();
    const c = cam();
    const g = makeGraph();

    d.onPointerDown(null, 100, 200);
    const cxBefore = c.cx;
    const result = d.onPointerMove(120, 210, c, g);

    expect(result.needsRender).toBe(true);
    expect(result.nodePositionChanged).toBe(false);
    // Camera panned: cx should have shifted by -dx/zoom = -20/1.
    expect(c.cx).toBe(cxBefore - 20);
  });

  it('pointerMove while dragging moves the node', () => {
    const d = new DragState();
    const c = cam();
    c.zoom = 2;
    const g = makeGraph('auth');
    const node = g.nodes.get('auth')!;
    const xBefore = node.x;

    d.onPointerDown(node, 100, 200);
    const result = d.onPointerMove(140, 200, c, g);

    expect(result.needsRender).toBe(true);
    expect(result.nodePositionChanged).toBe(true);
    // dx = 40, zoom = 2 → world delta = 20.
    expect(node.x).toBeCloseTo(xBefore + 20);
    expect(node.pinned).toBe(true);
  });

  it('pointerMove while idle is a noop', () => {
    const d = new DragState();
    const c = cam();
    const g = makeGraph();
    const result = d.onPointerMove(50, 50, c, g);
    expect(result.needsRender).toBe(false);
    expect(result.nodePositionChanged).toBe(false);
  });

  it('pointerUp from drag reports node moved', () => {
    const d = new DragState();
    d.onPointerDown(makeNode('auth'), 0, 0);

    const result = d.onPointerUp();
    expect(result.nodePositionChanged).toBe(true);
    expect(d.dragging).toBeNull();
    expect(d.panning).toBe(false);
  });

  it('pointerUp from pan reports no position change', () => {
    const d = new DragState();
    d.onPointerDown(null, 0, 0);

    const result = d.onPointerUp();
    expect(result.nodePositionChanged).toBe(false);
    expect(d.panning).toBe(false);
  });

  it('pointerUp from idle reports nothing', () => {
    const d = new DragState();
    const result = d.onPointerUp();
    expect(result.needsRender).toBe(false);
    expect(result.nodePositionChanged).toBe(false);
  });

  // ── Minimap coordinate conversion ─────────────────────────────────

  it('minimapToWorld converts correctly', () => {
    const d = new DragState();
    d.setMinimapTransform({
      minX: -500,
      minY: -300,
      scale: 0.1,
      ox: 10,
      oy: 5,
      mw: 180,
      mh: 120,
    });

    const result = d.minimapToWorld(60, 35);
    expect(result).not.toBeNull();
    // wx = -500 + (60 - 10) / 0.1 = -500 + 500 = 0
    expect(result!.wx).toBeCloseTo(0);
    // wy = -300 + (35 - 5) / 0.1 = -300 + 300 = 0
    expect(result!.wy).toBeCloseTo(0);
  });

  it('minimapToWorld returns null without transform', () => {
    const d = new DragState();
    expect(d.minimapToWorld(50, 50)).toBeNull();
  });

  it('minimap drag state toggles correctly', () => {
    const d = new DragState();
    expect(d.minimapDragging).toBe(false);

    // Can't test onMinimapDown without a real HTMLCanvasElement,
    // but we can test onMinimapUp resets the flag.
    d.onMinimapUp();
    expect(d.minimapDragging).toBe(false);
  });
});
