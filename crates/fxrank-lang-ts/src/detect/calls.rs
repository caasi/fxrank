//! World-effect detection: walks a function body for effectful function and
//! method calls (`fetch`, `console.log`, `Date.now`, `Math.random`,
//! `crypto.randomUUID`, `process.exit`, fs reads/writes), env reads
//! (`process.env.X`), constructor expressions (`new WebSocket(…)`), and `throw`
//! statements, emitting an [`Effect`] per signal.
//!
//! This is the swc analog of `fxrank-lang-rust`'s `detect/calls.rs`. Where syn
//! resolves a node's line standalone (`span().start().line`), swc spans are bare
//! `BytePos` offsets, so the walker carries a [`SpanLines`] (built from the same
//! `SourceMap` that parsed the file) to resolve each effect's line.
//!
//! **Bare globals classify without imports.** `fetch`, `Date`, `Math`,
//! `console`, and `process` are ambient globals — never in the `ImportTable` —
//! so they match on rendered name alone. `imports` is consulted only to map a
//! bare imported name (`readFile` from `node:fs`) back to its module for fs/db
//! classification.
//!
//! NOTE: namespace-import member calls (`import * as fs from 'node:fs';
//! fs.readFile()`) are NOT resolved through `imports` yet — only bare
//! single-ident imported names are. This is a documented Milestone-A limitation.

use fxrank_core::confidence::detection_confidence;
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;
use swc_ecma_ast::{Callee, Expr, MemberExpr, MemberProp, NewExpr, ThrowStmt};
use swc_ecma_visit::{Visit, VisitWith};

use crate::functions::FnBodyOwned;
use crate::imports::ImportTable;
use crate::source::SpanLines;

/// Detect world effects (IO, time, random, env-read, logging, throw) in `body`.
///
/// A pure function: it builds a fresh walker over `body`, resolving each effect
/// line through `lines` and classifying bare-imported fs/db names through
/// `imports`. The Task-7 `analyze_unit` will thread these in; tests call it
/// directly.
pub fn detect(body: &FnBodyOwned, imports: &ImportTable, lines: &SpanLines) -> Vec<Effect> {
    let mut walker = CallWalker {
        imports,
        lines,
        effects: Vec::new(),
    };
    body.walk_with(&mut walker);
    walker.effects
}

struct CallWalker<'a> {
    imports: &'a ImportTable,
    lines: &'a SpanLines,
    effects: Vec<Effect>,
}

impl Visit for CallWalker<'_> {
    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Some(rendered) = render_expr(callee)
            && let Some((kind, tier)) = self.classify_call(&rendered)
        {
            let line = self.lines.line(node.span);
            self.push(kind, tier, line, format!("{rendered}(…)"));
        }
        node.visit_children_with(self);
    }

    fn visit_member_expr(&mut self, node: &MemberExpr) {
        // Member access as a *value* (not a call) — e.g. reading
        // `process.env.HOME`. Matching only the exact 3-segment
        // `process.env.<X>` shape means the inner `process.env` (2 segments)
        // does not also fire, so each read is counted once.
        if let Some(rendered) = render_member(node)
            // Matches the 3-segment `process . env . VARNAME` access shape exactly.
            && let Some(rest) = rendered.strip_prefix("process.env.")
            && !rest.is_empty()
            && !rest.contains('.')
        {
            let line = self.lines.line(node.span);
            self.push(EffectKind::EnvRead, Tier::Heuristic, line, rendered);
        }
        node.visit_children_with(self);
    }

    fn visit_throw_stmt(&mut self, node: &ThrowStmt) {
        let line = self.lines.line(node.span);
        self.push(EffectKind::Panic, Tier::Exact, line, "throw".to_string());
        node.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, node: &NewExpr) {
        if let Some(name) = render_expr(&node.callee) {
            let no_args = node.args.as_ref().is_none_or(|a| a.is_empty());
            if let Some((kind, tier)) = classify_new_expr(&name, no_args) {
                let line = self.lines.line(node.span);
                self.push(kind, tier, line, format!("new {name}(…)"));
            }
        }
        node.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, _n: &swc_ecma_ast::ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
    fn visit_constructor(&mut self, _n: &swc_ecma_ast::Constructor) {}
}

