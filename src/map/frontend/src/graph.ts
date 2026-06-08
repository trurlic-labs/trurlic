import type {
  GraphSnapshot,
  DecisionNode,
  PatternNode,
  RenderNode,
  RenderEdge,
  WsEvent,
} from './types';
import { Quadtree } from './quadtree';

// ── Graph model ────────────────────────────────────────────────────────────

export class Graph {
  nodes: Map<string, RenderNode> = new Map();
  /** All edges — connects_to rendered at LOD 0, others at LOD 1+. */
  edges: RenderEdge[] = [];
  decisions: Map<string, DecisionNode> = new Map();
  patterns: Map<string, PatternNode> = new Map();
  projectName = '';
  projectDescription = '';
  layoutVersion = 0;
  quadtree = new Quadtree();

  loadSnapshot(snap: GraphSnapshot): void {
    this.nodes.clear();
    this.edges = [];
    this.decisions.clear();
    this.patterns.clear();
    this.projectName = snap.project.name;
    this.projectDescription = snap.project.description;
    this.layoutVersion = snap.layout_version;

    for (const c of snap.components) {
      this.nodes.set(c.name, {
        name: c.name,
        kind: 'component',
        x: c.position?.x ?? 0,
        y: c.position?.y ?? 0,
        w: 180,
        h: 60,
        pinned: c.pinned,
        description: c.description,
        decisionCount: c.decision_count,
        patternCount: c.pattern_count,
      });
    }

    for (const d of snap.decisions) {
      this.decisions.set(d.name, d);
    }

    for (const p of snap.patterns) {
      this.patterns.set(p.name, p);
    }

    // Store all edge types — renderer uses LOD to decide which to draw.
    for (const e of snap.edges) {
      this.edges.push({ from: e.from, to: e.to, kind: e.kind });
    }

    this.assignMissingPositions();
    this.rebuildQuadtree();
  }

  /** Rebuild the spatial index. Call after layout changes or drag. */
  rebuildQuadtree(): void {
    this.quadtree.build(this.nodes);
  }

  private assignMissingPositions(): void {
    let i = 0;
    const count = this.nodes.size;
    for (const node of this.nodes.values()) {
      if (node.x === 0 && node.y === 0 && !node.pinned) {
        const angle = (2 * Math.PI * i) / Math.max(count, 1);
        const radius = 200 + count * 20;
        node.x = Math.cos(angle) * radius;
        node.y = Math.sin(angle) * radius;
      }
      i++;
    }
  }

  /** Hit test using quadtree — O(log n) instead of linear scan. */
  nodeAt(wx: number, wy: number): RenderNode | null {
    const name = this.quadtree.hitTest(wx, wy);
    return name ? (this.nodes.get(name) ?? null) : null;
  }

  decisionsFor(component: string): DecisionNode[] {
    return [...this.decisions.values()].filter((d) => d.component === component);
  }
}

// ── REST client ────────────────────────────────────────────────────────────

export class ApiClient {
  private baseUrl: string;
  private token: string;

  constructor(token: string) {
    this.baseUrl = `${location.protocol}//${location.host}`;
    this.token = token;
  }

  async fetchGraph(): Promise<GraphSnapshot> {
    const res = await fetch(`${this.baseUrl}/api/graph`, {
      headers: { Authorization: `Bearer ${this.token}` },
    });
    if (!res.ok) throw new Error(`GET /api/graph: ${res.status}`);
    return res.json();
  }

  async saveLayout(
    positions: Record<string, { x: number; y: number; pinned: boolean }>,
    version: number,
  ): Promise<number> {
    const res = await fetch(`${this.baseUrl}/api/layout`, {
      method: 'PUT',
      headers: {
        Authorization: `Bearer ${this.token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ positions, layout_version: version }),
    });
    if (!res.ok) throw new Error(`PUT /api/layout: ${res.status}`);
    const data = await res.json();
    return data.layout_version;
  }

  // ── Mutations ───────────────────────────────────────────────────────

  async updateDecision(
    name: string,
    body: { choice?: string; reason?: string; tags?: string[] },
  ): Promise<void> {
    const res = await fetch(`${this.baseUrl}/api/decision/${enc(name)}`, {
      method: 'PUT',
      headers: this.jsonHeaders(),
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      const data = await res.json().catch(() => ({}));
      throw new Error(data.error ?? `PUT decision: ${res.status}`);
    }
  }

  async deleteDecision(name: string): Promise<void> {
    const res = await fetch(`${this.baseUrl}/api/decision/${enc(name)}`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${this.token}` },
    });
    if (!res.ok) {
      const data = await res.json().catch(() => ({}));
      throw new Error(data.error ?? `DELETE decision: ${res.status}`);
    }
  }

  async deleteComponent(name: string): Promise<void> {
    const res = await fetch(`${this.baseUrl}/api/component/${enc(name)}`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${this.token}` },
    });
    if (!res.ok) {
      const data = await res.json().catch(() => ({}));
      throw new Error(data.error ?? `DELETE component: ${res.status}`);
    }
  }

  async createConnection(from: string, to: string): Promise<void> {
    const res = await fetch(`${this.baseUrl}/api/connection`, {
      method: 'POST',
      headers: this.jsonHeaders(),
      body: JSON.stringify({ from, to }),
    });
    if (!res.ok) {
      const data = await res.json().catch(() => ({}));
      throw new Error(data.error ?? `POST connection: ${res.status}`);
    }
  }

  async deleteConnection(from: string, to: string): Promise<void> {
    const res = await fetch(`${this.baseUrl}/api/connection/${enc(from)}/${enc(to)}`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${this.token}` },
    });
    if (!res.ok) {
      const data = await res.json().catch(() => ({}));
      throw new Error(data.error ?? `DELETE connection: ${res.status}`);
    }
  }

  private jsonHeaders(): Record<string, string> {
    return {
      Authorization: `Bearer ${this.token}`,
      'Content-Type': 'application/json',
    };
  }
}

function enc(s: string): string {
  return encodeURIComponent(s);
}

// ── WebSocket ──────────────────────────────────────────────────────────────

export class WsConnection {
  private ws: WebSocket | null = null;
  private token: string;
  private onEvent: (event: WsEvent) => void;
  private reconnectDelay = 100;
  private maxReconnectDelay = 5000;

  constructor(token: string, onEvent: (event: WsEvent) => void) {
    this.token = token;
    this.onEvent = onEvent;
    this.connect();
  }

  private connect(): void {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${proto}//${location.host}/ws?token=${this.token}`;
    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      this.reconnectDelay = 100;
    };

    this.ws.onmessage = (e) => {
      try {
        const event: WsEvent = JSON.parse(e.data);
        this.onEvent(event);
      } catch {
        /* ignore malformed messages */
      }
    };

    this.ws.onclose = () => {
      setTimeout(() => this.connect(), this.reconnectDelay);
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, this.maxReconnectDelay);
    };

    this.ws.onerror = () => {
      this.ws?.close();
    };
  }
}
