import { describe, it, expect } from "vitest";
import {
  PORT_BLOCK_MIN,
  PORT_RANGE_DEFAULT_END,
  PORT_RANGE_DEFAULT_START,
  portRangeCapacity,
  portRangeError,
} from "./portRange";

describe("portRangeError", () => {
  it("accepts the default range and the issue's 3000-4000", () => {
    expect(portRangeError(PORT_RANGE_DEFAULT_START, PORT_RANGE_DEFAULT_END)).toBeNull();
    expect(portRangeError(3000, 4000)).toBeNull();
  });

  it("accepts a range holding exactly one block, rejects one port narrower", () => {
    expect(portRangeError(3000, 3000 + PORT_BLOCK_MIN - 1)).toBeNull();
    expect(portRangeError(3000, 3000 + PORT_BLOCK_MIN - 2)).toMatch(/consecutive ports/);
  });

  it("rejects privileged and out-of-range ports", () => {
    expect(portRangeError(80, 9000)).toMatch(/between 1024 and 65535/);
    expect(portRangeError(3000, 70000)).toMatch(/between 1024 and 65535/);
  });

  it("rejects a non-integer, which is what a half-typed field produces", () => {
    // The number input yields NaN for "" and for "3e", and the field must
    // not offer to save either.
    expect(portRangeError(NaN, 4000)).toMatch(/whole numbers/);
    expect(portRangeError(3000, 4000.5)).toMatch(/whole numbers/);
  });

  it("rejects an inverted range", () => {
    expect(portRangeError(4000, 3000)).toMatch(/must not be below/);
  });
});

describe("portRangeCapacity", () => {
  it("counts whole plain blocks and rounds down", () => {
    expect(portRangeCapacity(3000, 3011)).toBe(2);
    expect(portRangeCapacity(3000, 3016)).toBe(2); // 17 ports, 5 left over
    expect(portRangeCapacity(3000, 4000)).toBe(166);
  });

  it("is 0 for a range that would not save", () => {
    expect(portRangeCapacity(4000, 3000)).toBe(0);
    expect(portRangeCapacity(NaN, 4000)).toBe(0);
  });
});
