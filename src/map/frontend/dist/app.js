"use strict";
(() => {
  // src/renderer/camera.ts
  var motionQuery = typeof matchMedia !== "undefined" ? matchMedia("(prefers-reduced-motion: reduce)") : null;
  var reducedMotion = motionQuery?.matches ?? false;
  motionQuery?.addEventListener?.("change", (e) => {
    reducedMotion = e.matches;
  });
  var Camera = class {
    constructor() {
      /** Center of the viewport in world coordinates. */
      this.cx = 0;
      this.cy = 0;
      this.zoom = 1;
      /** Canvas pixel dimensions (set on resize). */
      this.screenW = 0;
      this.screenH = 0;
      this.minZoom = 0.05;
      this.maxZoom = 8;
      // ── Animation state ──────────────────────────────────────────────────
      this.anim = null;
    }
    /** World → screen. */
    toScreenX(wx) {
      return (wx - this.cx) * this.zoom + this.screenW / 2;
    }
    toScreenY(wy) {
      return (wy - this.cy) * this.zoom + this.screenH / 2;
    }
    /** Screen → world. */
    toWorldX(sx) {
      return (sx - this.screenW / 2) / this.zoom + this.cx;
    }
    toWorldY(sy) {
      return (sy - this.screenH / 2) / this.zoom + this.cy;
    }
    /** Pan by screen-space delta. */
    pan(dsx, dsy) {
      this.cx -= dsx / this.zoom;
      this.cy -= dsy / this.zoom;
      this.anim = null;
    }
    /** Zoom centered on a screen-space point. */
    zoomAt(sx, sy, factor) {
      const wx = this.toWorldX(sx);
      const wy = this.toWorldY(sy);
      this.zoom = Math.max(this.minZoom, Math.min(this.maxZoom, this.zoom * factor));
      this.cx = wx - (sx - this.screenW / 2) / this.zoom;
      this.cy = wy - (sy - this.screenH / 2) / this.zoom;
      this.anim = null;
    }
    /** Animate to fit a bounding box with padding. */
    fitBounds(minX, minY, maxX, maxY, padding = 80, durationMs = 300) {
      const bw = maxX - minX + padding * 2;
      const bh = maxY - minY + padding * 2;
      if (bw <= 0 || bh <= 0) return;
      const toCx = (minX + maxX) / 2;
      const toCy = (minY + maxY) / 2;
      let toZoom = Math.min(this.screenW / bw, this.screenH / bh, this.maxZoom);
      toZoom = Math.max(toZoom, this.minZoom);
      this.animateTo(toCx, toCy, toZoom, durationMs);
    }
    /**
     * Smoothly animate to a target position/zoom.
     *
     * When the user prefers reduced motion, snaps instantly instead of
     * animating. The preference is read from a cached `matchMedia` query
     * that auto-updates on system changes.
     */
    animateTo(cx, cy, zoom, durationMs = 300) {
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
        duration: durationMs
      };
    }
    /**
     * Advance the animation by one frame. Returns `true` if the camera
     * moved (caller should re-render and re-check LOD).
     */
    tick() {
      if (this.anim === null) return false;
      const elapsed = performance.now() - this.anim.start;
      const t = Math.min(elapsed / this.anim.duration, 1);
      const e = 1 - (1 - t) * (1 - t) * (1 - t);
      this.cx = this.anim.fromCx + (this.anim.toCx - this.anim.fromCx) * e;
      this.cy = this.anim.fromCy + (this.anim.toCy - this.anim.fromCy) * e;
      this.zoom = this.anim.fromZoom + (this.anim.toZoom - this.anim.fromZoom) * e;
      if (t >= 1) this.anim = null;
      return true;
    }
    /** Current visible world-space rectangle. */
    viewport() {
      const hw = this.screenW / (2 * this.zoom);
      const hh = this.screenH / (2 * this.zoom);
      return { x: this.cx - hw, y: this.cy - hh, w: hw * 2, h: hh * 2 };
    }
  };

  // src/renderer/culling.ts
  function intersects(a, b) {
    return Math.abs(a.cx - b.cx) < a.hw + b.hw && Math.abs(a.cy - b.cy) < a.hh + b.hh;
  }
  function containsPoint(box, px, py) {
    return Math.abs(px - box.cx) <= box.hw && Math.abs(py - box.cy) <= box.hh;
  }
  var MAX_DEPTH = 8;
  var CELL_CAPACITY = 8;
  var QTNode = class _QTNode {
    constructor(bounds, depth) {
      this.entries = [];
      this.children = null;
      this.bounds = bounds;
      this.depth = depth;
    }
    insert(entry) {
      if (!intersects(this.bounds, entry.bounds)) return;
      if (this.children === null) {
        this.entries.push(entry);
        if (this.entries.length > CELL_CAPACITY && this.depth < MAX_DEPTH) {
          this.subdivide();
        }
        return;
      }
      for (const child of this.children) {
        child.insert(entry);
      }
    }
    query(range, results) {
      if (!intersects(this.bounds, range)) return;
      for (const e of this.entries) {
        if (intersects(e.bounds, range)) {
          results.push(e.name);
        }
      }
      if (this.children !== null) {
        for (const child of this.children) {
          child.query(range, results);
        }
      }
    }
    /** Point query — returns the top-most (last-inserted) hit, or null. */
    queryPoint(px, py) {
      if (!containsPoint(this.bounds, px, py)) return null;
      if (this.children !== null) {
        for (let i = this.children.length - 1; i >= 0; i--) {
          const hit = this.children[i].queryPoint(px, py);
          if (hit !== null) return hit;
        }
      }
      for (let i = this.entries.length - 1; i >= 0; i--) {
        if (containsPoint(this.entries[i].bounds, px, py)) {
          return this.entries[i].name;
        }
      }
      return null;
    }
    subdivide() {
      const { cx, cy, hw, hh } = this.bounds;
      const qw = hw / 2;
      const qh = hh / 2;
      const d = this.depth + 1;
      this.children = [
        new _QTNode({ cx: cx - qw, cy: cy - qh, hw: qw, hh: qh }, d),
        // NW
        new _QTNode({ cx: cx + qw, cy: cy - qh, hw: qw, hh: qh }, d),
        // NE
        new _QTNode({ cx: cx - qw, cy: cy + qh, hw: qw, hh: qh }, d),
        // SW
        new _QTNode({ cx: cx + qw, cy: cy + qh, hw: qw, hh: qh }, d)
        // SE
      ];
      const entries = this.entries;
      this.entries = [];
      for (const entry of entries) {
        for (const child of this.children) {
          child.insert(entry);
        }
      }
    }
  };
  var Quadtree = class {
    constructor() {
      this.root = null;
    }
    /** Rebuild the tree from the current node positions. */
    build(nodes) {
      if (nodes.size === 0) {
        this.root = null;
        return;
      }
      let minX = Infinity;
      let minY = Infinity;
      let maxX = -Infinity;
      let maxY = -Infinity;
      for (const n of nodes.values()) {
        minX = Math.min(minX, n.x - n.w / 2);
        minY = Math.min(minY, n.y - n.h / 2);
        maxX = Math.max(maxX, n.x + n.w / 2);
        maxY = Math.max(maxY, n.y + n.h / 2);
      }
      const pad = 100;
      const cx = (minX + maxX) / 2;
      const cy = (minY + maxY) / 2;
      const hw = (maxX - minX) / 2 + pad;
      const hh = (maxY - minY) / 2 + pad;
      this.root = new QTNode({ cx, cy, hw, hh }, 0);
      for (const n of nodes.values()) {
        this.root.insert({
          name: n.name,
          bounds: { cx: n.x, cy: n.y, hw: n.w / 2, hh: n.h / 2 }
        });
      }
    }
    /** Return names of all nodes whose bounds intersect the viewport. */
    queryViewport(viewport) {
      if (this.root === null) return /* @__PURE__ */ new Set();
      const results = [];
      this.root.query(viewport, results);
      return new Set(results);
    }
    /** Return the name of the node at world-space point (wx, wy), or null. */
    hitTest(wx, wy) {
      if (this.root === null) return null;
      return this.root.queryPoint(wx, wy);
    }
  };

  // src/renderer/geometry.ts
  function cross(o, a, b) {
    return (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
  }
  function convexHull(points) {
    const pts = points.slice().sort((a, b) => a.x - b.x || a.y - b.y);
    const n = pts.length;
    if (n <= 2) return pts;
    const lower = [];
    for (const p of pts) {
      while (lower.length >= 2 && cross(lower[lower.length - 2], lower[lower.length - 1], p) <= 0) {
        lower.pop();
      }
      lower.push(p);
    }
    const upper = [];
    for (let i = n - 1; i >= 0; i--) {
      const p = pts[i];
      while (upper.length >= 2 && cross(upper[upper.length - 2], upper[upper.length - 1], p) <= 0) {
        upper.pop();
      }
      upper.push(p);
    }
    lower.pop();
    upper.pop();
    return lower.concat(upper);
  }
  function edgeOutward(a, b, cx, cy) {
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const len = Math.sqrt(dx * dx + dy * dy);
    if (len < 1e-10) return { x: 0, y: 0 };
    let nx = -dy / len;
    let ny = dx / len;
    const mx = (a.x + b.x) / 2 - cx;
    const my = (a.y + b.y) / 2 - cy;
    if (nx * mx + ny * my < 0) {
      nx = -nx;
      ny = -ny;
    }
    return { x: nx, y: ny };
  }
  function expandHull(hull, d) {
    const n = hull.length;
    if (n < 3) return hull.slice();
    const cx = hull.reduce((s, p) => s + p.x, 0) / n;
    const cy = hull.reduce((s, p) => s + p.y, 0) / n;
    const result = [];
    for (let i = 0; i < n; i++) {
      const prev = hull[(i - 1 + n) % n];
      const curr = hull[i];
      const next = hull[(i + 1) % n];
      const n1 = edgeOutward(prev, curr, cx, cy);
      const n2 = edgeOutward(curr, next, cx, cy);
      let bx = n1.x + n2.x;
      let by = n1.y + n2.y;
      const bLen = Math.sqrt(bx * bx + by * by);
      if (bLen < 1e-10) {
        result.push({ x: curr.x + n1.x * d, y: curr.y + n1.y * d });
      } else {
        bx /= bLen;
        by /= bLen;
        const cosHalf = n1.x * bx + n1.y * by;
        const scale = d / Math.max(cosHalf, 0.15);
        result.push({ x: curr.x + bx * scale, y: curr.y + by * scale });
      }
    }
    return result;
  }
  function roundedHullPath(ctx, hull, radius) {
    const n = hull.length;
    if (n < 3) return;
    ctx.beginPath();
    const last = hull[n - 1];
    const first = hull[0];
    ctx.moveTo((last.x + first.x) / 2, (last.y + first.y) / 2);
    for (let i = 0; i < n; i++) {
      const curr = hull[i];
      const next = hull[(i + 1) % n];
      ctx.arcTo(curr.x, curr.y, next.x, next.y, radius);
    }
    ctx.closePath();
  }
  function rayRectIntersect(cx, cy, hw, hh, dx, dy) {
    const absDx = Math.abs(dx);
    const absDy = Math.abs(dy);
    if (absDx < 1e-10 && absDy < 1e-10) return { x: cx, y: cy };
    const tx = absDx > 1e-10 ? hw / absDx : Infinity;
    const ty = absDy > 1e-10 ? hh / absDy : Infinity;
    const t = Math.min(tx, ty);
    return { x: cx + dx * t, y: cy + dy * t };
  }
  function nodeCorners(names, nodes) {
    const pts = [];
    for (const name of names) {
      const n = nodes.get(name);
      if (!n) continue;
      const hw = n.w / 2;
      const hh = n.h / 2;
      pts.push(
        { x: n.x - hw, y: n.y - hh },
        { x: n.x + hw, y: n.y - hh },
        { x: n.x + hw, y: n.y + hh },
        { x: n.x - hw, y: n.y + hh }
      );
    }
    return pts;
  }
  function pointInConvexPoly(px, py, poly) {
    const n = poly.length;
    if (n < 3) return false;
    let positive = 0;
    let negative = 0;
    for (let i = 0; i < n; i++) {
      const a = poly[i];
      const b = poly[(i + 1) % n];
      const cp = (b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x);
      if (cp > 0) positive++;
      else if (cp < 0) negative++;
      if (positive > 0 && negative > 0) return false;
    }
    return true;
  }
  function pointSegDistSq(px, py, ax, ay, bx, by) {
    const dx = bx - ax;
    const dy = by - ay;
    const lenSq = dx * dx + dy * dy;
    if (lenSq < 1e-10) return (px - ax) ** 2 + (py - ay) ** 2;
    const t = Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / lenSq));
    const nx = ax + t * dx;
    const ny = ay + t * dy;
    return (px - nx) ** 2 + (py - ny) ** 2;
  }
  function pointBezierDistSq(px, py, ax, ay, cpx, cpy, bx, by) {
    const N = 5;
    let prevX = ax;
    let prevY = ay;
    let minDist = Infinity;
    for (let i = 1; i <= N; i++) {
      const t = i / N;
      const s = 1 - t;
      const x = s * s * ax + 2 * s * t * cpx + t * t * bx;
      const y = s * s * ay + 2 * s * t * cpy + t * t * by;
      const d = pointSegDistSq(px, py, prevX, prevY, x, y);
      if (d < minDist) minDist = d;
      prevX = x;
      prevY = y;
    }
    return minDist;
  }

  // src/state/graph.ts
  var NO_DECISIONS = Object.freeze([]);
  var MIN_NODE_W = 200;
  var MAX_NODE_W = 320;
  var CHAR_WIDTH_ESTIMATE = 8.8;
  var NODE_PAD_X = 40;
  var BASE_NODE_H = 60;
  var Graph = class {
    constructor() {
      this.nodes = /* @__PURE__ */ new Map();
      /** All edges — connects_to rendered at LOD 0, others at LOD 1+. */
      this.edges = [];
      this.decisions = /* @__PURE__ */ new Map();
      this.patterns = /* @__PURE__ */ new Map();
      this.projectName = "";
      this.projectDescription = "";
      this.layoutVersion = 0;
      this.quadtree = new Quadtree();
      /** Pattern name -> expanded convex hull points (computed after layout). */
      this.patternHulls = /* @__PURE__ */ new Map();
      /** Component name → decisions index. O(1) lookup. */
      this.byComponent = /* @__PURE__ */ new Map();
    }
    loadSnapshot(snap) {
      this.nodes.clear();
      this.edges = [];
      this.decisions.clear();
      this.patterns.clear();
      this.byComponent.clear();
      this.projectName = snap.project.name;
      this.projectDescription = snap.project.description;
      this.layoutVersion = snap.layout_version;
      for (const c of snap.components) {
        this.nodes.set(c.name, {
          name: c.name,
          kind: "component",
          x: c.position?.x ?? 0,
          y: c.position?.y ?? 0,
          w: nodeWidth(c.name),
          h: BASE_NODE_H,
          pinned: c.pinned,
          description: c.description,
          decisionCount: c.decision_count,
          patternCount: c.pattern_count
        });
      }
      for (const d of snap.decisions) {
        this.decisions.set(d.name, d);
        const list = this.byComponent.get(d.component);
        if (list) list.push(d);
        else this.byComponent.set(d.component, [d]);
      }
      for (const p of snap.patterns) {
        this.patterns.set(p.name, p);
      }
      for (const e of snap.edges) {
        this.edges.push({ from: e.from, to: e.to, kind: e.kind });
      }
      this.assignMissingPositions();
      this.rebuildQuadtree();
      this.rebuildPatternHulls();
    }
    /** Rebuild the spatial index. Call after layout changes or drag. */
    rebuildQuadtree() {
      this.quadtree.build(this.nodes);
    }
    /** Rebuild expanded convex hulls for all patterns. Call after layout. */
    rebuildPatternHulls() {
      this.patternHulls.clear();
      for (const [name, pat] of this.patterns) {
        const corners = [];
        for (const cName of pat.components) {
          const n = this.nodes.get(cName);
          if (!n) continue;
          const hw = n.w / 2;
          const hh = n.h / 2;
          corners.push(
            { x: n.x - hw, y: n.y - hh },
            { x: n.x + hw, y: n.y - hh },
            { x: n.x + hw, y: n.y + hh },
            { x: n.x - hw, y: n.y + hh }
          );
        }
        if (corners.length < 3) continue;
        const hull = convexHull(corners);
        if (hull.length < 3) continue;
        const expanded = expandHull(hull, 50);
        this.patternHulls.set(name, expanded);
      }
    }
    /** Hit-test pattern regions. Returns the pattern name if (wx, wy) is inside any hull. Smaller patterns (fewer components) win over broad ones. */
    patternAt(wx, wy) {
      const sorted = [...this.patternHulls.entries()].sort((a, b) => a[1].length - b[1].length);
      for (const [name, hull] of sorted) {
        if (pointInConvexPoly(wx, wy, hull)) return name;
      }
      return null;
    }
    assignMissingPositions() {
      let i = 0;
      const count = this.nodes.size;
      for (const node of this.nodes.values()) {
        if (node.x === 0 && node.y === 0 && !node.pinned) {
          const angle = 2 * Math.PI * i / Math.max(count, 1);
          const radius = 350 + count * 45;
          node.x = Math.cos(angle) * radius;
          node.y = Math.sin(angle) * radius;
        }
        i++;
      }
    }
    /** Hit test using quadtree — O(log n) instead of linear scan. */
    nodeAt(wx, wy) {
      const name = this.quadtree.hitTest(wx, wy);
      return name ? this.nodes.get(name) ?? null : null;
    }
    /** O(1) lookup via pre-built index. Returns frozen empty array on miss. */
    decisionsFor(component) {
      return this.byComponent.get(component) ?? NO_DECISIONS;
    }
    /** All unique tags across every decision. Sorted alphabetically. */
    allTags() {
      const tags = /* @__PURE__ */ new Set();
      for (const d of this.decisions.values()) {
        for (const t of d.tags) tags.add(t);
      }
      return [...tags].sort();
    }
    // ── Incremental updates (WS diff processing) ─────────────────────────
    /**
     * Remove a node and all its related data. Used for `node_removed` WS
     * events to avoid a full graph refetch.
     */
    removeNode(name) {
      this.nodes.delete(name);
      this.decisions.delete(name);
      this.byComponent.delete(name);
      for (const [dName, d] of this.decisions) {
        if (d.component === name) {
          this.decisions.delete(dName);
        }
      }
      this.edges = this.edges.filter((e) => e.from !== name && e.to !== name);
      this.rebuildQuadtree();
    }
    /** Add an edge. Used for `edge_added` WS events. */
    addEdge(from, to, kind) {
      this.edges.push({ from, to, kind });
    }
    /** Remove a specific edge. Used for `edge_removed` WS events. */
    removeEdge(from, to, kind) {
      const idx = this.edges.findIndex((e) => e.from === from && e.to === to && e.kind === kind);
      if (idx !== -1) this.edges.splice(idx, 1);
    }
  };
  function nodeWidth(name) {
    const textWidth = name.length * CHAR_WIDTH_ESTIMATE + NODE_PAD_X;
    return Math.max(MIN_NODE_W, Math.min(MAX_NODE_W, textWidth));
  }

  // src/state/api.ts
  var ApiClient = class {
    constructor(token) {
      this.baseUrl = `${location.protocol}//${location.host}`;
      this.token = token;
    }
    async fetchGraph() {
      const res = await fetch(`${this.baseUrl}/api/graph`, {
        headers: { Authorization: `Bearer ${this.token}` }
      });
      if (!res.ok) throw new Error(`GET /api/graph: ${res.status}`);
      return res.json();
    }
    async saveLayout(positions, version) {
      const res = await fetch(`${this.baseUrl}/api/layout`, {
        method: "PUT",
        headers: this.jsonHeaders(),
        body: JSON.stringify({ positions, layout_version: version })
      });
      if (!res.ok) throw new Error(`PUT /api/layout: ${res.status}`);
      const data = await res.json();
      return data.layout_version;
    }
    async resetLayout() {
      const res = await fetch(`${this.baseUrl}/api/layout/reset`, {
        method: "POST",
        headers: { Authorization: `Bearer ${this.token}` }
      });
      if (!res.ok) throw new Error(`POST /api/layout/reset: ${res.status}`);
      const data = await res.json();
      return data.layout_version;
    }
    // ── Mutations ───────────────────────────────────────────────────────
    async createComponent(name, description) {
      const res = await fetch(`${this.baseUrl}/api/component`, {
        method: "POST",
        headers: this.jsonHeaders(),
        body: JSON.stringify({ name, description })
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error ?? `POST component: ${res.status}`);
      }
    }
    async createConnection(from, to) {
      const res = await fetch(`${this.baseUrl}/api/connection`, {
        method: "POST",
        headers: this.jsonHeaders(),
        body: JSON.stringify({ from, to })
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error ?? `POST connection: ${res.status}`);
      }
    }
    async updateDecision(name, body) {
      const res = await fetch(`${this.baseUrl}/api/decision/${enc(name)}`, {
        method: "PUT",
        headers: this.jsonHeaders(),
        body: JSON.stringify(body)
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error ?? `PUT decision: ${res.status}`);
      }
    }
    async deleteDecision(name) {
      const res = await fetch(`${this.baseUrl}/api/decision/${enc(name)}`, {
        method: "DELETE",
        headers: { Authorization: `Bearer ${this.token}` }
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error ?? `DELETE decision: ${res.status}`);
      }
    }
    async deleteComponent(name) {
      const res = await fetch(`${this.baseUrl}/api/component/${enc(name)}`, {
        method: "DELETE",
        headers: { Authorization: `Bearer ${this.token}` }
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error ?? `DELETE component: ${res.status}`);
      }
    }
    async deleteConnection(from, to) {
      const res = await fetch(`${this.baseUrl}/api/connection/${enc(from)}/${enc(to)}`, {
        method: "DELETE",
        headers: { Authorization: `Bearer ${this.token}` }
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error ?? `DELETE connection: ${res.status}`);
      }
    }
    jsonHeaders() {
      return {
        Authorization: `Bearer ${this.token}`,
        "Content-Type": "application/json"
      };
    }
  };
  function enc(s) {
    return encodeURIComponent(s);
  }

  // src/state/ws.ts
  var WsConnection = class {
    constructor(token, onEvent, onStateChange) {
      this.ws = null;
      this.reconnectDelay = 100;
      this.maxReconnectDelay = 5e3;
      this.token = token;
      this.onEvent = onEvent;
      this.onStateChange = onStateChange;
      this.connect();
    }
    connect() {
      const proto = location.protocol === "https:" ? "wss:" : "ws:";
      const url = `${proto}//${location.host}/ws?token=${this.token}`;
      this.ws = new WebSocket(url);
      this.ws.onopen = () => {
        this.reconnectDelay = 100;
        this.onStateChange("connected");
      };
      this.ws.onmessage = (e) => {
        try {
          const event = JSON.parse(e.data);
          this.onEvent(event);
        } catch {
        }
      };
      this.ws.onclose = () => {
        this.onStateChange("reconnecting");
        setTimeout(() => this.connect(), this.reconnectDelay);
        this.reconnectDelay = Math.min(this.reconnectDelay * 2, this.maxReconnectDelay);
      };
      this.ws.onerror = () => {
        this.ws?.close();
      };
    }
  };

  // src/layout/force.ts
  var ForceLayout = class {
    constructor() {
      this.repulsion = 18e3;
      this.springK = 4e-3;
      this.springLen = 400;
      this.gravity = 8e-3;
      this.damping = 0.88;
      this.collisionPad = 24;
      this.vx = /* @__PURE__ */ new Map();
      this.vy = /* @__PURE__ */ new Map();
    }
    run(nodes, edges, iterations) {
      for (const name of this.vx.keys()) {
        if (!nodes.has(name)) {
          this.vx.delete(name);
          this.vy.delete(name);
        }
      }
      for (const name of nodes.keys()) {
        if (!this.vx.has(name)) {
          this.vx.set(name, 0);
          this.vy.set(name, 0);
        }
      }
      const arr = [...nodes.values()];
      for (let i = 0; i < iterations; i++) {
        this.tick(arr, edges);
      }
    }
    tick(arr, edges) {
      const n = arr.length;
      const fxArr = new Float64Array(n);
      const fyArr = new Float64Array(n);
      for (let i = 0; i < n; i++) {
        for (let j = i + 1; j < n; j++) {
          const a = arr[i];
          const b = arr[j];
          const ddx = b.x - a.x;
          const ddy = b.y - a.y;
          const dist = Math.sqrt(ddx * ddx + ddy * ddy) || 1;
          const force = this.repulsion / (dist * dist);
          const fx = ddx / dist * force;
          const fy = ddy / dist * force;
          fxArr[i] -= fx;
          fyArr[i] -= fy;
          fxArr[j] += fx;
          fyArr[j] += fy;
        }
      }
      const idx = /* @__PURE__ */ new Map();
      for (let i = 0; i < n; i++) idx.set(arr[i].name, i);
      for (const e of edges) {
        const ai = idx.get(e.from);
        const bi = idx.get(e.to);
        if (ai === void 0 || bi === void 0) continue;
        const a = arr[ai];
        const b = arr[bi];
        const ddx = b.x - a.x;
        const ddy = b.y - a.y;
        const dist = Math.sqrt(ddx * ddx + ddy * ddy) || 1;
        const force = this.springK * (dist - this.springLen);
        const fx = ddx / dist * force;
        const fy = ddy / dist * force;
        fxArr[ai] += fx;
        fyArr[ai] += fy;
        fxArr[bi] -= fx;
        fyArr[bi] -= fy;
      }
      for (let i = 0; i < n; i++) {
        fxArr[i] -= arr[i].x * this.gravity;
        fyArr[i] -= arr[i].y * this.gravity;
      }
      for (let i = 0; i < n; i++) {
        const node = arr[i];
        if (node.pinned) continue;
        let nvx = ((this.vx.get(node.name) ?? 0) + fxArr[i]) * this.damping;
        let nvy = ((this.vy.get(node.name) ?? 0) + fyArr[i]) * this.damping;
        this.vx.set(node.name, nvx);
        this.vy.set(node.name, nvy);
        node.x += nvx;
        node.y += nvy;
      }
      this.separateOverlaps(arr);
    }
    /**
     * AABB overlap resolution. For each overlapping pair, push apart
     * along the axis of least overlap (shorter push = more stable).
     * Two passes per tick to handle transitive chains.
     */
    separateOverlaps(nodes) {
      const pad = this.collisionPad;
      const len = nodes.length;
      for (let pass = 0; pass < 2; pass++) {
        for (let i = 0; i < len; i++) {
          for (let j = i + 1; j < len; j++) {
            const a = nodes[i];
            const b = nodes[j];
            const dx = b.x - a.x;
            const dy = b.y - a.y;
            const overlapX = a.w / 2 + b.w / 2 + pad - Math.abs(dx);
            const overlapY = a.h / 2 + b.h / 2 + pad - Math.abs(dy);
            if (overlapX <= 0 || overlapY <= 0) continue;
            const aPinned = a.pinned;
            const bPinned = b.pinned;
            if (aPinned && bPinned) continue;
            if (overlapX < overlapY) {
              const sign = dx >= 0 ? 1 : -1;
              if (aPinned) {
                b.x += sign * overlapX;
              } else if (bPinned) {
                a.x -= sign * overlapX;
              } else {
                const half = overlapX / 2;
                a.x -= sign * half;
                b.x += sign * half;
              }
            } else {
              const sign = dy >= 0 ? 1 : -1;
              if (aPinned) {
                b.y += sign * overlapY;
              } else if (bPinned) {
                a.y -= sign * overlapY;
              } else {
                const half = overlapY / 2;
                a.y -= sign * half;
                b.y += sign * half;
              }
            }
          }
        }
      }
    }
  };

  // src/util.ts
  var _span = document.createElement("span");
  function esc(s) {
    _span.textContent = s;
    return _span.innerHTML;
  }

  // src/ui/panel.ts
  var SAVE_DEBOUNCE = 1e3;
  var Panel = class {
    constructor(el) {
      this.api = null;
      this.cb = null;
      this.saveTimer = null;
      this.history = [];
      this.el = el;
    }
    /** Wire up the API client and callbacks. Called once during app init. */
    init(api, cb) {
      this.api = api;
      this.cb = cb;
    }
    // ── Project view ────────────────────────────────────────────────────
    showProject(graph) {
      this.history = [];
      const dc = graph.decisions.size;
      const cc = graph.nodes.size;
      const pc = graph.patterns.size;
      this.el.innerHTML = `
      <h2>${esc(graph.projectName)}</h2>
      ${graph.projectDescription ? `<p class="dim">${esc(graph.projectDescription)}</p>` : ""}
      <div class="stats">
        <div class="stat"><span class="stat-n">${cc}</span> components</div>
        <div class="stat"><span class="stat-n">${dc}</span> decisions</div>
        <div class="stat"><span class="stat-n">${pc}</span> patterns</div>
      </div>
      <h3>Recent decisions</h3>
      ${recentDecisions(graph)}
      <h3>Patterns <span class="dim">(${pc})</span></h3>
      ${patternList(graph)}
    `;
      this.bindDecisionLinks(graph);
      this.bindPatternLinks(graph);
    }
    // ── Component view ──────────────────────────────────────────────────
    showComponent(node, graph) {
      this.history = [];
      const decs = graph.decisionsFor(node.name);
      const outgoing = graph.edges.filter((e) => e.from === node.name && e.kind === "connects_to");
      const incoming = graph.edges.filter((e) => e.to === node.name && e.kind === "connects_to");
      const allConns = [.../* @__PURE__ */ new Set([...outgoing.map((e) => e.to), ...incoming.map((e) => e.from)])];
      const patterns = [];
      for (const [, p] of graph.patterns) {
        if (p.components.includes(node.name)) patterns.push(p);
      }
      this.el.innerHTML = `
      <div class="panel-kind">component</div>
      <h2 data-component="${esc(node.name)}">${esc(node.name)}</h2>
      ${node.description ? `<p class="dim">${esc(node.description)}</p>` : ""}
      ${allConns.length > 0 ? `
        <h3>Connections</h3>
        <div class="chip-list">${allConns.map((c) => `<a class="chip nav-link" data-nav="${esc(c)}" href="#">${esc(c)}</a>`).join("")}</div>
      ` : ""}
      ${patterns.length > 0 ? `
        <h3>Patterns</h3>
        ${patterns.map((p) => `<div class="dec-card pattern-link" data-pattern="${esc(p.name)}"><div class="dec-choice">${esc(p.description)}</div></div>`).join("")}
      ` : ""}
      <h3>Decisions <span class="dim">(${decs.length})</span></h3>
      ${decs.length === 0 ? '<p class="dim">None yet</p>' : decs.map((d) => decisionRow(d)).join("")}
    `;
      this.bindNavLinks();
      this.bindDecisionLinks(graph);
      this.bindPatternLinks(graph);
    }
    // ── Decision view ───────────────────────────────────────────────────
    showDecision(name, graph) {
      const dec = graph.decisions.get(name);
      if (!dec) {
        this.showProject(graph);
        return;
      }
      this.pushCurrentView();
      const backHtml = this.history.length > 0 ? '<a href="#" class="panel-back">\u2190 back</a>' : "";
      this.el.innerHTML = `
      ${backHtml}
      <div class="panel-kind">decision</div>
      <h2 class="editable-heading" data-field="choice" data-dec="${esc(name)}" data-placeholder="Click to edit">${esc(dec.choice)}</h2>
      <p class="dim">
        <a class="nav-link" data-nav="${esc(dec.component)}" href="#">${esc(dec.component)}</a>
        \xB7 ${new Date(dec.created).toLocaleDateString()}
      </p>
      <h3>Reason</h3>
      <div class="editable-block" data-field="reason" data-dec="${esc(name)}" data-placeholder="Click to add reason">${esc(dec.reason)}</div>
      ${dec.tags.length > 0 ? `
        <h3>Tags</h3>
        <div class="chip-list">${dec.tags.map((t) => `<span class="chip tag">${esc(t)}</span>`).join("")}</div>
      ` : ""}
      ${dec.alternatives.length > 0 ? `
        <h3>Alternatives considered</h3>
        <ul class="alt-list">${dec.alternatives.map((a) => `<li class="dim">${esc(a)}</li>`).join("")}</ul>
      ` : ""}
      <div class="panel-actions">
        <button class="btn btn-danger" data-delete-dec="${esc(name)}">Delete decision</button>
      </div>
    `;
      this.bindBackLink(graph);
      this.bindNavLinks();
      this.bindEditableFields(name, graph);
      this.bindDeleteDecision(name, graph);
    }
    // ── Pattern view ────────────────────────────────────────────────────
    showPattern(name, graph) {
      const pat = graph.patterns.get(name);
      if (!pat) {
        this.showProject(graph);
        return;
      }
      this.pushCurrentView();
      const backHtml = this.history.length > 0 ? '<a href="#" class="panel-back">\u2190 back</a>' : "";
      const memberDecs = pat.decisions.map((d) => graph.decisions.get(d)).filter((d) => d != null);
      this.el.innerHTML = `
      ${backHtml}
      <div class="panel-kind">pattern</div>
      <h2>${esc(pat.description)}</h2>
      <p class="dim">${esc(name)}</p>
      ${pat.components.length > 0 ? `
        <h3>Applies to</h3>
        <div class="chip-list">${pat.components.map((c) => `<a class="chip nav-link" data-nav="${esc(c)}" href="#">${esc(c)}</a>`).join("")}</div>
      ` : ""}
      <h3>Decisions <span class="dim">(${memberDecs.length})</span></h3>
      ${memberDecs.map((d) => decisionRow(d)).join("")}
    `;
      this.bindBackLink(graph);
      this.bindNavLinks();
      this.bindDecisionLinks(graph);
    }
    showEmpty() {
      this.el.innerHTML = `<p class="dim" style="padding:24px">Click a component to inspect</p>`;
    }
    showLoading() {
      this.el.innerHTML = '<p class="dim" style="padding:24px">Loading\u2026</p>';
    }
    showLoadError(retry) {
      this.el.innerHTML = `
      <div style="padding: 24px;">
        <p style="margin-bottom: 12px; color: var(--text);">Could not load the architecture graph.</p>
        <button class="btn" id="retry-btn">Retry</button>
      </div>
    `;
      this.el.querySelector("#retry-btn")?.addEventListener("click", retry);
    }
    // ── Navigation history ───────────────────────────────────────────────
    pushCurrentView() {
      const kind = this.el.querySelector(".panel-kind");
      if (!kind) {
        this.history.push({ type: "project" });
        return;
      }
      const kindText = kind.textContent?.trim().toLowerCase();
      if (kindText === "component") {
        const heading = this.el.querySelector("h2");
        const name = heading?.dataset.component ?? heading?.textContent?.trim() ?? "";
        if (name) this.history.push({ type: "component", name });
      } else if (kindText === "decision") {
        const heading = this.el.querySelector(".editable-heading");
        const decName = heading?.dataset.dec ?? "";
        if (decName) this.history.push({ type: "decision", name: decName });
      } else if (kindText === "pattern") {
        const slug = this.el.querySelector("p.dim");
        const name = slug?.textContent?.trim() ?? "";
        if (name) this.history.push({ type: "pattern", name });
      } else {
        this.history.push({ type: "project" });
      }
    }
    bindBackLink(graph) {
      const link = this.el.querySelector(".panel-back");
      if (!link) return;
      link.addEventListener("click", (e) => {
        e.preventDefault();
        const prev = this.history.pop();
        if (!prev) return;
        switch (prev.type) {
          case "project":
            this.showProject(graph);
            break;
          case "component": {
            const node = graph.nodes.get(prev.name);
            if (node) this.showComponent(node, graph);
            else this.showProject(graph);
            break;
          }
          case "decision":
            this.showDecision(prev.name, graph);
            break;
          case "pattern":
            this.showPattern(prev.name, graph);
            break;
        }
      });
    }
    // ── Event binding ───────────────────────────────────────────────────
    bindNavLinks() {
      for (const link of this.el.querySelectorAll(".nav-link")) {
        link.addEventListener("click", (e) => {
          e.preventDefault();
          const target = link.dataset.nav;
          if (target) this.cb?.onNavigate(target);
        });
      }
    }
    bindDecisionLinks(graph) {
      for (const card of this.el.querySelectorAll(".dec-link")) {
        card.addEventListener("click", () => {
          const name = card.dataset.dec;
          if (name) this.showDecision(name, graph);
        });
      }
    }
    bindPatternLinks(graph) {
      for (const card of this.el.querySelectorAll(".pattern-link")) {
        card.addEventListener("click", () => {
          const name = card.dataset.pattern;
          if (name) this.showPattern(name, graph);
        });
      }
    }
    bindEditableFields(decName, _graph) {
      for (const el of this.el.querySelectorAll(".editable-heading, .editable-block")) {
        el.contentEditable = "true";
        el.addEventListener("blur", () => {
          const field = el.dataset.field;
          const value = el.textContent?.trim() ?? "";
          if (!value) return;
          this.debouncedSave(decName, { [field]: value });
        });
        el.addEventListener("keydown", (e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            el.blur();
          }
        });
      }
    }
    bindDeleteDecision(name, _graph) {
      const btn = this.el.querySelector(`[data-delete-dec="${name}"]`);
      if (!btn) return;
      btn.addEventListener("click", () => {
        if (!confirm(`Delete decision "${name}"? This cannot be undone.`)) return;
        this.api?.deleteDecision(name).then(() => {
          this.cb?.onMutated();
        }).catch((e) => this.showError(e.message));
      });
    }
    debouncedSave(decName, body) {
      if (this.saveTimer) clearTimeout(this.saveTimer);
      this.saveTimer = setTimeout(() => {
        this.api?.updateDecision(decName, body).then(() => {
          this.showSaveIndicator();
          this.cb?.onMutated();
        }).catch((e) => this.showError(e.message));
      }, SAVE_DEBOUNCE);
    }
    showSaveIndicator() {
      let indicator = this.el.querySelector(".save-indicator");
      if (!indicator) {
        indicator = document.createElement("div");
        indicator.className = "save-indicator";
        this.el.prepend(indicator);
      }
      indicator.textContent = "Saved";
      setTimeout(() => indicator?.remove(), 1500);
    }
    showError(msg) {
      let errEl = this.el.querySelector(".panel-error");
      if (!errEl) {
        errEl = document.createElement("div");
        errEl.className = "panel-error";
        this.el.prepend(errEl);
      }
      errEl.textContent = msg;
      setTimeout(() => errEl?.remove(), 5e3);
    }
  };
  function decisionRow(d) {
    return `
    <div class="dec-card dec-link" data-dec="${esc(d.name)}">
      <div class="dec-choice">${esc(d.choice)}</div>
      <div class="dec-reason dim">${esc(d.reason)}</div>
      ${d.tags.length > 0 ? `<div class="chip-list">${d.tags.map((t) => `<span class="chip tag">${esc(t)}</span>`).join("")}</div>` : ""}
    </div>
  `;
  }
  function patternList(graph) {
    const patterns = [...graph.patterns.values()];
    if (patterns.length === 0) return '<p class="dim">None yet</p>';
    return patterns.map(
      (p) => `
    <div class="dec-card pattern-link" data-pattern="${esc(p.name)}" style="cursor:pointer">
      <div class="dec-choice">${esc(p.description || p.name)}</div>
      <div class="dim" style="font-size:12px">${p.components.length} components \xB7 ${p.decisions.length} decisions</div>
    </div>
  `
    ).join("");
  }
  function recentDecisions(graph) {
    const sorted = [...graph.decisions.values()].sort((a, b) => b.created.localeCompare(a.created)).slice(0, 5);
    if (sorted.length === 0) return '<p class="dim">None yet</p>';
    return sorted.map(
      (d) => `
    <div class="dec-card dec-link" data-dec="${esc(d.name)}">
      <div class="dec-choice">${esc(d.choice)}</div>
      <div class="dim" style="font-size:12px">${esc(d.component)} \xB7 ${new Date(d.created).toLocaleDateString()}</div>
    </div>
  `
    ).join("");
  }

  // src/renderer/lod.ts
  var REFERENCE_AREA = 336e4;
  function computeLOD(visibleCount, viewportWorldArea) {
    if (viewportWorldArea <= 0) return 2 /* Decision */;
    const normalizedCount = visibleCount * (REFERENCE_AREA / viewportWorldArea);
    if (normalizedCount > 30) return 0 /* Overview */;
    if (normalizedCount > 3) return 1 /* Component */;
    return 2 /* Decision */;
  }

  // src/renderer/edges.ts
  var EDGE_DASH = {
    depends_on: [6, 4],
    constrains: [2, 3],
    supersedes: [8, 3, 2, 3]
  };
  var EDGE_OPACITY = {
    connects_to: 1,
    depends_on: 0.7,
    constrains: 0.55,
    supersedes: 0.4
  };
  function edgeColor(kind, c) {
    if (kind === "depends_on") return c.edgeDep;
    if (kind === "constrains") return c.edgeCon;
    if (kind === "supersedes") return c.edgeSup;
    return c.edge;
  }
  var CURVE_OFFSET_PX = 15;
  function edgeCurveCP(ax, ay, bx, by, zoom, reverse) {
    const mx = (ax + bx) / 2;
    const my = (ay + by) / 2;
    const dx = bx - ax;
    const dy = by - ay;
    const len = Math.sqrt(dx * dx + dy * dy);
    if (len < 1e-10) return { cpx: mx, cpy: my };
    const px = -dy / len;
    const py = dx / len;
    const offset = (reverse ? -CURVE_OFFSET_PX : CURVE_OFFSET_PX) / zoom;
    return {
      cpx: mx + px * offset,
      cpy: my + py * offset
    };
  }
  function buildEdgePairSet(edges) {
    const set = /* @__PURE__ */ new Set();
    for (const e of edges) {
      if (e.kind === "belongs_to") continue;
      set.add(`${e.from}\0${e.to}`);
    }
    return set;
  }

  // src/renderer/canvas.ts
  var cachedColors = null;
  function invalidateColors() {
    cachedColors = null;
  }
  if (typeof matchMedia !== "undefined") {
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", invalidateColors);
    matchMedia("(prefers-color-scheme: light)").addEventListener("change", invalidateColors);
    matchMedia("(prefers-contrast: more)").addEventListener("change", invalidateColors);
  }
  function snapshotColors() {
    if (cachedColors !== null) return cachedColors;
    const s = getComputedStyle(document.documentElement);
    const v = (prop, fb) => s.getPropertyValue(prop).trim() || fb;
    cachedColors = {
      bg: v("--bg", "#1c1c26"),
      surface: v("--surface", "#282832"),
      surfaceHi: v("--surface-hi", "#32323c"),
      border: v("--border", "#3d3d48"),
      text: v("--text", "#e4e4ec"),
      textDim: v("--text-dim", "#8b8b9a"),
      accent: v("--accent", "#e8993a"),
      accentDim: v("--accent-dim", "#a06828"),
      edge: v("--edge", "#4a4a56"),
      edgeDep: v("--edge-dep", "#7aad6a"),
      edgeCon: v("--edge-con", "#c09040"),
      edgeSup: v("--edge-sup", "#a07890"),
      selectRing: v("--select", "#e8993a"),
      badge: v("--badge", "#4a4a56"),
      minimap: v("--minimap-bg", "#1c1c26"),
      minimapVp: v("--minimap-vp", "rgba(232,153,58,0.25)"),
      gridDot: v("--grid-dot", "#28283230"),
      shadow: v("--shadow", "rgba(0,0,0,0.25)")
    };
    return cachedColors;
  }
  var DAY_MS = 864e5;
  function filterDecisions(decisions, f) {
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
  var PATTERN_HUES = [30, 200, 150, 340, 60, 270, 100, 310];
  var LOD_FADE_MS = 150;
  var HULL_EXPAND = 50;
  var HULL_RADIUS = 20;
  var NODE_RADIUS = 8;
  var Renderer = class {
    constructor(canvas, cam) {
      /** Per-frame hover state — set at the top of render(), read by draw methods. */
      this.fh = null;
      /** Cached edge pair set — rebuilt per render frame. */
      this.edgePairSet = /* @__PURE__ */ new Set();
      // LOD transition fade state.
      this.prevLod = 0 /* Overview */;
      this.lodFadeAlpha = 1;
      this.lodFadeStart = 0;
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("Canvas 2D not supported");
      this.ctx = ctx;
      this.cam = cam;
      this.dpr = window.devicePixelRatio || 1;
      this.c = snapshotColors();
    }
    get cachedEdgePairSet() {
      return this.edgePairSet;
    }
    resize(w, h) {
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
    render(graph, selected, lod, focus = null, filters, hover) {
      this.c = snapshotColors();
      this.fh = hover ?? null;
      this.edgePairSet = buildEdgePairSet(graph.edges);
      const { ctx, cam, dpr, c } = this;
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
      const vpAABB = {
        cx: vp.x + vp.w / 2,
        cy: vp.y + vp.h / 2,
        hw: vp.w / 2,
        hh: vp.h / 2
      };
      if (cam.zoom > 0.15) {
        this.drawGrid(vp, c);
      }
      const visibleNames = graph.quadtree.queryViewport(vpAABB);
      ctx.save();
      ctx.translate(cam.screenW / 2, cam.screenH / 2);
      ctx.scale(cam.zoom, cam.zoom);
      ctx.translate(-cam.cx, -cam.cy);
      if (lod <= 1 /* Component */) {
        this.drawPatternRegions(graph, visibleNames, focus, lod, filters);
      }
      this.drawEdges(graph, visibleNames, lod, focus, filters);
      this.drawNodes(graph, visibleNames, selected, lod, focus, filters);
      ctx.restore();
      if (hover?.tooltipVisible && hover.tooltipText && lod === 0 /* Overview */) {
        this.drawTooltip(hover.tooltipText, hover.tooltipX, hover.tooltipY);
      }
      if (hover?.edge && hover.edgeTooltipText && !hover?.tooltipVisible) {
        this.drawTooltip(hover.edgeTooltipText, hover.tooltipX, hover.tooltipY);
      }
      if (hover?.pattern && hover.patternDesc && hover.tooltipVisible && !hover.node) {
        this.drawTooltip(hover.patternDesc, hover.tooltipX, hover.tooltipY);
      }
      this.fh = null;
      return this.lodFadeAlpha < 1;
    }
    // ── Pattern regions ─────────────────────────────────────────────────────
    /**
     * Draw semi-transparent pattern regions behind nodes/edges.
     * Skipped at LOD 2 (regions would fill the entire viewport).
     */
    drawPatternRegions(graph, visible, focus, lod, filters) {
      if (graph.patterns.size === 0) return;
      const { ctx, cam } = this;
      const prefersLight = typeof matchMedia !== "undefined" ? matchMedia("(prefers-color-scheme: light)").matches : false;
      const lightness = prefersLight ? 48 : 55;
      const saturation = prefersLight ? 40 : 45;
      const baseFill = prefersLight ? 0.1 : 0.14;
      const baseStroke = 0.45;
      const dimFill = 0.03;
      let patIdx = 0;
      for (const [patName, pat] of graph.patterns) {
        const memberNames = pat.components.filter((name) => visible.has(name));
        if (memberNames.length === 0) {
          patIdx++;
          continue;
        }
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
        const dimmedByFocus = focus !== null && !pat.components.some((n) => focus.has(n));
        let dimmedByFilter = false;
        if (filters && filters.activeTags.size > 0) {
          dimmedByFilter = !pat.decisions.some((dName) => {
            const dec = graph.decisions.get(dName);
            return dec && dec.tags.some((t) => filters.activeTags.has(t));
          });
        }
        const isHovered = this.fh?.pattern === patName;
        const hoverFillBoost = isHovered ? 1.5 : 1;
        const hoverStrokeBoost = isHovered ? 1.3 : 1;
        const fillAlpha = (dimmedByFilter ? dimFill : baseFill) * hoverFillBoost;
        const strokeAlpha = baseStroke * hoverStrokeBoost;
        const hue = PATTERN_HUES[patIdx % PATTERN_HUES.length];
        ctx.globalAlpha = dimmedByFocus ? 0.15 : 1;
        ctx.fillStyle = `hsla(${hue}, ${saturation}%, ${lightness}%, ${fillAlpha})`;
        roundedHullPath(ctx, expanded, HULL_RADIUS);
        ctx.fill();
        ctx.strokeStyle = `hsla(${hue}, ${saturation}%, ${lightness}%, ${strokeAlpha})`;
        ctx.lineWidth = (isHovered ? 2.5 : 2) / cam.zoom;
        ctx.stroke();
        {
          const cx = expanded.reduce((s, p) => s + p.x, 0) / expanded.length;
          const minY = Math.min(...expanded.map((p) => p.y));
          const labelY = minY - 8 / cam.zoom;
          const rawLabel = pat.description || pat.name;
          const maxLen = lod >= 2 /* Decision */ ? 60 : lod >= 1 /* Component */ ? 40 : 25;
          const label = rawLabel.length > maxLen ? rawLabel.slice(0, maxLen - 1) + "\u2026" : rawLabel;
          const labelFontSize = lod >= 1 /* Component */ ? 13 / cam.zoom : 12 / cam.zoom;
          ctx.font = `600 ${labelFontSize}px system-ui, sans-serif`;
          const tw = ctx.measureText(label).width;
          const px = 8 / cam.zoom;
          const py = 4 / cam.zoom;
          const pillW = tw + px * 2;
          const pillH = labelFontSize + py * 2;
          ctx.globalAlpha = (dimmedByFocus ? 0.15 : 1) * 0.92;
          ctx.fillStyle = `hsla(${hue}, ${saturation}%, ${prefersLight ? 95 : 15}%, 0.95)`;
          this.roundRect(cx - pillW / 2, labelY - pillH, pillW, pillH, 6 / cam.zoom);
          ctx.fill();
          ctx.strokeStyle = `hsla(${hue}, ${saturation}%, ${lightness}%, 0.5)`;
          ctx.lineWidth = 1 / cam.zoom;
          ctx.stroke();
          ctx.globalAlpha = dimmedByFocus ? 0.15 : 1;
          ctx.fillStyle = `hsla(${hue}, ${saturation + 10}%, ${prefersLight ? 35 : 75}%, 1)`;
          ctx.textAlign = "center";
          ctx.textBaseline = "bottom";
          ctx.fillText(label, cx, labelY - py);
        }
        patIdx++;
      }
      ctx.globalAlpha = 1;
    }
    // ── Edges ──────────────────────────────────────────────────────────────
    drawEdges(graph, visible, lod, focus, filters) {
      const { ctx, cam, c, fh } = this;
      const baseWidth = 1.5 / cam.zoom;
      const pairSet = this.edgePairSet;
      for (const e of graph.edges) {
        if (lod === 0 /* Overview */ && e.kind !== "connects_to") continue;
        if (e.kind === "belongs_to") continue;
        if (filters && !filters.edgeKinds.has(e.kind)) continue;
        const a = graph.nodes.get(e.from);
        const b = graph.nodes.get(e.to);
        if (!a || !b) continue;
        if (!visible.has(e.from) && !visible.has(e.to)) continue;
        const dimmed = focus !== null && !focus.has(e.from) && !focus.has(e.to);
        const isHovered = fh?.edge !== null && fh?.edge !== void 0 && fh.edge.from === e.from && fh.edge.to === e.to && fh.edge.kind === e.kind;
        const kindOpacity = EDGE_OPACITY[e.kind] ?? 0.6;
        ctx.globalAlpha = dimmed ? 0.15 : kindOpacity;
        const color = edgeColor(e.kind, c);
        ctx.strokeStyle = isHovered ? c.accent : color;
        ctx.lineWidth = isHovered ? baseWidth * 2.5 : baseWidth;
        ctx.setLineDash((EDGE_DASH[e.kind] ?? []).map((v) => v / cam.zoom));
        const hasBi = pairSet.has(`${e.to}\0${e.from}`);
        const reverse = hasBi && e.from > e.to;
        const { cpx, cpy } = edgeCurveCP(a.x, a.y, b.x, b.y, cam.zoom, reverse);
        ctx.beginPath();
        ctx.moveTo(a.x, a.y);
        ctx.quadraticCurveTo(cpx, cpy, b.x, b.y);
        ctx.stroke();
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
          tipY - uy * headLen + ux * headLen * 0.4
        );
        ctx.lineTo(
          tipX - ux * headLen + uy * headLen * 0.4,
          tipY - uy * headLen - ux * headLen * 0.4
        );
        ctx.fill();
        if (lod >= 1 /* Component */ && (e.kind !== "connects_to" || isHovered)) {
          const lx = 0.25 * a.x + 0.5 * cpx + 0.25 * b.x;
          const ly = 0.25 * a.y + 0.5 * cpy + 0.25 * b.y;
          const labelSize = 10 / cam.zoom;
          const label = e.kind.replace(/_/g, " ");
          ctx.font = `400 ${labelSize}px system-ui, sans-serif`;
          const tw = ctx.measureText(label).width;
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
            3 / cam.zoom
          );
          ctx.fill();
          ctx.globalAlpha = savedAlpha;
          ctx.fillStyle = c.text;
          ctx.textAlign = "center";
          ctx.textBaseline = "bottom";
          ctx.fillText(label, lx, ly - 3 / cam.zoom);
        }
      }
      ctx.setLineDash([]);
      ctx.globalAlpha = 1;
    }
    // ── Nodes ──────────────────────────────────────────────────────────────
    drawNodes(graph, visible, selected, lod, focus, filters) {
      for (const name of visible) {
        const node = graph.nodes.get(name);
        if (!node) continue;
        const isSelected = name === selected;
        const dimmed = focus !== null && !focus.has(name);
        this.ctx.globalAlpha = dimmed ? 0.3 : 1;
        this.drawNodeCompact(node, isSelected, lod >= 1 /* Component */, graph, filters);
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
    drawNodeCompact(node, selected, showDescription, graph, filters) {
      const { ctx, cam, c } = this;
      if (selected) this.drawSelectRing(node);
      this.drawShadow(node);
      ctx.fillStyle = selected ? c.surfaceHi : c.surface;
      this.roundRect(node.x - node.w / 2, node.y - node.h / 2, node.w, node.h, NODE_RADIUS);
      ctx.fill();
      this.drawNodeBorder(node);
      const fontSize = Math.max(12, 14 / Math.max(cam.zoom, 0.5));
      const hasDesc = showDescription && !!node.description;
      const nameY = hasDesc ? node.y - 10 : node.y - 4;
      ctx.font = `600 ${fontSize}px ui-monospace, 'SF Mono', 'Cascadia Code', 'Consolas', monospace`;
      ctx.fillStyle = c.text;
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(node.name, node.x, nameY, node.w - 16);
      if (hasDesc) {
        const descSize = fontSize * 0.72;
        ctx.font = `400 ${descSize}px system-ui, sans-serif`;
        ctx.fillStyle = c.textDim;
        const desc = node.description.length > 55 ? node.description.slice(0, 52) + "\u2026" : node.description;
        ctx.fillText(desc, node.x, node.y + 4, node.w - 16);
      }
      const rawCount = node.decisionCount ?? 0;
      const count = filters && rawCount > 0 ? filterDecisions(graph.decisionsFor(node.name), filters).length : rawCount;
      const patCount = node.patternCount ?? 0;
      let badgeText = "";
      if (count > 0 && patCount > 0) badgeText = `${count} \xB7 ${patCount}P`;
      else if (count > 0) badgeText = `${count}`;
      else if (patCount > 0) badgeText = `${patCount}P`;
      if (badgeText) {
        const badgeFontSize = fontSize * 0.7;
        const badgeY = hasDesc ? node.y + 18 : node.y + 8;
        ctx.font = `500 ${badgeFontSize}px system-ui, sans-serif`;
        ctx.fillStyle = c.badge;
        const bw = ctx.measureText(badgeText).width + 10;
        this.roundRect(node.x - bw / 2, badgeY, bw, badgeFontSize + 6, 4);
        ctx.fill();
        ctx.strokeStyle = c.border;
        ctx.lineWidth = 1 / cam.zoom;
        this.roundRect(node.x - bw / 2, badgeY, bw, badgeFontSize + 6, 4);
        ctx.stroke();
        ctx.fillStyle = c.text;
        ctx.fillText(badgeText, node.x, badgeY + (badgeFontSize + 6) / 2, bw);
      }
    }
    // ── Node border (with hover highlight) ────────────────────────────────
    /**
     * Draw the node border. When the node is hovered, blends toward
     * --accent-dim with 1px extra width.
     */
    drawNodeBorder(node, overrideH) {
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
    drawSelectRing(node, overrideH) {
      const { ctx, cam, c } = this;
      const h = overrideH ?? node.h;
      ctx.strokeStyle = c.selectRing;
      ctx.lineWidth = 3 / cam.zoom;
      this.roundRect(node.x - node.w / 2 - 4, node.y - h / 2 - 4, node.w + 8, h + 8, 12);
      ctx.stroke();
    }
    // ── Tooltip ───────────────────────────────────────────────────────────
    /** Canvas-rendered tooltip in screen space. */
    drawTooltip(text, sx, sy) {
      const { ctx, c } = this;
      const fontSize = 12;
      const padding = 8;
      const offsetY = 20;
      const radius = 6;
      ctx.font = `400 ${fontSize}px system-ui, sans-serif`;
      const tw = ctx.measureText(text).width;
      const boxW = tw + padding * 2;
      const boxH = fontSize + padding * 2;
      let x = sx - boxW / 2;
      let y = sy + offsetY;
      const maxX = this.cam.screenW - boxW - 4;
      const maxY = this.cam.screenH - boxH - 4;
      if (x < 4) x = 4;
      if (x > maxX) x = maxX;
      if (y > maxY) y = sy - offsetY - boxH;
      ctx.fillStyle = "rgba(17, 15, 13, 0.92)";
      this.roundRect(x, y, boxW, boxH, radius);
      ctx.fill();
      ctx.fillStyle = c.text;
      ctx.textAlign = "center";
      ctx.textBaseline = "middle";
      ctx.fillText(text, x + boxW / 2, y + boxH / 2, boxW - padding * 2);
    }
    // ── Minimap ────────────────────────────────────────────────────────────
    renderMinimap(miniCtx, mw, mh, graph) {
      const { dpr, c } = this;
      miniCtx.setTransform(dpr, 0, 0, dpr, 0, 0);
      miniCtx.fillStyle = c.minimap;
      miniCtx.fillRect(0, 0, mw, mh);
      if (graph.nodes.size === 0) return;
      let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
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
      miniCtx.strokeStyle = c.edge;
      miniCtx.lineWidth = 0.5;
      miniCtx.beginPath();
      for (const e of graph.edges) {
        if (e.kind !== "connects_to") continue;
        const a = graph.nodes.get(e.from);
        const b = graph.nodes.get(e.to);
        if (!a || !b) continue;
        miniCtx.moveTo(ox + (a.x - minX) * scale, oy + (a.y - minY) * scale);
        miniCtx.lineTo(ox + (b.x - minX) * scale, oy + (b.y - minY) * scale);
      }
      miniCtx.stroke();
      miniCtx.fillStyle = c.accent;
      for (const n of graph.nodes.values()) {
        miniCtx.beginPath();
        miniCtx.arc(ox + (n.x - minX) * scale, oy + (n.y - minY) * scale, 3, 0, Math.PI * 2);
        miniCtx.fill();
      }
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
    drawGrid(vp, c) {
      const { ctx, cam, dpr } = this;
      const spacing = 80;
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
    drawShadow(node, overrideH) {
      const { ctx, cam, c } = this;
      const h = overrideH ?? node.h;
      const softOffset = 4 / cam.zoom;
      const savedAlpha = ctx.globalAlpha;
      ctx.globalAlpha = savedAlpha * 0.3;
      ctx.fillStyle = c.shadow;
      this.roundRect(
        node.x - node.w / 2 + softOffset,
        node.y - h / 2 + softOffset,
        node.w,
        h,
        NODE_RADIUS
      );
      ctx.fill();
      ctx.globalAlpha = savedAlpha;
      const offset = 2 / cam.zoom;
      ctx.fillStyle = c.shadow;
      this.roundRect(node.x - node.w / 2 + offset, node.y - h / 2 + offset, node.w, h, NODE_RADIUS);
      ctx.fill();
    }
    // ── Helpers ────────────────────────────────────────────────────────────
    roundRect(x, y, w, h, r) {
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
  };

  // src/ui/search.ts
  var MAX_RESULTS = 10;
  var MIN_TOKEN_LEN = 2;
  function search(graph, query) {
    const tokens = tokenize(query);
    if (tokens.length === 0) return [];
    const results = [];
    for (const [name, node] of graph.nodes) {
      const blob = `${name} ${node.description ?? ""}`.toLowerCase();
      const score = scoreTokens(tokens, blob);
      if (score > 0) {
        results.push({ name, kind: "component", label: name, score });
      }
    }
    for (const [name, dec] of graph.decisions) {
      const blob = `${name} ${dec.choice} ${dec.reason} ${dec.component} ${dec.tags.join(" ")}`.toLowerCase();
      const score = scoreTokens(tokens, blob);
      if (score > 0) {
        results.push({
          name,
          kind: "decision",
          label: `${dec.choice} (${dec.component})`,
          score
        });
      }
    }
    for (const [name, pat] of graph.patterns) {
      const blob = `${name} ${pat.description}`.toLowerCase();
      const score = scoreTokens(tokens, blob);
      if (score > 0) {
        results.push({ name, kind: "pattern", label: pat.description, score });
      }
    }
    results.sort((a, b) => b.score - a.score);
    return results.slice(0, MAX_RESULTS);
  }
  function neighborhood(graph, center) {
    const result = /* @__PURE__ */ new Set();
    result.add(center);
    const dec = graph.decisions.get(center);
    if (dec) result.add(dec.component);
    for (const e of graph.edges) {
      if (e.kind !== "connects_to") continue;
      if (e.from === center) result.add(e.to);
      if (e.to === center) result.add(e.from);
    }
    for (const name of [...result]) {
      for (const d of graph.decisionsFor(name)) {
        result.add(d.name);
      }
    }
    return result;
  }
  function tokenize(query) {
    return query.toLowerCase().split(/\s+/).filter((t) => t.length >= MIN_TOKEN_LEN);
  }
  function scoreTokens(tokens, blob) {
    let score = 0;
    for (const t of tokens) {
      if (blob.includes(t)) score++;
    }
    return score;
  }

  // src/ui/command.ts
  var CommandPalette = class {
    constructor() {
      this.actions = [];
      this.filtered = [];
      this.activeIndex = -1;
      this._open = false;
      this.backdrop = document.getElementById("palette-backdrop");
      this.input = document.getElementById("palette-input");
      this.results = document.getElementById("palette-results");
      this.backdrop.addEventListener("pointerdown", (e) => {
        if (e.target === this.backdrop) this.close();
      });
      this.input.addEventListener("input", () => {
        this.filter(this.input.value);
        this.render();
      });
      this.input.addEventListener("keydown", (e) => {
        if (e.key === "Escape") {
          this.close();
          return;
        }
        if (e.key === "ArrowDown") {
          e.preventDefault();
          if (this.activeIndex < this.filtered.length - 1) {
            this.activeIndex++;
            this.render();
          }
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          if (this.activeIndex > 0) {
            this.activeIndex--;
            this.render();
          }
          return;
        }
        if (e.key === "Enter") {
          e.preventDefault();
          if (this.activeIndex >= 0 && this.activeIndex < this.filtered.length) {
            const action = this.filtered[this.activeIndex];
            this.close();
            action.run();
          }
          return;
        }
      });
    }
    get isOpen() {
      return this._open;
    }
    open(actions) {
      this.actions = actions;
      this.filtered = actions;
      this.activeIndex = actions.length > 0 ? 0 : -1;
      this._open = true;
      this.backdrop.classList.remove("hidden");
      this.input.value = "";
      this.input.focus();
      this.render();
    }
    close() {
      this.backdrop.classList.add("hidden");
      this._open = false;
      this.filtered = [];
      this.activeIndex = -1;
    }
    filter(query) {
      const q = query.toLowerCase().trim();
      if (q.length === 0) {
        this.filtered = this.actions;
      } else {
        this.filtered = this.actions.filter((a) => a.label.toLowerCase().includes(q));
      }
      this.activeIndex = this.filtered.length > 0 ? 0 : -1;
    }
    render() {
      const el = this.results;
      if (this.filtered.length === 0) {
        el.innerHTML = '<div class="palette-action" style="color:var(--text-dim)">No matching commands</div>';
        return;
      }
      el.innerHTML = this.filtered.map((a, i) => {
        const active2 = i === this.activeIndex ? " active" : "";
        const shortcut = a.shortcut ? `<span class="palette-shortcut">${esc(a.shortcut)}</span>` : "";
        return `<div class="palette-action${active2}" data-idx="${i}">${esc(a.label)}${shortcut}</div>`;
      }).join("");
      for (const child of el.children) {
        child.addEventListener("click", () => {
          const idx = parseInt(child.dataset.idx ?? "-1", 10);
          if (idx >= 0 && idx < this.filtered.length) {
            this.close();
            this.filtered[idx].run();
          }
        });
      }
      const active = el.querySelector(".active");
      if (active) active.scrollIntoView({ block: "nearest" });
    }
  };

  // src/ui/breadcrumb.ts
  var Breadcrumb = class {
    constructor(callbacks) {
      this.el = document.getElementById("breadcrumb");
      this.onProject = callbacks.onProject;
      this.onComponent = callbacks.onComponent;
    }
    /** Update the trail. Pass `null` to clear (project-level view). */
    update(projectName, selected) {
      if (!selected) {
        this.el.innerHTML = "";
        return;
      }
      const label = projectName || "Project";
      this.el.innerHTML = `<span class="breadcrumb-seg" data-bc="project">${esc(label)}</span><span class="breadcrumb-sep">\u2192</span><span class="breadcrumb-seg" data-bc="${esc(selected)}">${esc(selected)}</span>`;
      for (const seg of this.el.querySelectorAll(".breadcrumb-seg")) {
        seg.addEventListener("click", () => {
          const target = seg.dataset.bc;
          if (target === "project") this.onProject();
          else if (target) this.onComponent(target);
        });
      }
    }
  };

  // src/types.ts
  function defaultFilterState() {
    return {
      edgeKinds: /* @__PURE__ */ new Set(["connects_to", "depends_on", "constrains", "supersedes"]),
      activeTags: /* @__PURE__ */ new Set(),
      focusMode: false,
      maxAgeDays: null
    };
  }

  // src/ui/toolbar.ts
  var AGE_OPTIONS = [
    { label: "All", days: null },
    { label: "1w", days: 7 },
    { label: "1m", days: 30 },
    { label: "3m", days: 90 },
    { label: "1y", days: 365 }
  ];
  var EDGE_KINDS = ["connects_to", "depends_on", "constrains", "supersedes"];
  var Toolbar = class {
    constructor(onChange) {
      this.tags = [];
      this.popoverOpen = false;
      this.tagFilter = "";
      this.documentPointerHandler = null;
      this.escapeHandler = null;
      this.el = document.getElementById("toolbar");
      this.state = defaultFilterState();
      this.onChange = onChange;
      this.render();
    }
    get filterState() {
      return this.state;
    }
    /** Update the tag list when the graph changes. Re-renders tag pills. */
    setAvailableTags(tags) {
      for (const t of this.state.activeTags) {
        if (!tags.includes(t)) this.state.activeTags.delete(t);
      }
      this.tags = tags;
      this.render();
    }
    /** Sync the focus pill visual without firing onChange (prevents loops). */
    setFocusActive(active) {
      if (this.state.focusMode === active) return;
      this.state.focusMode = active;
      this.render();
    }
    emit() {
      this.onChange(this.state);
    }
    render() {
      const parts = [];
      parts.push('<div class="toolbar-group">');
      parts.push('<span class="toolbar-label">Edges</span>');
      for (const kind of EDGE_KINDS) {
        const on = this.state.edgeKinds.has(kind) ? " on" : "";
        const label = kind.replace(/_/g, " ");
        parts.push(`<button class="toolbar-pill${on}" data-edge="${kind}">${esc(label)}</button>`);
      }
      parts.push("</div>");
      if (this.tags.length > 0) {
        const activeCount = this.state.activeTags.size;
        const label = activeCount > 0 ? `Tags (${activeCount})` : "Tags";
        const on = activeCount > 0 ? " on" : "";
        parts.push('<div class="toolbar-group has-popover">');
        parts.push('<span class="toolbar-label">Tags</span>');
        parts.push(`<button class="toolbar-pill${on}" data-role="tag-toggle">${esc(label)}</button>`);
        const hidden = this.popoverOpen ? "" : " hidden";
        parts.push(`<div class="tag-popover${hidden}">`);
        parts.push('<input type="text" placeholder="Filter tags\u2026" data-role="tag-filter">');
        parts.push('<div class="tag-popover-list">');
        const lowerFilter = this.tagFilter.toLowerCase();
        for (const tag of this.tags) {
          if (lowerFilter && !tag.toLowerCase().includes(lowerFilter)) continue;
          const checked = this.state.activeTags.has(tag) ? " checked" : "";
          parts.push(
            `<label class="tag-popover-item" data-tag="${esc(tag)}"><input type="checkbox"${checked} data-tag-check="${esc(tag)}">${esc(tag)}</label>`
          );
        }
        parts.push("</div>");
        parts.push("</div>");
        parts.push("</div>");
      }
      parts.push('<div class="toolbar-group">');
      parts.push('<span class="toolbar-label">Age</span>');
      parts.push('<select class="toolbar-select" data-role="age">');
      for (const opt of AGE_OPTIONS) {
        const sel = this.state.maxAgeDays === opt.days ? " selected" : "";
        const val = opt.days === null ? "" : String(opt.days);
        parts.push(`<option value="${val}"${sel}>${esc(opt.label)}</option>`);
      }
      parts.push("</select>");
      parts.push("</div>");
      parts.push('<div class="toolbar-group">');
      const fOn = this.state.focusMode ? " on" : "";
      parts.push(`<button class="toolbar-pill${fOn}" data-role="focus">Focus</button>`);
      parts.push("</div>");
      this.el.innerHTML = parts.join("");
      this.attachHandlers();
    }
    attachHandlers() {
      for (const btn of this.el.querySelectorAll("[data-edge]")) {
        btn.addEventListener("click", () => {
          const kind = btn.dataset.edge;
          if (this.state.edgeKinds.has(kind)) {
            this.state.edgeKinds.delete(kind);
          } else {
            this.state.edgeKinds.add(kind);
          }
          this.render();
          this.emit();
        });
      }
      const tagToggle = this.el.querySelector('[data-role="tag-toggle"]');
      if (tagToggle) {
        tagToggle.addEventListener("click", () => {
          if (this.popoverOpen) {
            this.closePopover();
          } else {
            this.openPopover();
          }
        });
      }
      const tagFilterInput = this.el.querySelector('[data-role="tag-filter"]');
      if (tagFilterInput) {
        tagFilterInput.value = this.tagFilter;
        tagFilterInput.addEventListener("input", () => {
          this.tagFilter = tagFilterInput.value;
          this.render();
          this.restorePopoverFocus();
        });
      }
      for (const cb of this.el.querySelectorAll("[data-tag-check]")) {
        cb.addEventListener("change", () => {
          const tag = cb.dataset.tagCheck;
          if (this.state.activeTags.has(tag)) {
            this.state.activeTags.delete(tag);
          } else {
            this.state.activeTags.add(tag);
          }
          this.render();
          this.restorePopoverFocus();
          this.emit();
        });
      }
      this.removeDocumentHandler();
      if (this.popoverOpen) {
        this.documentPointerHandler = (e) => {
          const popover = this.el.querySelector(".tag-popover");
          const toggle = this.el.querySelector('[data-role="tag-toggle"]');
          const target = e.target;
          if (popover && !popover.contains(target) && toggle && !toggle.contains(target)) {
            this.closePopover();
          }
        };
        document.addEventListener("pointerdown", this.documentPointerHandler);
      }
      const ageSel = this.el.querySelector('[data-role="age"]');
      if (ageSel) {
        ageSel.addEventListener("change", () => {
          this.state.maxAgeDays = ageSel.value === "" ? null : parseInt(ageSel.value, 10);
          this.emit();
        });
      }
      const focusBtn = this.el.querySelector('[data-role="focus"]');
      if (focusBtn) {
        focusBtn.addEventListener("click", () => {
          this.state.focusMode = !this.state.focusMode;
          this.render();
          this.emit();
        });
      }
      if (this.escapeHandler) this.el.removeEventListener("keydown", this.escapeHandler);
      this.escapeHandler = (e) => {
        if (e.key === "Escape" && this.popoverOpen) {
          this.closePopover();
        }
      };
      this.el.addEventListener("keydown", this.escapeHandler);
    }
    openPopover() {
      this.popoverOpen = true;
      this.tagFilter = "";
      this.render();
      this.restorePopoverFocus();
      this.clampPopoverPosition();
    }
    clampPopoverPosition() {
      const popover = this.el.querySelector(".tag-popover");
      if (!popover) return;
      const rect = popover.getBoundingClientRect();
      if (rect.bottom > window.innerHeight) {
        popover.style.maxHeight = `${window.innerHeight - rect.top - 20}px`;
      }
      if (rect.right > window.innerWidth) {
        popover.style.left = "auto";
        popover.style.right = "0";
      }
    }
    closePopover() {
      this.popoverOpen = false;
      this.tagFilter = "";
      this.removeDocumentHandler();
      this.render();
    }
    restorePopoverFocus() {
      const input = this.el.querySelector('[data-role="tag-filter"]');
      if (input) input.focus();
    }
    removeDocumentHandler() {
      if (this.documentPointerHandler) {
        document.removeEventListener("pointerdown", this.documentPointerHandler);
        this.documentPointerHandler = null;
      }
    }
  };

  // src/interaction/keyboard.ts
  var KeyboardDispatch = class {
    constructor(bindings) {
      this.bindings = [];
      this.bindings = bindings;
    }
    /** Install the global keydown listener. Returns a cleanup function. */
    attach() {
      const handler = (e) => this.handle(e);
      window.addEventListener("keydown", handler);
      return () => window.removeEventListener("keydown", handler);
    }
    handle(e) {
      for (const b of this.bindings) {
        if (b.match(e)) {
          b.run(e);
          return;
        }
      }
    }
  };
  var META = (e) => e.ctrlKey || e.metaKey;
  var Keys = {
    cmdK: (e) => META(e) && e.key === "k",
    search: (e) => e.key === "/" || META(e) && e.key === "f",
    undo: (e) => META(e) && !e.shiftKey && e.key === "z",
    redo: (e) => META(e) && e.shiftKey && e.key === "Z",
    escape: (e) => e.key === "Escape",
    zoomFit: (e) => META(e) && e.key === "0",
    zoomIn: (e) => e.key === "=" || e.key === "+",
    zoomOut: (e) => e.key === "-",
    arrowLeft: (e) => e.key === "ArrowLeft",
    arrowRight: (e) => e.key === "ArrowRight",
    arrowUp: (e) => e.key === "ArrowUp",
    arrowDown: (e) => e.key === "ArrowDown",
    tab: (e) => e.key === "Tab",
    enter: (e) => e.key === "Enter",
    del: (e) => e.key === "Delete" || e.key === "Backspace"
  };

  // src/app/undo.ts
  var UndoStack = class {
    constructor() {
      this.undos = [];
      this.redos = [];
      this.limit = 50;
    }
    push(cmd) {
      this.undos.push(cmd);
      if (this.undos.length > this.limit) this.undos.shift();
      this.redos.length = 0;
    }
    async undo() {
      const cmd = this.undos.pop();
      if (!cmd) return null;
      try {
        await cmd.undo();
        this.redos.push(cmd);
        return cmd.description;
      } catch (e) {
        console.error("Undo failed:", e);
        return null;
      }
    }
    async redo() {
      const cmd = this.redos.pop();
      if (!cmd) return null;
      try {
        await cmd.redo();
        this.undos.push(cmd);
        return cmd.description;
      } catch (e) {
        console.error("Redo failed:", e);
        return null;
      }
    }
    canUndo() {
      return this.undos.length > 0;
    }
    canRedo() {
      return this.redos.length > 0;
    }
  };

  // src/app/selection.ts
  var Selection = class {
    constructor() {
      this._selected = null;
      this._focusSet = null;
      this._componentNames = [];
      // Search state.
      this._searchOpen = false;
      this._searchResults = [];
      this._searchActiveIndex = -1;
    }
    // ── Accessors ──────────────────────────────────────────────────────
    get selected() {
      return this._selected;
    }
    get focusSet() {
      return this._focusSet;
    }
    get componentNames() {
      return this._componentNames;
    }
    get searchOpen() {
      return this._searchOpen;
    }
    get searchResults() {
      return this._searchResults;
    }
    get searchActiveIndex() {
      return this._searchActiveIndex;
    }
    // ── Selection ──────────────────────────────────────────────────────
    select(name) {
      this._selected = name;
    }
    // ── Focus ──────────────────────────────────────────────────────────
    clearFocus() {
      this._focusSet = null;
    }
    /** Compute and set the 1-hop neighborhood for `name`. */
    setFocus(name, graph) {
      this._focusSet = neighborhood(graph, name);
    }
    // ── Component cycling ──────────────────────────────────────────────
    setComponentNames(names) {
      this._componentNames = names;
    }
    /**
     * Cycle to the next (direction=1) or previous (direction=-1)
     * component in sorted order. Returns the target name, or null
     * if no components exist.
     */
    cycleComponent(direction) {
      const names = this._componentNames;
      if (names.length === 0) return null;
      const curIdx = this._selected ? names.indexOf(this._selected) : -1;
      let next = curIdx + direction;
      if (next < 0) next = names.length - 1;
      if (next >= names.length) next = 0;
      return names[next];
    }
    // ── Search ─────────────────────────────────────────────────────────
    openSearch() {
      this._searchOpen = true;
      this._searchResults = [];
      this._searchActiveIndex = -1;
    }
    closeSearch() {
      this._searchOpen = false;
      this._searchResults = [];
      this._searchActiveIndex = -1;
    }
    setSearchResults(results) {
      this._searchResults = results;
      this._searchActiveIndex = results.length > 0 ? 0 : -1;
    }
    nextSearchResult() {
      if (this._searchActiveIndex < this._searchResults.length - 1) {
        this._searchActiveIndex++;
      }
    }
    prevSearchResult() {
      if (this._searchActiveIndex > 0) {
        this._searchActiveIndex--;
      }
    }
    /** Return the currently highlighted search result, or null. */
    activeSearchResult() {
      const idx = this._searchActiveIndex;
      if (idx >= 0 && idx < this._searchResults.length) {
        return this._searchResults[idx];
      }
      return null;
    }
  };

  // src/app/drag.ts
  var LAYOUT_SAVE_DELAY = 500;
  var DragState = class {
    constructor() {
      this._dragging = null;
      this._panning = false;
      this.lastMouse = { x: 0, y: 0 };
      this.layoutSaveTimer = null;
      // Minimap
      this._minimapTransform = null;
      this._minimapDragging = false;
    }
    // ── Accessors ──────────────────────────────────────────────────────
    get dragging() {
      return this._dragging;
    }
    get panning() {
      return this._panning;
    }
    get minimapDragging() {
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
    onPointerDown(hit, sx, sy) {
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
    onPointerMove(sx, sy, camera, graph) {
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
    onPointerUp() {
      const wasDragging = this._dragging !== null;
      this._dragging = null;
      this._panning = false;
      return { needsRender: false, nodePositionChanged: wasDragging };
    }
    /**
     * Debounced layout persistence. Resets the timer on each call
     * so rapid drags coalesce into a single save.
     */
    scheduleLayoutSave(saveFn) {
      if (this.layoutSaveTimer != null) clearTimeout(this.layoutSaveTimer);
      this.layoutSaveTimer = window.setTimeout(saveFn, LAYOUT_SAVE_DELAY);
    }
    // ── Minimap ────────────────────────────────────────────────────────
    setMinimapTransform(t) {
      this._minimapTransform = t;
    }
    onMinimapDown(pointerId, target) {
      target.setPointerCapture(pointerId);
      this._minimapDragging = true;
    }
    onMinimapUp() {
      this._minimapDragging = false;
    }
    /**
     * Convert minimap screen coordinates to world coordinates.
     * Returns null if no minimap transform has been set.
     */
    minimapToWorld(sx, sy) {
      const t = this._minimapTransform;
      if (!t) return null;
      return {
        wx: t.minX + (sx - t.ox) / t.scale,
        wy: t.minY + (sy - t.oy) / t.scale
      };
    }
  };

  // src/app/navigation.ts
  var FIT_ALL_MS = 400;
  var FOCUS_NODE_MS = 300;
  var Navigation = class {
    constructor(camera) {
      this.camera = camera;
    }
    /** Fit every node into the viewport with padding. No-op on empty graph. */
    fitAll(graph) {
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
    focusNode(name, graph) {
      const node = graph.nodes.get(name);
      if (!node) return;
      const pad = 300;
      this.camera.fitBounds(
        node.x - pad,
        node.y - pad,
        node.x + pad,
        node.y + pad,
        80,
        FOCUS_NODE_MS
      );
    }
  };

  // src/app/filters.ts
  var Filters = class {
    get state() {
      return this._state;
    }
    update(state) {
      this._state = state;
    }
  };

  // src/app/hover.ts
  var BORDER_RAMP_MS = 100;
  var TOOLTIP_DWELL_MS = 400;
  var TOOLTIP_MAX_CHARS = 80;
  var HoverTracker = class {
    constructor() {
      // Node hover.
      this._node = null;
      this._borderAlpha = 0;
      this._tooltipVisible = false;
      this._tooltipText = "";
      this.enterTime = 0;
      // Pattern hover.
      this._pattern = null;
      this._patternDesc = "";
      // Edge hover.
      this._edge = null;
      this._edgeTooltipText = "";
      // Cursor position (screen-space).
      this._tooltipX = 0;
      this._tooltipY = 0;
    }
    // ── Getters (satisfy HoverRenderState) ─────────────────────────────
    get node() {
      return this._node;
    }
    get pattern() {
      return this._pattern;
    }
    get patternDesc() {
      return this._patternDesc;
    }
    get borderAlpha() {
      return this._borderAlpha;
    }
    get tooltipVisible() {
      return this._tooltipVisible;
    }
    get tooltipText() {
      return this._tooltipText;
    }
    get tooltipX() {
      return this._tooltipX;
    }
    get tooltipY() {
      return this._tooltipY;
    }
    get edge() {
      return this._edge;
    }
    get edgeTooltipText() {
      return this._edgeTooltipText;
    }
    // ── Update (called on pointer move) ────────────────────────────────
    /**
     * Update hover targets based on hit-test results.
     * Priority: node > pattern > edge.
     *
     * @param nodeName     Hit-tested node name, or null.
     * @param nodeDesc     Node description (for tooltip text).
     * @param patternName  Hit-tested pattern name, or null.
     * @param patternDesc  Pattern description (for tooltip text).
     * @param hitEdge      Hit-tested edge, or null (only when no node hit).
     * @param sx           Screen-space cursor X.
     * @param sy           Screen-space cursor Y.
     * @param now          Current timestamp (performance.now()).
     * @returns            True if the hover target changed (caller should re-render).
     */
    update(nodeName, nodeDesc, patternName, patternDesc, hitEdge, sx, sy, now) {
      this._tooltipX = sx;
      this._tooltipY = sy;
      let changed = false;
      if (nodeName !== this._node) {
        this._node = nodeName;
        this._tooltipText = nodeName ? truncate(nodeDesc, TOOLTIP_MAX_CHARS) : "";
        this._borderAlpha = 0;
        this._tooltipVisible = false;
        this.enterTime = nodeName ? now : 0;
        changed = true;
      }
      const effectivePattern = nodeName ? null : patternName;
      if (effectivePattern !== this._pattern) {
        this._pattern = effectivePattern;
        this._patternDesc = patternDesc;
        if (!this._node) {
          this._tooltipVisible = false;
          this.enterTime = effectivePattern ? now : 0;
        }
        changed = true;
      }
      const effectiveEdge = nodeName || effectivePattern ? null : hitEdge;
      if (!sameEdge(effectiveEdge, this._edge)) {
        this._edge = effectiveEdge;
        this._edgeTooltipText = effectiveEdge ? `${effectiveEdge.from} \u2192 ${effectiveEdge.to}` : "";
        changed = true;
      }
      return changed;
    }
    // ── Tick (called every render frame) ───────────────────────────────
    /**
     * Advance hover animations. Returns true if any visual state changed
     * (caller should set needsRender).
     */
    tick(now) {
      const hasTarget = this._node || this._pattern;
      if (!hasTarget) {
        let changed2 = false;
        if (this._borderAlpha > 0) {
          this._borderAlpha = 0;
          changed2 = true;
        }
        if (this._tooltipVisible) {
          this._tooltipVisible = false;
          changed2 = true;
        }
        return changed2;
      }
      const elapsed = now - this.enterTime;
      let changed = false;
      if (this._node) {
        const targetAlpha = Math.min(1, elapsed / BORDER_RAMP_MS);
        if (targetAlpha !== this._borderAlpha) {
          this._borderAlpha = targetAlpha;
          changed = true;
        }
      } else if (this._borderAlpha > 0) {
        this._borderAlpha = 0;
        changed = true;
      }
      const shouldShow = elapsed >= TOOLTIP_DWELL_MS;
      if (shouldShow !== this._tooltipVisible) {
        this._tooltipVisible = shouldShow;
        changed = true;
      }
      return changed;
    }
    // ── Clear (called on pointer down / drag start) ────────────────────
    clear() {
      this._node = null;
      this._pattern = null;
      this._patternDesc = "";
      this._edge = null;
      this._edgeTooltipText = "";
      this._borderAlpha = 0;
      this._tooltipVisible = false;
      this._tooltipText = "";
      this.enterTime = 0;
    }
  };
  function truncate(s, max) {
    return s.length > max ? s.slice(0, max - 1) + "\u2026" : s;
  }
  function sameEdge(a, b) {
    if (a === b) return true;
    if (!a || !b) return false;
    return a.from === b.from && a.to === b.to && a.kind === b.kind;
  }

  // src/main.ts
  var App = class {
    constructor() {
      // Infrastructure — immutable references, no domain state.
      this.graph = new Graph();
      this.camera = new Camera();
      this.layout = new ForceLayout();
      // Domain modules — own all mutable application state.
      this.undo = new UndoStack();
      this.selection = new Selection();
      this.drag = new DragState();
      this.filters = new Filters();
      this.hover = new HoverTracker();
      // Render-loop scheduling — derived per frame, not domain state.
      this.needsRender = true;
      this.lod = 0 /* Overview */;
      this.visibleCount = 0;
      // ── Render loop ─────────────────────────────────────────────────────────
      this.renderLoop = () => {
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
            this.hover
          );
          this.drag.setMinimapTransform(this.renderMinimap());
          this.needsRender = fading;
        }
        requestAnimationFrame(this.renderLoop);
      };
      const token = new URLSearchParams(location.search).get("token") ?? "";
      this.api = new ApiClient(token);
      const canvas = document.getElementById("canvas");
      this.canvas = canvas;
      this.renderer = new Renderer(canvas, this.camera);
      this.nav = new Navigation(this.camera);
      this.panel = new Panel(document.getElementById("panel"));
      this.panel.init(this.api, {
        onNavigate: (name) => this.selectAndFocus(name),
        onMutated: () => this.reloadGraph()
      });
      this.aria = document.getElementById("aria-live");
      this.palette = new CommandPalette();
      this.breadcrumb = new Breadcrumb({
        onProject: () => {
          this.selection.select(null);
          this.syncFocusClear();
          this.panel.showProject(this.graph);
          this.fitView();
          this.breadcrumb.update(this.graph.projectName, null);
        },
        onComponent: (name) => this.selectAndFocus(name)
      });
      this.toolbar = new Toolbar((state) => this.onFilterChange(state));
      const panelToggle = document.getElementById("panel-toggle");
      panelToggle.addEventListener("click", () => {
        const panel = document.getElementById("panel");
        const collapsed = panel.classList.toggle("collapsed");
        document.body.classList.toggle("panel-collapsed", collapsed);
        panelToggle.textContent = collapsed ? "\u203A" : "\u2039";
        this.handleResize();
      });
      const minimap = document.getElementById("minimap");
      const mctx = minimap.getContext("2d");
      if (!mctx) throw new Error("minimap context");
      this.miniCtx = mctx;
      this.setupCanvasEvents(canvas, minimap);
      this.setupSearch();
      this.installKeyboard();
      this.handleResize();
      this.setupPanelResize();
      window.addEventListener("resize", () => this.handleResize());
      this.panel.showLoading();
      this.api.fetchGraph().then((snap) => {
        document.getElementById("loading-overlay").classList.add("hidden");
        this.graph.loadSnapshot(snap);
        this.selection.setComponentNames([...this.graph.nodes.keys()].sort());
        this.layout.run(this.graph.nodes, this.graph.edges, 200);
        this.graph.rebuildQuadtree();
        this.graph.rebuildPatternHulls();
        this.fitView();
        this.updateLOD();
        this.panel.showProject(this.graph);
        this.breadcrumb.update(this.graph.projectName, null);
        this.toolbar.setAvailableTags(this.graph.allTags());
        this.needsRender = true;
        this.showFirstVisitHint();
      }).catch((e) => {
        console.error("Failed to load graph:", e);
        document.getElementById("loading-overlay").classList.add("hidden");
        this.panel.showLoadError(() => this.reloadGraph());
      });
      new WsConnection(
        token,
        (ev) => this.handleWsEvent(ev),
        (state) => this.handleWsState(state)
      );
      this.renderLoop();
    }
    // ── LOD ─────────────────────────────────────────────────────────────────
    updateLOD() {
      const vp = this.camera.viewport();
      const vpAABB = {
        cx: vp.x + vp.w / 2,
        cy: vp.y + vp.h / 2,
        hw: vp.w / 2,
        hh: vp.h / 2
      };
      const visible = this.graph.quadtree.queryViewport(vpAABB);
      this.visibleCount = visible.size;
      this.lod = computeLOD(this.visibleCount, vp.w * vp.h);
    }
    // ── Canvas pointer events ───────────────────────────────────────────────
    setupCanvasEvents(canvas, minimap) {
      canvas.addEventListener("pointerdown", (e) => this.onPointerDown(e));
      canvas.addEventListener("pointermove", (e) => this.onPointerMove(e));
      canvas.addEventListener("pointerup", () => this.onPointerUp());
      canvas.addEventListener("pointerleave", () => {
        this.onPointerUp();
        this.hover.clear();
        this.canvas.style.cursor = "";
        this.needsRender = true;
      });
      canvas.addEventListener("wheel", (e) => this.onWheel(e), { passive: false });
      minimap.addEventListener("pointerdown", (e) => this.onMinimapDown(e));
      minimap.addEventListener("pointermove", (e) => this.onMinimapMove(e));
      minimap.addEventListener("pointerup", () => this.drag.onMinimapUp());
      minimap.addEventListener("pointerleave", () => this.drag.onMinimapUp());
    }
    onPointerDown(e) {
      if (this.selection.searchOpen) this.closeSearch();
      if (this.palette.isOpen) {
        this.palette.close();
        this.canvas.focus();
      }
      this.hover.clear();
      this.canvas.style.cursor = "";
      const canvas = e.target;
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
        const patHit = this.graph.patternAt(wx, wy);
        if (patHit) {
          this.selection.select(null);
          this.syncFocusClear();
          this.panel.showPattern(patHit, this.graph);
          this.announce(`Selected pattern: ${patHit}`);
        } else {
          this.selection.select(null);
          this.syncFocusClear();
          this.panel.showProject(this.graph);
        }
      }
      this.breadcrumb.update(this.graph.projectName, this.selection.selected);
      this.needsRender = true;
    }
    onPointerMove(e) {
      if (this.drag.dragging || this.drag.panning) {
        const result = this.drag.onPointerMove(e.offsetX, e.offsetY, this.camera, this.graph);
        if (result.needsRender) {
          if (this.drag.panning) this.updateLOD();
          this.needsRender = true;
        }
        return;
      }
      const wx = this.camera.toWorldX(e.offsetX);
      const wy = this.camera.toWorldY(e.offsetY);
      const hit = this.graph.nodeAt(wx, wy);
      const hitName = hit?.name ?? null;
      const hitDesc = hit?.description ?? "";
      const hitPattern = hitName ? null : this.graph.patternAt(wx, wy);
      const hitPatternDesc = hitPattern ? this.graph.patterns.get(hitPattern)?.description ?? hitPattern : "";
      const hitEdge = hitName || hitPattern ? null : findHoveredEdge(
        this.graph,
        wx,
        wy,
        this.camera.zoom,
        this.lod,
        this.filters.state,
        this.renderer.cachedEdgePairSet
      );
      const now = performance.now();
      if (this.hover.update(
        hitName,
        hitDesc,
        hitPattern,
        hitPatternDesc,
        hitEdge,
        e.offsetX,
        e.offsetY,
        now
      )) {
        this.needsRender = true;
      }
      this.canvas.style.cursor = hitName || hitPattern || hitEdge ? "pointer" : "";
    }
    onPointerUp() {
      const result = this.drag.onPointerUp();
      if (result.nodePositionChanged) {
        this.graph.rebuildQuadtree();
        this.graph.rebuildPatternHulls();
        this.updateLOD();
        this.drag.scheduleLayoutSave(() => this.saveLayout());
      }
    }
    onWheel(e) {
      e.preventDefault();
      const factor = e.deltaY > 0 ? 0.9 : 1.1;
      this.camera.zoomAt(e.offsetX, e.offsetY, factor);
      this.updateLOD();
      this.needsRender = true;
    }
    // ── Minimap ─────────────────────────────────────────────────────────────
    onMinimapDown(e) {
      this.drag.onMinimapDown(e.pointerId, e.target);
      this.jumpToMinimapPoint(e.offsetX, e.offsetY);
    }
    onMinimapMove(e) {
      if (!this.drag.minimapDragging) return;
      this.jumpToMinimapPoint(e.offsetX, e.offsetY);
    }
    jumpToMinimapPoint(sx, sy) {
      const world = this.drag.minimapToWorld(sx, sy);
      if (!world) return;
      this.camera.cx = world.wx;
      this.camera.cy = world.wy;
      this.updateLOD();
      this.needsRender = true;
    }
    // ── Keyboard ────────────────────────────────────────────────────────────
    installKeyboard() {
      const PAN = 40;
      const zoomCenter = (f) => {
        this.camera.zoomAt(this.camera.screenW / 2, this.camera.screenH / 2, f);
        this.updateLOD();
        this.needsRender = true;
      };
      const pan = (dx, dy) => {
        this.camera.pan(dx, dy);
        this.updateLOD();
        this.needsRender = true;
      };
      new KeyboardDispatch([
        {
          match: Keys.cmdK,
          run: (e) => {
            e.preventDefault();
            if (this.palette.isOpen) {
              this.palette.close();
              this.canvas.focus();
            } else {
              this.palette.open(this.buildPaletteActions());
            }
          }
        },
        { match: () => this.palette.isOpen, run: () => {
        } },
        {
          match: Keys.search,
          run: (e) => {
            e.preventDefault();
            this.openSearch();
          }
        },
        { match: () => this.selection.searchOpen, run: () => {
        } },
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
          }
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
          }
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
              this.announce("Selection cleared");
              this.breadcrumb.update(this.graph.projectName, null);
              this.needsRender = true;
            }
          }
        },
        {
          match: Keys.zoomFit,
          run: (e) => {
            e.preventDefault();
            this.fitView();
          }
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
          }
        },
        {
          match: (e) => Keys.enter(e) && this.selection.selected !== null,
          run: () => {
            this.syncFocusSet(this.selection.selected);
            this.nav.focusNode(this.selection.selected, this.graph);
            this.updateLOD();
            this.needsRender = true;
          }
        },
        {
          match: (e) => {
            if (!Keys.del(e) || !this.selection.selected) return false;
            const tag = document.activeElement?.tagName;
            if (tag === "INPUT" || tag === "TEXTAREA") return false;
            if (document.activeElement?.isContentEditable) return false;
            return true;
          },
          run: (e) => {
            e.preventDefault();
            this.deleteSelected();
          }
        }
      ]).attach();
    }
    // ── Command palette actions ─────────────────────────────────────────────
    buildPaletteActions() {
      const actions = [
        { label: "Zoom to fit", shortcut: "Ctrl+0", run: () => this.fitView() },
        { label: "Search", shortcut: "Ctrl+F", run: () => this.openSearch() },
        {
          label: "Reset layout",
          run: () => {
            if (!confirm("Unpin all nodes and recompute layout? Pinned positions will be lost."))
              return;
            this.api.resetLayout().then((v) => {
              this.graph.layoutVersion = v;
              for (const n of this.graph.nodes.values()) n.pinned = false;
              this.layout.run(this.graph.nodes, this.graph.edges, 200);
              this.graph.rebuildQuadtree();
              this.graph.rebuildPatternHulls();
              this.fitView();
            }).catch((e) => console.error("Reset layout failed:", e));
          }
        }
      ];
      if (this.undo.canUndo()) {
        actions.push({
          label: "Undo",
          shortcut: "Ctrl+Z",
          run: () => {
            this.undo.undo().then((d) => {
              if (d) this.reloadGraph();
            });
          }
        });
      }
      if (this.undo.canRedo()) {
        actions.push({
          label: "Redo",
          shortcut: "Ctrl+Shift+Z",
          run: () => {
            this.undo.redo().then((d) => {
              if (d) this.reloadGraph();
            });
          }
        });
      }
      for (const name of this.selection.componentNames) {
        actions.push({
          label: `Focus: ${name}`,
          run: () => this.selectAndFocus(name)
        });
      }
      return actions;
    }
    // ── Filter state ─────────────────────────────────────────────────────────
    onFilterChange(state) {
      this.filters.update(state);
      if (state.focusMode && this.selection.selected) {
        this.syncFocusSet(this.selection.selected);
      } else if (!state.focusMode) {
        this.syncFocusClear();
      }
      this.needsRender = true;
    }
    // ── Delete selected ─────────────────────────────────────────────────────
    deleteSelected() {
      const name = this.selection.selected;
      if (!name) return;
      const node = this.graph.nodes.get(name);
      const decision = this.graph.decisions.get(name);
      if (node) {
        const desc = node.description ?? "";
        if (!confirm(
          `Delete component "${name}"?

This cannot be undone if cascade rules block re-creation.`
        )) {
          return;
        }
        this.api.deleteComponent(name).then(() => {
          this.undo.push({
            description: `delete component ${name}`,
            undo: () => this.api.createComponent(name, desc),
            redo: () => this.api.deleteComponent(name)
          });
          this.selection.select(null);
          this.breadcrumb.update(this.graph.projectName, null);
          this.reloadGraph();
        }).catch((e) => alert(e.message));
      } else if (decision) {
        if (!confirm(`Delete decision "${name}"?`)) return;
        this.api.deleteDecision(name).then(() => {
          this.undo.push({
            description: `delete decision ${name}`,
            undo: () => Promise.reject(new Error("Decision deletion cannot be undone via the map API")),
            redo: () => this.api.deleteDecision(name)
          });
          this.selection.select(null);
          this.breadcrumb.update(this.graph.projectName, null);
          this.reloadGraph();
        }).catch((e) => alert(e.message));
      }
    }
    // ── Search ──────────────────────────────────────────────────────────────
    setupSearch() {
      const input = document.getElementById("search-input");
      const results = document.getElementById("search-results");
      input.addEventListener("input", () => {
        this.selection.setSearchResults(search(this.graph, input.value));
        this.renderSearchResults(results);
      });
      input.addEventListener("keydown", (e) => {
        if (e.key === "Escape") {
          this.closeSearch();
          return;
        }
        if (e.key === "ArrowDown") {
          e.preventDefault();
          this.selection.nextSearchResult();
          this.renderSearchResults(results);
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          this.selection.prevSearchResult();
          this.renderSearchResults(results);
          return;
        }
        if (e.key === "Enter") {
          e.preventDefault();
          const result = this.selection.activeSearchResult();
          if (result) this.selectSearchResult(result);
          return;
        }
      });
    }
    openSearch() {
      const bar = document.getElementById("search-bar");
      const input = document.getElementById("search-input");
      bar.classList.remove("hidden");
      input.value = "";
      input.focus();
      this.selection.openSearch();
      document.getElementById("search-results").innerHTML = "";
    }
    closeSearch() {
      document.getElementById("search-bar").classList.add("hidden");
      this.selection.closeSearch();
      this.canvas.focus();
    }
    renderSearchResults(el) {
      const results = this.selection.searchResults;
      const activeIndex = this.selection.searchActiveIndex;
      if (results.length === 0) {
        el.innerHTML = "";
        return;
      }
      el.innerHTML = results.map((r, i) => {
        const active = i === activeIndex ? " active" : "";
        const kind = `<span class="search-result-kind">${esc(r.kind)}</span>`;
        return `<div class="search-result${active}" data-idx="${i}">${kind}${esc(r.label)}</div>`;
      }).join("");
      for (const child of el.children) {
        child.addEventListener("click", () => {
          const idx = parseInt(child.dataset.idx ?? "-1", 10);
          if (idx >= 0 && idx < results.length) {
            this.selectSearchResult(results[idx]);
          }
        });
      }
    }
    selectSearchResult(result) {
      this.closeSearch();
      if (result.kind === "component") {
        this.selectAndFocus(result.name);
        this.syncFocusSet(result.name);
      } else if (result.kind === "decision") {
        const dec = this.graph.decisions.get(result.name);
        if (dec) {
          this.selectAndFocus(dec.component);
          this.syncFocusSet(dec.component);
        }
      } else if (result.kind === "pattern") {
        const pat = this.graph.patterns.get(result.name);
        if (pat && pat.components.length > 0) {
          this.selectAndFocus(pat.components[0]);
          this.syncFocusSet(pat.components[0]);
        }
      }
      this.needsRender = true;
    }
    // ── Coordination helpers ────────────────────────────────────────────────
    selectAndFocus(name) {
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
    syncFocusClear() {
      this.selection.clearFocus();
      this.toolbar.setFocusActive(false);
    }
    syncFocusSet(name) {
      this.selection.setFocus(name, this.graph);
      this.toolbar.setFocusActive(true);
    }
    announce(text) {
      this.aria.textContent = text;
    }
    // ── First-visit hint ────────────────────────────────────────────────────
    showFirstVisitHint() {
      const autoLayout = ![...this.graph.nodes.values()].some((n) => n.pinned);
      if (!autoLayout) return;
      try {
        if (sessionStorage.getItem("trurlic-hint-shown")) return;
        sessionStorage.setItem("trurlic-hint-shown", "1");
      } catch {
        return;
      }
      const hint = document.getElementById("hint-overlay");
      if (!hint) return;
      hint.classList.remove("hidden");
      setTimeout(() => {
        hint.classList.add("fade-out");
        setTimeout(() => hint.classList.add("hidden"), 600);
      }, 4e3);
    }
    // ── WebSocket ───────────────────────────────────────────────────────────
    handleWsEvent(event) {
      switch (event.type) {
        case "node_removed": {
          const name = event.name;
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
        case "edge_added": {
          const edge = event.edge;
          if (edge) {
            this.graph.addEdge(edge.from, edge.to, edge.kind);
            this.needsRender = true;
          }
          return;
        }
        case "edge_removed": {
          const from = event.from;
          const to = event.to;
          const kind = event.kind;
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
    handleWsState(state) {
      const el = document.getElementById("ws-status");
      if (state === "reconnecting") {
        el.classList.remove("hidden");
      } else {
        el.classList.add("hidden");
        this.reloadGraph();
      }
    }
    reloadGraph() {
      this.api.fetchGraph().then((snap) => {
        this.graph.loadSnapshot(snap);
        this.selection.setComponentNames([...this.graph.nodes.keys()].sort());
        this.layout.run(this.graph.nodes, this.graph.edges, 50);
        this.graph.rebuildQuadtree();
        this.graph.rebuildPatternHulls();
        this.updateLOD();
        this.needsRender = true;
        this.toolbar.setAvailableTags(this.graph.allTags());
        this.refreshPanel();
      }).catch((e) => console.error("Reload failed:", e));
    }
    refreshPanel() {
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
    saveLayout() {
      const positions = {};
      for (const [name, n] of this.graph.nodes) {
        if (n.pinned) positions[name] = { x: n.x, y: n.y, pinned: true };
      }
      this.api.saveLayout(positions, this.graph.layoutVersion).then((v) => {
        this.graph.layoutVersion = v;
      }).catch((e) => console.error("Layout save failed:", e));
    }
    renderMinimap() {
      const mw = 220;
      const mh = 150;
      this.renderer.renderMinimap(this.miniCtx, mw, mh, this.graph);
      if (this.graph.nodes.size === 0) return null;
      let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
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
    fitView() {
      this.nav.fitAll(this.graph);
      this.syncFocusClear();
      this.updateLOD();
      this.needsRender = true;
    }
    handleResize() {
      const panel = document.getElementById("panel");
      const panelVisible = !panel.classList.contains("collapsed");
      const w = panelVisible ? window.innerWidth - panel.offsetWidth : window.innerWidth;
      const h = window.innerHeight;
      this.renderer.resize(w, h);
      const minimap = document.getElementById("minimap");
      const dpr = window.devicePixelRatio || 1;
      minimap.width = 220 * dpr;
      minimap.height = 150 * dpr;
      this.updateLOD();
      this.needsRender = true;
    }
    // ── Panel resize ──────────────────────────────────────────────────────
    setupPanelResize() {
      const handle = document.getElementById("panel-resize");
      const root = document.documentElement;
      const MIN_W = 220;
      const MAX_W = 560;
      try {
        const saved = sessionStorage.getItem("trurlic-panel-width");
        if (saved) {
          const w = Math.max(MIN_W, Math.min(MAX_W, parseInt(saved, 10)));
          if (!isNaN(w)) root.style.setProperty("--panel-width", `${w}px`);
        }
      } catch {
      }
      let dragging = false;
      handle.addEventListener("pointerdown", (e) => {
        e.preventDefault();
        dragging = true;
        handle.classList.add("active");
        handle.setPointerCapture(e.pointerId);
      });
      handle.addEventListener("pointermove", (e) => {
        if (!dragging) return;
        const w = Math.max(MIN_W, Math.min(MAX_W, window.innerWidth - e.clientX));
        root.style.setProperty("--panel-width", `${w}px`);
        this.handleResize();
      });
      handle.addEventListener("pointerup", () => {
        if (!dragging) return;
        dragging = false;
        handle.classList.remove("active");
        const current = getComputedStyle(root).getPropertyValue("--panel-width").trim();
        try {
          sessionStorage.setItem("trurlic-panel-width", parseInt(current, 10).toString());
        } catch {
        }
      });
    }
  };
  var EDGE_HIT_PX = 8;
  function findHoveredEdge(graph, wx, wy, zoom, lod, filters, pairSet) {
    if (lod < 1 /* Component */) return null;
    const threshold = EDGE_HIT_PX / zoom;
    const threshSq = threshold * threshold;
    let bestDistSq = threshSq;
    let best = null;
    for (const e of graph.edges) {
      if (e.kind === "belongs_to") continue;
      if (lod === 0 /* Overview */ && e.kind !== "connects_to") continue;
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
  new App();
})();
