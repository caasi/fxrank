//! JSX-prop & hook-arg function-value walker (spec 027 §4.5).
//!
//! `refs::extract` records only **calls** (`f()`, `a.b()`); a function passed as
//! a **value** — `<Button onClick={handleClick}>`, `useX(cb)` — is invisible to
//! it (its `visit_call_expr` stops at the callee, and `visit_arrow_expr` /
//! `visit_function` halt descent). This walker is the missing half: it finds
//! function VALUES handed to a JSX prop or a call argument and routes each by its
//! [`Provenance`]:
//!
//! - **inline** arrow / `function` expression → OWNED (the component owns a
//!   closure it defines inline) → its `(line, col)` enters `owned_value_sites`.
//! - **`LocalDefined`-named** handler (`onClick={handleClick}` where `handleClick`
//!   is a local `function`/arrow) → OWNED → the local decl's `(line, col)` enters
//!   `owned_value_sites`. A `LocalDefined` binding that is NOT a function (a
//!   `useState` setter, a plain `const`) has no entry in `decl_sites` → skipped.
//! - **`Imported`** handler → a graph EDGE (`edges`): the imported definition owns
//!   the effects, so they PROPAGATE to the component through the cross-file fold —
//!   never copied. The edge is built by the SAME `refs::ref_for_base` helper the
//!   call walker uses, so `handle()` and `onClick={handle}` agree.
//! - **`Received`** value (a param / destructured prop passed onward) → recorded
//!   in `received_passed` for completeness, NOT charged (origin wins, §2.3).
//! - **`Unknown`** value → skipped, and `unknown_count` is bumped so the caller
//!   can lower the component's confidence (never guessed).
//!
//! This walker is the **single source of truth** for "which function values does a
//! body own / propagate / receive"; the ownership pass ([`crate::ownership`])
//! consumes `owned_value_sites`, and `analyze_units` extends the component
//! record's refs with `edges`.
//!
//! ## Value vs. call (no double-count)
//!
//! In `visit_call_expr` the **callee** ident is a CALL (handled by
//! `refs::extract`); each **argument** ident that is a function value is a VALUE
//! (handled here). The two walkers partition: this one reports values, `refs`
//! reports calls.

use std::collections::HashMap;

use swc_ecma_ast::{ArrowExpr, CallExpr, Callee, Expr, JSXAttr, JSXAttrValue, JSXExpr};
use swc_ecma_visit::{Visit, VisitWith};

use crate::functions::FnBodyOwned;
use crate::imports::ImportTable;
use crate::module_map::TsModuleMap;
use crate::provenance::{Provenance, ProvenanceTable};
use crate::react::HookPhase;
use crate::source::SpanLines;

/// A function-value use-site the enclosing body owns, with the React phase of
/// the owning site (drives `EffectInRender` once adopted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnedValueSite {
    /// `(line, col)` anchor of the function VALUE — an inline arrow/fn-expr span,
    /// or the declaration span of a bare-ident handler (the same anchor
    /// `functions::collect` records, so it matches a `FnUnit`).
    pub anchor: (usize, usize),
    /// React phase of the owning site (`Event` for JSX handlers, `Effect` for
    /// effect-phase hooks, `Render` for render-phase hooks).
    pub phase: HookPhase,
}

/// The function values one body passes around, partitioned by routing.
pub struct FnValueSites {
    /// inline / LocalDefined-named function VALUES the body OWNS — fed into the
    /// ownership frontier (these become adopted units).
    pub owned_value_sites: Vec<OwnedValueSite>,
    /// graph edges for Imported function values passed onward (propagate the
    /// imported definition's effects; first-party resolves, third-party opaque).
    pub edges: Vec<fxrank_core::record::CallSiteRef>,
    /// Received values passed onward — recorded for completeness, NOT charged.
    pub received_passed: Vec<(usize, usize)>,
    /// Count of Unknown-provenance values passed as a value — the caller lowers
    /// the component confidence by this much (never guessed).
    pub unknown_count: usize,
}

