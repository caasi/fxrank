// fxrank-lang-ts: TypeScript frontend for FxRank (swc-based)

pub mod detect;
pub mod functions;
pub mod imports;
pub mod source;

use fxrank_core::frontend::{Frontend, FrontendOutput, Language, SourceFile};
use fxrank_core::model::Diagnostic;

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
/// dialect. `include_tests` is carried for parity with `RustFrontend` but is not
/// yet consumed — test-skipping logic arrives in a later task.
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
                    for unit in &units {
                        output
                            .functions
                            .push(detect::analyze_unit(unit, &imports, &lines));
                    }
                }
            }
        }

        output
    }
}
