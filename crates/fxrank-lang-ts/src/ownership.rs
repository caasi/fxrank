//! Tree-aware lexical ownership / adoption resolution (spec 027 §4.2).
//!
//! A React component **adopts** the nested function units it OWNS: each adopted
//! handler/arrow is suppressed as a standalone hotspot and its effects + outgoing
//! refs are MOVED (re-parented) onto the component's record — so every effect has
//! exactly one owner (no double-count).
//!
//! Unlike the old single-hop approach, this pass is
//! **tree-aware**: it walks the full lexical-ownership tree (component → nested
//! fn → nested-nested handler) at any depth. `functions::collect` already yields
//! every nested fn/arrow as a flat `FnUnit`, so the job is a *partition* — assign
//! each nested unit to the component that owns it — not a re-walk.
//!
//! ## What is "owned"
//!
//! Starting from the component body, a function VALUE is owned when it is:
//! - an inline arrow passed directly to a recognized hook (`useEffect`,
//!   `useMemo`, …) — phase from the hook;
//! - an inline arrow/`function` expression passed to a JSX prop
//!   (`onClick={() => …}`) — event phase;
//! - a **bare-ident** JSX prop value (`onClick={handleClick}`) whose name
//!   resolves through the component-scope provenance to a *locally defined*
//!   function (`Provenance::LocalDefined` with a backing `FnUnit`).
//!
//! The frontier then recurses into each owned unit's body to gather the function
//! values IT owns, transitively.
//!
//! ## What is NOT owned (the frontier stops)
//!
//! - A `Received` value (a function param / destructured prop, e.g. a callback
//!   passed in by a parent) — origin wins; never charged to the component.
//! - An `Imported` value (an imported function) — reached via a graph edge.
//! - A `LocalDefined` binding that is NOT a function (a `useState` setter, a
//!   plain `const`) — no backing `FnUnit`, so nothing to adopt.
//! - An `Unknown` value — never guessed.
//!
//! Such units stay standalone callables, reached across the call graph, not
//! adopted.

use std::collections::HashMap;

use swc_ecma_ast::{ArrowExpr, CallExpr, Decl, Expr, Pat, Stmt, VarDeclarator};
use swc_ecma_visit::{Visit, VisitWith};

use crate::detect::fnvalues::{self, OwnedValueSite};
use crate::functions::{FnBodyOwned, FnUnit};
use crate::imports::ImportTable;
use crate::module_map::TsModuleMap;
use crate::provenance::ProvenanceTable;
use crate::react::HookPhase;
use crate::source::SpanLines;

/// The deferral/phase context under which a component owns one nested unit.
#[derive(Clone, Copy)]
pub struct OwnedContext {
    /// React lifecycle phase of the owning site (drives `EffectInRender`).
    pub phase: HookPhase,
}

/// The lexical-ownership decision for one file's units relative to one component.
pub struct Adoption {
    /// `unit_id` → the context under which the component owns it. Present ⇒ the
    /// unit is ADOPTED (suppressed as a standalone hotspot + re-parented).
    pub owned: HashMap<String, OwnedContext>,
    /// Graph edges for IMPORTED function values the component passes as a JSX
    /// prop / hook arg (`onClick={importedHandler}`). The import owns the effects,
    /// so they PROPAGATE to the component via the cross-file fold — not adopted.
    /// `analyze_units` extends the component record's refs with these (spec 027
    /// §4.5, carry-forward #2).
    pub edges: Vec<fxrank_core::record::CallSiteRef>,
    /// Count of Unknown-provenance function values passed onward, so the caller
    /// can lower the component's confidence (never guessed).
    pub unknown_count: usize,
}

