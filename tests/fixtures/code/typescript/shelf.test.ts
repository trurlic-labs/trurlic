import { Shelf } from "./shelf";

describe("Shelf", () => {
  const made = () => new Shelf("x");

  it("opens a path", () => {
    expect(made()).toBeDefined();
  });

  describe.each([1, 2])("with %s books", (count) => {
    test(`holds ${count}`, () => {});
  });

  it.only("reads a::b and calls run()", () => {});
  it.skip('handles f(x)', () => {});
});

test.todo("weighs");
