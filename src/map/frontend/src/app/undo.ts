/** A reversible command for the undo/redo stack. */
export interface UndoCommand {
  readonly description: string;
  undo(): Promise<void>;
  redo(): Promise<void>;
}

/**
 * Bounded undo/redo stack.
 *
 * Maintains two stacks (undo and redo) with a configurable limit.
 * Pushing a new command clears the redo stack. Each operation is
 * async to support API-backed mutations.
 */
export class UndoStack {
  private undos: UndoCommand[] = [];
  private redos: UndoCommand[] = [];
  private readonly limit = 50;

  push(cmd: UndoCommand): void {
    this.undos.push(cmd);
    if (this.undos.length > this.limit) this.undos.shift();
    this.redos.length = 0;
  }

  async undo(): Promise<string | null> {
    const cmd = this.undos.pop();
    if (!cmd) return null;
    try {
      await cmd.undo();
      this.redos.push(cmd);
      return cmd.description;
    } catch (e) {
      console.error('Undo failed:', e);
      return null;
    }
  }

  async redo(): Promise<string | null> {
    const cmd = this.redos.pop();
    if (!cmd) return null;
    try {
      await cmd.redo();
      this.undos.push(cmd);
      return cmd.description;
    } catch (e) {
      console.error('Redo failed:', e);
      return null;
    }
  }

  canUndo(): boolean {
    return this.undos.length > 0;
  }

  canRedo(): boolean {
    return this.redos.length > 0;
  }
}
