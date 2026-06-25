// Spec 027 §5 — Phase principle (§2.4 conditionality discount):
// The same world effect scored differently by WHEN it runs:
//
//   - Render-phase (useMemo): runs unconditionally during render → full-weight
//     net.fs.db (class 7, no conditionality discount) + EffectInRender risk.
//   - Effect-phase (useEffect): runs after mount/update, not during render →
//     full-weight net.fs.db (class 7), no EffectInRender, no discount.
//   - Event-phase (JSX onClick): conditional on user interaction → net.fs.db
//     discounted one class (7 → 6), subreason "phase:event", no EffectInRender.
//
// Expected signals (one component per phase, each calling fetch('/api')):
//   - RenderPhase:  max_class 7, net.fs.db discounted_to null, EffectInRender.
//   - EffectPhase:  max_class 7, net.fs.db discounted_to null, no EffectInRender.
//   - EventPhase:   max_class 6, net.fs.db discounted_to 6, subreason "phase:event",
//                   no EffectInRender.

import React, { useEffect, useMemo } from "react";

// Render-phase: useMemo runs during render.
function RenderPhase() {
  const _v = useMemo(() => fetch("/api"), []);
  return <div />;
}

// Effect-phase: useEffect runs after render, not during.
function EffectPhase() {
  useEffect(() => {
    fetch("/api");
  }, []);
  return <div />;
}

// Event-phase: onClick only runs on interaction.
function EventPhase() {
  return <button onClick={() => fetch("/api")}>Go</button>;
}

export { RenderPhase, EffectPhase, EventPhase };
