// Controlled counter: useState + setter called from an onClick handler.
// Expected signals:
//   - StateTransition (class 1) from `useState` declaration
//   - The onClick arrow is an event handler → NOT EffectInRender
//   - The inline arrow is suppressed as a standalone hotspot (inherited callback
//     for the button's onClick is NOT a hook callback, so it appears separately)
// Note: onClick is a plain JSX prop arrow, NOT a hook callback, so it is NOT
// absorbed into the component via inherited_callbacks. It appears as its own
// hotspot. The component itself carries the StateTransition.

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
