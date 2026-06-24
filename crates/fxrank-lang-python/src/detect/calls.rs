//! World-effect detection: classifies effectful Python calls (fs/net/db, process
//! control, env read/write, concurrency, time, random, logging, stdin) plus bare
//! `assert`/`raise` statements, emitting an [`Effect`] per signal.
//!
//! This is the libcst analog of `fxrank-lang-rust`/`fxrank-lang-ts`'s
//! `detect/calls.rs`. It does **not** own traversal — the [`walk_own_body`] driver
//! in `detect/mod.rs` decides which nodes are evaluated in the enclosing body and
//! calls back through the [`EffectSink`] trait; this module classifies and pushes.
//!
//! # Resolution
//! A call's callee is rendered to a dotted string (`os.getenv`, `requests.get`).
//! The leading root name is resolved through [`Imports`] so an aliased import
//! (`import numpy as np`) maps back to its module, and the `import a.b.c` root-key
//! convention is honored (root `a` → `"a.b.c"`). Bare builtins (`open`, `input`,
//! `print`) need no import. Method-name-only signals (`.commit()`, `.to_csv()`) are
//! receiver-type-unknown → `Heuristic`.

use fxrank_core::confidence::detection_confidence;
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;
use libcst_native::{Assert, AssignTargetExpression, Call, Expression, Name, Raise, Subscript};

use super::{
    EffectSink,
    expr::{leftmost_name, render_expr},
    walk_own_body,
};
use crate::functions::FnUnit;
use crate::imports::Imports;
use crate::source::{SpanIndex, anchor_of_subslice};

/// Detect world effects (IO, process, env, time, random, logging, panic) charged to
/// `unit`'s own body, per the driver's attribution rules.
pub fn detect(unit: &FnUnit, imports: &Imports, span: &SpanIndex) -> Vec<Effect> {
    let mut sink = CallSink {
        imports,
        span,
        effects: Vec::new(),
    };
    walk_own_body(unit, &mut sink);
    sink.effects
}

struct CallSink<'a> {
    imports: &'a Imports,
    span: &'a SpanIndex<'a>,
    effects: Vec<Effect>,
}

impl EffectSink for CallSink<'_> {
    fn on_call(&mut self, call: &Call) {
        // Anchor on the callee's leading `Name` &str (borrowed from source).
        let Some(anchor) = leftmost_name(&call.func) else {
            return;
        };
        let (line, col) = name_line_col(anchor, self.span);
        let Some(rendered) = render_expr(&call.func) else {
            return;
        };

        // `subprocess(..., shell=True)` emits its process.control effect here; the
        // dynamic.code risk it ALSO emits is Task 10's job.
        if let Some((kind, tier, evidence)) = self.classify_call(&rendered) {
            self.push(kind, tier, line, col, evidence);
        }
    }

    fn on_assert(&mut self, assert: &Assert) {
        let (line, col) = match leftmost_name(&assert.test) {
            Some(n) => name_line_col(n, self.span),
            None => (0, 0),
        };
        self.push(
            EffectKind::Panic,
            Tier::Exact,
            line,
            col,
            "assert — stripped under -O".to_string(),
        );
    }

    fn on_raise(&mut self, raise: &Raise) {
        let (line, col) = raise
            .exc
            .as_ref()
            .and_then(leftmost_name)
            .map(|n| name_line_col(n, self.span))
            .unwrap_or((0, 0));
        self.push(
            EffectKind::Panic,
            Tier::Exact,
            line,
            col,
            "raise".to_string(),
        );
    }

    fn on_assign_target(&mut self, target: &AssignTargetExpression, _is_aug: bool) {
        // `os.environ[...] = …` → env.write (heuristic). The target is a Subscript
        // on `os.environ`.
        if let AssignTargetExpression::Subscript(sub) = target
            && let Some(rendered) = render_subscript_base(sub)
            && self.resolve_dotted(&rendered).as_deref() == Some("os.environ")
        {
            let (line, col) = leftmost_subscript_name(sub)
                .map(|n| name_line_col(n, self.span))
                .unwrap_or((0, 0));
            self.push(
                EffectKind::EnvWrite,
                Tier::Heuristic,
                line,
                col,
                "os.environ[...] = … — environment write".to_string(),
            );
        }
    }

    fn on_attribute_read(&mut self, attr: &Expression) {
        // Detect `sys.argv` (and `sys.argv[N]` — whose value walk reaches here as the
        // inner `sys.argv` Attribute) as an ambient-read.  Resolution: `sys` must map
        // to the `sys` module through the import table; `argv` must be the attribute name.
        let Expression::Attribute(a) = attr else {
            return;
        };
        if a.attr.value != "argv" {
            return;
        }
        let Some(rendered_base) = render_expr(&a.value) else {
            return;
        };
        if self.resolve_dotted(&rendered_base).as_deref() != Some("sys") {
            return;
        }
        let (line, col) = leftmost_name(attr)
            .map(|n| name_line_col(n, self.span))
            .unwrap_or((0, 0));
        self.push(
            EffectKind::AmbientRead,
            Tier::Path,
            line,
            col,
            "sys.argv".to_string(),
        );
    }
}

