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
//! The `FnUnit` struct retains `sig` and `block` so that later detector tasks
//! (T11–T15) can walk the body without re-parsing. Access pattern: callers that
//! need effect detection import `FnUnit` from `fxrank_lang_rust::functions` and
//! walk `.block`; the `Frontend` impl maps `FnUnit` to `Hotspot` for scoring.

use syn::{ImplItem, Item, ItemImpl, ItemTrait, TraitItem};

/// A concrete function unit — a named item with a body that can be analysed for
/// effects. `sig` and `block` are kept verbatim so detectors in `detect/` can
/// walk them without re-parsing.
pub struct FnUnit {
    /// Display symbol: `free_fn`, `S::method`, `<S as T>::method`, `T::defaulted`.
    pub symbol: String,
    /// Collision-resistant id: `path:line:col:symbol` (col is the 1-based char column).
    pub id: String,
    /// Source file path (as passed in).
    pub path: String,
    /// 1-based line number of the function's name (`sig.ident`).
    pub line: usize,
    /// The full function signature (for detectors to inspect attributes, asyncness, etc.).
    pub sig: syn::Signature,
    /// The function body (for detectors to walk expressions in T11–T15).
    pub block: syn::Block,
    /// Whether this function is test code (`#[test]`/`#[bench]`, or inside a
    /// `#[cfg(test)]` module). Computed at collection time; test units are
    /// excluded from scoring by default.
    pub is_test: bool,
}

/// Returns `true` when `attrs` contains `#[test]` or `#[bench]`.
fn has_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("test") || a.path().is_ident("bench"))
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
    collect_items(&file.items, path, false, &mut units);
    units
}

fn collect_items(items: &[Item], path: &str, in_cfg_test: bool, out: &mut Vec<FnUnit>) {
    for item in items {
        match item {
            Item::Fn(f) => {
                let symbol = f.sig.ident.to_string();
                let start = f.sig.ident.span().start();
                let line = start.line;
                let col = start.column + 1; // proc-macro2 column is 0-based
                let is_test = in_cfg_test || has_test_attr(&f.attrs);
                out.push(FnUnit {
                    id: format!("{path}:{line}:{col}:{symbol}"),
                    symbol,
                    path: path.to_string(),
                    line,
                    sig: f.sig.clone(),
                    block: *f.block.clone(),
                    is_test,
                });
            }

            Item::Impl(impl_block) => {
                collect_from_impl(impl_block, path, in_cfg_test, out);
            }

            Item::Trait(trait_item) => {
                collect_from_trait(trait_item, path, in_cfg_test, out);
            }

            Item::Mod(m) => {
                if let Some((_, nested_items)) = &m.content {
                    let nested_in_cfg_test = in_cfg_test || is_cfg_test(&m.attrs);
                    collect_items(nested_items, path, nested_in_cfg_test, out);
                }
                // `mod foo;` without a body is out-of-line — skip.
            }

            _ => {}
        }
    }
}

fn collect_from_impl(impl_block: &ItemImpl, path: &str, in_cfg_test: bool, out: &mut Vec<FnUnit>) {
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

            let is_test = in_cfg_test || has_test_attr(&method.attrs);
            out.push(FnUnit {
                id: format!("{path}:{line}:{col}:{symbol}"),
                symbol,
                path: path.to_string(),
                line,
                sig: method.sig.clone(),
                block: method.block.clone(),
                is_test,
            });
        }
    }
}

fn collect_from_trait(
    trait_item: &ItemTrait,
    path: &str,
    in_cfg_test: bool,
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

                let is_test = in_cfg_test || has_test_attr(&method.attrs);
                out.push(FnUnit {
                    id: format!("{path}:{line}:{col}:{symbol}"),
                    symbol,
                    path: path.to_string(),
                    line,
                    sig: method.sig.clone(),
                    block: block.clone(),
                    is_test,
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
