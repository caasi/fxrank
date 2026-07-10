//! Collect function units from a parsed `syn::File`.
//!
//! A "function unit" is any item with a concrete body:
//! - Free functions (`fn foo() {}`)
//! - Inherent impl methods (`impl S { fn method(&self) {} }`)
//! - Trait impl methods (`impl T for S { fn required(&self) {} }`)
//! - Trait items WITH a default body (`trait T { fn defaulted(&self) {} }`)
//!
//! Bodyless trait signatures (`fn required(&self);`) are NOT units — skip them.
//!
//! The `FnUnit` struct retains a normalized `sig` (`FnSig`) and `body`
//! (`FnBodyOwned`) so that detector tasks can walk the body without re-parsing.
//! Access pattern: callers that need effect detection import `FnUnit` from
//! `fxrank_lang_rust::functions` and drive `.body.walk_with(&mut visitor)`; the
//! `Frontend` impl maps `FnUnit` to `Hotspot` for scoring.

use syn::spanned::Spanned;
use syn::{ImplItem, Item, ItemImpl, ItemTrait, TraitItem};

/// A normalized, owned function signature.
///
/// Unifies a named fn/method (`syn::Signature`) and a closure (`ExprClosure`,
/// added in Task 2) into one shape. A named fn/method's typed params become
/// `Pat::Type(PatType{ pat, ty })` (so the `&mut`/`&` type stays available for
/// mutation seeding) and its receiver is split out into `receiver`; a closure's
/// params are its `Punctuated<Pat>` as-is with `receiver = None`. This is the
/// syn analog of the swc `FnSig { params: Vec<Pat> }` (see
/// `fxrank-lang-ts/src/functions.rs`).
#[derive(Clone)]
pub struct FnSig {
    /// Method receiver (`self`/`&self`/`&mut self`), else `None`. Free fns and
    /// closures have none — handled SEPARATELY from `inputs`.
    pub receiver: Option<syn::Receiver>,
    /// Parameter *patterns*. A named fn/method's typed param is a
    /// `Pat::Type(PatType{ pat, ty })`; a closure's params are its bare
    /// `Punctuated<Pat>` (possibly untyped `Pat::Ident`).
    pub inputs: Vec<syn::Pat>,
    /// True when the fn/method is `unsafe`. (Closures are never unsafe.)
    pub unsafety: bool,
    /// True when the fn/method/closure is `async`.
    pub asyncness: bool,
    /// True ONLY for a closure unit. The mutation seed uses it to route a
    /// closure's by-value `mut` param into `let_mut`; a named fn's by-value
    /// `mut` param stays silent. Distinct from `FnUnit`-level nesting identity.
    pub is_closure: bool,
    /// `sig.span().start()` mapped to `(line, col+1)` for the `UnsafeFn` risk
    /// anchor — `Some` only for an `unsafe fn`; `None` otherwise (incl. closures).
    pub unsafe_anchor: Option<(usize, usize)>,
}

impl FnSig {
    /// Build a `FnSig` from a named fn/method `syn::Signature`: the receiver is
    /// split out, each `FnArg::Typed(pt)` becomes `Pat::Type(pt.clone())`, and
    /// `unsafe_anchor` reproduces `risk.rs`'s old `sig.span().start()` anchor
    /// byte-for-byte (only for an `unsafe fn`).
    fn from_signature(sig: &syn::Signature) -> Self {
        let mut receiver = None;
        let mut inputs = Vec::new();
        for input in &sig.inputs {
            match input {
                syn::FnArg::Receiver(r) => receiver = Some(r.clone()),
                syn::FnArg::Typed(pt) => inputs.push(syn::Pat::Type(pt.clone())),
            }
        }
        let unsafety = sig.unsafety.is_some();
        let unsafe_anchor = if unsafety {
            let loc = sig.span().start();
            Some((loc.line, loc.column + 1))
        } else {
            None
        };
        FnSig {
            receiver,
            inputs,
            unsafety,
            asyncness: sig.asyncness.is_some(),
            is_closure: false,
            unsafe_anchor,
        }
    }
}

