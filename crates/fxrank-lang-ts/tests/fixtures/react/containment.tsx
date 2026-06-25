// Spec 027 §5 — Containment principle:
// Contained effects (declared and bounded) score LOW; escaping (world) effects
// score HIGH. The containment classifier preserves this distinction.
//
// `StateOnly`: holds traced state via `useState` — declared-and-bounded effect,
//   class 1. Also writes a `useRef` cell (hidden mutation class 3). Both effects
//   are present but their combined own_score stays low.
//
// `FetchingComponent`: calls `fetch` (net.fs.db class 7) — a true world effect.
//   Its own_score must be substantially higher than `StateOnly`.
//
// Expected signals:
//   - StateOnly: max_class ≤ 3 (state.transition class 1 + ref-cell class 3);
//     no net.fs.db effect; own_score low.
//   - FetchingComponent: max_class 7; net.fs.db present; own_score >> StateOnly.

import React, { useState, useRef } from "react";

// Contained: only traced state + private ref storage.
function StateOnly() {
  const [count, setCount] = useState(0);
  const cache = useRef<number>(0);

  // Private ref write — hidden mutation class 3, but still bounded within the component.
  cache.current = count;

  return <button onClick={() => setCount(count + 1)}>{count}</button>;
}

// Escaping: calls fetch — a world effect class 7.
function FetchingComponent() {
  const handleFetch = () => fetch("/api/data");
  return <button onClick={handleFetch}>Load</button>;
}

export { StateOnly, FetchingComponent };
