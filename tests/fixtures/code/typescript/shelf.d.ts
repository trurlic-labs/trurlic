import type { Shelf } from "./shelf";

declare function openShelf(path: string): Shelf;
declare function closeShelf(shelf: Shelf): void;
export declare const VERSION: string;
export { openShelf };
