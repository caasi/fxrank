// Spec 027 §5 — Attribution (乙) principle:
// Definition-site attribution: a component owns the effects of function
// values defined in its lexical scope and wired to its output.
//
// Sub-case 1 (named handler → JSX prop): `AttributionClick` has a named inner
//   function `handleSave` wired directly to onClick. Under pre-027 scoring this
//   was an orphaned hotspot (own_score 0 on the component). After spec 027
//   adoption `handleSave` is suppressed and `AttributionClick` owns its effects.
//
// Sub-case 2 (depth-2 nested in hook callback): `AttributionEffect` has a
//   useEffect whose arrow defines an inner const-arrow `inner` that calls fetch.
//   The hook adoption folds both levels in — `AttributionEffect` owns the fetch.
//
// Expected signals (per-component):
//   - AttributionClick: own_score > 0, max_class >= 6 (event-phase net.fs.db);
//     no standalone `handleSave` hotspot in the output.
//   - AttributionEffect: own_score > 0, max_class 7 (effect-phase net.fs.db);
//     no standalone <arrow@…> hotspots.

import React, { useEffect } from "react";

// Sub-case 1: named handler passed to JSX prop (onClick) — must be adopted.
function AttributionClick() {
  function handleSave() {
    fetch("/api/save");
  }

  return <button onClick={handleSave}>Save</button>;
}

// Sub-case 2: depth-2 nested callback inside useEffect — must be adopted.
function AttributionEffect() {
  useEffect(() => {
    const inner = () => fetch("/api/load");
    inner();
  }, []);

  return null;
}

export { AttributionClick, AttributionEffect };
