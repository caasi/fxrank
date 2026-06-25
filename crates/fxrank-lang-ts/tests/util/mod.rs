//! Test helpers shared by the React integration tests.

use fxrank_core::frontend::{Frontend, SourceFile};
use fxrank_core::model::Hotspot;
use fxrank_core::record::UnitRecord;
use fxrank_lang_ts::TsFrontend;
use fxrank_lang_ts::source::Lang;

/// Parse `src` as `.tsx`, run the full two-pass `TsFrontend::analyze`, and
/// return the resulting hotspots (`output.functions`).
///
/// Non-test path (`c.tsx`), `include_tests: false` — exercises the same code
/// path the CLI drives for ordinary app source.
pub fn analyze_tsx(src: &str) -> Vec<Hotspot> {
    let frontend = TsFrontend {
        lang: Lang::Tsx,
        include_tests: false,
        tsconfig: None,
    };
    let files = [SourceFile {
        path: "c.tsx".to_string(),
        text: src.to_string(),
    }];
    frontend.analyze(&files).functions
}

/// Like [`analyze_tsx`] but returns the language-neutral `UnitRecord`s
/// (`output.records`) so a test can assert on the cross-file graph edges a
/// component carries (e.g. an imported handler passed as a JSX prop must emit a
/// ref/edge — spec 027 §4.5).
#[allow(dead_code)]
pub fn analyze_tsx_records(src: &str) -> Vec<UnitRecord> {
    let frontend = TsFrontend {
        lang: Lang::Tsx,
        include_tests: false,
        tsconfig: None,
    };
    let files = [SourceFile {
        path: "c.tsx".to_string(),
        text: src.to_string(),
    }];
    frontend.analyze(&files).records
}
