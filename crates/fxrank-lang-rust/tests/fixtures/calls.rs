// Fixture exercising the call-effect signal matrix (Task 11).
// One function per family; the test asserts kind/class/tier/evidence.

use std::fs;
use std::process::Command;

// Path tier: net.fs.db via resolved `fs::write` -> `std::fs::write` (class 7).
fn fs_write() {
    let _ = fs::write("out.txt", b"data");
}

// Path tier: time.read via `Instant::now` (class 5).
fn time_read() {
    let _ = std::time::Instant::now();
    let _ = SystemTime::now();
}

// Path tier: env.read (class 4) and env.write (class 6).
fn env_calls() {
    let _ = std::env::var("X");
    std::env::set_var("Y", "1");
}

// Path tier: process.control via exit (class 6). NO effect from `exit` arg.
fn process_exit() {
    std::process::exit(1);
}

// process.control from `.spawn` ONLY — `Command::new` is a constructor, no effect.
fn command_spawn() {
    let _ = Command::new("ls").spawn();
}

// Heuristic tier: concurrency via `.send`.
fn channel_send(tx: std::sync::mpsc::Sender<u8>) {
    let _ = tx.send(1);
}

// Heuristic tier: panic via `.unwrap` (class 4).
fn unwraps(x: Option<u8>) {
    let _ = x.unwrap();
}

// Path tier: concurrency via thread::spawn; random via rand path.
fn spawn_and_random() {
    std::thread::spawn(|| {});
    let _ = rand::random::<u8>();
}

// Heuristic: net.fs.db via `.write_all`; ambient.read via `.load`.
fn io_and_atomic(mut w: impl std::io::Write, a: &std::sync::atomic::AtomicUsize) {
    let _ = w.write_all(b"hi");
    let _ = a.load(std::sync::atomic::Ordering::SeqCst);
}

// A pure function — should yield no effects.
fn pure(a: u8, b: u8) -> u8 {
    a + b
}

// Bring SystemTime into scope so the short form resolves in `time_read`.
use std::time::SystemTime;