/// Walk `component`'s lexical subtree (any depth) and decide adoption.
///
/// `units` is the whole file unit list. Lexical ownership is determined by
/// **value-flow**: starting from the component body we gather every function
/// value it owns (inline hook-callback arrows, inline JSX-handler arrows, and
/// bare-ident JSX handlers that resolve to a local `FnUnit`), then recurse into
/// each owned unit's body to gather the values IT owns. The frontier stops at
/// received / imported / non-function / unknown values (not adopted).
///
/// Returned ids are matched against the file's `units` by the `(line, col)`
/// anchor (`FnUnit.col` is a real field — never split `id`).
///
/// The function-value routing (inline owned, LocalDefined-named owned, Imported →
/// edge, Received / Unknown) is delegated entirely to
/// [`crate::detect::fnvalues::extract_fn_values`] — the **single source of truth**
/// for "which function values a body owns / propagates / receives" (spec 027
/// §4.5). This pass is the *driver*: it walks the lexical subtree, resolves each
/// owned anchor to a `FnUnit`, and collects the imported edges to surface to
/// `analyze_units`.
pub fn resolve_ownership(
    component: &FnUnit,
    units: &[FnUnit],
    prov: &ProvenanceTable,
    imports: &ImportTable,
    lines: &SpanLines,
    module_map: &TsModuleMap,
) -> Adoption {
    // (line, col) → &FnUnit, over the whole file. Owned sites resolve here.
    let by_anchor: HashMap<(usize, usize), &FnUnit> =
        units.iter().map(|u| ((u.line, u.col), u)).collect();

    // Component-scope declaration map: local function name → (line, col) of its
    // declaration anchor (the same span `functions::collect` uses). Seeded from
    // the component body and extended as we descend into owned units, so a named
    // handler declared in the component body is resolvable from any owned site.
    let mut decl_sites: HashMap<String, (usize, usize)> = HashMap::new();
    collect_local_fn_decls(&component.body, lines, &mut decl_sites);

    let mut owned: HashMap<String, OwnedContext> = HashMap::new();
    let mut edges: Vec<fxrank_core::record::CallSiteRef> = Vec::new();
    let mut unknown_count = 0usize;
    // Worklist of bodies to scan for owned function values. The first item is the
    // component body (a root: only values it explicitly hands to a hook/JSX prop
    // are owned). Every other item is the body of an already-adopted unit (a
    // descendant: any non-escaping nested function it defines is also owned —
    // "owned by an owned unit is owned").
    let mut worklist: Vec<WorkItem<'_>> = vec![WorkItem {
        body: &component.body,
        phase: HookPhase::Render,
        is_descendant: false,
    }];

    while let Some(item) = worklist.pop() {
        // Each owned unit may declare more local handlers; fold them into the
        // scope map before resolving this body's bare-ident references.
        collect_local_fn_decls(item.body, lines, &mut decl_sites);

        // Single source of truth: the function values this body owns / propagates
        // / receives, routed by provenance (spec 027 §4.5). Imported handlers
        // surface as graph EDGES (propagate, not adopt); Received / Unknown are
        // not owned.
        let fv = fnvalues::extract_fn_values(
            item.body,
            prov,
            imports,
            lines,
            &component.path,
            module_map,
            &decl_sites,
        );
        edges.extend(fv.edges);
        unknown_count += fv.unknown_count;
        let mut sites: Vec<OwnedValueSite> = fv.owned_value_sites;

        // Inside an already-owned unit, every directly-nested function it DEFINES
        // (and does not let escape) is part of that owned unit's execution, so it
        // is owned too — it inherits the owning site's phase. This is what makes
        // adoption tree-aware at any depth (`const run = () => …; run();` two hops
        // down still attributes to the component). The component body itself is
        // NOT treated this way: a root only owns what it hands to a hook/JSX.
        if item.is_descendant {
            sites.extend(nested_definitions(item.body, lines, item.phase));
        }

        for site in sites {
            let Some(unit) = by_anchor.get(&site.anchor) else {
                // A bare-ident resolved to a name with no backing FnUnit
                // (e.g. a useState setter) — skip (no adoption, no edge).
                continue;
            };
            // Single lexical parent ⇒ a unit is owned by at most one component;
            // `HashMap` insert is idempotent for the same component. (We never
            // call `resolve_ownership` for two components against the same unit
            // and expect both to win — the lib.rs caller suppresses on first
            // ownership.)
            if owned
                .insert(unit.id.clone(), OwnedContext { phase: site.phase })
                .is_none()
            {
                // Newly adopted — descend into its body for transitive ownership.
                worklist.push(WorkItem {
                    body: &unit.body,
                    phase: site.phase,
                    is_descendant: true,
                });
            }
        }
    }

    Adoption {
        owned,
        edges,
        unknown_count,
    }
}

