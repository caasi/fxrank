// Fixture for Task 18 snapshot tests — worked spec cases.
// These functions each represent a flagship scenario from the spec.
// Bodies use `syn`-parseable calls; they do not need to type-check.

// Minimal stubs so the fixture parses without imports.
struct User;
struct Db;

// ── Case 1: save_user — fs.write + time.read + param.mutation (discounted) ──

fn save_user(user: &mut User, db: &Db) {
    std::fs::write("path", b"data").unwrap();
    let _t = std::time::Instant::now();
    user.name = "updated".to_string();
}

// ── Case 2: logging_soup — several logging calls ─────────────────────────────

fn logging_soup() {
    log::info!("starting");
    log::info!("processing");
    println!("done");
    println!("flushed");
}

// ── Case 3: one_io — a single fs.write (max_class 7) ────────────────────────

fn one_io() {
    std::fs::write("out.txt", b"hello").unwrap();
}

// ── Case 4: inversion pair — Store::set_name vs Store::set ──────────────────

struct Store;

impl Store {
    // Declared &mut self — param.mutation, discounted (MutSelf -1 → class 2)
    fn set_name(&mut self, name: String) {
        self.name = name;
    }

    // Hidden interior mutation via borrow_mut through &self — hidden.mutation, no discount
    fn set(&self, name: String) {
        *self.name.borrow_mut() = name;
    }
}

// ── Case 5: pure_total — no effects ─────────────────────────────────────────

fn pure_total(items: &[u32]) -> u32 {
    items.iter().sum()
}

// ── Case 6: risk_only — only mem::forget (risk class 4) ─────────────────────

fn risk_only(x: String) {
    std::mem::forget(x);
}

// ── Case 7: fallible — uses ? but that is NOT an effect ──────────────────────
// `.parse::<i32>()` is a method call matched by no effect detector, so `?`
// merely propagates a value — it must NOT be scored as a panic/effect.

fn fallible() -> Result<i32, std::num::ParseIntError> {
    let n = "5".parse::<i32>()?;
    Ok(n)
}

// ── Case 8: async shell with .await ─────────────────────────────────────────

async fn shell(fut: impl std::future::Future<Output = ()>) {
    fut.await;
}

// ── Case 9: unsafe_cancel — write through *p inside unsafe{} ────────────────

fn unsafe_cancel(p: &mut u8) {
    unsafe {
        *p = 1;
    }
}
