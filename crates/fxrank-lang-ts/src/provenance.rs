//! Local provenance pass + function-value lattice (spec 027 §4.3).
//!
//! Pure analysis consumed by the React adoption pass (Task 3) and the
//! function-value walker (Task 5). It answers two questions:
//!
//! 1. **Where did a name visible inside a component come from?** A
//!    [`ProvenanceTable`] classifies each binding the component references as
//!    [`Provenance::Received`] (function param / destructured prop),
//!    [`Provenance::Imported`] (an ES/CJS import), [`Provenance::LocalDefined`]
//!    (a local `function`/arrow declarator — or *any* local binding, see the
//!    caveat below), or [`Provenance::Unknown`] (a spread / computed access that
//!    we refuse to guess about).
//!
//! 2. **What is the lattice class of a function VALUE at a use-site?**
//!    [`classify_value`] folds the binding's provenance together with two
//!    site-local facts (`deferred`, `escaped`) into a [`ValueClass`] with a
//!    fixed precedence: origin wins — a *received* callback is never charged to
//!    the component regardless of where it then flows.
//!
//! ## `LocalDefined` does NOT imply "holds a function"
//!
//! [`Provenance::LocalDefined`] records that a name was bound by a local
//! declarator in the component body. It does **not** assert the binding holds a
//! function value. A `useState` setter (`const [v, setV] = useState(0)` → `setV`)
//! and a plain `const x = 5` are both `LocalDefined`, yet neither has a backing
//! [`crate::functions::FnUnit`]. Tasks 3 (adoption) and 5 (fn-value walker) must
//! resolve a `LocalDefined` binding's `(line, col)` against the collected
//! `FnUnit`s; when there is no match the binding is "not a function value" and is
//! skipped — no adoption, no graph edge. The provenance pass only labels origin;
//! it never claims a binding is callable.

use std::collections::HashMap;

use swc_ecma_ast::{Decl, Expr, Pat, Stmt, VarDeclarator};
use swc_ecma_visit::Visit;

use crate::detect::mutation::collect_pat_bindings;
use crate::functions::FnUnit;
use crate::imports::ImportTable;

/// Where a name visible inside a component came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provenance {
    /// A function parameter or destructured prop (`function C({onChange}){…}`).
    Received,
    /// An ES `import` / CJS `require` binding resolved by the [`ImportTable`].
    Imported,
    /// A local `function`/arrow declarator (or any other local binding) in the
    /// component body. Does NOT imply the binding holds a function value — see
    /// the module docs.
    LocalDefined,
    /// Origin could not be determined (spread / computed access / absent name).
    /// Callers downgrade confidence; they never guess.
    Unknown,
}

/// Classification of a function VALUE the component references at a use-site.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueClass {
    /// A locally-owned value invoked immediately (render body / render-phase hook).
    OwnedImmediate,
    /// A locally-owned value scheduled for later invocation (event handler /
    /// effect-phase / unknown hook).
    OwnedDeferred,
    /// A locally-owned value that escapes (returned / exported / stored in
    /// module|global|context|ref, or passed to an unknown callee).
    EscapedValue,
    /// A value that originated from outside the component (a received callback).
    /// Never charged to the component regardless of how it then flows.
    ReceivedValue,
}

/// Per-component binding → provenance map, built once.
pub struct ProvenanceTable {
    map: HashMap<String, Provenance>,
}

