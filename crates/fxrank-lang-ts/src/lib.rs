// fxrank-lang-ts: TypeScript frontend for FxRank (swc-based)

pub mod detect;
pub mod functions;
pub mod source;

use fxrank_core::frontend::{Frontend, FrontendOutput, Language, SourceFile};
use fxrank_core::model::Diagnostic;

use crate::source::Lang;

/// The TypeScript/JavaScript language frontend.
///
/// `TsFrontend { lang }.analyze()` parses each `SourceFile` with the configured
/// `lang` dialect via `functions::parse_and_collect`, then maps each `FnUnit` to
/// a scored `Hotspot` via `detect::analyze_unit`. Un-parseable files become
/// `Diagnostic`s, not panics.
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
            match functions::parse_and_collect(&source.text, &source.path, self.lang) {
                Err(e) => {
                    output.diagnostics.push(Diagnostic {
                        path: source.path.clone(),
                        parsed: false,
                        error: format!("{e:?}"),
                    });
                }
                Ok(units) => {
                    for unit in &units {
                        output.functions.push(detect::analyze_unit(unit));
                    }
                }
            }
        }

        output
    }
}