/// Extract the function values `body` passes as JSX props / call arguments,
/// routing each by [`Provenance`] (see the module docs).
///
/// `decl_sites` maps a local function name → its `(line, col)` declaration
/// anchor (built by the ownership pass over the component subtree); a
/// `LocalDefined` ident resolves through it. A `LocalDefined` name absent from
/// `decl_sites` is "not a function value" → skipped.
///
/// Own-body only: descent stops at nested fn/arrow scopes (each owned unit is
/// scanned separately by the ownership worklist), matching the sibling walkers.
pub fn extract_fn_values(
    body: &FnBodyOwned,
    prov: &ProvenanceTable,
    imports: &ImportTable,
    lines: &SpanLines,
    referencing_file: &str,
    module_map: &TsModuleMap,
    decl_sites: &HashMap<String, (usize, usize)>,
) -> FnValueSites {
    let referencing_key = module_map.module_of(referencing_file);
    let mut walker = FnValueWalker {
        prov,
        imports,
        lines,
        referencing_file,
        referencing_key,
        module_map,
        decl_sites,
        out: FnValueSites {
            owned_value_sites: Vec::new(),
            edges: Vec::new(),
            received_passed: Vec::new(),
            unknown_count: 0,
        },
    };
    body.walk_with(&mut walker);
    walker.out
}

struct FnValueWalker<'a> {
    prov: &'a ProvenanceTable,
    imports: &'a ImportTable,
    lines: &'a SpanLines,
    referencing_file: &'a str,
    referencing_key: String,
    module_map: &'a TsModuleMap,
    decl_sites: &'a HashMap<String, (usize, usize)>,
    out: FnValueSites,
}

impl FnValueWalker<'_> {
    /// Record an inline arrow value as owned with `phase`.
    fn push_arrow(&mut self, arrow: &ArrowExpr, phase: HookPhase) {
        let anchor = self.lines.line_col(arrow.span);
        self.out
            .owned_value_sites
            .push(OwnedValueSite { anchor, phase });
    }

    /// Record an inline `function` expression value as owned with `phase`. The
    /// anchor matches `functions::collect`'s fn-expr span (`function.span`).
    fn push_fn_expr(&mut self, f: &swc_ecma_ast::FnExpr, phase: HookPhase) {
        let anchor = self.lines.line_col(f.function.span);
        self.out
            .owned_value_sites
            .push(OwnedValueSite { anchor, phase });
    }

    /// Route a bare-ident function VALUE referenced at `(line, col)` with `phase`.
    fn handle_ident_value(&mut self, name: &str, line: usize, col: usize, phase: HookPhase) {
        match self.prov.get(name) {
            // Local function → owned IF it resolves to a real local fn decl.
            Provenance::LocalDefined => {
                if let Some(&anchor) = self.decl_sites.get(name) {
                    self.out
                        .owned_value_sites
                        .push(OwnedValueSite { anchor, phase });
                }
                // No backing FnUnit decl (e.g. useState setter, plain const) → skip.
            }
            // Imported → a graph edge so the import's effects propagate (not copied).
            Provenance::Imported => {
                let r = super::refs::ref_for_base(
                    name.to_string(),
                    line,
                    col,
                    self.imports,
                    self.module_map,
                    self.referencing_file,
                    &self.referencing_key,
                );
                self.out.edges.push(r);
            }
            // Received → passed onward, never charged (origin wins, §2.3).
            Provenance::Received => self.out.received_passed.push((line, col)),
            // Unknown → never guessed; bump the confidence-lowering counter.
            Provenance::Unknown => self.out.unknown_count += 1,
        }
    }

    /// Route a JSX-prop expression value (`onX={value}`).
    ///
    /// JSX handlers run on interaction ⇒ **event phase** (spec 027 §2.4): their
    /// effects are conditional on interaction and earn the conditionality discount.
    fn handle_jsx_prop_value(&mut self, e: &Expr) {
        match e {
            // `onClick={() => …}` — inline arrow handler (event phase).
            Expr::Arrow(a) => self.push_arrow(a, HookPhase::Event),
            // `onClick={function(){…}}` — inline fn-expr handler (event phase).
            Expr::Fn(f) => self.push_fn_expr(f, HookPhase::Event),
            // `onClick={handleClick}` — bare-ident handler.
            Expr::Ident(id) => {
                let (line, col) = self.lines.line_col(id.span);
                self.handle_ident_value(id.sym.as_ref(), line, col, HookPhase::Event);
            }
            _ => {}
        }
    }
}

