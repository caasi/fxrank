// Component with useRef whose .current is written directly in the component body.
// Expected signals:
//   - hidden.mutation (class 3) with subreason: ref-cell-write for `inputRef.current = …`
//   - No EffectInRender (a ref write is not a world effect)
//   - The component body itself contains the ref write (not a nested function)
//     so the ref binding is visible in the component's own mutation walker.
// Note: the RHS is an inert literal so the fixture isolates the ref-cell-write
// signal and won't interfere when raw-DOM detection is added in a later milestone.

import React, { useRef } from "react";

function UncontrolledCell() {
  const inputRef = useRef<number | null>(null);

  // Direct ref write in the component body — the "hidden mutation differentiator".
  // This is unusual in real React but demonstrates the ref-cell-write detection.
  inputRef.current = 42;

  return (
    <div>
      <input ref={inputRef} type="text" />
    </div>
  );
}

export default UncontrolledCell;
