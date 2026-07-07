//! Mutation detection with escape analysis for Python — the `fxrank-lang-python`
//! analog of `fxrank-lang-ts`'s `detect/mutation.rs`.
//!
//! Python's mutation story is simpler than Rust's (no `&mut` / ownership) but
//! more nuanced than JS's: `global` and `nonlocal` declarations are function-wide
//! (not position-dependent), and `self.attr = …` inside `__init__` is construction
//! (contained), not escaping state mutation.
//!
//! ## Escape classification table
//!
//! | write site                                   | kind             | class | contained |
//! |----------------------------------------------|------------------|-------|-----------|
//! | `global x` declared, then `x = …` or `x += …` | `global.mutation` | 6  | false     |
//! | `nonlocal x` declared, then `x = …` or `x += …` | `this.mutation` | 3 | false     |
//! | `self.attr = …` in `__init__`               | `local.mutation` | 1     | **true**  |
//! | `self.attr = …` in a non-`__init__` method  | `this.mutation`  | 3     | false     |
//! | `self.x.append(…)` / `self[i] = …` (any method, incl. `__init__`) | `this.mutation` | 3 | false |
//! | write where root is a **param name**        | `param.mutation` | 3     | false     |
//! | write where root is a **local binding**     | `local.mutation` | 1     | **true**  |
//! | module top-level binding, content-mutated (no `global`) | `global.mutation` | 6 | false |
//!
//! ## Strategy
//!
//! 1. **Resolve** each write-site's root name against the file's `ScopeTree`
//!    (spec 078) — `global`/`nonlocal` declarations, params, locals, imports,
//!    and module-level bindings are all encoded in the tree's binding walk, so
//!    a single `scope.resolve(unit_scope, name)` replaces the old pre-scan +
//!    flat-set membership checks (Task 6 removed the last flat-set consumer).
//! 2. **Walk** the body classifying write targets: `Assign`/`AnnAssign`/`AugAssign`
//!    targets, and mutating method calls (`.append`, `.update`, `.add`) via
//!    `on_call` in the EffectSink.
//!
//! The `contained` bool returned alongside each `Effect` is the
//! boundary-containment signal that Task 9's discount consumes.

use std::collections::HashSet;

use fxrank_core::confidence::detection_confidence;
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;
use libcst_native::{Assert, AssignTargetExpression, Call, Expression, Name, Raise};

use super::expr::render_expr;
use super::{EffectSink, walk_own_body};
use crate::functions::FnUnit;
use crate::imports::Imports;
use crate::scope::{BindingOrigin, ScopeId, ScopeTree};
use crate::source::{SpanIndex, anchor_of_subslice};

/// Detect mutation effects in `unit`'s own body, with escape analysis.
///
/// Returns `(Effect, contained)` pairs. The `bool` is the containment flag —
/// `true` means the write is bounded to this function's scope (local init or
/// constructor init); `false` means it escapes.
///
/// Task 9 consumes the `contained` flags to apply boundary-containment discounts.
///
/// `scope` is threaded through so `MutSink` can compute `unit_scope` — the
/// unit's own `Function` scope in the file's `ScopeTree` (spec 078). Every
/// classification arm (`classify_and_push`, `on_assign_target`, and now
/// `on_call`'s #50 gate) resolves through `scope`/`unit_scope`; the flat
/// `params`/`globals`/`nonlocals`/`locals`/`module_bindings` sets that used to
/// back these checks are gone (Task 6, spec 078) — `on_call` was the last
/// consumer.
///
/// `imports` and `module_bindings` are kept as parameters (not read here) so
/// this function's public arity is unchanged — they are threaded through
/// `gather`/`analyze_unit`/`build_record` and 4 `lib.rs` call sites; re-rippling
/// that signature is out of scope for Task 6 (Task 9 may revisit).
pub(crate) fn detect(
    unit: &FnUnit,
    _imports: &Imports,
    _module_bindings: &HashSet<String>,
    scope: &ScopeTree,
    span: &SpanIndex,
) -> Vec<(Effect, bool)> {
    let is_init = unit.symbol == "__init__";
    let unit_scope = scope.unit_scope_of(unit);
    let mut sink = MutSink {
        is_init,
        scope,
        unit_scope,
        span,
        effects: Vec::new(),
    };
    walk_own_body(unit, &mut sink);
    sink.effects
}

// ─── write-site classifier (EffectSink) ──────────────────────────────────────

struct MutSink<'a> {
    /// True when analyzing `__init__` (so `self.attr = …` is local init).
    is_init: bool,
    /// The file's scope tree (spec 078). Consulted by `classify_and_push`,
    /// `on_assign_target`'s bare-name global/nonlocal gate, and (as of Task 6)
    /// `on_call`'s #50 module-import suppression gate — all via
    /// `scope.resolve(unit_scope, name)`. The flat `params`/`globals`/
    /// `nonlocals`/`locals`/`imports`/`module_bindings` fields this replaced
    /// are gone; `on_call` was the last consumer.
    scope: &'a ScopeTree,
    /// `unit.symbol`'s own `Function` scope in `scope`, computed once via
    /// `scope.unit_scope_of(unit)`. Every `resolve()` call in this sink starts
    /// here (there is no per-use-site cursor — see the guideline).
    unit_scope: ScopeId,
    span: &'a SpanIndex<'a>,
    effects: Vec<(Effect, bool)>,
}