impl Visit for FnValueWalker<'_> {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        // Recognized hooks: route the inline-arrow callback argument by phase.
        if let Some(name) = hook_callee_name(node) {
            match name {
                "useEffect" | "useLayoutEffect" => {
                    self.handle_call_arg(node, 0, HookPhase::Effect, true);
                    return; // own-body only: do not descend into the callback scope
                }
                "useCallback" => {
                    // Body runs on invocation (event-time), conditional on
                    // interaction ⇒ event phase (spec 027 §2.4).
                    self.handle_call_arg(node, 0, HookPhase::Event, true);
                    return;
                }
                "useMemo" | "useState" => {
                    self.handle_call_arg(node, 0, HookPhase::Render, true);
                    return;
                }
                "useReducer" => {
                    self.handle_call_arg(node, 2, HookPhase::Render, true);
                    return;
                }
                // An unrecognized `use[A-Z]…` hook: ownership of an inline-arrow
                // arg is certain (the component hands it over), but the invocation
                // schedule is not ⇒ Unknown phase (event-like + confidence
                // penalty applied in `adopt_effects`). Spec 027 §6/§4.3.
                // `is_builtin_hook` = false for custom hooks: their object arg IS
                // an options/callback bag and MUST be descended (e.g. useMutation).
                _ if crate::react::is_hook_name(name) => {
                    let is_builtin = crate::react::is_builtin_hook(name);
                    self.handle_call_arg(node, 0, HookPhase::Unknown, is_builtin);
                    return;
                }
                _ => {}
            }
        }
        // A non-hook call: its ARGUMENTS that are function values are passed onward.
        // We deliberately do NOT route a bare-ident arg to an unknown callee as
        // owned — that is an ESCAPE (the value flows into an opaque sink). Only an
        // inline arrow/fn argument to a recognized hook is owned (handled above).
        // The escape itself is observed by the ownership pass (it sees the
        // `received_passed` / nothing-owned outcome). Recurse so nested hook/JSX
        // usage inside argument expressions is still found.
        node.visit_children_with(self);
    }

    fn visit_jsx_attr(&mut self, node: &JSXAttr) {
        if let Some(JSXAttrValue::JSXExprContainer(c)) = &node.value
            && let JSXExpr::Expr(e) = &c.expr
        {
            self.handle_jsx_prop_value(e);
        }
        // Recurse so nested JSX (children) attributes are still visited.
        node.visit_children_with(self);
    }

    // Stop descent at nested function scopes — own-body discipline, matching the
    // sibling React walkers. Each owned unit is scanned separately.
    fn visit_arrow_expr(&mut self, _n: &ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
}

impl FnValueWalker<'_> {
    /// Route the `idx`-th argument of `node` as an owned inline callback with
    /// `phase` (only inline arrows/fn-exprs are owned here; a bare-ident hook
    /// argument is left to flow through normal recursion).
    ///
    /// `is_builtin` controls object-literal descent (spec 027 §6 / Task 9 +
    /// Codex P2 fix):
    ///
    /// - **Built-in hooks** (`is_builtin = true`): their signatures are known;
    ///   callbacks are direct positional args, so an object arg is **state/config
    ///   data** (e.g. `useState({ save: () => fetch(…) })` stores the object —
    ///   React never invokes `save`). Object args are NOT descended for function
    ///   values.
    /// - **Custom / library hooks** (`is_builtin = false`, e.g. `useMutation`,
    ///   `useQuery`): their object arg IS an opaque options/callback bag —
    ///   `useMutation({ mutationFn: () => fetch(…) })`. Descend to route each
    ///   top-level function-valued property by provenance.
    fn handle_call_arg(&mut self, node: &CallExpr, idx: usize, phase: HookPhase, is_builtin: bool) {
        match node.args.get(idx).map(|a| a.expr.as_ref()) {
            Some(Expr::Arrow(a)) => self.push_arrow(a, phase),
            Some(Expr::Fn(f)) => self.push_fn_expr(f, phase),
            Some(Expr::Object(obj)) if !is_builtin => self.handle_options_object(obj, phase),
            _ => {}
        }
    }

    /// Route the TOP-LEVEL function-valued properties of a hook options object
    /// (spec 027 §6 / Task 9). Each property is routed by the SAME rules as a
    /// direct callback / handler:
    ///
    /// - inline arrow / `function` value (`mutationFn: () => …`) or a method
    ///   shorthand (`mutationFn() {…}`) → OWNED with `phase`.
    /// - bare-ident value or shorthand (`mutationFn: doSave` / `{ mutationFn }`)
    ///   → routed by [`Self::handle_ident_value`] (local-owned / imported-edge /
    ///   received-passed / unknown-counted).
    /// - non-function value (`retry: 3`) → skipped.
    ///
    /// **Scope cap (deferred, §6):** only top-level properties. A nested object
    /// (`{ mutation: { mutationFn } }`) or an array of callbacks is NOT descended.
    fn handle_options_object(&mut self, obj: &swc_ecma_ast::ObjectLit, phase: HookPhase) {
        use swc_ecma_ast::{Prop, PropOrSpread};
        for prop in &obj.props {
            let PropOrSpread::Prop(prop) = prop else {
                // A `...spread` element is opaque — never guessed.
                continue;
            };
            match prop.as_ref() {
                // `mutationFn: () => …` / `mutationFn: function(){…}` — inline owned.
                Prop::KeyValue(kv) => match kv.value.as_ref() {
                    Expr::Arrow(a) => self.push_arrow(a, phase),
                    Expr::Fn(f) => self.push_fn_expr(f, phase),
                    // `mutationFn: doSave` — bare-ident value routed by provenance.
                    Expr::Ident(id) => {
                        let (line, col) = self.lines.line_col(id.span);
                        self.handle_ident_value(id.sym.as_ref(), line, col, phase);
                    }
                    // non-function value (`retry: 3`, a member expr, …) → skip.
                    _ => {}
                },
                // `{ mutationFn }` — shorthand routed by provenance.
                Prop::Shorthand(id) => {
                    let (line, col) = self.lines.line_col(id.span);
                    self.handle_ident_value(id.sym.as_ref(), line, col, phase);
                }
                // `mutationFn() {…}` — inline method value → owned.
                Prop::Method(m) => {
                    let anchor = self.lines.line_col(m.function.span);
                    self.out
                        .owned_value_sites
                        .push(OwnedValueSite { anchor, phase });
                }
                // getters/setters/assign are not function-value props we adopt.
                _ => {}
            }
        }
    }
}

