import { esc } from '../util';

/**
 * Breadcrumb trail: Project → Component.
 *
 * Rendered into #breadcrumb. Each segment is clickable — the host
 * provides `onProject` and `onComponent` callbacks.
 */
export class Breadcrumb {
  private el: HTMLElement;
  private onProject: () => void;
  private onComponent: (name: string) => void;

  constructor(callbacks: { onProject: () => void; onComponent: (name: string) => void }) {
    this.el = document.getElementById('breadcrumb')!;
    this.onProject = callbacks.onProject;
    this.onComponent = callbacks.onComponent;
  }

  /** Update the trail. Pass `null` to clear (project-level view). */
  update(projectName: string, selected: string | null): void {
    if (!selected) {
      this.el.innerHTML = '';
      return;
    }

    const label = projectName || 'Project';
    this.el.innerHTML =
      `<span class="breadcrumb-seg" data-bc="project">${esc(label)}</span>` +
      '<span class="breadcrumb-sep">\u2192</span>' +
      `<span class="breadcrumb-seg" data-bc="${esc(selected)}">${esc(selected)}</span>`;

    for (const seg of this.el.querySelectorAll('.breadcrumb-seg')) {
      seg.addEventListener('click', () => {
        const target = (seg as HTMLElement).dataset.bc;
        if (target === 'project') this.onProject();
        else if (target) this.onComponent(target);
      });
    }
  }
}