/// The owned body of a function unit.
///
/// A named fn/method carries a `syn::Block`; a closure (Task 2) may carry a bare
/// expression. Detectors walk whichever variant is present via [`Self::walk_with`]
/// (the syn analog of the swc `FnBodyOwned::walk_with`).
pub enum FnBodyOwned {
    /// A `{ … }` block (fn declarations, impl/trait methods, block-bodied closures).
    Block(syn::Block),
    /// A bare expression body, e.g. the `x + 1` in `|x| x + 1`.
    Expr(Box<syn::Expr>),
}

impl FnBodyOwned {
    /// Drive a syn `Visit` over this body (the block, or the single expr).
    pub fn walk_with<'ast, V: syn::visit::Visit<'ast>>(&'ast self, v: &mut V) {
        match self {
            FnBodyOwned::Block(b) => v.visit_block(b),
            FnBodyOwned::Expr(e) => v.visit_expr(e),
        }
    }
}

/// A concrete function unit — a named item with a body that can be analysed for
/// effects. `sig` and `body` are normalized owned clones so detectors in
/// `detect/` can walk them without re-parsing.
pub struct FnUnit {
    /// Display symbol: `free_fn`, `S::method`, `<S as T>::method`, `T::defaulted`.
    pub symbol: String,
    /// Collision-resistant id: `path:line:col:symbol` (col is the 1-based char column).
    pub id: String,
    /// Source file path (as passed in).
    pub path: String,
    /// 1-based line number of the function's name (`sig.ident`).
    pub line: usize,
    /// 1-based character column of the function's name (`sig.ident`).
    pub col: usize,
    /// The normalized function signature (for detectors to inspect asyncness,
    /// unsafety, receiver/params for mutation seeding, etc.).
    pub sig: FnSig,
    /// The normalized function body (for detectors to walk expressions).
    pub body: FnBodyOwned,
    /// Whether this function is test code: `#[test]`/`#[bench]`, or carrying a bare
    /// `#[cfg(test)]` (on the fn, its `impl`/`trait` block, or an enclosing module).
    /// Computed at collection time; test units are excluded from scoring by default.
    pub is_test: bool,
    /// Inline-`mod` nesting within the file (`["a","b"]` for `mod a { mod b { fn } }`).
    /// Empty for a top-level item. Combined with the file's module path to form
    /// the canonical path. (025-3e)
    pub mod_path: Vec<String>,
}

/// Returns `true` when `attrs` contains a `#[test]`/`#[bench]` attribute, in
/// either the bare form or a multi-segment runner form (`#[tokio::test]`,
/// `#[actix_rt::test]`, `#[async_std::test]`, …). Matched on the LAST path
/// segment so qualified test-runner attrs are recognised, not just `#[test]`.
fn has_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| s.ident == "test" || s.ident == "bench")
    })
}

/// Returns `true` when `attrs` contains the literal `#[cfg(test)]`.
///
/// Matches only the exact single-ident form (intentional — compound cfg
/// expressions such as `#[cfg(all(test, feature = "foo"))]` are intentionally
/// not matched). Also used by module-risk detection to suppress test modules.
pub(crate) fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && a.parse_args::<syn::Path>()
                .map(|p| p.is_ident("test"))
                .unwrap_or(false)
    })
}

/// Collect all function units from a parsed file at `path`.
///
/// Module items (`Item::Mod`) are walked recursively when they have an inline
/// body; out-of-line modules (just `mod foo;`) cannot be resolved here and are
/// skipped — the caller is expected to feed each file separately.
pub fn collect(file: &syn::File, path: &str) -> Vec<FnUnit> {
    let mut units = Vec::new();
    collect_items(&file.items, path, false, &[], &mut units);
    units
}

