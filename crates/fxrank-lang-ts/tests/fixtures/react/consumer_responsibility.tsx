// Spec 027 §5 — Consumer-responsibility principle:
// A component that RECEIVES a callback from its caller (via props) and only
// passes it onward is NOT charged for that callback's effects — the origin
// component owns the cost.
//
// `CallbackParent`: defines the real world effect (fetch inside a named handler)
//   and passes the handler as a prop to `CallbackChild`. Parent owns the state
//   and the handler's effects.
//
// `CallbackChild({ onAction })`: only passes the received callback onward via
//   a JSX prop. It never defines or calls `onAction` itself, so it is NOT
//   charged. max_class must be 0.
//
// Expected signals:
//   - CallbackParent: max_class > 0 (state.transition class 1 + adopted handler
//     net.fs.db discounted to class 6 — event-phase).
//   - CallbackChild: max_class == 0 (pure presentational — passes callback
//     onward without executing it or acquiring new world effects).

import React, { useState } from "react";

// Child: receives callback, passes it onward — pure.
function CallbackChild({ onAction }: { onAction: () => void }) {
  return <button onClick={onAction}>Act</button>;
}

// Parent: holds state + defines the handler that does real work.
function CallbackParent() {
  const [result, setResult] = useState<string | null>(null);

  function handleAction() {
    fetch("/api/action");
    setResult("done");
  }

  return (
    <div>
      <CallbackChild onAction={handleAction} />
      {result}
    </div>
  );
}

export { CallbackChild, CallbackParent };
