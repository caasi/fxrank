//! Dynamic-code risk detection for the Python frontend.
//!
//! [`detect`] walks a function's own body for dangerous Python patterns and
//! emits a [`RiskFeature`] per signal. This is the libcst analog of
//! `fxrank-lang-ts`'s `detect/risk.rs`.
//!
//! # Dedup note — `type.escape` is NOT detected here
//!
//! The `Any`-family `type.escape` risk is OWNED by the coverage gate in
//! `analyze_unit` (Task 9). This module detects `type.escape` only for the
//! non-null-assertion signal in the TS frontend — there is no direct Python
//! analog. Do not emit `type.escape` here.
//!
//! # Signals detected
//!
//! | signal                                      | `RiskKind`    | tier      |
//! |---------------------------------------------|---------------|-----------|
//! | `eval(…)`                                   | `DynamicCode` | exact     |
//! | `exec(…)`                                   | `DynamicCode` | exact     |
//! | `compile(…)`                                | `DynamicCode` | exact     |
//! | `__import__(…)`                             | `DynamicCode` | exact     |
//! | `pickle.load(…)` / `pickle.loads(…)`        | `DynamicCode` | path      |
//! | `yaml.load(…)` (not `yaml.safe_load`)       | `DynamicCode` | path      |
//! | `importlib.import_module(…)`                | `DynamicCode` | path      |
//! | `setattr(<imported module/class>, …)`       | `DynamicCode` | heuristic |
//! | `subprocess(…, shell=True)`                 | `DynamicCode` | path      |
//!
//! # Shell=True companion note
//!
//! Task 6 (`calls::detect`) already emits a `process.control` EFFECT for any
//! `subprocess.*` call. This module adds the RISK (dynamic.code class 7) only
//! when `shell=True` is a keyword argument — signalling shell-injection surface.
//! Do NOT emit a second `process.control` effect here.
//!
//! # Setattr guard
//!
//! Only `setattr` whose first argument is a name that resolves to an imported
//! module or imported class-like (i.e. present in the import table) is flagged
//! as monkey-patching. Ordinary `setattr(obj, "x", v)` on a non-imported
//! name is NOT flagged.

use fxrank_core::effect::{RiskFeature, RiskKind, Tier};
use fxrank_core::score::weight_for_class;
use libcst_native::{Arg, Assert, AssignTargetExpression, Call, Expression, Raise};

use super::{
    EffectSink,
    expr::{leftmost_name, render_expr},
    walk_own_body,
};
use crate::functions::FnUnit;
use crate::imports::Imports;
use crate::source::{SpanIndex, anchor_of_subslice};

/// Detect dynamic-code risk features in `unit`'s own body.
///
/// `path` is the source file path to embed in each emitted [`RiskFeature`].
///
/// Returns a `Vec<RiskFeature>` for each dangerous dynamic-code pattern found.
/// The `type.escape` risk is intentionally NOT detected here — it is owned by
/// the coverage gate in `analyze_unit`.
pub fn detect(unit: &FnUnit, imports: &Imports, span: &SpanIndex, path: &str) -> Vec<RiskFeature> {
    let mut sink = RiskSink {
        imports,
        span,
        path: path.to_owned(),
        features: Vec::new(),
    };
    walk_own_body(unit, &mut sink);
    sink.features
}

struct RiskSink<'a> {
    imports: &'a Imports,
    span: &'a SpanIndex<'a>,
    path: String,
    features: Vec<RiskFeature>,
}

impl RiskSink<'_> {
    fn push(&mut self, kind: RiskKind, tier: Tier, line: usize, col: usize, evidence: String) {
        let class = kind.class();
        self.features.push(RiskFeature {
            kind,
            class,
            weight: weight_for_class(class),
            path: self.path.clone(),
            line,
            col,
            evidence,
            tier,
        });
    }

    /// Resolve a rendered dotted name through the import table: split at the
    /// first dot, resolve the root, re-attach the trailing path.
    fn resolve_dotted(&self, rendered: &str) -> Option<String> {
        let (root, rest) = match rendered.split_once('.') {
            Some((r, rest)) => (r, Some(rest)),
            None => (rendered, None),
        };
        let base = self.imports.resolve(root)?;
        Some(match rest {
            Some(rest) => format!("{base}.{rest}"),
            None => base.to_string(),
        })
    }

    /// Return true if `name` is a name present in the import table (i.e. it
    /// is an imported module or imported class-like). Used for the `setattr`
    /// monkey-patch guard.
    fn is_imported_name(&self, name: &str) -> bool {
        self.imports.resolve(name).is_some()
    }
}

