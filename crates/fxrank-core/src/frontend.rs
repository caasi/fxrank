use crate::effect::RiskFeature;
use crate::model::{Diagnostic, Hotspot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Ts,
    Python,
    Shell,
}

pub struct SourceFile {
    pub path: String,
    pub text: String,
}

#[derive(Default)]
pub struct FrontendOutput {
    pub functions: Vec<Hotspot>,        // scored functions (pre-ranking)
    pub module_risks: Vec<RiskFeature>, // module-level (impl Drop, extern)
    pub diagnostics: Vec<Diagnostic>,
    pub skipped_tests: usize,
    pub records: Vec<crate::record::UnitRecord>, // cross-file fold input (populated by frontends in Task 3+)
}

pub trait Frontend {
    fn language(&self) -> Language;
    /// Parse the sources and emit per-symbol effect observations (with evidence
    /// and locations). Un-parseable files become diagnostics, not panics.
    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput;
    /// The frontend's corpus-hygiene profile (prune dirs, exclude globs, test-file
    /// globs, content-marker prunes). Default: empty. See `docs/corpus-profile-guideline.md`.
    fn corpus_profile(&self) -> crate::corpus::CorpusProfile {
        crate::corpus::CorpusProfile::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_output_carries_records() {
        let o = FrontendOutput::default();
        assert!(o.records.is_empty());
    }

    #[test]
    fn frontend_trait_object_safe() {
        struct Stub;
        impl Frontend for Stub {
            fn language(&self) -> Language {
                Language::Rust
            }
            fn analyze(&self, _f: &[SourceFile]) -> FrontendOutput {
                FrontendOutput::default()
            }
        }
        let f: &dyn Frontend = &Stub;
        assert_eq!(f.language(), Language::Rust);
    }
}
