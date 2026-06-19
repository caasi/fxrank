// Fixture for Task 14 risk_features detection tests.
// Each function (or item) exercises a distinct RiskKind.

// ── unsafe fn → UnsafeFn (class 5) ──────────────────────────────────────────

pub unsafe fn unsafe_fn_example() -> u8 {
    42
}

// ── unsafe { } block → UnsafeBlock (class 5) ────────────────────────────────

pub fn unsafe_block_example() {
    unsafe {
        let _x: u8 = 1 + 1;
    }
}

// ── std::mem::transmute → Transmute (class 7) ───────────────────────────────

pub fn transmute_example() -> i8 {
    unsafe { std::mem::transmute::<u8, i8>(255u8) }
}

// ── raw pointer deref inside unsafe → RawPtrDeref (class 7, heuristic) ──────

pub fn raw_ptr_deref_example() {
    let x: u32 = 42;
    let p: *const u32 = &x;
    unsafe {
        let _v = *p;
    }
}

// ── slice.get_unchecked → GetUnchecked (class 7, heuristic) ─────────────────

pub fn get_unchecked_example(slice: &[u8]) -> u8 {
    unsafe { *slice.get_unchecked(0) }
}

// ── MaybeUninit::uninit() → MaybeUninit (class 7) ───────────────────────────

pub fn maybe_uninit_example() {
    let _x: std::mem::MaybeUninit<u64> = std::mem::MaybeUninit::uninit();
}

// ── asm! macro → Asm (class 7) ──────────────────────────────────────────────

pub fn asm_example() {
    unsafe {
        std::arch::asm!("nop");
    }
}

// ── Box::leak → BoxLeak (class 4) ───────────────────────────────────────────

pub fn box_leak_example() -> &'static str {
    let b = Box::new(String::from("hello"));
    Box::leak(b)
}

// ── std::mem::forget → MemForget (class 4) ──────────────────────────────────

pub fn mem_forget_example() {
    let x = String::from("hello");
    std::mem::forget(x);
}

// ── ManuallyDrop::new → ManuallyDrop (class 4) ──────────────────────────────

pub fn manually_drop_example() {
    let _x = std::mem::ManuallyDrop::new(String::from("hi"));
}

// ── unsafe { std::env::set_var } → BOTH env.write + UnsafeBlock ─────────────

pub fn env_write_unsafe_combo() {
    unsafe {
        std::env::set_var("K", "v");
    }
}

// ── risk feeds ranking: only mem::forget → max_class == 4, risk_weight == 5 ─

pub fn forget_only() {
    let x = String::from("hello");
    std::mem::forget(x);
}

// ── std::ptr::write_volatile → RawPtrOp (class 7, exact) ────────────────────
//
// write_volatile is the actual volatile raw-pointer write; it is classified
// under RawPtrOp (together with ptr::read/write/copy_nonoverlapping) at class 7.

pub fn write_volatile_example() {
    let mut x: u32 = 0;
    unsafe {
        std::ptr::write_volatile(&mut x as *mut u32, 42);
    }
}

// ── Fix 1+2: exact-segment matching for MaybeUninit / ManuallyDrop ───────────
//
// Calls to types whose name merely *contains* the substring "MaybeUninit" or
// "ManuallyDrop" (e.g. `MaybeUninitWrapper`, `ManuallyDropGuard`) must NOT
// produce a false-positive risk.  We simulate that by calling a local wrapper
// struct defined here whose name embeds the substring.

struct MaybeUninitWrapper;
impl MaybeUninitWrapper {
    fn new() -> Self {
        MaybeUninitWrapper
    }
}

struct ManuallyDropGuard;
impl ManuallyDropGuard {
    fn new() -> Self {
        ManuallyDropGuard
    }
}

/// Calls `MaybeUninitWrapper::new()` — must produce NO MaybeUninit risk.
pub fn maybe_uninit_wrapper_no_risk() {
    let _x = MaybeUninitWrapper::new();
}

/// Calls `ManuallyDropGuard::new()` — must produce NO ManuallyDrop risk.
pub fn manually_drop_guard_no_risk() {
    let _x = ManuallyDropGuard::new();
}

// ── module-level risks (ImplDrop, ExternBlock) defined below ─────────────────

struct DropMe;

impl Drop for DropMe {
    fn drop(&mut self) {
        // intentional: tests ImplDrop detection
    }
}

extern "C" {
    fn c_fn() -> i32;
}
