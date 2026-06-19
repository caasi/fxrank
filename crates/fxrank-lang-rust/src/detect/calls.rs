//! Call-effect detection: walks a function body for path-tier function calls
//! and heuristic-tier method calls, emitting an `Effect` per recognised signal.
//!
//! Path calls (`std::fs::write(..)`, `Instant::now()`) are matched by rendering
//! the callee path and resolving its leading segment through the file's
//! `ImportTable` (so `fs::write` with `use std::fs;` becomes `std::fs::write`).
//! Method calls (`.send(..)`, `.unwrap()`) are matched on the method name alone
//! — we can't know the receiver's type, so those are heuristic-tier.

use fxrank_core::confidence::detection_confidence;
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;
use syn::spanned::Spanned;
use syn::visit::Visit;

/// Detect call-based effects in `block`. Path calls are resolved through
/// `imports`; `path` is unused here today but kept for parity with sibling
/// detectors (T12–T14) that emit `RiskFeature`s carrying a path.
///
/// `statics` is the set of top-level `static` names from the same file. Bare
/// path expressions (not in callee position) whose single-segment ident matches
/// a name in `statics` are emitted as `ambient.read` (class 2, Heuristic).
/// `visit_expr_call` sets `in_callee = true` before visiting the callee, so
/// `visit_expr_path` skips static-name emission for callee paths.
pub fn detect(
    block: &syn::Block,
    imports: &super::Imports,
    _path: &str,
    statics: &std::collections::HashSet<String>,
) -> Vec<Effect> {
    let mut walker = CallWalker {
        imports,
        statics,
        effects: Vec::new(),
        in_callee: false,
    };
    walker.visit_block(block);
    walker.effects
}

struct CallWalker<'a> {
    imports: &'a super::Imports,
    statics: &'a std::collections::HashSet<String>,
    effects: Vec<Effect>,
    /// True while visiting the callee sub-expression of a `Call` node.
    /// Prevents `visit_expr_path` from emitting `ambient.read` for a static
    /// that appears in the function position (e.g. `CONFIG()` where
    /// `static CONFIG: fn() = ...`).
    in_callee: bool,
}

impl<'a, 'ast> Visit<'ast> for CallWalker<'a> {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*node.func {
            let rendered = render_path(&p.path);
            let resolved = self.resolve(&rendered);
            if let Some(kind) = classify_path_call(&resolved) {
                let line = node.span().start().line;
                self.push(kind, Tier::Path, line, rendered);
            }
        }
        // Recurse manually so we can set `in_callee` for the callee sub-expression.
        // This prevents `visit_expr_path` from emitting a spurious `ambient.read`
        // when the callee's single-segment name happens to match a known `static`.
        // We do NOT call `syn::visit::visit_expr_call` here to avoid double-visiting.
        self.in_callee = true;
        self.visit_expr(&node.func);
        self.in_callee = false;
        for arg in &node.args {
            self.visit_expr(arg);
        }
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        if let Some(kind) = classify_method_call(&method) {
            let line = node.span().start().line;
            self.push(kind, Tier::Heuristic, line, format!(".{method}"));
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    /// Detect bare reads of file-level `static` names as `ambient.read`.
    ///
    /// `visit_expr_call` recurses into the callee with `self.in_callee = true`
    /// before calling this, so callee paths (e.g. `CONFIG()` where
    /// `static CONFIG: fn() = ...`) are suppressed here. Only non-callee
    /// single-segment paths that match a known static emit `ambient.read`.
    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        // Only flag single-segment paths (no `::` qualification) that match a
        // known static name, and only when not in a callee position.
        if !self.in_callee && node.path.segments.len() == 1 {
            let ident = node.path.segments[0].ident.to_string();
            if self.statics.contains(&ident) {
                let line = node.span().start().line;
                self.push(EffectKind::AmbientRead, Tier::Heuristic, line, ident);
            }
        }
        syn::visit::visit_expr_path(self, node);
    }
}