impl CallWalker<'_> {
    fn push(&mut self, kind: EffectKind, tier: Tier, line: usize, evidence: String) {
        let class = kind.base_class();
        // Path tier carries a shadow penalty when the file has a dynamic import,
        // because a bare name could resolve to a module we never see (the
        // dynamic-import analog of the Rust frontend's glob-import shadow).
        let shadowed = matches!(tier, Tier::Path) && self.imports.has_dynamic();
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
            subreason: None,
            confidence,
        });
    }

    /// Classify a rendered callee (`fetch`, `console.log`, `Date.now`,
    /// `process.exit`, `readFile`) into an effect kind + detectability tier.
    ///
    /// Bare globals match on name directly; a single bare ident that is not a
    /// known global is resolved through `imports` to catch fs/db functions
    /// imported by name (`import { readFile } from 'node:fs'`).
    fn classify_call(&self, rendered: &str) -> Option<(EffectKind, Tier)> {
        use EffectKind::*;

        // --- Bare global idents (no `.`) -------------------------------------
        // net.fs.db — fetch only; XMLHttpRequest/WebSocket/EventSource are
        // constructors (NewExpr) and are classified in visit_new_expr.
        if rendered == "fetch" {
            return Some((NetFsDb, Tier::Path));
        }

        // --- `obj.method` member callees ------------------------------------
        if let Some((obj, method)) = rendered.rsplit_once('.') {
            // `console.<anything>` → logging.
            if obj == "console" {
                return Some((Logging, Tier::Path));
            }
            // `Date.now()` → time.read.
            if obj == "Date" && method == "now" {
                return Some((TimeRead, Tier::Path));
            }
            // `Math.random()` → random.
            if obj == "Math" && method == "random" {
                return Some((Random, Tier::Path));
            }
            // `crypto.randomUUID` / `crypto.getRandomValues` → random.
            if obj == "crypto" && matches!(method, "randomUUID" | "getRandomValues") {
                return Some((Random, Tier::Path));
            }
            // `process.exit` / `process.abort` → process.control.
            if obj == "process" && matches!(method, "exit" | "abort") {
                return Some((ProcessControl, Tier::Path));
            }
            // Unknown receiver — fall back to method-name-only heuristic.
            if let Some(result) = classify_method_call(method) {
                return Some(result);
            }
        }

        // --- A bare imported name resolved through the import table ----------
        // e.g. `import { readFile } from 'node:fs'; readFile(...)`. Only single
        // bare idents (no `.`) reach here meaningfully — a member call's leading
        // object is handled above.
        if !rendered.contains('.')
            && let Some(module) = self.imports.resolve(rendered)
            && is_fs_db_module(module)
        {
            return Some((NetFsDb, Tier::Heuristic));
        }

        None
    }
}

/// Render a (possibly nested) callee/member `Expr` into a dotted string:
/// `Expr::Ident("fetch")` → `fetch`, `Date.now` → `Date.now`,
/// `process.env.HOME` → `process.env.HOME`. Returns `None` for shapes we don't
/// model (computed indexing, calls-of-calls, `this`, etc.).
fn render_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(id) => Some(id.sym.to_string()),
        Expr::Member(m) => render_member(m),
        _ => None,
    }
}

/// Render a `MemberExpr` chain to `a.b.c`. Only `Ident` properties on
/// renderable objects are kept; computed/private props yield `None`.
fn render_member(m: &MemberExpr) -> Option<String> {
    let obj = render_expr(&m.obj)?;
    match &m.prop {
        MemberProp::Ident(name) => Some(format!("{obj}.{}", name.sym)),
        _ => None,
    }
}

/// Classify a `new <Name>(…)` constructor expression.
///
/// `new Date()` with no arguments is a time read; with arguments it constructs
/// a specific date and is not classified. Network/worker constructors are always
/// classified regardless of argument count.
fn classify_new_expr(name: &str, no_args: bool) -> Option<(EffectKind, Tier)> {
    use EffectKind::*;
    match name {
        // time.read — only no-arg Date() reads the current time.
        "Date" if no_args => Some((TimeRead, Tier::Path)),
        // net.fs.db — network constructors.
        "XMLHttpRequest" | "WebSocket" | "EventSource" => Some((NetFsDb, Tier::Path)),
        // concurrency — Worker constructor.
        "Worker" => Some((Concurrency, Tier::Path)),
        _ => None,
    }
}

/// Classify a method call by name alone (receiver type unknown → always
/// `Tier::Heuristic`). Called as a fallback when the full `obj.method` form
/// does not match any known-receiver pattern.
fn classify_method_call(method: &str) -> Option<(EffectKind, Tier)> {
    use EffectKind::*;
    match method {
        // net.fs.db — DB client; receiver type unknown.
        "query" | "execute" => Some((NetFsDb, Tier::Heuristic)),
        _ => None,
    }
}

/// Whether a module specifier denotes node's filesystem API.
fn is_fs_db_module(module: &str) -> bool {
    matches!(
        module,
        "fs" | "node:fs" | "fs/promises" | "node:fs/promises"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use crate::source::Lang;

    /// Parse `src`, build `SpanLines` + `ImportTable`, collect the unit named
    /// `fn_name`, and return the wire kinds of the effects `detect` finds.
    fn kinds(src: &str, fn_name: &str) -> Vec<String> {
        let (module, cm) = functions::parse_module(src, "t.ts", Lang::Ts).expect("parse");
        let lines = SpanLines::new(cm);
        let imports = ImportTable::from_module(&module);
        let units = functions::collect(&module, "t.ts", &lines);
        let unit = units
            .iter()
            .find(|u| u.symbol == fn_name)
            .expect("unit not found");
        detect(&unit.body, &imports, &lines)
            .iter()
            .map(|e| e.kind.wire().to_string())
            .collect()
    }

    #[test]
    fn bare_globals_need_no_imports() {
        let src = "function f() { fetch('x'); console.log('y'); const t = Date.now(); }";
        let k = kinds(src, "f");
        assert!(k.contains(&"net.fs.db".to_string()));
        assert!(k.contains(&"logging".to_string()));
        assert!(k.contains(&"time.read".to_string()));
    }

    #[test]
    fn env_read_counted_once() {
        let src = "function f() { const e = process.env.HOME; }";
        let k = kinds(src, "f");
        assert_eq!(
            k.iter().filter(|x| *x == "env.read").count(),
            1,
            "process.env.X should fire exactly once"
        );
    }

    #[test]
    fn resolves_imported_fs_function() {
        let src = "import { readFile } from 'node:fs';\nfunction f() { readFile('p'); }";
        let k = kinds(src, "f");
        assert!(k.contains(&"net.fs.db".to_string()));
    }
}
