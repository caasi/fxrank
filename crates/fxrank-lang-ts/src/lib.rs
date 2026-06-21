// fxrank-lang-ts: TypeScript frontend for FxRank (swc-based)

pub mod coverage;
pub mod detect;
pub mod functions;
pub mod imports;
pub mod react;
pub mod source;

use std::collections::{HashMap, HashSet};

use fxrank_core::frontend::{Frontend, FrontendOutput, Language, SourceFile};
use fxrank_core::model::{Diagnostic, Hotspot};

use crate::functions::FnUnit;
use crate::imports::ImportTable;
use crate::source::{Lang, SpanLines};

/// The TypeScript/JavaScript language frontend.
///
/// `TsFrontend { lang }.analyze()` parses each `SourceFile` with the configured
/// `lang` dialect via `functions::parse_module`, builds a `SpanLines` from the
/// same `SourceMap` used for parsing (so effect-line resolution works), then
/// maps each `FnUnit` to a scored `Hotspot` via `detect::analyze_unit`.
/// Un-parseable files become `Diagnostic`s, not panics.
///
/// `lang` is the dialect used for *all* this frontend's sources; the CLI groups
/// sources by resolved `Lang` so each group gets a `TsFrontend` with the right
/// dialect. When `include_tests` is `false` (the default), whole files whose path
/// contains `.test.` or `.spec.` (e.g. `foo.test.ts`, `bar.spec.tsx`) or any
/// path segment equals `__tests__` are skipped; their unit count is tallied in
/// `FrontendOutput::skipped_tests`. JS/TS convention keeps tests in separate
/// files, so skipping is by file path (not by detecting `describe`/`it` inside
/// app code), mirroring the Rust frontend's `skipped_tests` contract.
#[derive(Default)]
pub struct TsFrontend {
    pub lang: Lang,
    pub include_tests: bool,
}

impl Frontend for TsFrontend {
    fn language(&self) -> Language {
        Language::Ts
    }

    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput {
        let mut output = FrontendOutput::default();

        for source in files {
            match functions::parse_module(&source.text, &source.path, self.lang) {
                Err(e) => {
                    // FIXME(diagnostic-UX): swc Error has no Display; Debug output is
                    // verbose — extract just the message in a later pass.
                    output.diagnostics.push(Diagnostic {
                        path: source.path.clone(),
                        parsed: false,
                        error: format!("{e:?}"),
                    });
                }
                Ok((module, cm)) => {
                    // Keep the SourceMap alive through detection: swc spans are
                    // bare BytePos offsets, and SpanLines needs the same cm that
                    // parsed the file to resolve them to line numbers.
                    let lines = SpanLines::new(cm);
                    let imports = ImportTable::from_module(&module);
                    let units = functions::collect(&module, &source.path, &lines);
                    if !self.include_tests && is_test_file(&source.path) {
                        output.skipped_tests += units.len();
                    } else {
                        analyze_units(&units, &imports, &lines, &mut output.functions);
                    }
                }
            }
        }

        output
    }
}

/// Score every `FnUnit` in one parsed file, routing inline hook-callback arrows
/// into the components that own them (the React two-pass).
///
/// **Pass 1** — find the components (`returns_jsx`), their `useRef`-binding sets,
/// and the inline arrows they pass to built-in hooks (`inherited_callbacks`),
/// keyed by `(line, col)`.
///
/// **Pass 2** — score each unit. An arrow whose `(line, col)` is an inherited
/// callback is **suppressed** as a standalone hotspot; its raw (pre-discount)
/// signals are stashed and later folded into the owning component
/// (`absorb_inherited`). A component's own hotspot is additionally augmented with
/// its render-body React signals (`augment_component`). Emission order matches
/// the input unit order.
fn analyze_units(
    units: &[FnUnit],
    imports: &ImportTable,
    lines: &SpanLines,
    out: &mut Vec<Hotspot>,
) {
    // Pass 1: components, their ref-binding sets, and the inherited arrows.
    let components: Vec<&FnUnit> = units
        .iter()
        .filter(|u| react::returns_jsx(&u.body))
        .collect();
    let comp_refs: HashMap<String, HashSet<String>> = components
        .iter()
        .map(|c| (c.id.clone(), react::ref_bindings(&c.body)))
        .collect();
    // (line, col) of an inline hook arrow -> (owning component id, phase).
    let mut inherited: HashMap<(usize, usize), (String, react::HookPhase)> = HashMap::new();
    for c in &components {
        for ((l, col), phase) in react::inherited_callbacks(&c.body, lines) {
            inherited.insert((l, col), (c.id.clone(), phase));
        }
    }

    // Pass 2: score each unit, routing inherited arrows into their component.
    let mut by_id: HashMap<String, Hotspot> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut pending: HashMap<String, Vec<(react::HookPhase, detect::RawSignals)>> = HashMap::new();
    for unit in units {
        // `unit.col` is a real field (Task 4) — NEVER parse it out of `id`.
        let key = (unit.line, unit.col);
        if let Some((comp_id, phase)) = inherited.get(&key).cloned() {
            // Suppress this arrow as a standalone hotspot; stash its raw signals.
            // Thread the owning component's ref-binding set so a `r.current = …`
            // write inside this callback still classifies as ref-cell-write (the
            // arrow alone can't know `r` is a useRef binding from the component).
            let refs = comp_refs.get(&comp_id).cloned().unwrap_or_default();
            let raw = detect::raw_signals(unit, imports, lines, &refs);
            pending.entry(comp_id).or_default().push((phase, raw));
            continue;
        }
        let mut h = detect::analyze_unit(unit, imports, lines);
        if react::returns_jsx(&unit.body) {
            detect::augment_component(&mut h, unit, lines);
        }
        by_id.insert(unit.id.clone(), h);
        order.push(unit.id.clone());
    }

    // Fold each component's inherited raw signals in, then recompute.
    for (comp_id, raws) in pending {
        if let Some(h) = by_id.get_mut(&comp_id) {
            detect::absorb_inherited(h, raws);
        }
    }

    for id in order {
        out.push(by_id.remove(&id).expect("hotspot present for ordered id"));
    }
}

/// Return `true` if `path` identifies a test file by convention.
///
/// A file is a test file when:
/// - the file name contains `.test.` or `.spec.` (e.g. `foo.test.ts`, `bar.spec.tsx`), OR
/// - any path segment is exactly `__tests__` (e.g. `src/__tests__/foo.ts`).
///
/// Only these two well-established JS/TS conventions are checked. Stdin
/// (`"stdin"`) and ordinary `.ts`/`.js` files are never test files.
pub fn is_test_file(path: &str) -> bool {
    // Use the file name only for the infix check (avoid matching `.test.` in a
    // directory component like `my.test.project/app.ts`). Split on both `/` and
    // `\` so a Windows directory like `my.test.project\app.ts` isn't false-matched.
    let file_name = path.split(['/', '\\']).next_back().unwrap_or(path);
    if file_name.contains(".test.") || file_name.contains(".spec.") {
        return true;
    }
    // Any path segment equal to `__tests__` marks the whole file as a test file.
    // Split on both `/` and `\` to handle Windows paths (e.g. `src\__tests__\foo.ts`).
    path.split(['/', '\\']).any(|seg| seg == "__tests__")
}
