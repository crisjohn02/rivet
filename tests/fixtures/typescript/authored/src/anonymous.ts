import { double } from "./util";

// gold: (q) an anonymous default-exported class: neither it nor its method is
// a symbol, and uses inside have no named container.
export default class {
  run(): number {
    return double(1);
  }
}

// gold: (q) a callback inside a named function attaches to that function.
export function mapAll(values: number[]): number[] {
  return values.map((value) => double(value));
}

// gold: (q) a function-expression IIFE: its uses have no named container.
(function () {
  double(2);
})();

// gold: (q) an arrow IIFE and a top-level callback: no named container.
(() => double(3))();
[4, 5].forEach((value) => {
  double(value);
});
