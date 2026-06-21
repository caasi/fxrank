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
            AmbientRead | UnknownMacro => 2,
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
    pub tier: Tier,
    #[serde(skip_serializing_if = "is_false")]
    pub hidden: bool,
    pub evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discount: Option<String>,
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
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskFeature {
    #[serde(serialize_with = "ser_risk")]
    pub kind: RiskKind,
    pub class: u8,
    pub weight: u32,
    pub path: String,
    pub line: usize,
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
}