impl EffectSink for MutSink<'_> {
    fn on_call(&mut self, call: &Call) {
        // Detect mutating method calls: `receiver.append(…)`, `.update(…)`, `.add(…)`.
        let Expression::Attribute(attr) = call.func.as_ref() else {
            return;
        };
        if !is_mutating_method(attr.attr.value) {
            return;
        }
        // #50: a mutating-method call whose receiver is a bare imported **module**
        // name (`os.remove()`) is a call on the module, not a mutation of it — the
        // effect belongs to the callee and is scored by the call detector. Suppress
        // it, but only when the name genuinely resolves to the module here.
        // `resolve` returns `ModuleImport` ONLY when the name is a plain `import`
        // (not a `from m import n` name — those may bind a mutable object, e.g.
        // `from cfg import CACHE; CACHE.append(x)` stays a global.mutation) AND it
        // is not shadowed by a closer param/local/global/nonlocal binding (the
        // function scope wins the walk before the module scope) — so this is
        // exactly `!shadowed && is_module_import(name)`.
        // An *attribute* receiver reached through an import (`config.settings.append()`)
        // still mutates module-shared state and stays a global.mutation.
        if let Expression::Name(recv) = attr.value.as_ref()
            && matches!(
                self.scope.resolve(self.unit_scope, recv.value).0,
                BindingOrigin::ModuleImport
            )
        {
            return;
        }
        // The receiver's root name is the write target.
        let Some(root) = root_name_of_expr(&attr.value) else {
            return;
        };
        let (line, col) = name_line_col_expr(&attr.value, self.span);
        // Render the full receiver expression (the attribute chain) for evidence,
        // e.g. `self.items.append(…)` — not the misleading root-only `self.append(…)`.
        // Fall back to the root name for shapes `render_expr` doesn't model.
        let receiver = render_expr(&attr.value).unwrap_or_else(|| root.clone());
        let evidence = format!("{receiver}.{}(…)", attr.attr.value);
        self.classify_and_push(root, line, col, evidence);
    }

    fn on_assert(&mut self, _assert: &Assert) {}
    fn on_raise(&mut self, _raise: &Raise) {}

    fn on_assign_target(&mut self, target: &AssignTargetExpression, is_aug: bool) {
        match target {
            // `self.attr = …` → check is_init for LocalMutation vs ThisMutation.
            AssignTargetExpression::Attribute(attr) => {
                if let Expression::Name(n) = attr.value.as_ref()
                    && n.value == "self"
                {
                    let (line, col) = name_line_col(n, self.span);
                    if self.is_init {
                        self.push(
                            EffectKind::LocalMutation,
                            Tier::Heuristic,
                            line,
                            col,
                            "self.x = … (constructor init, contained)".to_string(),
                            true,
                        );
                    } else {
                        self.push(
                            EffectKind::ThisMutation,
                            Tier::Heuristic,
                            line,
                            col,
                            format!("self.{} = … (instance state)", attr.attr.value),
                            false,
                        );
                    }
                    return;
                }
                // Non-self attribute write: `obj.attr = …` — root is `obj`.
                if let Some(root) = root_name_of_expr(&attr.value) {
                    let (line, col) = name_line_col_expr(&attr.value, self.span);
                    let evidence = format!("{root}.{} = …", attr.attr.value);
                    self.classify_and_push(root, line, col, evidence);
                }
            }
            // `x = …` bare name. A plain `=` to a bare name is a *binding*, not a
            // mutation of pre-existing state (spec: `local.mutation` is `.append()` /
            // `d[k] = …` / `+=` on a locally-created binding — never the binding
            // itself) — UNLESS the name is declared `global`/`nonlocal`, in which case
            // a plain `g = …` rebinds the enclosing/global binding and IS an escaping
            // mutation. An augmented `x += …` is always a mutation.
            AssignTargetExpression::Name(n) if is_aug => {
                let name = n.value.to_owned();
                let (line, col) = name_line_col(n, self.span);
                let evidence = format!("{name} += …");
                self.classify_and_push(name, line, col, evidence);
            }
            // Plain `=` to a bare name declared `global`/`nonlocal` is an escaping
            // rebind, not a local binding — emit. A true local binding emits nothing.
            AssignTargetExpression::Name(n)
                if matches!(
                    self.scope.resolve(self.unit_scope, n.value).0,
                    BindingOrigin::Global | BindingOrigin::EnclosingCaptured
                ) =>
            {
                let name = n.value.to_owned();
                let (line, col) = name_line_col(n, self.span);
                let evidence = format!("{name} = …");
                self.classify_and_push(name, line, col, evidence);
            }
            AssignTargetExpression::Name(_) => {}
            // `d[k] = …` subscript write — root is `d`.
            AssignTargetExpression::Subscript(sub) => {
                if let Some(root) = root_name_of_expr(&sub.value) {
                    let (line, col) = name_line_col_expr(&sub.value, self.span);
                    let evidence = format!("{root}[…] = …");
                    self.classify_and_push(root, line, col, evidence);
                }
            }
            // Starred / Tuple / List destructuring — skip (no single root).
            _ => {}
        }
    }
}

