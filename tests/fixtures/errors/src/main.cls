interface Todo { id: number; title: string }
function f(x: string | null): number {
  return x.length;
}
function g(x: string | null): number {
  if (x === null) { return 0; }
  return x.length;
}
const a: number = "s";
let b = 1;
b = "x";
const c = 5; c = 6;
function h(n: number): string { if (n > 1) { return "a"; } }
const t: Todo = { id: 1, title: "x", extra: 2 };
console.log(undefinedName);
const r = [1,2,3].map(x => x.foo);

function k(a) { return a; }
const j = 1 + "a" + {};
async function z(): Promise<number> { return 1; }
function nz() { const v = await z(); }
type S = { kind: "a"; x: number } | { kind: "b"; y: string };
function sw(s: S): number { switch (s.kind) { case "a": return s.x; case "b": return s.y; } }
function sw2(s: S): number { switch (s.kind) { case "a": return s.x; case "b": return s.y.length; } }
@tool("x") function tt(p: (x: number) => number): number { return 1; }
