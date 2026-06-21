use crate::effect::RiskFeature;
use crate::model::{Diagnostic, Hotspot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Ts,
    Python,
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
}

pub trait Frontend {
    fn language(&self) -> Language;
    /// Parse the sources and emit per-symbol effect observations (with evidence
    /// and locations). Un-parseable files become diagnostics, not panics.
    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput;
}

#[cfg(test)]
mod tests {
    use super::*;

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
