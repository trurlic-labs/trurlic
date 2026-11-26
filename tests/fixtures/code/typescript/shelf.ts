// A shelf of books: one per construct the TypeScript query reads.
import { readFile } from "node:fs";

export const CAPACITY = 40, half = (n: number) => n / 2;
let opened = 0;
var legacy = function () {
  return opened;
};
export const { label, sizes: [first, ...rest] } = { label: "main", sizes: [1, 2] };

export function open(path: string): Shelf;
export function open(path: string, mode: number): Shelf;
export function open(path: string, mode?: number): Shelf {
  function check(shelf: Shelf) {
    const local = shelf;
    return local !== undefined;
  }
  const settle = () => check;
  return new Shelf(path);
}

export async function* scan() {}

declare function ambient(): void;
declare const flag: boolean;

export abstract class Base {
  abstract weigh(): number;
}

export class Shelf extends Base {
  @observable // why it is watched
  @logged
  read(): string {
    return this.path;
  }
  private secret = 1;
  #hidden = 2;
  protected get size(): number {
    return 1;
  }
  set size(value: number) {}
  static make(): Shelf;
  static make(path?: string): Shelf {
    return new Shelf(path ?? "");
  }
  "odd name"() {}
  weigh(): number {
    return 1;
  }
  constructor(public path: string) {
    super();
  }
}

export const Crate = class {
  carry() {}
};

export interface Book {
  title: string;
  pages(): number;
  "isbn-13"?: string;
}

interface Book {
  author: string;
}

type Point = { x: number, y: number; norm(): number };
export type Width = number | string;

export const enum Color {
  Red,
  Green = 2,
  "Blue-ish" = 3,
}

declare module "shelf.config" {
  export function load(): void;
}

namespace Index.Pages {
  export function find() {}
  function hidden() {}
  export const SIZE = 1;
  export const tools = { make() {} };
}

export const api = {
  get() {},
  post: async () => {},
  limit: 3,
};

function listed() {}
class Later {}
export { listed, Later as Renamed };
export { readFile } from "node:fs";
