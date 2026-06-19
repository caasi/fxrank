// Fixture exercising the mutation detector + containment discount (Task 13).
// One function per scenario; tests assert kind/class/discounted_to/tier/hidden.

// 13a: &mut param mutated via a mutating method → param.mutation, discount MutParam (down 2).
fn fill(b: &mut Vec<u8>) {
    b.push(1);
}

// 13a: &mut self field assignment → param.mutation, discount MutSelf (down 1).
struct User {
    name: String,
    dirty: bool,
}
impl User {
    fn set_name(&mut self, n: String) {
        self.name = n;
    }
}

// 13a (channel-scoped): the param.mutation discounts to 1, while the net.fs.db
// effect from the std::fs::write call stays class 7 (discount is per-effect).
fn save(u: &mut User) -> std::io::Result<()> {
    std::fs::write("x", b"")?;
    u.dirty = true;
    Ok(())
}

// 13b: &self with interior-mutability write through RefCell::borrow_mut →
// hidden.mutation (class 3, hidden, no discount). Scores HIGHER than &mut self.
impl User {
    fn set(&self, n: String) {
        *self.name.borrow_mut() = n;
    }
}

// 13b: shared &Context param with Cell::set → hidden.mutation (not just &self).
struct Context;
fn bump(c: &Context) {
    c.counter.set(1);
}

// 13c: `let mut x` write sites → local.mutation (class 1, exact); the
// declaration alone produces NONE — two writes here (`+=` and `=`).
fn locals() {
    let mut x = 0;
    x += 1;
    x = 2;
}

// 13d: write to a SCREAMING_SNAKE ident bound nowhere → global.mutation (class
// 6, heuristic). The UPPERCASE convention is our proxy for a `static mut`.
static mut COUNT: u32 = 0;
fn inc() {
    unsafe {
        COUNT += 1;
    }
}

// 13e (cancels): a &mut write INSIDE an unsafe block — the containment discount
// is cancelled (an unsafe reborrow may alias), so it stays class 3.
fn w_unsafe(p: &mut u8) {
    unsafe {
        *p = 1;
    }
}

// 13e (does NOT cancel): the mutating write is OUTSIDE the unsafe block, so the
// discount applies and it goes to class 1.
fn w_outside(p: &mut Vec<u8>) {
    unsafe {
        let _ = 1;
    }
    p.push(1);
}

// Fix 2 – destructured let: `let (mut x, y) = …; x = 1` must be ONE
// local.mutation and ZERO global.mutation (x is a known local, not a
// SCREAMING_SNAKE global candidate).
fn destructured_let_no_global() {
    let (mut x, _y) = (0i32, 0i32);
    x = 1;
    let _ = x;
}

// Fix 3 – destructured &mut tuple param mutated → must NOT produce
// global.mutation as a false positive.  Ideally also produces param.mutation;
// at minimum the global false-positive must be absent.
fn destructured_mut_param((mut a, b): &mut (i32, i32)) {
    *a = 1;
    let _ = b;
}
