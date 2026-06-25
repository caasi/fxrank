// Controlled counter: useState + setter called from a named onClick handler.
// Expected signals (spec 027 §4.2 adoption):
//   - Counter carries the state.transition (class 1) from `useState`
//   - handleClick is a named inner function passed to the JSX onClick prop;
//     under spec 027 adoption it is re-parented into Counter and suppressed as
//     a standalone hotspot (it does NOT appear separately in the output).
//   - Calling setCount is not a world effect (no external IO or hidden mutation),
//     so Counter's max_class stays at 1 (state.transition only).

import React, { useState } from "react";

function Counter() {
  const [count, setCount] = useState(0);

  function handleClick() {
    setCount(count + 1);
  }

  return (
    <div>
      <p>Count: {count}</p>
      <button onClick={handleClick}>Increment</button>
    </div>
  );
}

export default Counter;