impl EffectSink for RiskSink<'_> {
    fn on_call(&mut self, call: &Call) {
        let Some(rendered) = render_expr(&call.func) else {
            return;
        };

        // ── (line, col) from the leftmost name anchor ──
        let (line, col) = leftmost_name(&call.func)
            .map(|n| {
                self.span
                    .line_col(anchor_of_subslice(self.span.src(), n.value))
            })
            .unwrap_or((0, 0));

        // ── Bare builtin names — eval/exec/compile/__import__ ──────────────
        match rendered.as_str() {
            "eval" => {
                self.push(
                    RiskKind::DynamicCode,
                    Tier::Exact,
                    line,
                    col,
                    "eval(…) — dynamic code execution".into(),
                );
                return;
            }
            "exec" => {
                self.push(
                    RiskKind::DynamicCode,
                    Tier::Exact,
                    line,
                    col,
                    "exec(…) — dynamic code execution".into(),
                );
                return;
            }
            "compile" => {
                self.push(
                    RiskKind::DynamicCode,
                    Tier::Exact,
                    line,
                    col,
                    "compile(…) — dynamic code compilation".into(),
                );
                return;
            }
            "__import__" => {
                self.push(
                    RiskKind::DynamicCode,
                    Tier::Exact,
                    line,
                    col,
                    "__import__(…) — dynamic import".into(),
                );
                return;
            }
            _ => {}
        }

        // ── setattr monkey-patch guard ──────────────────────────────────────
        // Flag `setattr(target, name, value)` only when `target` is an
        // imported name — i.e. module or class re-binding (monkey-patching).
        // Ordinary `setattr(obj, ...)` on non-imported objects is NOT flagged.
        if rendered == "setattr" {
            if let Some(first_arg) = call.args.first() {
                if let Expression::Name(n) = &first_arg.value
                    && self.is_imported_name(n.value)
                {
                    self.push(
                        RiskKind::DynamicCode,
                        Tier::Heuristic,
                        line,
                        col,
                        format!("setattr({}, …) — monkey-patch on imported name", n.value),
                    );
                }
            }
            return;
        }

        // ── Path-resolved through the import table ──────────────────────────
        let resolved = self.resolve_dotted(&rendered);

        // `subprocess.*(…, shell=True)` — dynamic.code RISK only (process.control
        // effect is already emitted by calls::detect).
        if let Some(ref full) = resolved {
            let root = full.split('.').next().unwrap_or(full.as_str());
            if root == "subprocess" && has_shell_true(call) {
                self.push(
                    RiskKind::DynamicCode,
                    Tier::Path,
                    line,
                    col,
                    "subprocess(shell=True) — shell-injection surface".into(),
                );
                return;
            }
        }

        // `pickle.load` / `pickle.loads`
        if let Some(ref full) = resolved {
            if matches!(full.as_str(), "pickle.load" | "pickle.loads") {
                self.push(
                    RiskKind::DynamicCode,
                    Tier::Path,
                    line,
                    col,
                    format!("{full}(…) — unsafe deserialization"),
                );
                return;
            }
        }

        // `yaml.load` (NOT `yaml.safe_load`)
        if let Some(ref full) = resolved {
            if full == "yaml.load" {
                self.push(
                    RiskKind::DynamicCode,
                    Tier::Path,
                    line,
                    col,
                    "yaml.load(…) — unsafe YAML deserialization (use safe_load)".into(),
                );
                return;
            }
        }

        // `importlib.import_module`
        if let Some(ref full) = resolved
            && full == "importlib.import_module"
        {
            self.push(
                RiskKind::DynamicCode,
                Tier::Path,
                line,
                col,
                "importlib.import_module(…) — dynamic import".into(),
            );
        }
    }

    // Risk detection does not classify assert/raise/assignment targets.
    fn on_assert(&mut self, _assert: &Assert) {}
    fn on_raise(&mut self, _raise: &Raise) {}
    fn on_assign_target(&mut self, _target: &AssignTargetExpression, _is_aug: bool) {}
}

