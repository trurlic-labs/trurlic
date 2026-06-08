import { describe, it, expect } from 'vitest';
import { LOD, computeLOD } from './types';

describe('computeLOD', () => {
  it('returns Overview when many nodes visible', () => {
    expect(computeLOD(41)).toBe(LOD.Overview);
    expect(computeLOD(100)).toBe(LOD.Overview);
    expect(computeLOD(5000)).toBe(LOD.Overview);
  });

  it('returns Component at mid-range', () => {
    expect(computeLOD(11)).toBe(LOD.Component);
    expect(computeLOD(25)).toBe(LOD.Component);
    expect(computeLOD(40)).toBe(LOD.Component);
  });

  it('returns Decision when few nodes visible', () => {
    expect(computeLOD(10)).toBe(LOD.Decision);
    expect(computeLOD(5)).toBe(LOD.Decision);
    expect(computeLOD(1)).toBe(LOD.Decision);
  });

  it('returns Decision for zero (empty viewport)', () => {
    expect(computeLOD(0)).toBe(LOD.Decision);
  });
});
