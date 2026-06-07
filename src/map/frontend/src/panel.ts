import type { RenderNode, DecisionNode } from './types';
import type { Graph } from './graph';

/**
 * Right-side detail/inspector panel. Standard HTML — fully accessible,
 * serves as the primary content surface while the canvas provides
 * spatial context.
 */
export class Panel {
  private el: HTMLElement;

  constructor(el: HTMLElement) {
    this.el = el;
  }

  showProject(graph: Graph): void {
    const decCount = graph.decisions.size;
    const compCount = graph.nodes.size;
    const patCount = graph.patterns.size;
    this.el.innerHTML = `
      <h2>${esc(graph.projectName)}</h2>
      ${graph.projectDescription ? `<p class="dim">${esc(graph.projectDescription)}</p>` : ''}
      <div class="stats">
        <div class="stat"><span class="stat-n">${compCount}</span> components</div>
        <div class="stat"><span class="stat-n">${decCount}</span> decisions</div>
        <div class="stat"><span class="stat-n">${patCount}</span> patterns</div>
      </div>
      <h3>Recent decisions</h3>
      ${recentDecisions(graph)}
    `;
  }

  showComponent(node: RenderNode, graph: Graph): void {
    const decs = graph.decisionsFor(node.name);
    const connections = graph.edges
      .filter((e) => e.from === node.name || e.to === node.name)
      .map((e) => (e.from === node.name ? e.to : e.from));

    this.el.innerHTML = `
      <div class="panel-kind">component</div>
      <h2>${esc(node.name)}</h2>
      ${node.description ? `<p class="dim">${esc(node.description)}</p>` : ''}
      ${
        connections.length > 0
          ? `
        <h3>Connections</h3>
        <div class="chip-list">${connections.map((c) => `<span class="chip">${esc(c)}</span>`).join('')}</div>
      `
          : ''
      }
      <h3>Decisions <span class="dim">(${decs.length})</span></h3>
      ${decs.length === 0 ? '<p class="dim">None yet</p>' : decs.map(decisionCard).join('')}
    `;
  }

  showEmpty(): void {
    this.el.innerHTML = `<p class="dim" style="padding:24px">Click a component to inspect</p>`;
  }
}

function decisionCard(d: DecisionNode): string {
  return `
    <div class="dec-card">
      <div class="dec-choice">${esc(d.choice)}</div>
      <div class="dec-reason dim">${esc(d.reason)}</div>
      ${d.tags.length > 0 ? `<div class="chip-list">${d.tags.map((t) => `<span class="chip tag">${esc(t)}</span>`).join('')}</div>` : ''}
      ${d.alternatives.length > 0 ? `<details><summary class="dim">Alternatives</summary><ul>${d.alternatives.map((a) => `<li class="dim">${esc(a)}</li>`).join('')}</ul></details>` : ''}
    </div>
  `;
}

function recentDecisions(graph: Graph): string {
  const sorted = [...graph.decisions.values()]
    .sort((a, b) => b.created.localeCompare(a.created))
    .slice(0, 5);
  if (sorted.length === 0) return '<p class="dim">None yet</p>';
  return sorted
    .map(
      (d) => `
    <div class="dec-card">
      <div class="dec-choice">${esc(d.choice)}</div>
      <div class="dim" style="font-size:12px">${esc(d.component)} · ${new Date(d.created).toLocaleDateString()}</div>
    </div>
  `,
    )
    .join('');
}

function esc(s: string): string {
  const el = document.createElement('span');
  el.textContent = s;
  return el.innerHTML;
}