impl ProvenanceTable {
    /// Build the provenance map for `component` against the file `imports`.
    ///
    /// Resolution order (origin dominates — a shadowing param must win, per
    /// §4.3 precedence):
    /// 1. Component `sig.params` → every bound name is [`Provenance::Received`].
    /// 2. Local declarators in the component's own body, walked in source order:
    ///    - `function f(){…}` / `const f = () => …` / `const f = function(){…}`
    ///      → [`Provenance::LocalDefined`].
    ///    - `const g = f;` (ident init of a *known* name) → `g` inherits `f`'s
    ///      provenance (alias carry).
    ///    - `const {a} = src;` where `src` is a known name → each destructured
    ///      name inherits `src`'s provenance.
    ///    - computed / spread / unknown inits (`const x = obj[k]`,
    ///      `const {...rest} = props`) → [`Provenance::Unknown`].
    /// 3. Any name still absent that the [`ImportTable`] resolves →
    ///    [`Provenance::Imported`]. (Checked LAST so a shadowing param or local
    ///    wins over an import of the same name.)
    pub fn build(component: &FnUnit, imports: &ImportTable) -> Self {
        let mut map: HashMap<String, Provenance> = HashMap::new();

        // 1. Params → Received. (Highest precedence; never overwritten below.)
        let mut received = std::collections::HashSet::new();
        for pat in &component.sig.params {
            collect_pat_bindings(pat, &mut received);
        }
        for name in received {
            map.insert(name, Provenance::Received);
        }

        // 2. Local declarators in source order. We collect the body's top-level
        //    declaration statements (own-body only) and process them in order so
        //    `const g = f;` can see an earlier `f`. A param-shadowing local is
        //    NOT recorded (params already won at step 1); a local shadowing an
        //    import wins because locals are recorded BEFORE the import fallback.
        let mut walker = LocalDeclWalker { stmts: Vec::new() };
        component.body.walk_with(&mut walker);
        for decl in &walker.stmts {
            classify_local_decl(decl, imports, &mut map);
        }

        // 3. Import fallback — checked LAST so a shadowing param (step 1) or
        //    local (step 2) wins over an import of the same name (origin
        //    dominates, §4.3). A bare reference (e.g. a hook call `useImported()`
        //    that introduces no local binding) resolves to Imported here.
        for name in imports.local_names() {
            map.entry(name.to_string()).or_insert(Provenance::Imported);
        }

        ProvenanceTable { map }
    }

    /// Provenance of `name`, or [`Provenance::Unknown`] if the name is not a
    /// binding visible to the component.
    pub fn get(&self, name: &str) -> Provenance {
        self.map.get(name).copied().unwrap_or(Provenance::Unknown)
    }
}

/// Classify the bindings introduced by one top-level declaration of the
/// component body, recording each into `map` (origin dominates: a name already
/// resolved to [`Provenance::Received`] at step 1 is left untouched).
fn classify_local_decl(decl: &Decl, imports: &ImportTable, map: &mut HashMap<String, Provenance>) {
    match decl {
        // `function f(){…}` → the name is a local function.
        Decl::Fn(f) => {
            insert_if_absent(map, f.ident.sym.to_string(), Provenance::LocalDefined);
        }
        Decl::Var(var) => {
            for d in &var.decls {
                classify_var_declarator(d, imports, map);
            }
        }
        // class / TS-only declarations are not function-value sources we model.
        _ => {}
    }
}

/// Classify a single `const/let/var` declarator.
fn classify_var_declarator(
    d: &VarDeclarator,
    imports: &ImportTable,
    map: &mut HashMap<String, Provenance>,
) {
    let Some(init) = &d.init else {
        // A bare `let x;` declares a local binding with no value — LocalDefined
        // for a plain ident name; we cannot say more.
        if let Pat::Ident(b) = &d.name {
            insert_if_absent(map, b.id.sym.to_string(), Provenance::LocalDefined);
        }
        return;
    };

    match &d.name {
        // `const f = <init>` — a single named binding.
        Pat::Ident(b) => {
            let name = b.id.sym.to_string();
            let prov = provenance_of_init(init, imports, map);
            insert_if_absent(map, name, prov);
        }
        // `const {a, b} = <src>` / `const [a, b] = <src>` — destructuring.
        Pat::Object(_) | Pat::Array(_) => {
            let carried = destructure_source_provenance(&d.name, init, imports, map);
            let mut names = std::collections::HashSet::new();
            collect_pat_bindings(&d.name, &mut names);
            for name in names {
                insert_if_absent(map, name, carried);
            }
        }
        _ => {}
    }
}