fn collect_items(
    items: &[Item],
    path: &str,
    in_cfg_test: bool,
    mod_path: &[String],
    out: &mut Vec<FnUnit>,
) {
    for item in items {
        match item {
            Item::Fn(f) => {
                let symbol = f.sig.ident.to_string();
                let start = f.sig.ident.span().start();
                let line = start.line;
                let col = start.column + 1; // proc-macro2 column is 0-based
                let is_test = in_cfg_test || has_test_attr(&f.attrs) || is_cfg_test(&f.attrs);
                out.push(FnUnit {
                    id: format!("{path}:{line}:{col}:{symbol}"),
                    symbol,
                    path: path.to_string(),
                    line,
                    col,
                    sig: FnSig::from_signature(&f.sig),
                    body: FnBodyOwned::Block(*f.block.clone()),
                    is_test,
                    mod_path: mod_path.to_vec(),
                });
            }

            Item::Impl(impl_block) => {
                // A `#[cfg(test)] impl …` block makes all its methods test-only.
                let in_cfg_test = in_cfg_test || is_cfg_test(&impl_block.attrs);
                collect_from_impl(impl_block, path, in_cfg_test, mod_path, out);
            }

            Item::Trait(trait_item) => {
                // A `#[cfg(test)] trait …` makes its default-bodied methods test-only.
                let in_cfg_test = in_cfg_test || is_cfg_test(&trait_item.attrs);
                collect_from_trait(trait_item, path, in_cfg_test, mod_path, out);
            }

            Item::Mod(m) => {
                if let Some((_, nested_items)) = &m.content {
                    let nested_in_cfg_test = in_cfg_test || is_cfg_test(&m.attrs);
                    let mut child = mod_path.to_vec();
                    child.push(m.ident.to_string());
                    collect_items(nested_items, path, nested_in_cfg_test, &child, out);
                }
                // `mod foo;` without a body is out-of-line — skip.
            }

            _ => {}
        }
    }
}

fn collect_from_impl(
    impl_block: &ItemImpl,
    path: &str,
    in_cfg_test: bool,
    mod_path: &[String],
    out: &mut Vec<FnUnit>,
) {
    // Render the self type as the last path-segment ident (e.g. `S` for `impl S`).
    let type_name = last_path_ident(&impl_block.self_ty);

    // Is this a trait impl?  `impl T for S` vs bare `impl S`.
    let trait_name: Option<String> = impl_block
        .trait_
        .as_ref()
        .map(|(_, path, _)| last_segment_ident(path));

    for item in &impl_block.items {
        if let ImplItem::Fn(method) = item {
            let method_name = method.sig.ident.to_string();
            let start = method.sig.ident.span().start();
            let line = start.line;
            let col = start.column + 1; // proc-macro2 column is 0-based

            let symbol = match &trait_name {
                Some(tr) => format!("<{type_name} as {tr}>::{method_name}"),
                None => format!("{type_name}::{method_name}"),
            };

            let is_test = in_cfg_test || has_test_attr(&method.attrs) || is_cfg_test(&method.attrs);
            out.push(FnUnit {
                id: format!("{path}:{line}:{col}:{symbol}"),
                symbol,
                path: path.to_string(),
                line,
                col,
                sig: FnSig::from_signature(&method.sig),
                body: FnBodyOwned::Block(method.block.clone()),
                is_test,
                mod_path: mod_path.to_vec(),
            });
        }
    }
}

fn collect_from_trait(
    trait_item: &ItemTrait,
    path: &str,
    in_cfg_test: bool,
    mod_path: &[String],
    out: &mut Vec<FnUnit>,
) {
    let trait_name = trait_item.ident.to_string();

    for item in &trait_item.items {
        if let TraitItem::Fn(method) = item {
            // Only emit a unit when there is a default body.
            if let Some(block) = &method.default {
                let method_name = method.sig.ident.to_string();
                let start = method.sig.ident.span().start();
                let line = start.line;
                let col = start.column + 1; // proc-macro2 column is 0-based
                let symbol = format!("{trait_name}::{method_name}");

                let is_test =
                    in_cfg_test || has_test_attr(&method.attrs) || is_cfg_test(&method.attrs);
                out.push(FnUnit {
                    id: format!("{path}:{line}:{col}:{symbol}"),
                    symbol,
                    path: path.to_string(),
                    line,
                    col,
                    sig: FnSig::from_signature(&method.sig),
                    body: FnBodyOwned::Block(block.clone()),
                    is_test,
                    mod_path: mod_path.to_vec(),
                });
            }
            // Bodyless `fn required(&self);` — skip.
        }
    }
}