/// One body to scan, with the phase its owning site contributes and whether it
/// is a descendant of the component (vs the component body itself).
struct WorkItem<'a> {
    body: &'a FnBodyOwned,
    /// Phase of the site that adopted this body (component root uses `Render` as
    /// a neutral seed; it is never read for the root, only for descendants).
    phase: HookPhase,
    is_descendant: bool,
}

/// Collect the directly-nested function DEFINITIONS of an already-owned body
/// (`function f(){…}` / `const f = () => …` / `const f = function(){…}`),
/// assigning each `phase`. Own-body only (descent stops at nested fn/arrow
/// scopes; each owned unit is scanned separately).
///
/// This is what gives adoption depth beyond hooks/JSX: inside a hook callback,
/// a `const run = () => fetch(…); run();` helper is owned by the same component.
///
/// ## Escape filter (spec 027 §4.5, carry-forward #3)
///
/// A definition whose name **escapes** is excluded — the frontier stops at
/// `EscapedValue`. A name escapes when it is:
/// - **returned** from the body (`return run;` — a closure handed out), or
/// - **passed as an argument to a call** (`registerCallback(run)` — the value
///   flows into a callee we do not own). This closes T3's over-adoption gap,
///   where `const run = () => fetch(); registerCallback(run);` wrongly charged
///   the fetch to the component.
fn nested_definitions(
    body: &FnBodyOwned,
    lines: &SpanLines,
    phase: HookPhase,
) -> Vec<OwnedValueSite> {
    let mut walker = NestedDefWalker {
        lines,
        phase,
        defs: Vec::new(),
        escaped: std::collections::HashSet::new(),
    };
    body.walk_with(&mut walker);
    // Drop definitions whose declared name escaped (returned or passed to a call).
    walker
        .defs
        .into_iter()
        .filter(|(name, _)| name.as_deref().is_none_or(|n| !walker.escaped.contains(n)))
        .map(|(_, site)| site)
        .collect()
}

/// Walker for [`nested_definitions`]: records top-level function definitions and
/// the names that escape the body (returned, or passed as a call argument).
struct NestedDefWalker<'a> {
    lines: &'a SpanLines,
    phase: HookPhase,
    /// (declared name if any, owned site).
    defs: Vec<(Option<String>, OwnedValueSite)>,
    /// Identifier names that escape this body (returned or passed to a callee).
    escaped: std::collections::HashSet<String>,
}

impl NestedDefWalker<'_> {
    fn push_def(&mut self, name: String, anchor: (usize, usize)) {
        self.defs.push((
            Some(name),
            OwnedValueSite {
                anchor,
                phase: self.phase,
            },
        ));
    }
}

impl Visit for NestedDefWalker<'_> {
    fn visit_stmt(&mut self, n: &Stmt) {
        match n {
            Stmt::Decl(Decl::Fn(f)) => {
                let anchor = self.lines.line_col(f.ident.span);
                self.push_def(f.ident.sym.to_string(), anchor);
            }
            Stmt::Decl(Decl::Var(var)) => {
                for d in &var.decls {
                    let Pat::Ident(b) = &d.name else { continue };
                    let Some(init) = &d.init else { continue };
                    let anchor = match init.as_ref() {
                        Expr::Arrow(a) => self.lines.line_col(a.span),
                        Expr::Fn(fe) => self.lines.line_col(fe.function.span),
                        _ => continue,
                    };
                    self.push_def(b.id.sym.to_string(), anchor);
                }
            }
            Stmt::Return(r) => {
                if let Some(arg) = &r.arg
                    && let Expr::Ident(id) = arg.as_ref()
                {
                    self.escaped.insert(id.sym.to_string());
                }
            }
            _ => {}
        }
        // Recurse into nested blocks/scopes via the default visit so inner
        // statements (including `foo(run)` escape arguments) are reached —
        // see visit_call_expr below.  Descent stops at nested function/arrow
        // scopes via the visit_arrow_expr / visit_function overrides, which
        // prevents crossing into a child function's own body.
        n.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, node: &CallExpr) {
        // Escape gap (#3): a bare-ident argument flows into the callee — the value
        // escapes our ownership (we do not own what the callee does with it). Mark
        // it escaped so the def is NOT adopted. The callee ident itself is a CALL,
        // not a value, so it is not marked.
        for arg in &node.args {
            if let Expr::Ident(id) = arg.expr.as_ref() {
                self.escaped.insert(id.sym.to_string());
            }
        }
        node.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, _n: &ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
}

