export const DEFAULT_RANGE: Range = { lo: 0, hi: 10 };
export function clamp(x: number, r: Range): number {
  return Math.min(Math.max(x, r.lo), r.hi);
}
