async function loadUser(id: string): Promise<unknown> {
  const res = await fetch(`/api/users/${id}`);
  return res.json();
}
function buildTyped(xs: number[]): number[] {
  const acc: number[] = [];
  for (const x of xs) acc.push(x * 2);
  return acc;
}
function buildUntyped(xs) {
  const acc = [];
  acc.push(1);
  return acc;
}
function risky(x: unknown): number {
  return (x as any).count;
}
