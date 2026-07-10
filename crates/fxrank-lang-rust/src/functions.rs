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
use syn::visit::{self, Visit};

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
    /// Whether this unit is nested and therefore has no importable name: any
    /// closure (unconditionally — even a depth-0 `const`/`static` closure, F8),
    /// or any named `fn`/method emitted while inside another unit's body. Drives
    /// canonical-path emptiness (a nested unit gets `vec![]` so it can never
    /// false-resolve via `CanonicalIndex.primary`). DISTINCT from
    /// `FnSig.is_closure` (which only routes closure mutation seeding). (#83)
    pub is_nested: bool,
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
/// Uses a [`Collector`] (`syn::visit::Visit`) that descends into fn/method
/// bodies and every `ExprClosure`, emitting one [`FnUnit`] each — so a nested
/// `fn`, a nested-`impl`/`trait` method, and a closure each become their own
/// unit (the syn analog of the swc `Collector` in `fxrank-lang-ts`). Inline
/// modules (`mod m { … }`) are threaded into `mod_path`; out-of-line modules
/// (just `mod foo;`) carry no body and are skipped.
pub fn collect(file: &syn::File, path: &str) -> Vec<FnUnit> {
    let mut collector = Collector {
        path,
        units: Vec::new(),
        mod_path: Vec::new(),
        in_test: false,
        in_unit_body: false,
        type_ctx: None,
        pending_name: None,
    };
    collector.visit_file(file);
    collector.units
}

/// The enclosing `impl`/`trait` type context, threaded like TS's `class_name`.
///
/// `self_ty` is the last path-segment ident of the impl self-type (or the trait
/// name for a `trait` block); `trait_name` is `Some` only for a trait *impl*
/// (`impl T for S`), which produces the `<S as T>::m` symbol.
struct TypeCtx {
    self_ty: String,
    trait_name: Option<String>,
}

/// A `syn::visit::Visit` that collects every function unit in a file.
///
/// Save/restore of `type_ctx`/`in_test`/`mod_path` around each recursion lets a
/// nested `impl` (inside a fn body), a `#[cfg(test)]` module, and a nested test
/// unit compose correctly. `pending_name` carries a `let`-binding ident to the
/// next closure so `let f = |…|` adopts `f`.
struct Collector<'a> {
    path: &'a str,
    units: Vec<FnUnit>,
    /// Inline-`mod` nesting for the item currently being walked.
    mod_path: Vec<String>,
    /// True while inside a test unit's body or a `#[cfg(test)]` module — nested
    /// units inherit `is_test` from it.
    in_test: bool,
    /// True while walking *inside another unit's body* (a fn/method/trait-fn or
    /// closure body). A named unit emitted with this set is nested → `is_nested`.
    /// NOT set by `impl`/`trait`/`mod` blocks (those are not unit bodies), so a
    /// top-level-impl method and a top-level-in-module fn stay non-nested. (#83)
    in_unit_body: bool,
    /// The enclosing `impl`/`trait` type context (for method symbols).
    type_ctx: Option<TypeCtx>,
    /// Binding name for a closure that is the direct `init` of the `let` being
    /// walked; consumed by the next `visit_expr_closure` via `.take()`.
    pending_name: Option<String>,
}

impl Collector<'_> {
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        symbol: String,
        line: usize,
        col: usize,
        sig: FnSig,
        body: FnBodyOwned,
        is_test: bool,
        is_nested: bool,
    ) {
        let path = self.path;
        self.units.push(FnUnit {
            id: format!("{path}:{line}:{col}:{symbol}"),
            symbol,
            path: path.to_string(),
            line,
            col,
            sig,
            body,
            is_test,
            mod_path: self.mod_path.clone(),
            is_nested,
        });
    }
}

/// The 1-based `(line, col)` of a name span (`col` is 1-based; proc-macro2 is 0-based).
fn name_line_col(span: proc_macro2::Span) -> (usize, usize) {
    let start = span.start();
    (start.line, start.column + 1)
}

/// The binding ident of a `let <ident> = …` pattern, else `None` (only the
/// dominant `let f = |…|` form adopts a name; destructuring binds nothing).
fn let_binding_name(pat: &syn::Pat) -> Option<String> {
    match pat {
        syn::Pat::Ident(pi) => Some(pi.ident.to_string()),
        _ => None,
    }
}