impl CallSink<'_> {
    fn push(&mut self, kind: EffectKind, tier: Tier, line: usize, col: usize, evidence: String) {
        let class = kind.base_class();
        // Path-tier effects carry a shadow penalty when the file imports dynamic-import
        // infrastructure (importlib/__import__), because a bare name might resolve to
        // a module we cannot see statically — mirrors the TS frontend's glob/dynamic shadow.
        let shadowed = matches!(tier, Tier::Path) && self.imports.has_dynamic();
        let confidence = detection_confidence(tier, false, shadowed);
        self.effects.push(Effect {
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
            confidence,
        });
    }

    /// Resolve a rendered dotted callee through the import table, honoring the
    /// `import a.b.c` root-key convention: split off the root name, resolve it, and
    /// re-attach the trailing path. `requests.get` with `import requests` → root
    /// `requests` resolves to `"requests"` → `"requests.get"`. `r.get` with
    /// `import requests as r` → `"requests.get"`.
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

    /// Classify a rendered callee into (kind, tier, evidence).
    fn classify_call(&self, rendered: &str) -> Option<(EffectKind, Tier, String)> {
        use EffectKind::*;

        // ── Bare builtins (no import resolution; could be shadowed, accepted) ──
        match rendered {
            "input" => {
                return Some((
                    EnvRead,
                    Tier::Exact,
                    "input() — interactive stdin read".to_string(),
                ));
            }
            "print" => return Some((Logging, Tier::Exact, "print()".to_string())),
            "open" => {
                return Some((
                    NetFsDb,
                    Tier::Exact,
                    "open(…) — file read/write".to_string(),
                ));
            }
            _ => {}
        }

        // ── Path-resolved through the import table ──
        if let Some(full) = self.resolve_dotted(rendered)
            && let Some((kind, tier)) = classify_resolved(&full)
        {
            return Some((kind, tier, format!("{full}(…)")));
        }

        // ── Method-name-only heuristics (receiver type unknown) ──
        if let Some((_, method)) = rendered.rsplit_once('.')
            && let Some(kind) = classify_method(method)
        {
            return Some((kind, Tier::Heuristic, format!("{rendered}(…)")));
        }

        None
    }
}

