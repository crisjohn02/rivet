// gold: (l) overload signatures followed by their implementation.
export function toLabel(value: number): string;
export function toLabel(value: string): string;
export function toLabel(value: number | string): string {
  return String(value);
}

export function format(value: number): string {
  return toLabel(value).trim();
}

// gold: (l) a const bound to an arrow function is one function symbol.
export const double = (n: number): number => n * 2;

// gold: (l) a const bound to a function expression is one function symbol.
export const launchAll = function (count: number): number {
  return double(count);
};

// gold: (m) plain named consts, one statement declaring two names.
export const RETRY_LIMIT = 3;
export const MIN_COUNT = 1, MAX_COUNT = 10;

// gold: (n) a default export that names a local declaration.
export default function helper(): string {
  return format(RETRY_LIMIT);
}