/// Collect the top-level local function declarations of `body` into
/// `out`: name → `(line, col)` declaration anchor, matching the span
/// `functions::collect` records for each shape. Own-body only (descent stops at
/// nested fn/arrow scopes).
///
/// Recognized shapes:
/// - `function f(){…}` → `f.ident.span`
/// - `const f = () => …` → arrow `span`
/// - `const f = function(){…}` → fn-expr `function.span`
fn collect_local_fn_decls(
    body: &FnBodyOwned,
    lines: &SpanLines,
    out: &mut HashMap<String, (usize, usize)>,
) {
    let mut walker = DeclSiteWalker { lines, out };
    body.walk_with(&mut walker);
}

struct DeclSiteWalker<'a> {
    lines: &'a SpanLines,
    out: &'a mut HashMap<String, (usize, usize)>,
}

impl Visit for DeclSiteWalker<'_> {
    fn visit_stmt(&mut self, n: &Stmt) {
        if let Stmt::Decl(decl) = n {
            self.record_decl(decl);
        }
        // Do NOT recurse: only direct body statements (own-body, top level),
        // matching the provenance pass's `LocalDeclWalker`.
    }

    fn visit_arrow_expr(&mut self, _n: &ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
}

impl DeclSiteWalker<'_> {
    fn record_decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Fn(f) => {
                let anchor = self.lines.line_col(f.ident.span);
                // Keep the FIRST declaration (source order); a re-declaration in
                // the same scope is unusual and we do not model it.
                self.out.entry(f.ident.sym.to_string()).or_insert(anchor);
            }
            Decl::Var(var) => {
                for d in &var.decls {
                    self.record_var(d);
                }
            }
            _ => {}
        }
    }

    fn record_var(&mut self, d: &VarDeclarator) {
        let Pat::Ident(b) = &d.name else {
            return;
        };
        let Some(init) = &d.init else {
            return;
        };
        let anchor = match init.as_ref() {
            Expr::Arrow(a) => self.lines.line_col(a.span),
            Expr::Fn(f) => self.lines.line_col(f.function.span),
            _ => return,
        };
        self.out.entry(b.id.sym.to_string()).or_insert(anchor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::{collect, parse_module};
    use crate::imports::ImportTable;
    use crate::source::Lang;

    /// Resolve ownership for the component named `comp` in `src`.
    fn adopt_for(src: &str, comp: &str) -> (Adoption, Vec<FnUnit>) {
        let (module, cm) = parse_module(src, "t.tsx", Lang::Tsx).expect("parse");
        let imports = ImportTable::from_module(&module);
        let lines = SpanLines::new(cm);
        let units = collect(&module, "t.tsx", &lines);
        let component = units
            .iter()
            .find(|u| u.symbol == comp)
            .expect("component unit");
        let prov = ProvenanceTable::build(component, &imports);
        let mmap = TsModuleMap::build(&[fxrank_core::frontend::SourceFile {
            path: "t.tsx".into(),
            text: String::new(),
        }]);
        let adoption = resolve_ownership(component, &units, &prov, &imports, &lines, &mmap);
        // Re-collect units to return an owned Vec (the borrow above ends here).
        let (module2, cm2) = parse_module(src, "t.tsx", Lang::Tsx).expect("parse");
        let lines2 = SpanLines::new(cm2);
        let units2 = collect(&module2, "t.tsx", &lines2);
        (adoption, units2)
    }

    /// Number of units whose id is adopted.
    fn owned_symbols<'a>(a: &Adoption, units: &'a [FnUnit]) -> Vec<&'a str> {
        units
            .iter()
            .filter(|u| a.owned.contains_key(&u.id))
            .map(|u| u.symbol.as_str())
            .collect()
    }

    #[test]
    fn hook_arrow_is_owned() {
        let (a, units) = adopt_for(
            "function C(){ useEffect(() => { fetch('/x'); }, []); return <div/>; }",
            "C",
        );
        // The single inline arrow is adopted; the component itself is not.
        assert_eq!(a.owned.len(), 1);
        let owned = owned_symbols(&a, &units);
        assert!(owned.iter().all(|s| s.starts_with("<arrow@")));
    }

    #[test]
    fn named_local_handler_is_owned() {
        let (a, units) = adopt_for(
            "function C(){ function onClick(){ fetch('/x'); } return <button onClick={onClick}/>; }",
            "C",
        );
        let owned = owned_symbols(&a, &units);
        assert!(
            owned.contains(&"onClick"),
            "named local handler must be owned; owned={owned:?}"
        );
    }

    #[test]
    fn depth_two_nested_arrow_is_owned() {
        let (a, units) = adopt_for(
            "function C(){ useEffect(() => { const run = () => fetch('/x'); run(); }, []); return <div/>; }",
            "C",
        );
        // Both the outer hook arrow AND the inner `run` arrow are adopted.
        assert_eq!(a.owned.len(), 2, "tree-aware: depth-2 arrow adopted too");
        let owned = owned_symbols(&a, &units);
        assert!(
            owned
                .iter()
                .all(|s| s.starts_with("<arrow@") || *s == "run")
        );
    }

    #[test]
    fn received_prop_handler_is_not_owned() {
        // onChange is a received prop — origin wins, never adopted.
        let (a, units) = adopt_for(
            "function C({onChange}){ return <input onChange={onChange}/>; }",
            "C",
        );
        assert!(a.owned.is_empty(), "received prop must not be adopted");
        let _ = units;
    }

    #[test]
    fn usestate_setter_handler_is_not_owned() {
        // setV is LocalDefined but not a function — skip (no FnUnit).
        let (a, units) = adopt_for(
            "function C(){ const [v,setV]=useState(0); return <input onChange={setV}/>; }",
            "C",
        );
        assert!(
            a.owned.is_empty(),
            "useState setter is not a function value → not adopted"
        );
        let _ = units;
    }

    #[test]
    fn imported_handler_is_edge_not_owned() {
        let (a, _units) = adopt_for(
            "import { handler } from './h';\n\
             function C(){ return <button onClick={handler}/>; }",
            "C",
        );
        assert!(
            a.owned.is_empty(),
            "imported handler reached via edge, not adopted"
        );
        // Carry-forward #2: the import surfaces as a graph edge to propagate.
        assert_eq!(a.edges.len(), 1, "imported handler must emit one edge");
        assert_eq!(a.edges[0].base, "handler");
        assert_eq!(a.edges[0].module.as_deref(), Some("./h"));
    }

    #[test]
    fn value_escaped_to_unknown_callee_is_not_adopted() {
        // Carry-forward #3: `run` is defined then passed to the unknown
        // `registerCallback` — it ESCAPES, so it must NOT be adopted even though
        // it lives inside an owned hook callback.
        let (a, units) = adopt_for(
            "function C(){ useEffect(() => { const run = () => fetch('/x'); registerCallback(run); }, []); return <div/>; }",
            "C",
        );
        // Only the outer hook arrow is adopted; `run` is excluded (escaped).
        let owned = owned_symbols(&a, &units);
        assert!(
            !owned.contains(&"run"),
            "an escaped-to-unknown-callee value must NOT be adopted; owned={owned:?}"
        );
    }
}