/// Classify a fully-resolved dotted module path (`requests.get`, `subprocess.run`,
/// `os.getenv`, `time.time`) into (kind, tier).
fn classify_resolved(full: &str) -> Option<(EffectKind, Tier)> {
    use EffectKind::*;

    let root = full.split('.').next().unwrap_or(full);
    let leaf = full.rsplit('.').next().unwrap_or(full);

    // ── net.fs.db (class 7) ──
    if matches!(root, "shutil" | "tempfile" | "csv" | "socket")
        || matches!(
            root,
            "requests" | "httpx" | "urllib" | "aiohttp" | "sqlite3" | "sqlalchemy"
        )
    {
        return Some((NetFsDb, Tier::Path));
    }
    if root == "pathlib"
        && matches!(
            leaf,
            "read_text" | "write_text" | "read_bytes" | "write_bytes"
        )
    {
        return Some((NetFsDb, Tier::Path));
    }
    if root == "json" && matches!(leaf, "load" | "dump") {
        return Some((NetFsDb, Tier::Path));
    }
    if root == "pandas" && matches!(leaf, "read_csv" | "read_excel") {
        return Some((NetFsDb, Tier::Path));
    }
    // `os` filesystem ops (a representative set; broadened by dogfooding).
    if root == "os"
        && matches!(
            leaf,
            "remove"
                | "unlink"
                | "rename"
                | "replace"
                | "mkdir"
                | "makedirs"
                | "rmdir"
                | "removedirs"
                | "listdir"
                | "scandir"
                | "stat"
                | "open"
                | "read"
                | "write"
                | "chmod"
                | "chown"
                | "walk"
        )
    {
        return Some((NetFsDb, Tier::Path));
    }

    // ── process.control (class 6) ──
    if root == "subprocess" {
        return Some((ProcessControl, Tier::Path));
    }
    if full == "os.system" || full == "sys.exit" {
        return Some((ProcessControl, Tier::Path));
    }

    // ── env.write (class 6) ──
    // `dotenv.load_dotenv` is constrained to the `dotenv` package: only flag when the
    // call resolves to `dotenv.load_dotenv` (root == "dotenv"), not an arbitrary
    // `load_dotenv` imported from any user package.
    if full == "os.putenv" || full == "dotenv.load_dotenv" {
        return Some((EnvWrite, Tier::Path));
    }

    // ── concurrency (class 6) ──
    if matches!(root, "threading" | "multiprocessing" | "asyncio") {
        return Some((Concurrency, Tier::Heuristic));
    }

    // ── time.read (class 5) ──
    if root == "time" {
        return Some((TimeRead, Tier::Path));
    }
    if root == "datetime" && matches!(leaf, "now" | "today" | "utcnow") {
        return Some((TimeRead, Tier::Heuristic));
    }

    // ── random (class 5) ──
    if matches!(root, "random" | "secrets") {
        return Some((Random, Tier::Path));
    }

    // ── env.read (class 4) ──
    if full == "os.getenv" || full == "os.environ.get" {
        return Some((EnvRead, Tier::Path));
    }

    // ── logging (class 4) ──
    if root == "logging" {
        return Some((Logging, Tier::Path));
    }

    None
}

/// Method-name-only DB/file-write heuristics (receiver type unknown → all
/// `net.fs.db` class 7, `Heuristic`).
fn classify_method(method: &str) -> Option<EffectKind> {
    match method {
        "commit" | "save" | "execute" | "to_sql" | "to_csv" | "create" => Some(EffectKind::NetFsDb),
        _ => None,
    }
}

// ─── callee rendering ─────────────────────────────────────────────────────────

/// Render the base of a subscript target (`os.environ[...]` → `"os.environ"`).
fn render_subscript_base(sub: &Subscript) -> Option<String> {
    render_expr(&sub.value)
}

/// The leftmost `Name` of a subscript target's base.
fn leftmost_subscript_name<'a>(sub: &'a Subscript<'a>) -> Option<&'a Name<'a>> {
    leftmost_name(&sub.value)
}

