import { describe, it, expect } from 'vitest';

// Minimal DOM shim — esc() calls document.createElement('span') at module scope.
// Install before the dynamic import so the module-level initializer succeeds.
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

const { esc } = await import('./util');

describe('esc', () => {
  it('escapes angle brackets', () => {
    expect(esc('<script>alert(1)</script>')).toBe('&lt;script&gt;alert(1)&lt;/script&gt;');
  });

  it('escapes ampersands', () => {
    expect(esc('a & b')).toBe('a &amp; b');
  });

  it('passes through plain text unchanged', () => {
    expect(esc('hello world')).toBe('hello world');
  });

  it('handles empty string', () => {
    expect(esc('')).toBe('');
  });
});