// ─── shell=True detection ─────────────────────────────────────────────────────

/// Return `true` when the call contains a `shell=True` keyword argument.
fn has_shell_true(call: &Call) -> bool {
    call.args.iter().any(|arg| is_shell_true_kwarg(arg))
}

/// Return `true` when `arg` is a `shell=True` keyword argument.
fn is_shell_true_kwarg(arg: &Arg) -> bool {
    let Some(kw) = &arg.keyword else { return false };
    if kw.value != "shell" {
        return false;
    }
    matches!(
        &arg.value,
        Expression::Name(n) if n.value == "True"
    )
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use crate::imports::Imports;
    use crate::source::SpanIndex;
    use std::collections::HashMap;

    /// Parse `tests/fixtures/<name>.py`, run `detect` per unit, and return a
    /// `HashMap<symbol, Vec<risk_kind_wire_string>>`.
    fn risk_features(name: &str) -> HashMap<String, Vec<String>> {
        let src = std::fs::read_to_string(format!("tests/fixtures/{name}.py")).unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = Imports::build(&module);
        let span = SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for unit in &units {
            let features = detect(unit, &imports, &span, "");
            out.insert(
                unit.symbol.clone(),
                features.iter().map(|r| r.kind.wire().to_string()).collect(),
            );
        }
        out
    }

    #[test]
    fn detects_dynamic_code_and_shell() {
        let r = risk_features("risk");
        assert!(r["dyn"].contains(&"dynamic.code".to_string()));
        assert!(r["deserialize"].contains(&"dynamic.code".to_string()));
        assert!(r["shell"].contains(&"dynamic.code".to_string())); // shell=True
    }

    #[test]
    fn detects_compile_and_dunder_import() {
        let r = risk_features("risk");
        // compile(…) → dynamic.code (exact)
        assert!(
            r["uses_compile"].contains(&"dynamic.code".to_string()),
            "compile() must emit dynamic.code"
        );
        // __import__(…) → dynamic.code (exact)
        assert!(
            r["uses_dunder_import"].contains(&"dynamic.code".to_string()),
            "__import__() must emit dynamic.code"
        );
    }

    #[test]
    fn detects_yaml_load_but_not_safe_load() {
        let r = risk_features("risk");
        // yaml.load(…) → dynamic.code (path)
        assert!(
            r["unsafe_yaml"].contains(&"dynamic.code".to_string()),
            "yaml.load() must emit dynamic.code"
        );
        // yaml.safe_load(…) → NO risk
        assert!(
            !r["safe_yaml"].contains(&"dynamic.code".to_string()),
            "yaml.safe_load() must NOT emit dynamic.code"
        );
    }

    #[test]
    fn detects_importlib_import_module() {
        let r = risk_features("risk");
        // importlib.import_module(…) → dynamic.code (path)
        assert!(
            r["dynamic_import"].contains(&"dynamic.code".to_string()),
            "importlib.import_module() must emit dynamic.code"
        );
    }

    #[test]
    fn detects_setattr_monkey_patch_on_imported_name_only() {
        let r = risk_features("risk");
        // setattr(<imported module>, …) → dynamic.code (heuristic)
        assert!(
            r["monkey_patch"].contains(&"dynamic.code".to_string()),
            "setattr on imported name must emit dynamic.code"
        );
        // setattr(<non-imported name>, …) → NO risk
        assert!(
            !r["plain_setattr"].contains(&"dynamic.code".to_string()),
            "setattr on non-imported name must NOT emit dynamic.code"
        );
    }
}