/// Provenance for a `const f = <init>` single-ident binding.
///
/// - arrow / `function` expression init → [`Provenance::LocalDefined`].
/// - bare ident init `const g = f;` → `g` inherits `f`'s known provenance
///   (alias carry); an unknown `f` yields [`Provenance::Unknown`].
/// - anything else (calls, member/computed access, literals, …) →
///   [`Provenance::LocalDefined`] for a literal/expression we cannot follow but
///   that IS a local binding. (Callable-ness is resolved later via FnUnit lookup;
///   provenance only records origin = "defined locally".)
fn provenance_of_init(
    init: &Expr,
    imports: &ImportTable,
    map: &HashMap<String, Provenance>,
) -> Provenance {
    match init {
        // Local function value.
        Expr::Arrow(_) | Expr::Fn(_) => Provenance::LocalDefined,
        // Alias: `const g = f;` carries f's provenance through.
        Expr::Ident(id) => resolve_name(id.sym.as_ref(), imports, map),
        // Member / computed access we refuse to follow → Unknown.
        Expr::Member(_) => Provenance::Unknown,
        // Any other expression (call result, literal, …) is locally defined.
        // It may or may not hold a function; the FnUnit lookup decides that.
        _ => Provenance::LocalDefined,
    }
}

/// Provenance carried to every name of a destructuring pattern `pat = init`.
///
/// - A `...rest` element makes the binding opaque → [`Provenance::Unknown`]
///   (the rest captures arbitrary keys we cannot reason about).
/// - A *simple* known ident source (`const {a} = props`) carries that source's
///   provenance through (alias carry).
/// - Any other source (`const [v, setV] = useState(0)` — a call result) still
///   binds locally: the names ARE defined in the component body, so they are
///   [`Provenance::LocalDefined`]. (Callable-ness — e.g. `setV` is not a fn —
///   is decided later by the FnUnit lookup, not here.)
fn destructure_source_provenance(
    pat: &Pat,
    init: &Expr,
    imports: &ImportTable,
    map: &HashMap<String, Provenance>,
) -> Provenance {
    if pattern_has_rest(pat) {
        return Provenance::Unknown;
    }
    match init {
        Expr::Ident(id) => resolve_name(id.sym.as_ref(), imports, map),
        _ => Provenance::LocalDefined,
    }
}

/// Resolve a *referenced* name to a provenance: an already-known local/param
/// binding wins; otherwise consult the import table; else Unknown.
fn resolve_name(
    name: &str,
    imports: &ImportTable,
    map: &HashMap<String, Provenance>,
) -> Provenance {
    if let Some(p) = map.get(name) {
        return *p;
    }
    if imports.resolve(name).is_some() {
        return Provenance::Imported;
    }
    Provenance::Unknown
}

/// Insert `name → prov` only if `name` is not already recorded (origin
/// dominates: a param-Received or earlier binding is never overwritten).
/// Imported is the fallback applied here too: if the name is still absent and
/// `prov` is its computed local provenance, that stands; a name never seen as a
/// local but resolvable as an import is handled by callers via [`resolve_name`].
fn insert_if_absent(map: &mut HashMap<String, Provenance>, name: String, prov: Provenance) {
    map.entry(name).or_insert(prov);
}

/// True if `pat` contains a rest (`...rest`) element anywhere at the top level
/// of an object/array destructuring (which makes carry-through unsafe).
fn pattern_has_rest(pat: &Pat) -> bool {
    match pat {
        Pat::Object(o) => o
            .props
            .iter()
            .any(|p| matches!(p, swc_ecma_ast::ObjectPatProp::Rest(_))),
        Pat::Array(a) => a.elems.iter().flatten().any(|e| matches!(e, Pat::Rest(_))),
        _ => false,
    }
}