impl MutSink<'_> {
    /// Classify a write by root name and push an `(Effect, contained)` pair.
    fn classify_and_push(&mut self, root: String, line: usize, col: usize, evidence: String) {
        // Root-`self` method/subscript/aug writes (`self.items.append(…)`,
        // `self[i] = v`, `self += …`) mutate already-existing instance state — they
        // are escaping `ThisMutation`, contained=false, *even in `__init__`*. The
        // contained build-then-expose case is only the *direct* `self.attr = …`
        // assignment, which `on_assign_target` handles before reaching here.
        // Preserve the actual write-site evidence passed in.
        if root == "self" {
            self.push(
                EffectKind::ThisMutation,
                Tier::Heuristic,
                line,
                col,
                evidence,
                false,
            );
            return;
        }

        // Resolve the root name via the scope tree, starting at this unit's own
        // scope. Each arm below reproduces one case of the flat cascade this
        // replaces (F5 imports, F2-analog module_bindings, F1 captured-fallback —
        // see the origin→effect table in the spec 078 Task 5 brief), byte-identical.
        match self.scope.resolve(self.unit_scope, &root).0 {
            // Global declaration: `global x` → `GlobalMutation`. Include the write
            // expression in evidence so the report is traceable to the write site.
            BindingOrigin::Global => {
                self.push(
                    EffectKind::GlobalMutation,
                    Tier::Exact,
                    line,
                    col,
                    format!("global {root} ({evidence})"),
                    false,
                );
            }

            // Nonlocal declaration: `nonlocal x` → `ThisMutation` (escaping outer
            // scope). Include the write expression for the same reason.
            BindingOrigin::EnclosingCaptured => {
                self.push(
                    EffectKind::ThisMutation,
                    Tier::Exact,
                    line,
                    col,
                    format!("nonlocal {root} ({evidence})"),
                    false,
                );
            }

            // Parameter mutation: root is a param name (but NOT `self`, handled above).
            BindingOrigin::Param => {
                self.push(
                    EffectKind::ParamMutation,
                    Tier::Heuristic,
                    line,
                    col,
                    evidence,
                    false,
                );
            }

            // Local binding mutation: root is locally assigned (and not global/nonlocal).
            BindingOrigin::Local => {
                self.push(
                    EffectKind::LocalMutation,
                    Tier::Exact,
                    line,
                    col,
                    evidence,
                    true,
                );
            }

            // F5: root resolves through the import table → module-level state (the
            // imported module/name) escaping the function. global.mutation (class 6,
            // Heuristic), contained=false. A same-named LOCAL/param/global/nonlocal
            // already won above (resolve's nearest-binding precedence).
            BindingOrigin::ModuleImport | BindingOrigin::FromImport => {
                self.push(
                    EffectKind::GlobalMutation,
                    Tier::Heuristic,
                    line,
                    col,
                    format!("{evidence} (imported `{root}`)"),
                    false,
                );
            }

            // F2 analog (Python #29): root is a MODULE top-level binding (a
            // module-level name / def / class) whose contents are mutated
            // (subscript/attr/method) — module-shared state used for cross-function /
            // cross-module communication → global.mutation (class 6, Heuristic). A
            // bare rebind without `global` is a LOCAL (Python semantics) and already
            // won above; an explicit `global x` rebind already hit the Global arm.
            // So this catches exactly the content-mutation-of-module-container case.
            BindingOrigin::ModuleBinding => {
                self.push(
                    EffectKind::GlobalMutation,
                    Tier::Heuristic,
                    line,
                    col,
                    format!("{evidence} (module-level `{root}`)"),
                    false,
                );
            }

            // F1: the root resolves to NONE of self/global/nonlocal/param/local/import/
            // module-binding — a captured outer (enclosing-function) binding we cannot
            // bound syntactically → hidden.mutation (class 3, hidden:true,
            // contained:false), subreason "captured-binding". Mirrors the TS `captured`
            // hidden case (Milestone A left it un-emitted).
            BindingOrigin::Unresolved => {
                self.push_hidden(line, col, evidence, "captured-binding");
            }

            // Never constructed by the Python resolver (host-global ambient IO is
            // a TS-only concern, see `scope.rs`'s `BindingOrigin::HostGlobalIo`
            // doc) — panics loudly rather than silently folding into Unresolved,
            // so a resolver change that starts emitting it here is caught fast.
            BindingOrigin::HostGlobalIo => {
                unreachable!("Python resolver never emits HostGlobalIo")
            }
        }
    }

    /// Push a `HiddenMutation` (`hidden:true` + a `subreason`), always escaping
    /// (`contained:false`). Used for writes whose root is an opaque captured /
    /// unresolved binding — the fallback after all named-binding cases are handled.
    fn push_hidden(&mut self, line: usize, col: usize, evidence: String, subreason: &str) {
        let kind = EffectKind::HiddenMutation;
        let tier = Tier::Heuristic;
        let class = kind.base_class();
        self.effects.push((
            Effect {
                kind,
                class,
                discounted_to: None,
                weight: weight_for_class(class),
                line,
                col,
                tier,
                hidden: true,
                contained: false,
                evidence,
                discount: None,
                subreason: Some(subreason.to_owned()),
                confidence: detection_confidence(tier, false, false),
            },
            false,
        ));
    }

    fn push(
        &mut self,
        kind: EffectKind,
        tier: Tier,
        line: usize,
        col: usize,
        evidence: String,
        contained: bool,
    ) {
        let class = kind.base_class();
        self.effects.push((
            Effect {
                kind,
                class,
                discounted_to: None,
                weight: weight_for_class(class),
                line,
                col,
                tier,
                hidden: false,
                contained: false,
                evidence,
                discount: None,
                subreason: None,
                confidence: detection_confidence(tier, false, false),
            },
            contained,
        ));
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Return the root `Name.value` of an expression chain (`a.b.c` → `"a"`,
/// `a[k]` → `"a"`, `Name("x")` → `"x"`).
fn root_name_of_expr(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Name(n) => Some(n.value.to_owned()),
        Expression::Attribute(a) => root_name_of_expr(&a.value),
        Expression::Subscript(s) => root_name_of_expr(&s.value),
        Expression::Call(c) => root_name_of_expr(&c.func),
        _ => None,
    }
}

/// True for method names that mutate their receiver.
fn is_mutating_method(name: &str) -> bool {
    matches!(
        name,
        "append"
            | "extend"
            | "insert"
            | "remove"
            | "pop"
            | "clear"
            | "sort"
            | "reverse"
            | "update"
            | "add"
            | "discard"
            | "setdefault"
    )
}

/// 1-based `(line, col)` of the leftmost `Name` in an expression.
fn name_line_col_expr(expr: &Expression, span: &SpanIndex) -> (usize, usize) {
    leftmost_name(expr)
        .map(|n| name_line_col(n, span))
        .unwrap_or((0, 0))
}

/// The leftmost `Name` in an expression chain.
fn leftmost_name<'a>(expr: &'a Expression<'a>) -> Option<&'a Name<'a>> {
    match expr {
        Expression::Name(n) => Some(n),
        Expression::Attribute(a) => leftmost_name(&a.value),
        Expression::Subscript(s) => leftmost_name(&s.value),
        Expression::Call(c) => leftmost_name(&c.func),
        _ => None,
    }
}

