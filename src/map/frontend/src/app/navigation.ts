import type { Camera } from '../renderer/camera';
import type { Graph } from '../state/graph';

/** Duration (ms) for fit-all-nodes animation. */
const FIT_ALL_MS = 400;

/** Duration (ms) for focus-single-node animation. */
const FOCUS_NODE_MS = 300;

/**
 * High-level camera navigation.
 *
 * Wraps {@link Camera.fitBounds} / {@link Camera.animateTo} into
 * semantic operations (fit-all, focus-node) so callers don't need
 * to compute bounding boxes manually. The Camera handles animation
 * and reduced-motion preferences internally.
 */
export class Navigation {
  private readonly camera: Camera;

  constructor(camera: Camera) {
    this.camera = camera;
  }

  /** Fit every node into the viewport with padding. No-op on empty graph. */
  fitAll(graph: Graph): void {
    if (graph.nodes.size === 0) return;
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const n of graph.nodes.values()) {
      minX = Math.min(minX, n.x - n.w / 2);
      minY = Math.min(minY, n.y - n.h / 2);
      maxX = Math.max(maxX, n.x + n.w / 2);
      maxY = Math.max(maxY, n.y + n.h / 2);
    }
    this.camera.fitBounds(minX, minY, maxX, maxY, 80, FIT_ALL_MS);
  }

  /** Zoom to center a specific node with generous padding. */
  focusNode(name: string, graph: Graph): void {
    const node = graph.nodes.get(name);
    if (!node) return;
    const pad = 300;
    this.camera.fitBounds(
      node.x - pad,
      node.y - pad,
      node.x + pad,
      node.y + pad,
      80,
      FOCUS_NODE_MS,
    );
  }
}
