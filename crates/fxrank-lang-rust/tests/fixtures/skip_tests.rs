fn prod(p: &std::path::Path) { let _ = std::fs::read(p); }   // production: net.fs.db

#[test]
fn free_test() { assert!(true); }

#[cfg(test)]
fn bare_cfg_test_fn() { assert!(true); }                         // bare #[cfg(test)] fn, not in a mod

#[cfg(test)]
mod tests {
    fn helper() { let _ = std::fs::read("x"); }              // in-module helper (no #[test])
    struct S;
    impl S { fn method(&self) { assert_eq!(1, 1); } }        // method inside cfg(test) mod
}

struct P;
impl P {
    fn prod_method(&self) { let _ = std::fs::read("y"); }    // production method — scored
    #[cfg(test)]
    fn cfg_test_method(&self) { assert!(true); }             // bare #[cfg(test)] method → test
}

#[cfg(test)]
impl P { fn whole_impl_test(&self) { assert!(true); } }      // #[cfg(test)] impl block → test

trait Tr {
    fn prod_default(&self) { let _ = std::fs::read("z"); }   // production trait default — scored
    #[cfg(test)]
    fn cfg_test_default(&self) { assert!(true); }            // bare #[cfg(test)] trait method → test
}

#[cfg(test)]
trait TrTest { fn whole_trait_test(&self) { assert!(true); } } // #[cfg(test)] trait block → test
