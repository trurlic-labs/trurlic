import type { FilterState } from '../types';

/**
 * Toolbar → Renderer filter bridge.
 *
 * Holds the active {@link FilterState} as set by the Toolbar. The
 * App reads {@link state} each render frame and passes it to the
 * Renderer. Keeping this in a dedicated module means filter logic
 * never leaks into the render loop or selection code.
 */
export class Filters {
  private _state: FilterState | undefined;

  get state(): FilterState | undefined {
    return this._state;
  }

  update(state: FilterState): void {
    this._state = state;
  }
}
