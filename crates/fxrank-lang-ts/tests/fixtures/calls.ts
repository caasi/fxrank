async function io(): Promise<void> {
  await fetch('https://x');
  console.log('hi');
  const t = Date.now();
  const r = Math.random();
  const e = process.env.HOME;
  if (!e) throw new Error('x');
}

function ctorsAndMethods(db: { query(s: string): void }): void {
  db.query('select 1');           // net.fs.db (heuristic, unknown-receiver method)
  const d = new Date();           // time.read (no-arg constructor)
  const ws = new WebSocket('wss://x'); // net.fs.db (constructor)
}
