export interface Stats { count: number; sum: number; mean: number; min: number; max: number }

export function summarize(values: number[]): Stats {
  if (values.length === 0) { throw Error("values must not be empty"); }
  let sum = 0;
  let min = values[0]!;
  let max = values[0]!;
  for (const v of values) {
    sum += v;
    if (v < min) { min = v; }
    if (v > max) { max = v; }
  }
  return { count: values.length, sum, mean: sum / values.length, min, max };
}
