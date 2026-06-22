// Component with useEffect(fetch) and useMemo(fetch).
// Expected signals:
//   - useEffect arrow: HookPhase::Effect → inherited into FetchData at class 7,
//     NO EffectInRender (effect-phase baseline is acceptable)
//   - useMemo arrow: HookPhase::Render → inherited into FetchData at class 7,
//     PLUS EffectInRender risk (world effect in render phase)
//   - Both arrows suppressed as standalone hotspots (no <arrow@…> entries)
//   - FetchData component carries both inherited fetch effects

import React, { useEffect, useMemo } from "react";

function FetchData() {
  useEffect(() => {
    fetch("/api/data");
  }, []);

  const result = useMemo(() => fetch("/api/computed"), []);

  return <div>{String(result)}</div>;
}

export default FetchData;