/// Classify a function value at a use-site. `prov` is the provenance of the
/// referenced binding (use [`Provenance::Unknown`] for an inline anonymous
/// arrow — it has no name, so it is owned-by-definition and never `Received`).
/// `deferred` = the site schedules the value for later invocation (event
/// handler / effect-phase / unknown hook) vs immediate (render body /
/// render-phase hook). `escaped` = the value is returned / exported / stored in
/// module|global|context|ref or passed to an unknown callee.
///
/// Precedence is FIXED per §4.3: **`ReceivedValue` first** (origin wins — a
/// received callback is never charged to the component regardless of where it
/// then flows), then `EscapedValue`, then `OwnedImmediate`/`OwnedDeferred`.
pub fn classify_value(prov: Provenance, deferred: bool, escaped: bool) -> ValueClass {
    if prov == Provenance::Received {
        return ValueClass::ReceivedValue; // origin wins
    }
    if escaped {
        return ValueClass::EscapedValue;
    }
    if deferred {
        ValueClass::OwnedDeferred
    } else {
        ValueClass::OwnedImmediate
    }
}

/// Walker collecting the component body's **top-level** declaration statements
/// in source order (own-body only — descent stops at nested fn/arrow scopes).
///
/// We retain a clone of each `Decl` so `ProvenanceTable::build` can process them
/// after the walk in source order (alias carry depends on order). Only
/// statement-position declarations of the body are kept; declarations nested
/// inside blocks (`if`/`for`/…) are an accepted miss for this pass, matching the
/// own-body single-scope discipline of the other React walkers.
struct LocalDeclWalker {
    stmts: Vec<Decl>,
}

impl Visit for LocalDeclWalker {
    fn visit_stmt(&mut self, n: &Stmt) {
        if let Stmt::Decl(decl) = n {
            self.stmts.push(decl.clone());
        }
        // Do NOT recurse: only direct body statements count (own-body, top
        // level). Nested-block declarations are an accepted miss.
    }

    // Defensive: stop descent at nested function scopes too (the non-recursing
    // visit_stmt already prevents this, but matches the sibling walkers).
    fn visit_arrow_expr(&mut self, _n: &swc_ecma_ast::ArrowExpr) {}
    fn visit_function(&mut self, _n: &swc_ecma_ast::Function) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::{collect, parse_module};
    use crate::source::{Lang, SpanLines};

    /// Build a [`ProvenanceTable`] for the component named `comp_symbol` in `src`.
    ///
    /// Mirrors `react.rs`'s `unit_with_lines` plumbing: parse Tsx (keeping the
    /// `SourceMap`), build the [`ImportTable`] from the module, collect the
    /// `FnUnit`s, find the component, and build its provenance table.
    fn build_table_for(src: &str, comp_symbol: &str) -> ProvenanceTable {
        let (module, cm) = parse_module(src, "t.tsx", Lang::Tsx).expect("parse");
        let imports = ImportTable::from_module(&module);
        let lines = SpanLines::new(cm);
        let unit = collect(&module, "t.tsx", &lines)
            .into_iter()
            .find(|u| u.symbol == comp_symbol)
            .expect("component unit");
        ProvenanceTable::build(&unit, &imports)
    }

    #[test]
    fn params_are_received_imports_are_imported_locals_are_localdefined() {
        let t = build_table_for(
            "import { useImported } from 'x';\n\
             function C({onChange}){ const local = () => {}; useImported(); return null; }",
            "C",
        );
        assert_eq!(t.get("onChange"), Provenance::Received);
        assert_eq!(t.get("useImported"), Provenance::Imported);
        assert_eq!(t.get("local"), Provenance::LocalDefined);
        assert_eq!(t.get("nope"), Provenance::Unknown);
    }

    #[test]
    fn alias_carries_provenance() {
        let t = build_table_for("function C({cb}){ const g = cb; return null; }", "C");
        assert_eq!(
            t.get("g"),
            Provenance::Received,
            "alias of a received prop is received"
        );
    }

