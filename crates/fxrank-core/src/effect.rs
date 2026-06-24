use crate::score::weight_for_class;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Exact,
    Path,
    Heuristic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    NetFsDb,
    ProcessControl,
    EnvWrite,
    Concurrency,
    TimeRead,
    Random,
    EnvRead,
    Logging,
    Panic,
    GlobalMutation,
    HiddenMutation,
    ParamMutation,
    AmbientRead,
    LocalMutation,
    UnknownMacro,
    ThisMutation,
    StateTransition,
    ExternalUnresolved,
}
impl EffectKind {
    pub fn wire(self) -> &'static str {
        use EffectKind::*;
        match self {
            NetFsDb => "net.fs.db",
            ProcessControl => "process.control",
            EnvWrite => "env.write",
            Concurrency => "concurrency",
            TimeRead => "time.read",
            Random => "random",
            EnvRead => "env.read",
            Logging => "logging",
            Panic => "panic",
            GlobalMutation => "global.mutation",
            HiddenMutation => "hidden.mutation",
            ParamMutation => "param.mutation",
            AmbientRead => "ambient.read",
            LocalMutation => "local.mutation",
            UnknownMacro => "unknown.macro",
            ThisMutation => "this.mutation",
            StateTransition => "state.transition",
            ExternalUnresolved => "external.unresolved",
        }
    }
    pub fn base_class(self) -> u8 {
        use EffectKind::*;
        match self {
            NetFsDb => 7,
            ProcessControl | EnvWrite | Concurrency => 6,
            TimeRead | Random => 5,
            EnvRead | Logging | Panic => 4,
            GlobalMutation => 6,
            HiddenMutation | ParamMutation => 3,
            AmbientRead | UnknownMacro | ExternalUnresolved => 2,
            LocalMutation | StateTransition => 1,
            ThisMutation => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskKind {
    Transmute,
    RawPtrDeref,
    FfiCall,
    Asm,
    RawPtrOp,
    MaybeUninit,
    FromRaw,
    GetUnchecked,
    UnsafeBlock,
    UnsafeFn,
    UnsafeImpl,
    BoxLeak,
    MemForget,
    ManuallyDrop,
    ImplDrop,
    ExternBlock,
    TypeEscape,
    DynamicCode,
    ProtoPollution,
    HtmlInjection,
    EffectInRender,
}
impl RiskKind {
    pub fn wire(self) -> &'static str {
        use RiskKind::*;
        match self {
            Transmute => "transmute",
            RawPtrDeref => "raw.ptr.deref",
            FfiCall => "ffi.call",
            Asm => "asm",
            RawPtrOp => "raw.ptr.op",
            MaybeUninit => "maybe.uninit",
            FromRaw => "from.raw",
            GetUnchecked => "get.unchecked",
            UnsafeBlock => "unsafe.block",
            UnsafeFn => "unsafe.fn",
            UnsafeImpl => "unsafe.impl",
            BoxLeak => "box.leak",
            MemForget => "mem.forget",
            ManuallyDrop => "manually.drop",
            ImplDrop => "impl.drop",
            ExternBlock => "extern.block",
            TypeEscape => "type.escape",
            DynamicCode => "dynamic.code",
            ProtoPollution => "proto.pollution",
            HtmlInjection => "html.injection",
            EffectInRender => "effect.in.render",
        }
    }
    pub fn class(self) -> u8 {
        use RiskKind::*;
        match self {
            Transmute | RawPtrDeref | FfiCall | Asm | RawPtrOp | MaybeUninit | FromRaw
            | GetUnchecked | DynamicCode => 7,
            UnsafeBlock | UnsafeFn | UnsafeImpl | HtmlInjection => 5,
            BoxLeak | MemForget | ManuallyDrop | ProtoPollution | EffectInRender => 4,
            TypeEscape => 3,
            ImplDrop | ExternBlock => 2,
        }
    }

    /// Whether this risk propagates to a caller (capability) or is encapsulated by
    /// the callee. Spec 025 sec 7 / sec 15.7 — a judgment table, change here if dogfooding shifts it.
    pub fn escapes(self) -> bool {
        use RiskKind::*;
        matches!(
            self,
            DynamicCode | FfiCall | HtmlInjection | ProtoPollution | EffectInRender
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Effect {
    #[serde(serialize_with = "ser_kind")]
    pub kind: EffectKind,
    pub class: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discounted_to: Option<u8>,
    pub weight: u32,
    pub line: usize,
    pub col: usize,
    pub tier: Tier,
    #[serde(skip_serializing_if = "is_false")]
    pub hidden: bool,
    #[serde(skip)]
    pub contained: bool,
    pub evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subreason: Option<String>,
    #[serde(skip)]
    pub confidence: f64,
}
impl Effect {
    pub fn effective_class(&self) -> u8 {
        self.discounted_to.unwrap_or(self.class)
    }
    pub fn sync_weight(&mut self) {
        self.weight = weight_for_class(self.effective_class());
    }
    /// Returns `true` if this effect propagates to the caller.
    /// `ExternalUnresolved` always escapes (it represents an unresolved call boundary).
    /// All other effects escape when not marked as contained.
    pub fn escapes(&self) -> bool {
        matches!(self.kind, EffectKind::ExternalUnresolved) || !self.contained
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskFeature {
    #[serde(serialize_with = "ser_risk")]
    pub kind: RiskKind,
    pub class: u8,
    pub weight: u32,
    pub path: String,
    pub line: usize,
    pub col: usize,
    pub evidence: String,
    pub tier: Tier,
}

fn is_false(b: &bool) -> bool {
    !*b
}
fn ser_kind<S: serde::Serializer>(k: &EffectKind, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(k.wire())
}
fn ser_risk<S: serde::Serializer>(k: &RiskKind, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(k.wire())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_and_risk_metadata() {
        assert_eq!(EffectKind::NetFsDb.wire(), "net.fs.db");
        assert_eq!(EffectKind::NetFsDb.base_class(), 7);
        assert_eq!(EffectKind::GlobalMutation.base_class(), 6); // spec default, not 3
        assert_eq!(EffectKind::HiddenMutation.base_class(), 3);
        assert_eq!(EffectKind::UnknownMacro.base_class(), 2);
        assert_eq!(RiskKind::Transmute.class(), 7);
        assert_eq!(RiskKind::MemForget.wire(), "mem.forget");
        assert_eq!(RiskKind::ImplDrop.class(), 2);
    }

    #[test]
    fn ts_vocabulary_metadata() {
        assert_eq!(EffectKind::ThisMutation.wire(), "this.mutation");
        assert_eq!(EffectKind::ThisMutation.base_class(), 3);
        assert_eq!(RiskKind::TypeEscape.wire(), "type.escape");
        assert_eq!(RiskKind::TypeEscape.class(), 3);
        assert_eq!(RiskKind::DynamicCode.class(), 7);
        assert_eq!(RiskKind::ProtoPollution.class(), 4);
        assert_eq!(RiskKind::HtmlInjection.class(), 5);
    }

    #[test]
    fn react_vocabulary_metadata() {
        assert_eq!(EffectKind::StateTransition.wire(), "state.transition");
        assert_eq!(EffectKind::StateTransition.base_class(), 1);
        assert_eq!(RiskKind::EffectInRender.wire(), "effect.in.render");
        assert_eq!(RiskKind::EffectInRender.class(), 4);
    }

    #[test]
    fn effect_and_risk_carry_col() {
        let e = Effect {
            kind: EffectKind::NetFsDb,
            class: 7,
            discounted_to: None,
            weight: 21,
            line: 4,
            col: 9,
            tier: Tier::Path,
            hidden: false,
            contained: false,
            evidence: "fetch(x)".into(),
            discount: None,
            subreason: None,
            confidence: 1.0,
        };
        assert_eq!(e.col, 9);
        let j = serde_json::to_string(&e).unwrap();
        assert!(j.contains("\"line\":4,\"col\":9"));
    }

    #[test]
    fn external_unresolved_is_class_2() {
        assert_eq!(EffectKind::ExternalUnresolved.wire(), "external.unresolved");
        assert_eq!(EffectKind::ExternalUnresolved.base_class(), 2);
    }

    #[test]
    fn risk_escaping_predicate() {
        // capability risks the caller transitively triggers -> escape
        assert!(RiskKind::DynamicCode.escapes());
        assert!(RiskKind::FfiCall.escapes());
        assert!(RiskKind::HtmlInjection.escapes());
        assert!(RiskKind::ProtoPollution.escapes());
        assert!(RiskKind::EffectInRender.escapes());
        // encapsulated risks the callee owns -> do not escape
        assert!(!RiskKind::UnsafeBlock.escapes());
        assert!(!RiskKind::Transmute.escapes());
        assert!(!RiskKind::RawPtrDeref.escapes());
        assert!(!RiskKind::MemForget.escapes());
        assert!(!RiskKind::ImplDrop.escapes());
    }

    #[test]
    fn effect_escapes_unless_contained() {
        let mut e = Effect {
            kind: EffectKind::LocalMutation,
            class: 1,
            discounted_to: None,
            weight: 1,
            line: 1,
            col: 1,
            tier: Tier::Exact,
            hidden: false,
            contained: true,
            evidence: "s = 1".into(),
            discount: None,
            subreason: None,
            confidence: 1.0,
        };
        assert!(!e.escapes()); // contained local mutation stays put
        e.contained = false;
        assert!(e.escapes()); // escaping mutation propagates
        e.kind = EffectKind::ExternalUnresolved;
        e.contained = true;
        assert!(e.escapes()); // external.unresolved always escapes
    }

    #[test]
    fn subreason_serializes_only_when_present() {
        let mut e = Effect {
            kind: EffectKind::HiddenMutation,
            class: 3,
            discounted_to: None,
            weight: 3,
            line: 1,
            col: 1,
            tier: Tier::Heuristic,
            hidden: true,
            contained: false,
            evidence: "x".into(),
            discount: None,
            subreason: Some("ref-cell-write".into()),
            confidence: 1.0,
        };
        let j = serde_json::to_string(&e).unwrap();
        assert!(j.contains("\"subreason\":\"ref-cell-write\""));
        e.subreason = None;
        assert!(!serde_json::to_string(&e).unwrap().contains("subreason"));
    }
}
