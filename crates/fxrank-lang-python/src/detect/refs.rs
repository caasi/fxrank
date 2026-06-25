//! Call-reference extraction: walks a function body for every outgoing call
//! (free-function calls and method calls) and emits a language-neutral
//! `CallSiteRef` per site.  Unlike `calls.rs`, this module records *every*
//! named callee rather than filtering to known-effectful paths — it is the
//! graph-edge source for cross-file propagation.
//!
//! # Python `qualified` rule
//! A reference is a qualified outward reference iff its leading name resolves to
//! an import in the file's `Imports` table (`os.getcwd` where `os` is imported;
//! `from sub import run; run()`). Bare locals and `self.`/receiver methods (root
//! not imported) → `qualified = false`.

use fxrank_core::record::{CallSiteRef, RefKind};
use libcst_native::Call;

use super::{
    EffectSink,
    expr::{leftmost_name, render_expr},
    walk_own_body,
};
use crate::functions::FnUnit;
use crate::imports::Imports;
use crate::source::{SpanIndex, anchor_of_subslice};

/// Extract all outgoing call references from `unit`'s own body.
///
/// For each call node encountered:
/// - `base = render_expr(&call.func)` (e.g. `"os.getcwd"`, `"self.method"`,
///   `"foo"`). Calls where `render_expr` returns `None` are skipped.
/// - `root = base.split('.').next()` ; `module = imports.resolve(root)`.
/// - `qualified = module.is_some()` — the Python rule.
/// - `kind = RefKind::Method` if `base.contains('.')` AND `module.is_none()`
///   (a receiver attribute/method like `self.foo`/`x.bar`); else `RefKind::Free`.
/// - `line`/`col` from the `leftmost_name` anchor via the span.
/// - `resolved_target` via the unified expand-split-resolve rule (see brief §5.3):
///   Step A: expand `base` to a full dotted path using `imports.resolve(root)`,
///   distinguishing the `import a.b.c` form (R is a segment-boundary prefix of
///   base → use base as-is) from the `from m import n` form (R is not a prefix
///   → replace root with R, keep trailing members). Step B: split full into
///   `target_module` + `name`. Step C: resolve `target_module` via the map
///   (relative if `imports.relative_level(root)` is Some, else absolute) and
///   emit `[..key, name]` on a hit; else `None`.
pub fn extract(
    unit: &FnUnit,
    imports: &Imports,
    span: &SpanIndex,
    referencing_module: &[String],
    referencing_is_package: bool,
    module_map: &crate::module_map::PyModuleMap,
) -> Vec<CallSiteRef> {
    let mut sink = RefSink {
        imports,
        span,
        referencing_module,
        referencing_is_package,
        module_map,
        refs: Vec::new(),
    };
    walk_own_body(unit, &mut sink);
    sink.refs
}

struct RefSink<'a> {
    imports: &'a Imports,
    span: &'a SpanIndex<'a>,
    referencing_module: &'a [String],
    referencing_is_package: bool,
    module_map: &'a crate::module_map::PyModuleMap,
    refs: Vec<CallSiteRef>,
}

impl EffectSink for RefSink<'_> {
    fn on_call(&mut self, call: &Call) {
        let Some(rendered) = render_expr(&call.func) else {
            return;
        };

        let root = rendered.split('.').next().unwrap_or(&rendered);
        let module = self.imports.resolve(root).map(|m| {
            // module = the import module of the leading name (e.g. "os" for
            // os.getcwd, "subprocess.run" for `from subprocess import run`),
            // not the full call path.
            m.to_string()
        });

        let qualified = module.is_some();
        let kind = if rendered.contains('.') && module.is_none() {
            RefKind::Method
        } else {
            RefKind::Free
        };

        let (line, col) = leftmost_name(&call.func)
            .map(|anchor| {
                let byte_off = anchor_of_subslice(self.span.src(), anchor.value);
                self.span.line_col(byte_off)
            })
            .unwrap_or((0, 0));

        let first_party = self.imports.is_relative(root);

        let resolved_target = resolve_in_project(
            &rendered,
            root,
            &module,
            self.imports,
            self.referencing_module,
            self.referencing_is_package,
            self.module_map,
        );

        self.refs.push(CallSiteRef {
            kind,
            base: rendered,
            module,
            line,
            col,
            qualified,
            first_party,
            resolved_target,
        });
    }

    fn on_assert(&mut self, _assert: &libcst_native::Assert) {}
    fn on_raise(&mut self, _raise: &libcst_native::Raise) {}
    fn on_assign_target(&mut self, _target: &libcst_native::AssignTargetExpression, _is_aug: bool) {
    }
}