impl<'a> CallWalker<'a> {
    /// Resolve a rendered path's leading segment through the import table.
    /// `fs::write` + `use std::fs;` → `std::fs::write`. Bare or already-qualified
    /// paths are returned unchanged.
    fn resolve(&self, rendered: &str) -> String {
        let (head, tail) = match rendered.split_once("::") {
            Some((h, t)) => (h, Some(t)),
            None => (rendered, None),
        };
        match self.imports.resolve(head) {
            Some(full) => match tail {
                Some(t) => format!("{full}::{t}"),
                None => full.to_string(),
            },
            None => rendered.to_string(),
        }
    }

    fn push(&mut self, kind: EffectKind, tier: Tier, line: usize, evidence: String) {
        let class = kind.base_class();
        // Path tier carries a shadow penalty when the file has a glob import,
        // because a bare name could resolve to something we never see.
        let shadowed = matches!(tier, Tier::Path) && self.imports.has_glob();
        let confidence = detection_confidence(tier, false, shadowed);
        self.effects.push(Effect {
            kind,
            class,
            discounted_to: None,
            weight: weight_for_class(class),
            line,
            tier,
            hidden: false,
            evidence,
            discount: None,
            confidence,
        });
    }
}

/// Render a `syn::Path` to its `::`-joined segment idents (`std::fs::write`).
/// Type-qualified leading segments (`<T as Tr>::f`) are skipped — rare for
/// effectful call targets.
fn render_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Match a resolved call path against the path-tier signal matrix.
fn classify_path_call(p: &str) -> Option<EffectKind> {
    use EffectKind::*;

    // process.control — exit/abort only; Command::new is a CONSTRUCTOR, not an effect.
    if p == "std::process::exit" || p == "std::process::abort" {
        return Some(ProcessControl);
    }

    // env.write
    if p == "std::env::set_var" || p == "std::env::remove_var" || p == "std::env::set_current_dir" {
        return Some(EnvWrite);
    }

    // env.read
    if matches!(
        p,
        "std::env::var"
            | "std::env::vars"
            | "std::env::args"
            | "std::env::current_dir"
            | "std::env::current_exe"
            | "std::env::temp_dir"
    ) {
        return Some(EnvRead);
    }

    // time.read — fully-qualified or short forms (Instant::now / SystemTime::now).
    if p == "std::time::Instant::now"
        || p == "std::time::SystemTime::now"
        || p == "Instant::now"
        || p == "SystemTime::now"
    {
        return Some(TimeRead);
    }

    // random
    if p.starts_with("rand::") || p == "thread_rng" || p.ends_with("::thread_rng") {
        return Some(Random);
    }

    // concurrency
    if p == "std::thread::spawn"
        || p == "std::thread::sleep"
        || p == "tokio::spawn"
        || p.starts_with("rayon::")
        || p.starts_with("JoinSet::")
        || p.contains("::JoinSet::")
    {
        return Some(Concurrency);
    }

    // net.fs.db — std::fs, std::net, tokio::fs, reqwest, sqlx.
    if p.starts_with("std::fs::")
        || p.starts_with("std::net::")
        || p.starts_with("tokio::fs::")
        || p.starts_with("reqwest::")
        || p.starts_with("sqlx::")
    {
        return Some(NetFsDb);
    }

    // stdin/stdout/stderr free handles → net.fs.db.
    if matches!(
        p,
        "std::io::stdin" | "std::io::stdout" | "std::io::stderr" | "stdin" | "stdout" | "stderr"
    ) {
        return Some(NetFsDb);
    }

    None
}

/// Match a method name against the heuristic-tier signal matrix.
fn classify_method_call(method: &str) -> Option<EffectKind> {
    use EffectKind::*;
    match method {
        // net.fs.db — Read/Write trait methods (receiver type unknown).
        "read_line" | "write_all" | "read_to_string" | "read_to_end" => Some(NetFsDb),
        // process.control — Command/Child methods.
        "spawn" | "status" | "output" | "kill" => Some(ProcessControl),
        // concurrency — channel methods.
        "send" | "recv" | "try_recv" => Some(Concurrency),
        // ambient.read — Atomic loads.
        "load" => Some(AmbientRead),
        // panic — Option/Result unwrapping.
        "unwrap" | "expect" => Some(Panic),
        _ => None,
    }
}
