import { describe, it, expect } from 'vitest';

// Minimal DOM shim — esc() calls document.createElement('span') at module scope.
if (typeof document === 'undefined') {
  const escMap: Record<string, string> = { '&': '&amp;', '<': '&lt;', '>': '&gt;' };
  (globalThis as Record<string, unknown>).document = {
    createElement: () => {
      let text = '';
      return {
        set textContent(v: string) {
          text = v;
        },
        get innerHTML() {
          return text.replace(/[&<>]/g, (ch: string) => escMap[ch] ?? ch);
        },
      };
    },
  };
}

const { renderCodeRefs, renderAttribution, renderRevisionCount } = await import('./panel');

describe('renderCodeRefs', () => {
  it('returns empty string for 0 refs (section absent)', () => {
    expect(renderCodeRefs([])).toBe('');
  });

  it('renders a ref with file only', () => {
    const html = renderCodeRefs([{ file: 'src/store/write.rs' }]);
    expect(html).toContain('Code references');
    expect(html).toContain('src/store/write.rs');
    expect(html).not.toContain('::');
  });

  it('renders a ref with file and symbol', () => {
    const html = renderCodeRefs([{ file: 'src/store/write.rs', symbol: 'commit_with_graph' }]);
    expect(html).toContain('Code references');
    expect(html).toContain('src/store/write.rs');
    expect(html).toContain('::');
    expect(html).toContain('commit_with_graph');
  });

  it('renders multiple refs', () => {
    const html = renderCodeRefs([{ file: 'src/a.rs', symbol: 'foo' }, { file: 'src/b.rs' }]);
    expect(html).toContain('src/a.rs');
    expect(html).toContain('foo');
    expect(html).toContain('src/b.rs');
  });

  it('escapes XSS in file name', () => {
    const html = renderCodeRefs([{ file: '<script>alert(1)</script>' }]);
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;');
  });

  it('escapes XSS in symbol', () => {
    const html = renderCodeRefs([{ file: 'a.rs', symbol: '"><img src=x onerror=alert(1)>' }]);
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;img');
  });
});

describe('renderAttribution', () => {
  it('renders an agent badge for agent attribution', () => {
    const html = renderAttribution('agent');
    expect(html).toContain('agent');
    expect(html).toContain('unreviewed');
    expect(html).toContain('attr-badge');
  });

  it('returns empty string for user attribution (neutral)', () => {
    expect(renderAttribution('user')).toBe('');
  });

  it('returns empty string when attribution is undefined', () => {
    expect(renderAttribution(undefined)).toBe('');
  });
});

describe('renderRevisionCount', () => {
  it('returns empty string for 0 revisions', () => {
    expect(renderRevisionCount(0)).toBe('');
  });

  it('renders count for 1 revision', () => {
    const html = renderRevisionCount(1);
    expect(html).toContain('Revised');
    expect(html).toContain('1');
    expect(html).toContain('×');
  });

  it('renders count for multiple revisions', () => {
    const html = renderRevisionCount(5);
    expect(html).toContain('Revised');
    expect(html).toContain('5');
    expect(html).toContain('×');
  });

  it('returns empty string for undefined', () => {
    expect(renderRevisionCount(undefined as unknown as number)).toBe('');
  });
});
