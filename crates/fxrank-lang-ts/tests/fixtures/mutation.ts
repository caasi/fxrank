function buildLocal(): number[] {
  const acc: number[] = [];
  acc.push(1);
  return acc;
}
function mutParam(xs: number[]): void { xs.push(1); }
let counter = 0;
function viaClosure(): void { counter += 1; }
class Box { v = 0; set(n: number): void { this.v = n; } }
function viaGlobal(): void { (globalThis as any).z = 1; }
class WithCtor { x = 0; constructor() { this.x = 1; } }
