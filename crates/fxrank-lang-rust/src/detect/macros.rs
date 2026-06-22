//! Macro-invocation effect detection.
//!
//! Walks a function body for `syn::Macro` nodes (which appear in expression,
//! statement, and item positions) and classifies them by path:
//!
//! - **logging** (class 4, Tier::Exact): `println!`, `eprintln!`, `print!`,
//!   `eprint!`, `dbg!`; or Tier::Path when the leading segment is `log` or
//!   `tracing` (e.g. `log::info!`, `tracing::warn!`).
//! - **panic** (class 4, Tier::Exact): `panic!`, `unreachable!`, `todo!`,
//!   `unimplemented!`, `assert!`, `assert_eq!`, `assert_ne!`,
//!   `debug_assert!`, `debug_assert_eq!`, `debug_assert_ne!`.
//! - **net.fs.db** (class 7, Tier::Heuristic): `write!`, `writeln!`.
//! - **whitelist** (emit nothing): `vec`, `format`, `matches`, `concat`,
//!   `stringify`, `cfg`, `line`, `column`, `file`.
//! - **unknown.macro** (class 2, confidence 0.4): everything else.

use fxrank_core::confidence::detection_confidence;
use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;
use syn::spanned::Spanned;
use syn::visit::Visit;

/// Detect macro-invocation effects in `block`.
pub fn detect(block: &syn::Block) -> Vec<Effect> {
    let mut walker = MacroWalker {
        effects: Vec::new(),
    };
    walker.visit_block(block);
    walker.effects
}

struct MacroWalker {
    effects: Vec<Effect>,
}

impl<'ast> Visit<'ast> for MacroWalker {
    /// `visit_macro` fires for every `syn::Macro` node regardless of whether
    /// it appears in expression position (`Expr::Macro`), statement position
    /// (`Stmt::Macro`), or item position (`Item::Macro`). All three ultimately
    /// contain a `syn::Macro`, and `syn::visit` recurses into each of them
    /// calling `visit_macro`, so a single override here covers all cases.
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let path = &mac.path;
        let segments: Vec<String> = path
            .segments
            .iter()
            .map(|seg| seg.ident.to_string())
            .collect();

        let last = match segments.last() {
            Some(s) => s.as_str(),
            None => return,
        };
        let first = segments[0].as_str();
        let line = mac.span().start().line;

        // Classify by matching the path.
        if let Some((kind, tier)) = classify_macro(first, last, segments.len()) {
            self.push(kind, tier, line, format_evidence(&segments));
        }
        // Note: visit_macro does NOT recurse into the macro token stream because
        // syn sees the invocation unexpanded. No further descent needed here.
    }
}

impl MacroWalker {
    fn push(&mut self, kind: EffectKind, tier: Tier, line: usize, evidence: String) {
        let class = kind.base_class();
        // unknown.macro uses a fixed 0.4 confidence (lower than heuristic 0.6)
        // to flag it as low-trust. All other macros use detection_confidence.
        let confidence = if kind == EffectKind::UnknownMacro {
            0.4
        } else {
            detection_confidence(tier, false, false)
        };
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
}

/// Classify a macro path into `(EffectKind, Tier)` or `None` for whitelisted.
///
/// Returns `None` for whitelisted macros (emit no effect).
/// Returns `Some((UnknownMacro, Heuristic))` for unrecognised macros.
fn classify_macro(first: &str, last: &str, _segment_count: usize) -> Option<(EffectKind, Tier)> {
    use EffectKind::*;

    // Built-in macros are matched on the LAST path segment, so qualified forms
    // (`std::println!`, `core::panic!`, `alloc::vec!`) classify the same as the
    // bare invocation rather than falling through to `unknown.macro`.
    match last {
        // ── Logging: exact ───────────────────────────────────────────────────
        "println" | "eprintln" | "print" | "eprint" | "dbg" => {
            return Some((Logging, Tier::Exact));
        }
        // ── Panic: exact ─────────────────────────────────────────────────────
        "panic" | "unreachable" | "todo" | "unimplemented" | "assert" | "assert_eq"
        | "assert_ne" | "debug_assert" | "debug_assert_eq" | "debug_assert_ne" => {
            return Some((Panic, Tier::Exact));
        }
        // ── net.fs.db: write!/writeln! ───────────────────────────────────────
        "write" | "writeln" => {
            return Some((NetFsDb, Tier::Heuristic));
        }
        // ── Whitelist ────────────────────────────────────────────────────────
        "vec" | "format" | "matches" | "concat" | "stringify" | "cfg" | "line" | "column"
        | "file" => {
            return None; // no effect
        }
        _ => {}
    }

    // ── Logging crates: `log::*` / `tracing::*` ──────────────────────────────
    // Their last segments (`info`/`warn`/…) aren't in the set above, so match on
    // the FIRST segment. These MUST NOT fall through to unknown.macro.
    if matches!(first, "log" | "tracing") {
        return Some((Logging, Tier::Path));
    }

    // ── Unknown macro ───────────────────────────────────────────────────────
    Some((UnknownMacro, Tier::Heuristic))
}

/// Build the evidence string: full `::` path with trailing `!`.
fn format_evidence(segments: &[String]) -> String {
    format!("{}!", segments.join("::"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_builtins_match_on_last_segment() {
        // Qualified built-ins classify like their bare forms (last-segment match).
        assert_eq!(
            classify_macro("std", "println", 2),
            Some((EffectKind::Logging, Tier::Exact))
        );
        assert_eq!(
            classify_macro("core", "panic", 2),
            Some((EffectKind::Panic, Tier::Exact))
        );
        assert_eq!(classify_macro("alloc", "vec", 2), None);
        // `log::*` / `tracing::*` still classified by first segment.
        assert_eq!(
            classify_macro("log", "info", 2),
            Some((EffectKind::Logging, Tier::Path))
        );
        // Genuinely unknown macros still fall through.
        assert_eq!(
            classify_macro("mycrate", "my_macro", 2),
            Some((EffectKind::UnknownMacro, Tier::Heuristic))
        );
    }
}
