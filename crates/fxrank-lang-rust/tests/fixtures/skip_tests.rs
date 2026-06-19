fn prod(p: &std::path::Path) { let _ = std::fs::read(p); }   // production: net.fs.db

#[test]
fn free_test() { assert!(true); }

#[cfg(test)]
mod tests {
    fn helper() { let _ = std::fs::read("x"); }              // in-module helper (no #[test])
    struct S;
    impl S { fn method(&self) { assert_eq!(1, 1); } }        // method inside cfg(test) mod
}
