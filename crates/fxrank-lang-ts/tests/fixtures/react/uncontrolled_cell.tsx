// Component with useRef whose .current is written directly in the component body.
// Expected signals:
//   - hidden.mutation (class 3) with subreason: ref-cell-write for `inputRef.current = …`
//   - No EffectInRender (a ref write is not a world effect)
//   - The component body itself contains the ref write (not a nested function)
//     so the ref binding is visible in the component's own mutation walker.

import React, { useRef } from "react";

function UncontrolledCell() {
  const inputRef = useRef<HTMLInputElement>(null);

  // Direct ref write in the component body — the "hidden mutation differentiator".
  // This is unusual in real React but demonstrates the ref-cell-write detection.
  inputRef.current = document.getElementById("cell") as HTMLInputElement;

  return (
    <div>
      <input ref={inputRef} type="text" />
    </div>
  );
}

export default UncontrolledCell;
