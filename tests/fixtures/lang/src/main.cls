interface Point { x: number; y: number }
type Shape = { kind: "circle"; r: number } | { kind: "rect"; w: number; h: number };

function area(s: Shape): number {
  switch (s.kind) {
    case "circle": return 3 * s.r * s.r;
    case "rect": return s.w * s.h;
  }
}

function identity<T>(x: T): T { return x; }
function mapAll<T, U>(xs: T[], f: (x: T) => U): U[] { return xs.map(f); }

function counter(): () => number {
  let n = 0;
  return () => { n += 1; return n; };
}

async function fetchNum(x: number): Promise<number> {
  if (x < 0) { throw Error("negative"); }
  return x * 2;
}

function fib(n: number): number { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

test "closures" {
  const c = counter();
  c(); c();
  assertEq(c(), 3);
  const fs: (() => number)[] = [];
  for (let i = 0; i < 3; i++) { fs.push(() => i); }
  assertEq(fs.map((f) => f()), [0, 1, 2]);
}

test "generics and arrays" {
  assertEq(identity<string>("a"), "a");
  assertEq(mapAll([1, 2, 3], (x) => x * x), [1, 4, 9]);
  assertEq([3, 1, 2].sort((a, b) => a - b), [1, 2, 3]);
  assertEq([1, 2, 3, 4].filter((x) => x % 2 === 0).reduce((a, x) => a + x, 0), 6);
  const [a, b, ...rest] = [1, 2, 3, 4];
  assertEq(rest, [3, 4]);
  const { x, ...others } = { x: 1, y: 2, z: 3 };
  assertEq(others, { y: 2, z: 3 });
  assertEq([...[1, 2], ...[3]], [1, 2, 3]);
}

test "strings" {
  assertEq("héllo".length, 5);
  assertEq("a,b,c".split(","), ["a", "b", "c"]);
  assertEq(`x${1 + 1}y`, "x2y");
  assertEq("abc".toUpperCase(), "ABC");
  assertEq((3.14159).toFixed(2), "3.14");
  assertEq("abc".padStart(5, "-"), "--abc");
}

test "unions and narrowing" {
  assertEq(area({ kind: "rect", w: 2, h: 3 }), 6);
  assertEq(area({ kind: "circle", r: 2 }), 12);
  const p: Point | null = { x: 1, y: 2 };
  assertEq(p?.x, 1);
  const q: Point | null = null;
  assertEq(q?.x ?? -1, -1);
}

test "errors" {
  let msg = "";
  try { throw Error("boom"); } catch (e) { msg = e.message; } finally { msg = msg + "!"; }
  assertEq(msg, "boom!");
  const err = assertThrows(() => { const arr: number[] = []; return arr[0]!; });
  assertEq(err.name, "TypeError");
}

test "async" {
  const v = await fetchNum(21);
  assertEq(v, 42);
  const e = await assertRejects(fetchNum(-1));
  assertEq(e.message, "negative");
  const all = await Promise.all([fetchNum(1), fetchNum(2)]);
  assertEq(all, [2, 4]);
}

test "recursion and loops" {
  assertEq(fib(15), 610);
  let s = 0;
  for (const x of [1, 2, 3]) { if (x === 2) { continue; } s += x; }
  assertEq(s, 4);
  let i = 0;
  while (true) { i++; if (i > 5) { break; } }
  assertEq(i, 6);
  const o: Record<string, number> = { a: 1, b: 2 };
  assertEq(Object.keys(o), ["a", "b"]);
  assertEq(JSON.stringify({ a: [1, 2] }), "{\"a\":[1,2]}");
  assertEq(JSON.parse("{\"k\":1}"), { k: 1 });
}
