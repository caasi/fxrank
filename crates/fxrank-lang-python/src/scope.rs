//! Frontend-internal scope-aware binding resolution (spec 078).
//! See docs/scope-aware-binding-resolution-guideline.md. NOTHING here crosses
//! the UnitRecord boundary; fxrank-core knows none of these types.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum BindingOrigin {
    Param,
    Local,
    ModuleImport,
    FromImport,
    ModuleBinding,
    Global,
    EnclosingCaptured,
    HostGlobalIo,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct BindingId {
    pub scope_segments: Vec<String>,
    pub kind_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_origin_variants_exist() {
        // Compile-time coverage: every guideline origin is representable.
        let _ = [
            BindingOrigin::Param,
            BindingOrigin::Local,
            BindingOrigin::ModuleImport,
            BindingOrigin::FromImport,
            BindingOrigin::ModuleBinding,
            BindingOrigin::Global,
            BindingOrigin::EnclosingCaptured,
            BindingOrigin::HostGlobalIo,
            BindingOrigin::Unresolved,
        ];
        let id = BindingId {
            scope_segments: vec!["b3".into()],
            kind_name: "local:x".into(),
        };
        assert_eq!(id.kind_name, "local:x");
    }
}
