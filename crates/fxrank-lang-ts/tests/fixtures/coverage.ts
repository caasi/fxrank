function fullyTyped(xs: number[]): number[] { const a: number[] = []; a.push(1); return a; }
function partlyTyped(xs: number[]) { const a: number[] = []; a.push(1); return a; }
function untyped(xs) { const a = []; a.push(1); return a; }
function poisoned(xs: number[]): number[] { const a = xs as any; a.push(1); return a; }
