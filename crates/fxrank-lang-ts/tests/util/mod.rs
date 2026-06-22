//! Test helpers shared by the React integration tests.

use fxrank_core::frontend::{Frontend, SourceFile};
use fxrank_core::model::Hotspot;
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
    };
    let files = [SourceFile {
        path: "c.tsx".to_string(),
        text: src.to_string(),
    }];
    frontend.analyze(&files).functions
}