/// Return the bare-ident callee name of a call, if any.
fn hook_callee_name(node: &CallExpr) -> Option<&str> {
    match &node.callee {
        Callee::Expr(e) => match e.as_ref() {
            Expr::Ident(i) => Some(i.sym.as_ref()),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::{collect, parse_module};
    use crate::source::Lang;

    /// Build the analysis context for the component `comp` in `src` and run
    /// `extract_fn_values` over its body. `decl_sites` is seeded from the
    /// component's local function declarations (a tiny inline collector, since
    /// the real one lives in `ownership`).
    fn fn_values(src: &str, comp: &str) -> FnValueSites {
        let (module, cm) = parse_module(src, "t.tsx", Lang::Tsx).expect("parse");
        let imports = ImportTable::from_module(&module);
        let lines = SpanLines::new(cm);
        let units = collect(&module, "t.tsx", &lines);
        let component = units.iter().find(|u| u.symbol == comp).expect("component");
        let prov = ProvenanceTable::build(component, &imports);
        let mmap = TsModuleMap::build(&[fxrank_core::frontend::SourceFile {
            path: "t.tsx".into(),
            text: String::new(),
        }]);
        // Seed decl_sites with the component's own local fn declarations by anchor.
        let mut decl_sites: HashMap<String, (usize, usize)> = HashMap::new();
        for u in &units {
            // Local handlers are the units whose name is a plain identifier and
            // whose declaration sits inside the component (we approximate by name).
            if u.symbol != comp && !u.symbol.contains('.') && !u.symbol.starts_with("<arrow@") {
                decl_sites
                    .entry(u.symbol.clone())
                    .or_insert((u.line, u.col));
            }
            // arrow/fn-expr bound to a const keep their binding name as symbol too.
        }
        extract_fn_values(
            &component.body,
            &prov,
            &imports,
            &lines,
            "t.tsx",
            &mmap,
            &decl_sites,
        )
    }

    #[test]
    fn inline_arrow_jsx_handler_is_owned() {
        let v = fn_values(
            "function C(){ return <button onClick={() => fetch('/x')}/>; }",
            "C",
        );
        assert_eq!(
            v.owned_value_sites.len(),
            1,
            "inline arrow handler is owned"
        );
        assert_eq!(
            v.owned_value_sites[0].phase,
            HookPhase::Event,
            "JSX handler runs on interaction → event phase (spec 027 §2.4)"
        );
        assert!(v.edges.is_empty());
        assert!(v.received_passed.is_empty());
    }

    #[test]
    fn local_named_handler_is_owned() {
        let v = fn_values(
            "function C(){ const onClick = () => { fetch('/x'); }; return <button onClick={onClick}/>; }",
            "C",
        );
        assert_eq!(
            v.owned_value_sites.len(),
            1,
            "LocalDefined named handler is owned"
        );
        assert!(v.edges.is_empty());
    }

    #[test]
    fn imported_handler_is_edge_not_owned() {
        let v = fn_values(
            "import { handle } from './h';\n\
             function C(){ return <button onClick={handle}/>; }",
            "C",
        );
        assert!(
            v.owned_value_sites.is_empty(),
            "imported handler is NOT owned"
        );
        assert_eq!(v.edges.len(), 1, "imported handler emits one edge");
        assert_eq!(v.edges[0].base, "handle");
        assert_eq!(v.edges[0].module.as_deref(), Some("./h"));
    }

    #[test]
    fn received_handler_is_passed_not_charged() {
        let v = fn_values(
            "function C({onSave}){ return <button onClick={onSave}/>; }",
            "C",
        );
        assert!(v.owned_value_sites.is_empty(), "received handler not owned");
        assert!(v.edges.is_empty(), "received handler emits no edge");
        assert_eq!(v.received_passed.len(), 1, "received handler is recorded");
    }

    #[test]
    fn usestate_setter_is_not_a_function_value() {
        // setV is LocalDefined but has no backing fn decl in decl_sites → skipped.
        let v = fn_values(
            "function C(){ const [v,setV]=useState(0); return <input onChange={setV}/>; }",
            "C",
        );
        assert!(
            v.owned_value_sites.is_empty(),
            "useState setter is not a function value → not owned"
        );
        assert!(v.edges.is_empty());
        // setV resolves to LocalDefined (not Received) → not in received_passed.
        assert!(v.received_passed.is_empty());
    }

    #[test]
    fn hook_arrow_callback_is_owned_with_phase() {
        let v = fn_values(
            "function C(){ useEffect(() => { fetch('/x'); }, []); return <div/>; }",
            "C",
        );
        assert_eq!(v.owned_value_sites.len(), 1);
        assert_eq!(
            v.owned_value_sites[0].phase,
            HookPhase::Effect,
            "useEffect callback is effect phase"
        );
    }

    #[test]
    fn usememo_arrow_callback_is_render_phase() {
        let v = fn_values(
            "function C(){ const x = useMemo(() => fetch('/b'), []); return <div/>; }",
            "C",
        );
        assert_eq!(v.owned_value_sites.len(), 1);
        assert_eq!(v.owned_value_sites[0].phase, HookPhase::Render);
    }

    #[test]
    fn usecallback_arrow_callback_is_event_phase() {
        // useCallback body runs on invocation (event-time) → event phase.
        let v = fn_values(
            "function C(){ const f = useCallback(() => fetch('/x'), []); return <div/>; }",
            "C",
        );
        assert_eq!(v.owned_value_sites.len(), 1);
        assert_eq!(v.owned_value_sites[0].phase, HookPhase::Event);
    }

    #[test]
    fn unknown_hook_arrow_callback_is_unknown_phase() {
        // An unrecognized use[A-Z]… hook: inline-arrow arg is owned, schedule unknown.
        let v = fn_values(
            "function C(){ useMystery(() => fetch('/x')); return <div/>; }",
            "C",
        );
        assert_eq!(v.owned_value_sites.len(), 1, "unknown hook arg is owned");
        assert_eq!(v.owned_value_sites[0].phase, HookPhase::Unknown);
    }

    #[test]
    fn object_literal_hook_arg_props_are_owned_unknown_phase() {
        // `useMutation({ mutationFn: () => fetch(), onError: () => console.warn() })`
        // Both inline function-value props are OWNED by the component; the unknown
        // (non-built-in) hook schedule ⇒ Unknown phase. A non-function prop is
        // skipped. Spec 027 §6 (Task 9).
        let v = fn_values(
            "function C(){ useMutation({ mutationFn: () => fetch('/x'), onError: () => console.warn('e'), retry: 3 }); return <div/>; }",
            "C",
        );
        assert_eq!(
            v.owned_value_sites.len(),
            2,
            "both function-valued props are owned; the numeric prop is skipped"
        );
        for site in &v.owned_value_sites {
            assert_eq!(
                site.phase,
                HookPhase::Unknown,
                "non-built-in hook schedule is unknown"
            );
        }
        assert!(v.edges.is_empty());
        assert!(v.received_passed.is_empty());
    }

    #[test]
    fn object_literal_shorthand_local_prop_is_owned() {
        // `{ mutationFn }` shorthand where `mutationFn` is a local fn → owned.
        let v = fn_values(
            "function C(){ const mutationFn = () => { fetch('/x'); }; useMutation({ mutationFn }); return <div/>; }",
            "C",
        );
        assert_eq!(
            v.owned_value_sites.len(),
            1,
            "shorthand referencing a local fn is owned"
        );
        assert!(v.edges.is_empty());
    }

    #[test]
    fn object_literal_imported_prop_is_edge() {
        // `{ mutationFn: doSave }` where `doSave` is imported → graph edge (propagate).
        let v = fn_values(
            "import { doSave } from './save';\n\
             function C(){ useMutation({ mutationFn: doSave }); return <div/>; }",
            "C",
        );
        assert!(v.owned_value_sites.is_empty(), "imported prop is not owned");
        assert_eq!(v.edges.len(), 1, "imported prop emits one edge");
        assert_eq!(v.edges[0].base, "doSave");
        assert_eq!(v.edges[0].module.as_deref(), Some("./save"));
    }

    #[test]
    fn object_literal_received_prop_is_passed_not_charged() {
        // `{ mutationFn: onSave }` where `onSave` is a received prop → not charged.
        let v = fn_values(
            "function C({onSave}){ useMutation({ mutationFn: onSave }); return <div/>; }",
            "C",
        );
        assert!(v.owned_value_sites.is_empty(), "received prop not owned");
        assert!(v.edges.is_empty(), "received prop emits no edge");
        assert_eq!(v.received_passed.len(), 1, "received prop is recorded");
    }

    #[test]
    fn usestate_object_arg_is_state_data_not_adopted() {
        // Codex P2 fix: `useState({ save: () => fetch('/x') })` stores the object
        // as state data; React never invokes `save`. The function value MUST NOT
        // be adopted as an owned callback — doing so produced a false
        // `net.fs.db`/`effect.in.render` on the component.
        let v = fn_values(
            "function C(){ const [obj, setObj] = useState({ save: () => fetch('/x') }); return <div/>; }",
            "C",
        );
        assert!(
            v.owned_value_sites.is_empty(),
            "useState object arg is state DATA — the inline function is NOT adopted as a callback"
        );
        assert!(
            v.edges.is_empty(),
            "no edge emitted for useState object arg"
        );
    }

    #[test]
    fn usemutation_object_arg_props_are_still_owned() {
        // Gate regression: non-built-in hooks whose object arg IS an options bag
        // must still have their function-valued properties descended and adopted.
        // `useMutation({ mutationFn: () => fetch('/x') })` — `mutationFn` is a
        // callback the hook will invoke, so it IS owned (T9 behavior must not regress).
        let v = fn_values(
            "function C(){ useMutation({ mutationFn: () => fetch('/x') }); return <div/>; }",
            "C",
        );
        assert_eq!(
            v.owned_value_sites.len(),
            1,
            "useMutation object arg prop is still descended and adopted (T9 must not regress)"
        );
        assert_eq!(
            v.owned_value_sites[0].phase,
            HookPhase::Unknown,
            "custom hook ⇒ Unknown phase"
        );
        assert!(v.edges.is_empty());
    }

    #[test]
    fn object_arg_to_non_hook_call_is_not_descended() {
        // The hook-vs-non-hook boundary is the discriminator: an object arg handed
        // to a NON-hook callee is NOT descended (T3 escape rule holds — do not
        // adopt a non-hook call's callbacks). Spec 027 §6 (Task 9).
        let v = fn_values(
            "function C(){ configure({ onSave: () => fetch('/x') }); return <div/>; }",
            "C",
        );
        assert!(
            v.owned_value_sites.is_empty(),
            "object arg to a non-hook call is not descended (escape, not owned)"
        );
        assert!(v.edges.is_empty());
        assert!(v.received_passed.is_empty());
    }

    #[test]
    fn useoptimistic_object_arg_is_state_data_not_adopted() {
        // Codex P2 fix (027 §6): `useOptimistic({ save: () => fetch('/x') })` passes
        // a state-data object as the initial optimistic state; React never invokes
        // `save`. The inline function MUST NOT be adopted as an owned callback.
        // Before the fix, is_builtin_hook returned false for useOptimistic, so the
        // object was descended as a callback bag → false net.fs.db on the component.
        let v = fn_values(
            "function C(){ const [opt, addOpt] = useOptimistic({ save: () => fetch('/x') }, (s,a)=>s); return <div/>; }",
            "C",
        );
        assert!(
            v.owned_value_sites.is_empty(),
            "useOptimistic object arg is state DATA — the inline function must NOT be adopted as a callback"
        );
        assert!(
            v.edges.is_empty(),
            "no edge emitted for useOptimistic state-data object arg"
        );
    }
}
