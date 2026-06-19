// Fixture for Task 15: static-path ambient.read detection.

static CONFIG: u32 = 5;
static TIMEOUT_MS: u64 = 1000;

// A callable static — same name (`CONFIG_FN`) as a static, but called as a function.
// Calling it must NOT emit ambient.read; only bare reads should.
static CONFIG_FN: fn() -> u32 = || 5;

/// Reads CONFIG bare — should emit ambient.read (class 2, heuristic).
pub fn read_cfg() -> u32 {
    CONFIG
}

/// Reads CONFIG in an expression — still ambient.read.
pub fn doubled_cfg() -> u32 {
    CONFIG * 2
}

/// Calls a function (not a bare path read) — should NOT double-count as ambient.read.
/// The path `some_fn` is a callee, not a bare read.
pub fn callee_not_read() -> u32 {
    some_helper()
}

fn some_helper() -> u32 {
    0
}

/// Calls a static that is itself a callable — the callee position must NOT emit
/// ambient.read even though `CONFIG_FN` is a known static.
pub fn calls_callable_static() -> u32 {
    CONFIG_FN()
}

/// Uses .load() method on an atomic (existing heuristic) in addition to reading a static.
/// Should emit both: one ambient.read from .load() and one from CONFIG.
pub fn load_and_static(counter: &std::sync::atomic::AtomicU32) -> u32 {
    let a = counter.load(std::sync::atomic::Ordering::Relaxed);
    let b = CONFIG;
    a + b
}

/// A function that does NOT read any static — no ambient.read from path detection.
pub fn no_static_read(x: u32) -> u32 {
    x + 1
}
