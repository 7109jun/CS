import { clamp, DEFAULT_RANGE, Range } from "./mathx";

@tool("Clamp a number into the default range")
function clampDefault(x: number): number { return clamp(x, DEFAULT_RANGE); }

test "header" {
  assertEq(clampDefault(15), 10);
  assertEq(clamp(-3, { lo: -1, hi: 1 }), -1);
}
