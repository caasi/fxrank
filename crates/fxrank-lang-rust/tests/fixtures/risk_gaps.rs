// Fixture for gap-filling tests: raw-ptr-deref in unsafe fn body (gap 1)
// and unsafe impl (gap 2).

// ── Gap 1: raw-ptr-deref in unsafe fn body, no inner unsafe {} block ─────────
//
// `deref_in_unsafe_fn` must produce BOTH UnsafeFn (class 5) and RawPtrDeref
// (class 7).  Before the fix only UnsafeFn was emitted because `inside_unsafe()`
// only checked `unsafe_depth > 0`, which stays 0 when there is no nested block.

pub unsafe fn deref_in_unsafe_fn(p: *const u8) -> u8 {
    *p
}

// ── Gap 2a: plain unsafe impl Send → UnsafeImpl (class 5) ────────────────────

struct MySend;

unsafe impl Send for MySend {}

// ── Gap 2b: unsafe impl Drop → BOTH ImplDrop (class 2) AND UnsafeImpl (class 5)
//
// An impl can be simultaneously unsafe AND a Drop impl.  Both risks must be
// emitted independently — neither should be suppressed by an else-branch.

struct UnsafeDropMe;

unsafe impl Drop for UnsafeDropMe {
    fn drop(&mut self) {
        // intentional: tests that unsafe impl Drop emits both ImplDrop and UnsafeImpl
    }
}