    #[test]
    fn received_wins_precedence_in_classify() {
        // a received prop that is then passed onward is still ReceivedValue
        assert_eq!(
            classify_value(Provenance::Received, true, true),
            ValueClass::ReceivedValue
        );
        assert_eq!(
            classify_value(Provenance::LocalDefined, false, false),
            ValueClass::OwnedImmediate
        );
        assert_eq!(
            classify_value(Provenance::LocalDefined, true, false),
            ValueClass::OwnedDeferred
        );
        assert_eq!(
            classify_value(Provenance::LocalDefined, false, true),
            ValueClass::EscapedValue
        );
    }

    #[test]
    fn function_decl_is_localdefined() {
        let t = build_table_for("function C(){ function handler(){} return null; }", "C");
        assert_eq!(t.get("handler"), Provenance::LocalDefined);
    }

    #[test]
    fn function_expression_init_is_localdefined() {
        let t = build_table_for("function C(){ const f = function(){}; return null; }", "C");
        assert_eq!(t.get("f"), Provenance::LocalDefined);
    }

    #[test]
    fn usestate_setter_is_localdefined_binding() {
        // `setV` is a LocalDefined BINDING — but it does NOT hold a function
        // unit. The provenance pass labels origin only; the FnUnit lookup in
        // Tasks 3/5 decides callable-ness and skips it (no adoption, no edge).
        let t = build_table_for(
            "function C(){ const [v, setV] = useState(0); return null; }",
            "C",
        );
        assert_eq!(
            t.get("setV"),
            Provenance::LocalDefined,
            "useState setter is a LocalDefined binding (callable-ness decided later)"
        );
        assert_eq!(t.get("v"), Provenance::LocalDefined);
    }

    #[test]
    fn plain_const_is_localdefined_not_function() {
        // `const x = 5;` is LocalDefined (origin = locally defined). It is not a
        // function; later FnUnit lookup classifies it "not a function value".
        let t = build_table_for("function C(){ const x = 5; return null; }", "C");
        assert_eq!(t.get("x"), Provenance::LocalDefined);
    }

    #[test]
    fn destructure_from_received_props_carries_received() {
        let t = build_table_for(
            "function C(props){ const { a, b } = props; return null; }",
            "C",
        );
        assert_eq!(t.get("a"), Provenance::Received);
        assert_eq!(t.get("b"), Provenance::Received);
    }

    #[test]
    fn rest_spread_destructure_is_unknown() {
        // `const {...rest} = props` — opaque spread → Unknown (do not guess).
        let t = build_table_for(
            "function C(props){ const { ...rest } = props; return null; }",
            "C",
        );
        assert_eq!(
            t.get("rest"),
            Provenance::Unknown,
            "rest spread is opaque — never guessed"
        );
    }

    #[test]
    fn computed_member_access_is_unknown() {
        // `const x = obj[k];` — computed access → Unknown.
        let t = build_table_for(
            "function C({obj, k}){ const x = obj[k]; return null; }",
            "C",
        );
        assert_eq!(
            t.get("x"),
            Provenance::Unknown,
            "computed member access is opaque — never guessed"
        );
    }

    #[test]
    fn received_param_shadows_import_of_same_name() {
        // A param named `foo` and an `import { foo }` — Received must win
        // (origin dominates per §4.3 precedence).
        let t = build_table_for(
            "import { foo } from 'x';\n\
             function C(foo){ return null; }",
            "C",
        );
        assert_eq!(
            t.get("foo"),
            Provenance::Received,
            "shadowing param wins over an import of the same name"
        );
    }

    #[test]
    fn alias_of_import_carries_imported() {
        let t = build_table_for(
            "import { thing } from 'x';\n\
             function C(){ const g = thing; return null; }",
            "C",
        );
        assert_eq!(
            t.get("g"),
            Provenance::Imported,
            "alias of an imported name is imported"
        );
    }

    #[test]
    fn alias_of_unknown_is_unknown() {
        let t = build_table_for(
            "function C(){ const g = somethingExternal; return null; }",
            "C",
        );
        assert_eq!(t.get("g"), Provenance::Unknown);
    }
}