/// 1-based `(line, col)` of a `Name`'s anchor (its `value` &str borrows the source buffer).
fn name_line_col(name: &Name, span: &SpanIndex) -> (usize, usize) {
    span.line_col(anchor_of_subslice(span.src(), name.value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions;
    use fxrank_core::effect::EffectKind::{self, *};
    use std::collections::HashMap;

    /// Parse `tests/fixtures/<name>.py`, collect units, run `detect` per unit, and
    /// return `symbol → Vec<(EffectKind, class)>`.
    fn analyze_fixture(name: &str) -> HashMap<String, Vec<(EffectKind, u8)>> {
        let src = std::fs::read_to_string(format!("tests/fixtures/{name}.py")).unwrap();
        let module = libcst_native::parse_module(&src, None).unwrap();
        let imports = Imports::build(&module);
        let span = SpanIndex::new(&src);
        let anchors = crate::source::lambda_anchors(&src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, &src, &span, &anchors);
        let mut out: HashMap<String, Vec<(EffectKind, u8)>> = HashMap::new();
        for unit in &units {
            let effects = detect(unit, &imports, &span);
            out.insert(
                unit.symbol.clone(),
                effects.iter().map(|e| (e.kind, e.class)).collect(),
            );
        }
        out
    }

    #[test]
    fn detects_world_effects() {
        let by_fn = analyze_fixture("calls");

        let io: Vec<_> = by_fn["io_boundary"].clone();
        assert!(io.contains(&(NetFsDb, 7))); // open + requests.get
        assert!(io.contains(&(Logging, 4))); // logging.info

        let env = &by_fn["env_and_rng"];
        assert!(env.contains(&(ProcessControl, 6))); // subprocess.run
        assert!(env.contains(&(EnvRead, 4))); // os.getenv
        assert!(env.contains(&(Random, 5)) && env.contains(&(TimeRead, 5)));

        assert!(by_fn["reads_stdin"].contains(&(EnvRead, 4))); // input()
        assert!(by_fn["db_write"].contains(&(NetFsDb, 7))); // session.commit() heuristic

        // wrapper attribution: with-open and eager comprehension ARE charged...
        assert!(by_fn["in_wrapper"].contains(&(NetFsDb, 7))); // with open(...)
        assert!(by_fn["eager_comp"].contains(&(NetFsDb, 7))); // [requests.get(u) for ...]

        // ...but a lazy genexp's element body is NOT charged (deferred execution)
        assert!(
            !by_fn
                .get("lazy_gen")
                .is_some_and(|e| e.contains(&(NetFsDb, 7)))
        );

        // sys.argv attribute read → AmbientRead class 2
        assert!(by_fn["cli_args"].contains(&(AmbientRead, 2)));
    }

    /// FIX 1: `load_dotenv` is only flagged when it resolves to the `dotenv` package.
    ///
    /// Positive case: `from dotenv import load_dotenv; load_dotenv()` → env.write.
    /// Negative case: `from myapp.config import load_dotenv; load_dotenv()` → NOT flagged.
    #[test]
    fn load_dotenv_constrained_to_dotenv_package() {
        // Positive: imported from `dotenv` → must flag EnvWrite class 6.
        let pos = analyze_fixture("load_dotenv_positive");
        assert!(
            pos["configure_env"].contains(&(EnvWrite, 6)),
            "load_dotenv from dotenv package must flag env.write; got: {:?}",
            pos.get("configure_env")
        );

        // Negative: imported from a user package → must NOT flag EnvWrite.
        let neg = analyze_fixture("load_dotenv_negative");
        assert!(
            !neg["configure_env"].iter().any(|(k, _)| *k == EnvWrite),
            "load_dotenv from a user package must NOT flag env.write; got: {:?}",
            neg.get("configure_env")
        );
    }

    /// Parse inline source and return effects for the first (or only) function unit.
    fn effects_for_src(src: &str) -> Vec<Effect> {
        let module = libcst_native::parse_module(src, None).unwrap();
        let imports = Imports::build(&module);
        let span = SpanIndex::new(src);
        let anchors = crate::source::lambda_anchors(src).expect("tokenize must succeed");
        let (units, _) = functions::collect(&module, src, &span, &anchors);
        let unit = units.first().expect("at least one unit");
        detect(unit, &imports, &span)
    }

    /// Copilot FIX 1: `open` is a bare builtin → must be `Tier::Exact`, not `Tier::Path`.
    /// With a dynamic import present, its confidence must NOT be shadow-penalized
    /// (shadow penalty applies only to Path-tier).
    #[test]
    fn open_bare_builtin_is_exact_tier_and_unshadowed() {
        // Plain case: `open(p)` → NetFsDb, Tier::Exact.
        let effects = effects_for_src("def f(p):\n    return open(p).read()\n");
        let e = effects
            .iter()
            .find(|e| e.kind == NetFsDb)
            .expect("open(p) must emit NetFsDb");
        assert_eq!(
            e.tier,
            Tier::Exact,
            "open() is a bare builtin and must be Tier::Exact, got {:?}",
            e.tier
        );

        // With a dynamic import: shadow penalty applies only to Path-tier imports;
        // Exact-tier builtins must not be penalized.
        let src_dyn = "import importlib\ndef f(p):\n    return open(p).read()\n";
        let effects_dyn = effects_for_src(src_dyn);
        let e_dyn = effects_dyn
            .iter()
            .find(|e| e.kind == NetFsDb)
            .expect("open(p) must emit NetFsDb even with dynamic imports present");
        // Exact base = 1.0; shadow penalty is only applied when tier == Path.
        // confidence must equal 1.0 (no penalty).
        assert!(
            (e_dyn.confidence - 1.0).abs() < f64::EPSILON,
            "open() Exact-tier confidence must be 1.0 (no shadow penalty), got {}",
            e_dyn.confidence
        );
    }

    /// FIX 1 (Copilot): import-resolved env signals must be `Tier::Path`, not `Tier::Heuristic`.
    /// `os.getenv` / `os.environ.get` (EnvRead) and `dotenv.load_dotenv` (EnvWrite) are
    /// gated on `full == "..."` after import resolution — same mechanism as `requests.get`.
    #[test]
    fn env_signals_resolved_via_import_table_are_path_tier() {
        // os.getenv → EnvRead, Tier::Path
        let effects = effects_for_src("import os\ndef f():\n    return os.getenv(\"X\")\n");
        let e = effects
            .iter()
            .find(|e| e.kind == EnvRead)
            .expect("os.getenv must emit EnvRead");
        assert_eq!(
            e.tier,
            Tier::Path,
            "os.getenv is import-resolved; must be Tier::Path, got {:?}",
            e.tier
        );

        // dotenv.load_dotenv → EnvWrite, Tier::Path
        let effects2 =
            effects_for_src("from dotenv import load_dotenv\ndef f():\n    load_dotenv()\n");
        let e2 = effects2
            .iter()
            .find(|e| e.kind == EnvWrite)
            .expect("dotenv.load_dotenv must emit EnvWrite");
        assert_eq!(
            e2.tier,
            Tier::Path,
            "dotenv.load_dotenv is import-resolved; must be Tier::Path, got {:?}",
            e2.tier
        );
    }

    /// Two same-kind effects on the **same line** at different columns must
    /// produce distinct `col` values, not both zero. Verifies the col fix
    /// prevents SiteKey collapse in the cross-file fold.
    #[test]
    fn two_same_kind_effects_same_line_have_distinct_col() {
        // Both `open` calls are on line 2, separated by a semicolon (whitespace apart).
        let src = "def f():\n    open('a'); open('b')\n";
        let effects = effects_for_src(src);
        let net: Vec<_> = effects.iter().filter(|e| e.kind == NetFsDb).collect();
        assert_eq!(net.len(), 2, "expected two net.fs.db effects; got {net:?}");
        assert_eq!(
            net[0].line, net[1].line,
            "both open() calls must be on line 2"
        );
        assert_ne!(
            net[0].col, net[1].col,
            "two open() calls on the same line must have distinct cols, got col={} and col={}",
            net[0].col, net[1].col
        );
    }
}