/// The unified expand-split-resolve rule for `resolved_target` (spec 025-3e §5.3).
///
/// `base` is the full rendered callee (e.g. `"os.getcwd"`, `"run"`, `"pkg.util.write"`).
/// `root` is `base.split('.').next()`.
/// `module` is `imports.resolve(root)` (already computed by the caller).
///
/// Step A — expand to a full dotted callee path:
/// - `RefKind::Method` (`base.contains('.')` AND `module.is_none()`) → `None` (unresolvable
///   receiver call; `module` being `None` is exactly the `kind == Method` condition).
/// - If `module` (= R) is `Some`:
///   - If R is a segment-boundary prefix of `base` (the `import a.b.c` form, where the code
///     already spells the full path) → `full = base` (use as-is).
///   - Else (the `from m import n` form) → `full = R + base[root.len()..]` (replace root
///     with its resolved dotted path, keep any trailing `.member` suffix).
/// - If `module` is `None` AND `base` has no `.` (bare free call) → same-module candidate:
///   `[..referencing_module, root]`.
/// - Else → `None`.
///
/// Step B — split: `name = last segment of full`, `target_module = all-but-last joined`.
///
/// Step C — resolve `target_module` against the map; emit `[..key, name]` on hit, else `None`.
fn resolve_in_project(
    base: &str,
    root: &str,
    module: &Option<String>,
    imports: &Imports,
    referencing_module: &[String],
    referencing_is_package: bool,
    module_map: &crate::module_map::PyModuleMap,
) -> Option<Vec<String>> {
    // Step A — expand base to full dotted callee path.
    let full: String = match module {
        Some(r) => {
            // Is R a segment-boundary prefix of base?
            // A segment-boundary prefix means `base` starts with `r` followed by either
            // end-of-string or a `.` (so we don't match `os` as a prefix of `osx`).
            let r_is_segment_prefix = base == r.as_str() || base.starts_with(&format!("{r}."));
            if r_is_segment_prefix {
                // `import a.b.c` form — base already spells the full path.
                base.to_string()
            } else {
                // `from m import n` form — replace root with R, keep trailing suffix.
                // base[root.len()..] is either "" (bare `n()`) or ".member..." (`n.method()`).
                format!("{r}{}", &base[root.len()..])
            }
        }
        None => {
            if !base.contains('.') {
                // Bare free call, no import → same-module candidate.
                let mut target: Vec<String> = referencing_module.to_vec();
                target.push(root.to_string());
                return Some(target);
            } else {
                // Method call on a non-imported receiver → unresolvable.
                return None;
            }
        }
    };

    // Step B — split: name = last segment, target_module = all-but-last.
    // A bare relative from-import like `from . import write` expands to a dot-free
    // `full` (e.g. `"write"`). In that case target_module is empty and the anchor
    // package is resolved by `resolve_relative("", level)` below (Step C).
    let (target_module, name): (&str, &str) = match full.rfind('.') {
        Some(p) => (&full[..p], &full[p + 1..]),
        None => ("", full.as_str()), // relative bare from-import: name = full, pkg via resolve_relative("")
    };

    // Step C — resolve target_module; emit [..key, name] on hit.
    let key = if let Some(level) = imports.relative_level(root) {
        module_map.resolve_relative(
            referencing_module,
            referencing_is_package,
            level,
            target_module,
        )?
    } else {
        module_map.resolve_absolute(target_module)?
    };

    let mut result = key;
    result.push(name.to_string());
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use crate::module_map::PyModuleMap;
    use fxrank_core::frontend::SourceFile;
    use fxrank_core::record::RefKind;

    /// Parse inline source and return refs for the function named `sym`.
    fn refs_for(src: &str, sym: &str) -> Vec<CallSiteRef> {
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, src, &span, &anchors);
        let unit = units
            .iter()
            .find(|u| u.symbol == sym)
            .unwrap_or_else(|| panic!("symbol {sym} not found"));
        let empty_map = PyModuleMap::build(&[]);
        extract(unit, &imports, &span, &[], false, &empty_map)
    }

    /// Parse `src` as if it lives at `file`, build a `PyModuleMap` from `batch_files`,
    /// and return refs for the function named `sym`. The referencing module is derived
    /// from `file` via the map.
    fn refs_with_map(src: &str, file: &str, sym: &str, batch_files: &[&str]) -> Vec<CallSiteRef> {
        let files: Vec<SourceFile> = batch_files
            .iter()
            .map(|p| SourceFile {
                path: p.to_string(),
                text: String::new(),
            })
            .collect();
        let module_map = PyModuleMap::build(&files);
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, src, &span, &anchors);
        let unit = units
            .iter()
            .find(|u| u.symbol == sym)
            .unwrap_or_else(|| panic!("symbol {sym} not found"));
        let referencing_module = module_map.module_of(file).unwrap_or_default();
        let referencing_is_package = module_map.is_package(file);
        extract(
            unit,
            &imports,
            &span,
            &referencing_module,
            referencing_is_package,
            &module_map,
        )
    }

    #[test]
    fn extracts_refs_with_qualified_rule() {
        let src = "import os\nfrom sub import run\ndef f():\n    os.getcwd()\n    run()\n    self.foo()\n    bare()\n";
        let refs = refs_for(src, "f");

        // os.getcwd — root `os` resolves to import `os`, qualified=true
        let os_ref = refs
            .iter()
            .find(|r| r.base == "os.getcwd")
            .unwrap_or_else(|| panic!("os.getcwd not found; refs: {refs:?}"));
        assert_eq!(
            os_ref.module.as_deref(),
            Some("os"),
            "os.getcwd module must be Some(\"os\")"
        );
        assert!(os_ref.qualified, "os.getcwd must be qualified=true");

        // run — resolves to `sub.run`, qualified=true, kind=Free (no dot in base, module present)
        let run_ref = refs
            .iter()
            .find(|r| r.base == "run")
            .unwrap_or_else(|| panic!("run not found; refs: {refs:?}"));
        assert_eq!(
            run_ref.module.as_deref(),
            Some("sub.run"),
            "run module must be Some(\"sub.run\")"
        );
        assert!(run_ref.qualified, "run must be qualified=true");

        // self.foo — root `self` not imported, qualified=false, kind=Method (has dot, no module)
        let self_foo = refs
            .iter()
            .find(|r| r.base == "self.foo")
            .unwrap_or_else(|| panic!("self.foo not found; refs: {refs:?}"));
        assert_eq!(self_foo.module, None, "self.foo module must be None");
        assert!(!self_foo.qualified, "self.foo must be qualified=false");
        assert!(
            matches!(self_foo.kind, RefKind::Method),
            "self.foo must be RefKind::Method"
        );

        // bare — root `bare` not imported, qualified=false, kind=Free
        let bare_ref = refs
            .iter()
            .find(|r| r.base == "bare")
            .unwrap_or_else(|| panic!("bare not found; refs: {refs:?}"));
        assert_eq!(bare_ref.module, None, "bare module must be None");
        assert!(!bare_ref.qualified, "bare must be qualified=false");
    }

    #[test]
    fn first_party_set_for_relative_imports() {
        let src = "from .utils import helper\nfrom . import sibling\nimport os\ndef f():\n    helper()\n    sibling.thing()\n    os.getcwd()\n";
        let refs = refs_for(src, "f");

        let helper_ref = refs
            .iter()
            .find(|r| r.base == "helper")
            .unwrap_or_else(|| panic!("helper not found; refs: {refs:?}"));
        assert!(helper_ref.first_party, "helper must be first_party=true");

        let sibling_ref = refs
            .iter()
            .find(|r| r.base == "sibling.thing")
            .unwrap_or_else(|| panic!("sibling.thing not found; refs: {refs:?}"));
        assert!(
            sibling_ref.first_party,
            "sibling.thing must be first_party=true"
        );

        let os_ref = refs
            .iter()
            .find(|r| r.base == "os.getcwd")
            .unwrap_or_else(|| panic!("os.getcwd not found; refs: {refs:?}"));
        assert!(!os_ref.first_party, "os.getcwd must be first_party=false");
    }

    #[test]
    fn line_is_populated() {
        let src = "import os\ndef f():\n    os.getcwd()\n";
        let refs = refs_for(src, "f");
        let r = refs
            .iter()
            .find(|r| r.base == "os.getcwd")
            .expect("os.getcwd not found");
        assert_eq!(r.line, 3, "os.getcwd is on line 3");
        assert!(r.col >= 1);
    }

    #[test]
    fn absolute_in_batch_import_resolves() {
        // from pkg.util import write; write()  → ["pkg","util","write"]
        let src = "from pkg.util import write\ndef caller():\n    write()\n";
        let refs = refs_with_map(
            src,
            "pkg/app.py",
            "caller",
            &["pkg/__init__.py", "pkg/app.py", "pkg/util.py"],
        );
        let r = refs.iter().find(|r| r.base == "write").unwrap();
        assert_eq!(
            r.resolved_target,
            Some(vec!["pkg".into(), "util".into(), "write".into()])
        );
    }

    #[test]
    fn stdlib_import_stays_unresolved_for_opaque() {
        // from subprocess import run; run()  → None (not in batch → opaque, the false-resolve fix)
        let src = "from subprocess import run\ndef caller():\n    run(['ls'])\n";
        let refs = refs_with_map(
            src,
            "pkg/app.py",
            "caller",
            &["pkg/__init__.py", "pkg/app.py"],
        );
        let r = refs.iter().find(|r| r.base == "run").unwrap();
        assert_eq!(
            r.resolved_target, None,
            "subprocess.run must be unresolved (→ opaque), never a local run"
        );
    }

    #[test]
    fn relative_import_resolves_with_level() {
        // in pkg.sub.mod: from ..util import write
        let src = "from ..util import write\ndef caller():\n    write()\n";
        let refs = refs_with_map(
            src,
            "pkg/sub/mod.py",
            "caller",
            &[
                "pkg/__init__.py",
                "pkg/sub/__init__.py",
                "pkg/sub/mod.py",
                "pkg/util.py",
            ],
        );
        let r = refs.iter().find(|r| r.base == "write").unwrap();
        assert_eq!(
            r.resolved_target,
            Some(vec!["pkg".into(), "util".into(), "write".into()])
        );
    }

    #[test]
    fn dotted_module_member_call_resolves() {
        // import pkg.util; pkg.util.write()  → ["pkg","util","write"] (unified expand-split rule)
        let src = "import pkg.util\ndef caller():\n    pkg.util.write()\n";
        let refs = refs_with_map(
            src,
            "pkg/app.py",
            "caller",
            &["pkg/__init__.py", "pkg/app.py", "pkg/util.py"],
        );
        let r = refs.iter().find(|r| r.base == "pkg.util.write").unwrap();
        assert_eq!(
            r.resolved_target,
            Some(vec!["pkg".into(), "util".into(), "write".into()])
        );
    }

    #[test]
    fn method_call_on_from_imported_value_is_unresolved() {
        // from pkg import Client; Client.get()  — Client is a CLASS (pkg.Client is NOT an in-batch
        // module), so the expand→ "pkg.Client.get" → module "pkg.Client" → resolve_absolute miss →
        // None. Must NOT resolve `get` to a coincidental module member (never-guess).
        let src = "from pkg import Client\ndef caller():\n    Client.get()\n";
        let refs = refs_with_map(
            src,
            "pkg/app.py",
            "caller",
            &["pkg/__init__.py", "pkg/app.py"],
        );
        let r = refs.iter().find(|r| r.base.starts_with("Client")).unwrap();
        assert_eq!(
            r.resolved_target, None,
            "method call on a from-imported value must be opaque"
        );
    }

    #[test]
    fn same_module_bare_call_resolves_to_own_module() {
        let src = "def helper():\n    pass\ndef caller():\n    helper()\n";
        let refs = refs_with_map(
            src,
            "pkg/app.py",
            "caller",
            &["pkg/__init__.py", "pkg/app.py"],
        );
        let r = refs.iter().find(|r| r.base == "helper").unwrap();
        assert_eq!(
            r.resolved_target,
            Some(vec!["pkg".into(), "app".into(), "helper".into()])
        );
    }

    #[test]
    fn relative_bare_from_import_resolves_to_package_member() {
        // pkg/sub/mod.py: `from . import write; write()`
        // `from . import write` → level=1, full="write" (no module_path), so rfind('.') was
        // returning None before the fix (the call went opaque). With the fix, target_module=""
        // and resolve_relative("pkg.sub.mod", is_package=false, level=1, suffix="") →
        // anchor=pkg.sub, up=0 → pkg.sub (= pkg/sub/__init__.py key) → [pkg,sub,write].
        let src = "from . import write\ndef caller():\n    write()\n";
        let refs = refs_with_map(
            src,
            "pkg/sub/mod.py",
            "caller",
            &[
                "pkg/__init__.py",
                "pkg/sub/__init__.py",
                "pkg/sub/mod.py",
                "pkg/sub/write.py",
            ],
        );
        let r = refs.iter().find(|r| r.base == "write").unwrap();
        assert_eq!(
            r.resolved_target,
            Some(vec!["pkg".into(), "sub".into(), "write".into()])
        );
    }
}
