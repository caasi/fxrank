// Fixture for Task 12 macro effect detection tests.
// Each function exercises a distinct macro-detection rule.

use std::fmt::Write as FmtWrite;

macro_rules! my_macro {
    () => {};
}

// ── Logging macros (exact tier) ──────────────────────────────────────────────

pub fn logging_exact() {
    println!("hello");
    eprintln!("err");
    print!("no newline");
    eprint!("err no newline");
    dbg!(42);
}

// ── Logging macros via qualified paths (path tier) ───────────────────────────

pub fn logging_qualified() {
    log::info!("info message");
    tracing::warn!("warn message");
}

// ── Panic macros (exact tier) ────────────────────────────────────────────────

pub fn panic_macros() {
    panic!("oh no");
    unreachable!("unreachable branch");
    todo!("not yet");
    unimplemented!("not done");
    assert!(true);
    assert_eq!(1, 1);
    assert_ne!(1, 2);
    debug_assert!(true);
    debug_assert_eq!(1, 1);
    debug_assert_ne!(1, 2);
}

// ── Whitelisted macros — no effect emitted ───────────────────────────────────

pub fn whitelisted() -> Vec<i32> {
    let v = vec![1, 2, 3];
    let _s = format!("{}", 1);
    let _m = matches!(v[0], 1);
    let _c = concat!("a", "b");
    let _st = stringify!(foo);
    v
}

// ── write!/writeln! macros (net.fs.db, heuristic tier) ───────────────────────

pub fn write_macros() {
    let mut s = String::new();
    write!(s, "hello").unwrap();
    writeln!(s, "world").unwrap();
}

// ── Unknown macro (class 2, confidence 0.4) ──────────────────────────────────

pub fn unknown_macro_only() {
    my_macro!();
}
