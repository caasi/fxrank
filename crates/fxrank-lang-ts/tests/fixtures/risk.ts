function dyn(s: string): unknown { return eval(s); }
function proto(o: object): void { Object.setPrototypeOf(o, null); }
function html(el: HTMLElement, s: string): void { el.innerHTML = s; }
function nonNull(x: string | null): string { return x!; }
function pureAsAny(x: unknown): number { return (x as any).n; }
