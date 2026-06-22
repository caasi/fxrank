// Controlled counter: useState + setter called from a named onClick handler.
// Expected signals:
//   - StateTransition (class 1) from `useState` declaration on Counter
//   - handleClick is a named function (its own FnUnit), scored as a pure hotspot
//     (calling a setter is not a world effect — no external IO or hidden mutation)
//   - Counter carries the state.transition; handleClick scores 0.0 with no effects
// Note: handleClick is a named inner function, NOT an inline arrow. It is its own
// FnUnit and appears as a separate hotspot. The component itself carries the
// StateTransition from the useState declaration.

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
