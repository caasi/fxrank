function buildLocal(): number[] {
  const acc: number[] = [];
  acc.push(1);
  return acc;
}
function mutParam(xs: number[]): void { xs.push(1); }
// A module-level binding mutated from inside a function: cross-component shared
// state -> global.mutation (class 6), per issue #29.
let counter = 0;
function viaModuleVar(): void { counter += 1; }
// A captured ENCLOSING-FUNCTION local (not module-level): the nested `inner`
// writes `acc`, a local of `viaEnclosing` -> hidden.mutation (class 3).
function viaEnclosing(): () => void {
  let acc = 0;
  function inner(): void { acc += 1; }
  return inner;
}
class Box { v = 0; set(n: number): void { this.v = n; } }
function viaGlobal(): void { (globalThis as any).z = 1; }
class WithCtor { x = 0; constructor() { this.x = 1; } }
function delLocal(): void { const o: Record<string, number> = { a: 1 }; delete o.a; }
function delParam(o: Record<string, number>): void { delete o.x; }
