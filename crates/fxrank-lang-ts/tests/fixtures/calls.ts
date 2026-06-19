async function io(): Promise<void> {
  await fetch('https://x');
  console.log('hi');
  const t = Date.now();
  const r = Math.random();
  const e = process.env.HOME;
  if (!e) throw new Error('x');
}