/// 1-based `(line, col)` of a `Name` node (pointer-arithmetic on its borrowed &str).
fn name_line_col(name: &Name, span: &SpanIndex) -> (usize, usize) {
    span.line_col(anchor_of_subslice(span.src(), name.value))
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use fxrank_core::effect::EffectKind::{self, *};
    use std::collections::HashMap;

    /// Parse `tests/fixtures/<name>.py`, collect units, run `detect` per unit, and
    /// return `symbol → Vec<(EffectKind, bool)>` (kind + contained flag).
    fn mutation_effects(name: &str) -> HashMap<String, Vec<(EffectKind, bool)>> {
        let src = std::fs::read_to_string(format!("tests/fixtures/{name}.py")).unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let scope = crate::scope::ScopeTree::build(&module, &imports, &module_bindings);
        let span = crate::source::SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let mut out: HashMap<String, Vec<(EffectKind, bool)>> = HashMap::new();
        for unit in &units {
            let pairs = detect(unit, &imports, &module_bindings, &scope, &span);
            out.insert(
                unit.symbol.clone(),
                pairs.iter().map(|(e, c)| (e.kind, *c)).collect(),
            );
        }
        out
    }

    /// Like `mutation_effects` but retains each effect's evidence string.
    fn mutation_evidence(name: &str) -> HashMap<String, Vec<(EffectKind, bool, String)>> {
        let src = std::fs::read_to_string(format!("tests/fixtures/{name}.py")).unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let scope = crate::scope::ScopeTree::build(&module, &imports, &module_bindings);
        let span = crate::source::SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let mut out: HashMap<String, Vec<(EffectKind, bool, String)>> = HashMap::new();
        for unit in &units {
            let pairs = detect(unit, &imports, &module_bindings, &scope, &span);
            out.insert(
                unit.symbol.clone(),
                pairs
                    .iter()
                    .map(|(e, c)| (e.kind, *c, e.evidence.clone()))
                    .collect(),
            );
        }
        out
    }

    #[test]
    fn classifies_mutation_by_escape() {
        let m = mutation_effects("mutation");

        // `global _counter` then `_counter += 1` → GlobalMutation, not contained.
        assert!(
            m["uses_global"].contains(&(GlobalMutation, false)),
            "uses_global should have GlobalMutation(contained=false), got: {:?}",
            m["uses_global"]
        );

        // `self.n += 1` in a non-__init__ method → ThisMutation, not contained.
        assert!(
            m["bump"].contains(&(ThisMutation, false)),
            "bump should have ThisMutation(contained=false), got: {:?}",
            m["bump"]
        );

        // `lst.append(1)` where lst is a param → ParamMutation, not contained.
        assert!(
            m["mutates_param"].contains(&(ParamMutation, false)),
            "mutates_param should have ParamMutation(contained=false), got: {:?}",
            m["mutates_param"]
        );

        // `acc.append(1)` where acc is a local → LocalMutation, contained.
        assert!(
            m["builds_local"].contains(&(LocalMutation, true)),
            "builds_local should have LocalMutation(contained=true), got: {:?}",
            m["builds_local"]
        );

        // `self.n = n` inside `__init__` → LocalMutation, contained (constructor init).
        assert!(
            m["__init__"].contains(&(LocalMutation, true)),
            "__init__ should have LocalMutation(contained=true), got: {:?}",
            m["__init__"]
        );
    }

    /// FIX 1: a plain `=` to a name declared `global`/`nonlocal` is an escaping
    /// rebind and MUST emit — only a TRUE local binding (`y = 1`, no declaration)
    /// is a no-emit binding. Pre-fix the classifier only ran for bare names when
    /// `is_aug`, so `global g; g = 1` and `nonlocal x; x = 1` were silently dropped.
    #[test]
    fn plain_assign_to_global_nonlocal_names_escapes() {
        let m = mutation_effects("mutation");

        // `global _counter; _counter = 1` → GlobalMutation, not contained.
        assert!(
            m["plain_global_rebind"].contains(&(GlobalMutation, false)),
            "plain `=` to a global name must emit GlobalMutation(false), got: {:?}",
            m["plain_global_rebind"]
        );

        // `nonlocal x; x = 1` (inside the nested def) → ThisMutation, not contained.
        assert!(
            m["plain_nonlocal_rebind"].contains(&(ThisMutation, false)),
            "plain `=` to a nonlocal name must emit ThisMutation(false), got: {:?}",
            m["plain_nonlocal_rebind"]
        );

        // `y = 1` with no global/nonlocal declaration → NO mutation effect (binding).
        assert!(
            m["plain_local_binding"].is_empty(),
            "plain `=` to a true local must emit NO mutation, got: {:?}",
            m["plain_local_binding"]
        );
    }

    /// FIX 1: root-`self` method/subscript mutations are escaping instance-state
    /// (`ThisMutation`, contained=false) even inside `__init__` — they mutate
    /// already-existing instance state, NOT the contained build-then-expose
    /// `self.attr = …` case. The direct `self.attr = …`-in-init case stays
    /// `LocalMutation` contained.
    #[test]
    fn self_method_and_subscript_mutations_escape_even_in_init() {
        let m = mutation_effects("mutation");

        // `self.items = []` in __init__ → LocalMutation contained (unchanged).
        assert!(
            m["__init__"].contains(&(LocalMutation, true)),
            "direct `self.attr = …` in __init__ stays LocalMutation(true), got: {:?}",
            m["__init__"]
        );

        // `self.items.append(1)` in __init__ → ThisMutation, contained=false
        // (escaping — was wrongly contained before the fix).
        assert!(
            m["__init__"].contains(&(ThisMutation, false)),
            "`self.items.append(…)` in __init__ must be ThisMutation(false), got: {:?}",
            m["__init__"]
        );

        // `self[i] = v` in a non-init method → ThisMutation, contained=false.
        assert!(
            m["store"].contains(&(ThisMutation, false)),
            "`self[i] = v` must be ThisMutation(false), got: {:?}",
            m["store"]
        );
    }

    /// PREREQ 1: MutSink can emit a HiddenMutation (hidden:true + subreason) —
    /// the channel Python has never used. `push` stays the honest hidden:false path.
    #[test]
    fn push_hidden_emits_hidden_mutation_with_subreason() {
        let src = "x\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = HashSet::new();
        let scope = crate::scope::ScopeTree::build(&module, &imports, &module_bindings);
        let unit_scope = scope.module_scope();
        let span = crate::source::SpanIndex::new(src);
        let mut sink = MutSink {
            is_init: false,
            scope: &scope,
            unit_scope,
            span: &span,
            effects: Vec::new(),
        };
        sink.push_hidden(1, 1, "outer_acc.append(…)".to_string(), "captured-binding");

        assert_eq!(sink.effects.len(), 1);
        let (effect, contained) = &sink.effects[0];
        assert_eq!(effect.kind, EffectKind::HiddenMutation);
        assert_eq!(effect.class, 3);
        assert!(effect.hidden, "push_hidden must set hidden:true");
        assert_eq!(effect.subreason.as_deref(), Some("captured-binding"));
        assert!(!contained, "hidden writes escape — contained=false");
    }

    /// PREREQ 2: mutation::detect accepts the ImportTable so F5 + F1 can resolve roots.
    #[test]
    fn detect_accepts_imports_param() {
        let src = "def f(lst):\n    lst.append(1)\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let scope = crate::scope::ScopeTree::build(&module, &imports, &module_bindings);
        let span = crate::source::SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, src, &span, &anchors);
        let f = units.iter().find(|u| u.symbol == "f").unwrap();
        let pairs = detect(f, &imports, &module_bindings, &scope, &span);
        assert!(
            pairs.iter().any(|(e, _)| e.kind == ParamMutation),
            "lst.append where lst is a param → ParamMutation, got: {:?}",
            pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );
    }

    /// Task 4 (#78): `detect` accepts a `&ScopeTree` and still finds the same
    /// param-mutation effect — the compile-level proof that threading the scope
    /// tree through is pure wiring (no behavior change).
    #[test]
    fn detect_accepts_scope_tree() {
        let src = "def f(lst):\n    lst.append(1)\n";
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let mb = crate::imports::module_bindings(&module);
        let tree = crate::scope::ScopeTree::build(&module, &imports, &mb);
        let span = crate::source::SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).unwrap();
        let (units, _) = crate::functions::collect(&module, src, &span, &anchors);
        let f = units.iter().find(|u| u.symbol == "f").unwrap();
        let pairs = detect(f, &imports, &mb, &tree, &span);
        assert!(pairs.iter().any(|(e, _)| e.kind == ParamMutation));
    }

    /// F5: a write whose root resolves through the ImportTable is module-level state
    /// escaping the function → global.mutation/6, contained=false. Inserted AFTER the
    /// `locals` arm (a same-named local shadows the import) and BEFORE the F1 fallback.
    #[test]
    fn import_rooted_write_is_global_mutation() {
        let m = mutation_effects("mutation");
        assert!(
            m["mutates_imported_module"].contains(&(GlobalMutation, false)),
            "config.settings.append(…) where `config` is imported must be GlobalMutation(false), got: {:?}",
            m["mutates_imported_module"]
        );
    }

    /// refs #55: attribute-tagging a locally-defined nested `def` (the Django
    /// `fn.alters_data = True` pattern) must be LocalMutation(contained), not a
    /// spurious `hidden.mutation` "captured-binding" — the def name IS a local binding.
    #[test]
    fn nested_def_name_is_local_binding_not_captured() {
        let m = mutation_effects("mutation");
        let effs = &m["tags_nested_def"];
        assert!(
            effs.contains(&(LocalMutation, true)),
            "helper.alters_data = True should be LocalMutation(contained=true), got: {effs:?}"
        );
        assert!(
            !effs.iter().any(|(k, _)| *k == HiddenMutation),
            "must not be a captured-binding hidden.mutation, got: {effs:?}"
        );
    }

    /// refs #55 (Opus review): the `seed_defs` flag must reach nested defs inside
    /// CONTROL FLOW, not just at the top of a function body. A `def` nested in an
    /// `if` is still a function-local, so tagging it is LocalMutation — this guards
    /// that the `If`/orelse recursion arm forwards `seed_defs`.
    #[test]
    fn nested_def_inside_control_flow_is_local() {
        let m = mutation_effects("mutation");
        let effs = &m["tags_nested_def_in_branch"];
        assert!(
            effs.contains(&(LocalMutation, true)),
            "helper_in_if.alters_data (nested def inside `if`) should be LocalMutation, got: {effs:?}"
        );
        assert!(
            !effs.iter().any(|(k, _)| *k == HiddenMutation),
            "must not be captured-binding hidden.mutation, got: {effs:?}"
        );
    }

    /// refs #55 (regression guard): the nested-def-name seeding must NOT reach the
    /// synthetic `<module>` unit. At module scope a def/class name is a module-level
    /// binding, so `module_level_taggable.alters_data = True` stays GlobalMutation/6
    /// (via module_bindings), not LocalMutation. The `<module>` unit is built by
    /// `module_init_unit`, not `functions::collect`, so this test drives it directly.
    #[test]
    fn module_level_def_attr_write_stays_global_not_local() {
        let src = std::fs::read_to_string("tests/fixtures/mutation.py").unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let scope = crate::scope::ScopeTree::build(&module, &imports, &module_bindings);
        let span = crate::source::SpanIndex::new(&src);
        let unit =
            crate::functions::module_init_unit(&module).expect("module has executable top-level");
        let effs: Vec<(EffectKind, bool)> =
            detect(&unit, &imports, &module_bindings, &scope, &span)
                .iter()
                .map(|(e, c)| (e.kind, *c))
                .collect();
        assert!(
            effs.contains(&(GlobalMutation, false)),
            "module-level `def.attr = …` must be GlobalMutation(false), got: {effs:?}"
        );
        assert!(
            !effs.contains(&(LocalMutation, true)),
            "module-level def-attr write must NOT be downgraded to LocalMutation, got: {effs:?}"
        );
    }

    /// F1/F3: a write whose root resolves to NONE of {self, globals, nonlocals, params,
    /// locals, import} is a captured outer/opaque binding. Pre-fix the cascade fell off
    /// silently; now it emits hidden.mutation/3, hidden=true, contained=false, subreason
    /// "captured-binding" (the Python analog of the TS `captured` hidden case).
    #[test]
    fn captured_binding_subreason_is_set() {
        let src = std::fs::read_to_string("tests/fixtures/mutation.py").unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let scope = crate::scope::ScopeTree::build(&module, &imports, &module_bindings);
        let span = crate::source::SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let inner = units.iter().find(|u| u.symbol == "inner").unwrap();
        let pairs = detect(inner, &imports, &module_bindings, &scope, &span);
        let hidden = pairs
            .iter()
            .find(|(e, _)| e.kind == HiddenMutation)
            .map(|(e, _)| e)
            .expect("inner must emit a HiddenMutation");
        assert_eq!(hidden.class, 3);
        assert!(
            hidden.hidden,
            "captured-binding HiddenMutation must be hidden:true"
        );
        assert_eq!(hidden.subreason.as_deref(), Some("captured-binding"));
        assert!(
            pairs.iter().any(|(e, c)| e.kind == HiddenMutation && !*c),
            "captured-binding write escapes — contained=false"
        );
    }

    /// FIX 2: mutating-method evidence renders the full receiver expression
    /// (the attribute chain) — `self.items.append(…)`, not the misleading
    /// `self.append(…)` built from just the root name.
    #[test]
    fn mutating_method_evidence_uses_full_receiver() {
        let m = mutation_evidence("mutation");
        let init = &m["__init__"];
        let append = init
            .iter()
            .find(|(k, _, _)| *k == ThisMutation)
            .unwrap_or_else(|| panic!("expected a ThisMutation in __init__, got: {init:?}"));
        assert!(
            append.2.contains("self.items"),
            "evidence must name the full receiver `self.items`, got: {:?}",
            append.2
        );
    }

    fn detect_src(src: &str, fn_name: &str) -> Vec<(Effect, bool)> {
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = crate::imports::Imports::build(&module);
        let module_bindings = crate::imports::module_bindings(&module);
        let scope = crate::scope::ScopeTree::build(&module, &imports, &module_bindings);
        let span = crate::source::SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, src, &span, &anchors);
        let unit = units
            .iter()
            .find(|u| u.symbol == fn_name)
            .expect("unit not found");
        detect(unit, &imports, &module_bindings, &scope, &span)
    }

    #[test]
    fn module_level_content_mutation_is_global() {
        // A module-level dict mutated by content (no `global` decl) is module-shared
        // state -> global.mutation (class 6), not the hidden captured-binding fallback.
        let src = "_cache = {}\ndef f():\n    _cache['k'] = 1\n";
        let pairs = detect_src(src, "f");
        assert!(
            pairs.iter().any(|(e, c)| e.kind == GlobalMutation && !*c),
            "module-level `_cache['k']=1` (no `global`) must be GlobalMutation(false), got: {:?}",
            pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );
        assert!(
            !pairs.iter().any(|(e, _)| e.kind == HiddenMutation),
            "module-level content mutation must not be hidden.mutation"
        );
    }

    /// #50: a mutating-method CALL whose receiver is a **bare imported name**
    /// (`os.remove()`, a namespace import `V.add()`) is a call on a module /
    /// namespace, not a mutation of it. It must NOT escalate to global.mutation
    /// (the effect, if any, is the callee's — already scored by the call detector).
    #[test]
    fn method_call_on_bare_imported_name_is_not_mutation() {
        // `os.remove(...)` — receiver is the bare imported module `os`.
        let pairs = detect_src("import os\ndef f():\n    os.remove(\"x\")\n", "f");
        assert!(
            !pairs.iter().any(|(e, _)| e.kind == GlobalMutation),
            "os.remove() must not emit GlobalMutation (call on an imported module), got: {:?}",
            pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );

        // Namespace import used as a pure-math helper: `import vecmath as V; V.add(a, b)`.
        let pairs2 = detect_src(
            "import vecmath as V\ndef g(a, b):\n    return V.add(a, b)\n",
            "g",
        );
        assert!(
            !pairs2.iter().any(|(e, _)| e.kind == GlobalMutation),
            "V.add() on a namespace import must not emit GlobalMutation, got: {:?}",
            pairs2.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );

        // BOUNDARY (must stay a true positive): a method call on an *attribute*
        // reached through an imported module mutates module-shared state.
        let pairs3 = detect_src(
            "import config\ndef h():\n    config.settings.append(1)\n",
            "h",
        );
        assert!(
            pairs3.iter().any(|(e, c)| e.kind == GlobalMutation && !*c),
            "config.settings.append() must stay GlobalMutation (module-shared state), got: {:?}",
            pairs3.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );

        // BOUNDARY (must stay a true positive): a bare *module-level binding*
        // (not an import) mutated by content is module-shared state.
        let pairs4 = detect_src("SHARED = []\ndef k():\n    SHARED.append(1)\n", "k");
        assert!(
            pairs4.iter().any(|(e, c)| e.kind == GlobalMutation && !*c),
            "SHARED.append() on a module-level binding must stay GlobalMutation, got: {:?}",
            pairs4.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );
    }

    /// #50 regression (local review): the module-import suppression must respect
    /// Python binding precedence (a param/local shadowing the module name wins) and
    /// must not swallow a `from`-imported name (which may bind a mutable object).
    #[test]
    fn method_call_suppression_respects_shadowing_and_from_imports() {
        // A param shadows the imported module name → the param mutation must survive
        // (not be dropped by the import gate).
        let shadowed = detect_src("import os\ndef f(os):\n    os.append(1)\n", "f");
        assert!(
            shadowed.iter().any(|(e, _)| e.kind == ParamMutation),
            "a param `os` shadowing `import os` must stay ParamMutation, got: {:?}",
            shadowed.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );

        // A LOCAL binding shadows the imported module name → local.mutation, not
        // suppressed (the pre-scan marks `os` local for the whole function).
        let local_shadow = detect_src("import os\ndef f():\n    os = []\n    os.append(1)\n", "f");
        assert!(
            local_shadow.iter().any(|(e, _)| e.kind == LocalMutation),
            "a local `os` shadowing `import os` must stay LocalMutation, got: {:?}",
            local_shadow.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );

        // A `from`-imported name may be a mutable object → keep global.mutation
        // (only plain `import module` receivers are suppressed).
        let from_import = detect_src(
            "from cfg import CACHE\ndef f():\n    CACHE.append(1)\n",
            "f",
        );
        assert!(
            from_import
                .iter()
                .any(|(e, c)| e.kind == GlobalMutation && !*c),
            "from-imported `CACHE.append()` must stay GlobalMutation, got: {:?}",
            from_import.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );
    }

    /// #78: g's function-local `import os as CACHE` no longer suppresses f's
    /// CACHE mutation. Scope-aware import resolution (this task) closes the
    /// cross-function collision the flat/file-wide `Imports` table used to
    /// cause: module scope now holds only TOP-LEVEL imports, and a
    /// function-body import binds only that function's own scope — so `g`'s
    /// local `import os as CACHE` never reaches module scope, and `f`'s
    /// `CACHE` resolves through module scope to the top-level
    /// `from cfg import CACHE` (`FromImport`, not `on_call`-suppressed) —
    /// `GlobalMutation`. Was pinned as an accepted limitation before this fix
    /// (see git history); this is an intentional, deliberate signal recovery.
    #[test]
    fn cross_function_import_does_not_leak_across_functions() {
        let src = "from cfg import CACHE\n\
                   def f():\n    CACHE.append(1)\n\
                   def g():\n    import os as CACHE\n";
        let pairs = detect_src(src, "f");
        assert!(
            pairs
                .iter()
                .any(|(e, c)| e.kind == EffectKind::GlobalMutation && !*c),
            "f's CACHE (module-level from-import) must be GlobalMutation, got {:?}",
            pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );
    }

    #[test]
    fn local_shadowing_module_binding_is_local() {
        // A bare local rebind shadows the module name (Python creates a local) ->
        // local.mutation; the shadow wins because locals are checked before the
        // module-binding arm.
        let src = "_cache = {}\ndef f():\n    _cache = {}\n    _cache['k'] = 1\n";
        let pairs = detect_src(src, "f");
        assert!(
            pairs.iter().any(|(e, c)| e.kind == LocalMutation && *c),
            "shadowing local `_cache` must be LocalMutation(true), got: {:?}",
            pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );
        assert!(
            !pairs.iter().any(|(e, _)| e.kind == GlobalMutation),
            "shadowing local must not escalate to GlobalMutation"
        );
    }

    /// #78 final-review MUST-FIX: a function-local import reassigned to a local
    /// in the SAME function must resolve as `Local`, not `ModuleImport`. The
    /// flat cascade checks `locals` BEFORE `imports` (F5), so `build_funcdef`'s
    /// insertion order must make locals win — imports are the lowest-precedence
    /// function-scope binding (inserted before locals/params/nonlocals/globals).
    #[test]
    fn function_local_import_reassigned_is_local_not_global() {
        // Flat cascade checks locals before imports (F5); a name both imported AND
        // locally reassigned in the same function is a Local, not a module import.
        let src = "def f():\n    import os\n    os = {}\n    os['k'] = 1\n";
        let pairs = detect_src(src, "f");
        assert!(
            pairs
                .iter()
                .any(|(e, c)| e.kind == EffectKind::LocalMutation && *c),
            "os['k']=1 must be LocalMutation/contained, got {:?}",
            pairs.iter().map(|(e, c)| (e.kind, *c)).collect::<Vec<_>>()
        );
        assert!(
            !pairs
                .iter()
                .any(|(e, _)| e.kind == EffectKind::GlobalMutation)
        );
    }

    /// Prescan fix (for/with-as/except-as): a name introduced by a `for` loop target
    /// in a function body is a Python local for the WHOLE function (PEP 3104). A
    /// module binding of the same name must be shadowed by the for-target local.
    #[test]
    fn for_target_shadow_stays_local() {
        // `_cache` is a module-level binding; `for _cache in []` shadows it locally.
        // The write `_cache['k'] = 1` inside the loop must be LocalMutation (contained),
        // not GlobalMutation. Before the prescan fix this was GlobalMutation because
        // the prescan did not collect for-loop targets.
        let src = "_cache = {}\ndef f():\n    for _cache in []:\n        _cache['k'] = 1\n";
        let pairs = detect_src(src, "f");
        let writes: Vec<_> = pairs
            .iter()
            .filter(|(e, _)| {
                matches!(
                    e.kind,
                    LocalMutation | GlobalMutation | HiddenMutation | ThisMutation
                )
            })
            .collect();
        assert!(
            writes.iter().any(|(e, c)| e.kind == LocalMutation && *c),
            "expected LocalMutation(contained=true) for for-target shadow, got: {:?}",
            writes.iter().map(|(e, c)| (e.kind, *c)).collect::<Vec<_>>()
        );
        assert!(
            !writes.iter().any(|(e, _)| e.kind == GlobalMutation),
            "expected NO GlobalMutation for for-target shadow, got: {:?}",
            writes.iter().map(|(e, c)| (e.kind, *c)).collect::<Vec<_>>()
        );
    }

    /// Prescan fix: a name introduced by DESTRUCTURING assignment in a function body
    /// is a Python local (Python scoping: any binding-assignment makes the name local
    /// for the whole function). A module binding of the same name must be shadowed.
    #[test]
    fn local_destructured_shadow_stays_local() {
        // `_cache` is a module-level binding. `f` rebinds it via tuple destructuring
        // `(_cache,) = ({},)` — Python considers `_cache` local to `f` for the whole
        // function. The subsequent `_cache['k'] = 1` must be LocalMutation (contained),
        // not GlobalMutation. Before the prescan fix this was GlobalMutation because
        // the prescan only collected bare-Name targets.
        let src = "_cache = {}\ndef f():\n    (_cache,) = ({},)\n    _cache['k'] = 1\n";
        let pairs = detect_src(src, "f");
        assert!(
            pairs.iter().any(|(e, c)| e.kind == LocalMutation && *c),
            "destructuring-local `_cache` must be LocalMutation(true), got: {:?}",
            pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );
        assert!(
            !pairs.iter().any(|(e, _)| e.kind == GlobalMutation),
            "destructured local must not escalate to GlobalMutation"
        );
    }

    /// Characterization pin (spec 078 Task 5): the resolver-based path must
    /// agree with today's flat-set cascade for one representative write per
    /// `BindingOrigin`. Passes on the pre-reroute flat code (characterizing
    /// existing behavior) and must keep passing after `classify_and_push`/
    /// `on_assign_target` are rerouted onto `ScopeTree::resolve`.
    #[test]
    fn classify_origins_match_flat_behavior() {
        // one representative write per origin; assert exact (kind, contained)
        let cases: &[(&str, &str, EffectKind, bool)] = &[
            (
                "import os\ndef f():\n    global g\n    g = 1\n",
                "f",
                EffectKind::GlobalMutation,
                false,
            ), // Global (rebind under `global`, via on_assign_target)
            (
                "def outer():\n    n = []\n    def f():\n        nonlocal n\n        n.append(1)\n",
                "f",
                EffectKind::ThisMutation,
                false,
            ), // EnclosingCaptured (nonlocal)
            (
                "def f(p):\n    p.append(1)\n",
                "f",
                EffectKind::ParamMutation,
                false,
            ), // Param
            (
                "def f():\n    x = []\n    x.append(1)\n",
                "f",
                EffectKind::LocalMutation,
                true,
            ), // Local
            (
                "import config\ndef f():\n    config.settings.append(1)\n",
                "f",
                EffectKind::GlobalMutation,
                false,
            ), // ModuleImport (attribute receiver, not on_call-suppressed)
            (
                "from cfg import CACHE\ndef f():\n    CACHE.append(1)\n",
                "f",
                EffectKind::GlobalMutation,
                false,
            ), // FromImport
            (
                "STORE = {}\ndef f():\n    STORE['k'] = 1\n",
                "f",
                EffectKind::GlobalMutation,
                false,
            ), // ModuleBinding
            (
                "def f():\n    captured.append(1)\n",
                "f",
                EffectKind::HiddenMutation,
                false,
            ), // Unresolved -> captured-binding
        ];
        for (src, sym, kind, contained) in cases {
            let pairs = detect_src(src, sym);
            assert!(
                pairs
                    .iter()
                    .any(|(e, c)| e.kind == *kind && *c == *contained),
                "src {src:?}: expected {kind:?} contained={contained}, got {:?}",
                pairs.iter().map(|(e, c)| (e.kind, *c)).collect::<Vec<_>>()
            );
        }
    }

    /// #55/Task 8: a `match`-case capture (`case [name, *args]:`) binds `args`
    /// as a function-local — mutating it must be LocalMutation(contained), not
    /// a captured-binding hidden.mutation (the function-scope builder's
    /// prescan previously never walked `MatchPattern`s at all).
    #[test]
    fn match_capture_is_local_not_captured() {
        let src = "def f(cmd):\n    match cmd:\n        case [name, *args]:\n            args.append(1)\n";
        let pairs = detect_src(src, "f");
        assert!(
            pairs
                .iter()
                .any(|(e, c)| e.kind == EffectKind::LocalMutation && *c)
        );
        assert!(
            !pairs
                .iter()
                .any(|(e, _)| e.kind == EffectKind::HiddenMutation)
        );
    }

    /// #55/Task 8: a walrus `:=` target binds a function-local wherever it
    /// appears (here, inside an `if` test) — mutating it must be
    /// LocalMutation(contained), not captured-binding.
    #[test]
    fn walrus_target_is_local_not_captured() {
        let src = "def f(xs):\n    if (buf := list(xs)):\n        buf.append(1)\n";
        let pairs = detect_src(src, "f");
        assert!(
            pairs
                .iter()
                .any(|(e, c)| e.kind == EffectKind::LocalMutation && *c)
        );
        assert!(
            !pairs
                .iter()
                .any(|(e, _)| e.kind == EffectKind::HiddenMutation)
        );
    }

    /// #55/Task 8: a comprehension `for` target binds a function-local (this
    /// task collects it into the FUNCTION scope, not a comprehension-own scope
    /// — a deliberate simplification, see `scope.rs`'s `prescan_comp_for` doc
    /// comment) — mutating it inside the comprehension must not fall to
    /// captured-binding.
    #[test]
    fn comprehension_target_mutation_is_local_not_captured() {
        let src = "def f(rows):\n    return [r.sort() or r for r in rows]\n";
        let pairs = detect_src(src, "f");
        assert!(
            !pairs
                .iter()
                .any(|(e, _)| e.kind == EffectKind::HiddenMutation),
            "comprehension target `r` must resolve locally, got {:?}",
            pairs.iter().map(|(e, _)| e.kind).collect::<Vec<_>>()
        );
    }
}
