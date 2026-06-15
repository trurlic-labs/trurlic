/** Shared HTML-escape using a cached DOM element to avoid per-call allocation. */
const _span = document.createElement('span');
export function esc(s: string): string {
  _span.textContent = s;
  return _span.innerHTML;
}