/// Extract the identifier of the last path segment from a `Type`.
///
/// Works for the common cases (`Type::Path`, `Type::Reference` wrapping a path).
/// Falls back to `"_"` for exotic types (raw pointers, tuples, etc.) that are
/// unlikely to appear as impl self-types in normal code.
fn last_path_ident(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(tp) => last_segment_ident(&tp.path),
        syn::Type::Reference(r) => last_path_ident(&r.elem),
        _ => "_".to_string(),
    }
}

fn last_segment_ident(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|seg| seg.ident.to_string())
        .unwrap_or_else(|| "_".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnunit_exposes_col() {
        let file = syn::parse_file("fn foo() {}").unwrap();
        let units = collect(&file, "a.rs");
        assert_eq!(units[0].col, 4); // 1-based column of `foo`
        assert!(units[0].id.ends_with(":4:foo")); // col already in id
    }

    #[test]
    fn inline_module_nesting_recorded_in_mod_path() {
        let src = r#"
            fn top() {}
            mod a {
                fn mid() {}
                mod b {
                    fn deep() {}
                }
            }
        "#;
        let file = syn::parse_file(src).unwrap();
        let units = collect(&file, "x.rs");
        let by = |name: &str| units.iter().find(|u| u.symbol == name).unwrap();
        assert_eq!(by("top").mod_path, Vec::<String>::new());
        assert_eq!(by("mid").mod_path, vec!["a".to_string()]);
        assert_eq!(by("deep").mod_path, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn qualified_test_runner_attrs_mark_unit_as_test() {
        // #53: multi-segment test-runner attrs (#[tokio::test], #[actix_rt::test])
        // count as test code, not just the bare #[test]/#[bench].
        let src = "#[test] fn a() {}\n\
                   #[bench] fn b() {}\n\
                   #[tokio::test] async fn c() {}\n\
                   #[actix_rt::test] async fn d() {}\n\
                   fn prod() {}\n";
        let file = syn::parse_file(src).unwrap();
        let units = collect(&file, "x.rs");
        let is_test = |name: &str| units.iter().find(|u| u.symbol == name).unwrap().is_test;
        assert!(is_test("a"), "#[test] must be test");
        assert!(is_test("b"), "#[bench] must be test");
        assert!(is_test("c"), "#[tokio::test] must be test");
        assert!(is_test("d"), "#[actix_rt::test] must be test");
        assert!(!is_test("prod"), "a plain fn must not be test");
    }

    #[test]
    fn fnsig_and_body_normalize_named_fn() {
        let file = syn::parse_file("unsafe fn foo(x: &mut i32) { *x += 1; }").unwrap();
        let units = collect(&file, "a.rs");
        let u = &units[0];
        assert_eq!(u.sig.inputs.len(), 1); // one typed param `x` (receiver is separate)
        assert!(u.sig.receiver.is_none());
        assert!(!u.sig.is_closure); // a named fn is not a closure
        assert!(u.sig.unsafety);
        assert!(!u.sig.asyncness);
        assert!(matches!(u.body, FnBodyOwned::Block(_)));
        // walk_with drives a syn visitor over the block.
        struct Counter(usize);
        impl<'ast> syn::visit::Visit<'ast> for Counter {
            fn visit_expr_binary(&mut self, n: &'ast syn::ExprBinary) {
                self.0 += 1;
                syn::visit::visit_expr_binary(self, n);
            }
        }
        let mut c = Counter(0);
        u.body.walk_with(&mut c);
        assert_eq!(
            c.0, 1,
            "walk_with must reach the compound-assign in the body"
        );
    }
}