impl<'ast> Visit<'ast> for Collector<'ast> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let symbol = node.sig.ident.to_string();
        let (line, col) = name_line_col(node.sig.ident.span());
        let is_test = self.in_test || has_test_attr(&node.attrs) || is_cfg_test(&node.attrs);
        self.emit(
            symbol,
            line,
            col,
            FnSig::from_signature(&node.sig),
            FnBodyOwned::Block((*node.block).clone()),
            is_test,
            self.in_unit_body,
        );
        // Recurse into the body so nested fns / methods / closures are collected,
        // inheriting this unit's test-ness and marking them nested.
        let saved_test = self.in_test;
        let saved_in_unit = self.in_unit_body;
        self.in_test = is_test;
        self.in_unit_body = true;
        visit::visit_item_fn(self, node);
        self.in_test = saved_test;
        self.in_unit_body = saved_in_unit;
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        // Self type as the last path-segment ident (e.g. `S` for `impl S`).
        let self_ty = last_path_ident(&node.self_ty);
        // Trait impl?  `impl T for S` vs bare `impl S`.
        let trait_name = node
            .trait_
            .as_ref()
            .map(|(_, path, _)| last_segment_ident(path));

        let saved_ctx = self.type_ctx.replace(TypeCtx {
            self_ty,
            trait_name,
        });
        // A `#[cfg(test)] impl …` makes all its methods test-only.
        let saved_test = self.in_test;
        self.in_test = self.in_test || is_cfg_test(&node.attrs);
        visit::visit_item_impl(self, node);
        self.in_test = saved_test;
        self.type_ctx = saved_ctx;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let method_name = node.sig.ident.to_string();
        let symbol = match &self.type_ctx {
            Some(ctx) => match &ctx.trait_name {
                Some(tr) => format!("<{} as {tr}>::{method_name}", ctx.self_ty),
                None => format!("{}::{method_name}", ctx.self_ty),
            },
            None => method_name,
        };
        let (line, col) = name_line_col(node.sig.ident.span());
        let is_test = self.in_test || has_test_attr(&node.attrs) || is_cfg_test(&node.attrs);
        self.emit(
            symbol,
            line,
            col,
            FnSig::from_signature(&node.sig),
            FnBodyOwned::Block(node.block.clone()),
            is_test,
            self.in_unit_body,
        );
        let saved_test = self.in_test;
        let saved_in_unit = self.in_unit_body;
        self.in_test = is_test;
        self.in_unit_body = true;
        visit::visit_impl_item_fn(self, node);
        self.in_test = saved_test;
        self.in_unit_body = saved_in_unit;
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        let saved_ctx = self.type_ctx.replace(TypeCtx {
            self_ty: node.ident.to_string(),
            trait_name: None,
        });
        // A `#[cfg(test)] trait …` makes its default-bodied methods test-only.
        let saved_test = self.in_test;
        self.in_test = self.in_test || is_cfg_test(&node.attrs);
        visit::visit_item_trait(self, node);
        self.in_test = saved_test;
        self.type_ctx = saved_ctx;
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        // Only trait methods WITH a default body are units; bodyless sigs skip.
        let Some(block) = &node.default else {
            return;
        };
        let method_name = node.sig.ident.to_string();
        let symbol = match &self.type_ctx {
            Some(ctx) => format!("{}::{method_name}", ctx.self_ty),
            None => method_name,
        };
        let (line, col) = name_line_col(node.sig.ident.span());
        let is_test = self.in_test || has_test_attr(&node.attrs) || is_cfg_test(&node.attrs);
        self.emit(
            symbol,
            line,
            col,
            FnSig::from_signature(&node.sig),
            FnBodyOwned::Block(block.clone()),
            is_test,
            self.in_unit_body,
        );
        let saved_test = self.in_test;
        let saved_in_unit = self.in_unit_body;
        self.in_test = is_test;
        self.in_unit_body = true;
        visit::visit_trait_item_fn(self, node);
        self.in_test = saved_test;
        self.in_unit_body = saved_in_unit;
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        // `mod foo;` without a body is out-of-line — nothing to collect.
        if node.content.is_none() {
            return;
        }
        let saved_test = self.in_test;
        self.in_test = self.in_test || is_cfg_test(&node.attrs);
        self.mod_path.push(node.ident.to_string());
        visit::visit_item_mod(self, node);
        self.mod_path.pop();
        self.in_test = saved_test;
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        // `let f = |…|` — hand the binding name to the next closure visited.
        if let Some(init) = &node.init
            && matches!(&*init.expr, syn::Expr::Closure(_))
        {
            self.pending_name = let_binding_name(&node.pat);
        }
        visit::visit_local(self, node);
        // Defensive clear in case it wasn't consumed.
        self.pending_name = None;
    }

    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        let (line, col) = name_line_col(node.span());
        let symbol = self
            .pending_name
            .take()
            .unwrap_or_else(|| format!("<closure@L{line}C{col}>"));
        let sig = FnSig {
            receiver: None,
            inputs: node.inputs.iter().cloned().collect(),
            unsafety: false,
            asyncness: node.asyncness.is_some(),
            is_closure: true,
            unsafe_anchor: None,
        };
        let body = match &*node.body {
            syn::Expr::Block(eb) => FnBodyOwned::Block(eb.block.clone()),
            other => FnBodyOwned::Expr(Box::new(other.clone())),
        };
        // A closure inherits its enclosing unit's test-ness (never adds any).
        let is_test = self.in_test;
        // F8 — a closure is ALWAYS nested: it has no importable name, so it must
        // never populate `CanonicalIndex.primary`, even at depth 0 (a `const`/
        // `static` closure like `const ADD: fn(i32)->i32 = |x| x+1;`).
        self.emit(symbol, line, col, sig, body, is_test, true);
        // Recurse so nested closures / fns inside this one are collected (nested).
        let saved_in_unit = self.in_unit_body;
        self.in_unit_body = true;
        visit::visit_expr_closure(self, node);
        self.in_unit_body = saved_in_unit;
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

    #[test]
    fn nested_fn_is_its_own_unit() {
        let file =
            syn::parse_file("fn outer() { fn helper(v: &mut Vec<i32>) { v.push(1); } }").unwrap();
        let units = collect(&file, "a.rs");
        let syms: Vec<&str> = units.iter().map(|u| u.symbol.as_str()).collect();
        assert!(syms.contains(&"outer"));
        assert!(
            syms.contains(&"helper"),
            "nested fn must be its own unit; got {syms:?}"
        );
    }

    #[test]
    fn closure_takes_binding_name_else_positional_fallback() {
        let src = "fn f() { let g = |x: i32| x + 1; let _ = (|y: i32| y, |z: i32| z); }";
        let file = syn::parse_file(src).unwrap();
        let units = collect(&file, "a.rs");
        let syms: Vec<&str> = units.iter().map(|u| u.symbol.as_str()).collect();
        assert!(
            syms.contains(&"g"),
            "let-bound closure adopts `g`; got {syms:?}"
        );
        let anon: Vec<&str> = syms
            .iter()
            .copied()
            .filter(|s| s.starts_with("<closure@L"))
            .collect();
        assert_eq!(
            anon.len(),
            2,
            "two same-line closures → two units; got {anon:?}"
        );
        assert_ne!(
            anon[0], anon[1],
            "same-line closures must differ by col; got {anon:?}"
        );
    }

    #[test]
    fn nested_units_inherit_is_test() {
        let file = syn::parse_file("#[test]\nfn t() { fn helper() {} let c = || 1; }").unwrap();
        let units = collect(&file, "a.rs");
        for name in ["t", "helper", "c"] {
            assert!(
                units.iter().find(|u| u.symbol == name).unwrap().is_test,
                "`{name}` must inherit is_test"
            );
        }
    }

    #[test]
    fn tokio_test_nested_closure_inherits_is_test() {
        let file = syn::parse_file(
            "#[tokio::test]\nasync fn t() { let c = || { let d = || 1; d() }; c(); }",
        )
        .unwrap();
        let units = collect(&file, "a.rs");
        for u in &units {
            assert!(
                u.is_test,
                "`{}` under #[tokio::test] must inherit is_test",
                u.symbol
            );
        }
        for name in ["c", "d"] {
            assert!(
                units.iter().any(|u| u.symbol == name),
                "let-bound nested closure `{name}` must be its own unit under #[tokio::test]"
            );
        }
    }

    #[test]
    fn cfg_test_mod_composes_with_unit_level_inheritance() {
        let file = syn::parse_file(
            "#[cfg(test)]\nmod tests { fn helper_in_test_mod() { let c = || 1; } }",
        )
        .unwrap();
        let units = collect(&file, "a.rs");
        for name in ["helper_in_test_mod", "c"] {
            assert!(
                units.iter().find(|u| u.symbol == name).unwrap().is_test,
                "`{name}` in a #[cfg(test)] mod must be test (module-level ∘ unit-level)"
            );
        }
    }
}
