//! Command classifier core — name → effect (spec 029 §4).
//!
//! `classify_command` is a **tri-state** name classifier so recognized-but-effectless
//! names (`tr`, filter tools, `read`/`mapfile`, declaration builtins, `MUT_OWNED`
//! mutation builtins) do not fall through to the `Unknown → process.control/6` spawn
//! branch. Single ownership across detectors: `FILTER`'s fs verdict is decided by Task
//! 5's `classify_conditional`; `DECL`/`MUT_OWNED`'s mutation is owned by `mutation.rs`
//! (Task 8) — `calls.rs` returns [`Cls::NoEffect`] for both so neither double-emits.

use std::collections::HashSet;

use brush_parser::ast;

use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;

use crate::functions::FnUnit;
use crate::walk;

/// Pure builtins: no effect, ever.
const PURE: &[&str] = &[
    ":", "true", "false", "test", "[", "[[", "return", "break", "continue",
];
/// Filesystem mutators/readers that are *always* an fs effect regardless of context.
const FS_ALWAYS: &[&str] = &[
    "cp", "mv", "rm", "mkdir", "rmdir", "touch", "ln", "chmod", "chown", "dd", "truncate",
    "install", "shred", "mktemp", "stat", "readlink", "ls", "find",
];
const NET: &[&str] = &[
    "curl", "wget", "ssh", "scp", "sftp", "rsync", "nc", "telnet", "ftp",
];
const DB: &[&str] = &["psql", "mysql", "sqlite3", "mongo", "redis-cli"];
const DEPLOY: &[&str] = &[
    "docker",
    "kubectl",
    "helm",
    "terraform",
    "ansible",
    "aws",
    "gcloud",
    "az",
    "systemctl",
    "service",
];
/// Job-control BUILTINS (concurrency/6); `coproc` is NOT here — it's
/// `CompoundCommand::Coprocess`, detected in Task 7.
const CONCURRENCY: &[&str] = &["wait", "jobs", "disown"];

// Names recognized as effectless in CALLS (owned elsewhere), so they must NOT fall
// through to the Unknown → process.control spawn branch. SINGLE OWNERSHIP (no
// double-emit):
//  - FILTER / read / mapfile: fs decided by Task 5's classify_conditional
//  - tr: never fs
//  - DECL declaration/assignment builtins AND MUT_OWNED (cd/set/export/…): owned by
//    mutation.rs (Task 8)
const FILTER: &[&str] = &[
    "cat", "grep", "sed", "awk", "head", "tail", "sort", "uniq", "wc", "cut", "rev", "tee",
];
const NEVER_FS: &[&str] = &["tr"];
const DECL: &[&str] = &[
    "local",
    "declare",
    "typeset",
    "readonly",
    "let",
    "getopts",
    "shift",
    "read",
    "mapfile",
    "readarray",
];
/// Builtins whose ENTIRE effect is a mutation (env.write / global.mutation) —
/// mutation.rs is the sole owner; calls.rs must return `NoEffect` for them or they'd
/// double-emit with Task 8.
const MUT_OWNED: &[&str] = &[
    "export", "unset", "cd", "pushd", "popd", "set", "shopt", "umask", "ulimit",
];

/// Tri-state name classifier verdict.
pub enum Cls {
    /// Recognized but effectless in `calls.rs` — owned by another detector, or truly pure.
    NoEffect,
    /// A known effect family.
    Effect(EffectKind, u8),
    /// Not recognized at all — a spawn.
    Unknown,
}

/// Tri-state name classifier. `NoEffect` = recognized & effectless-in-calls; `Unknown` =
/// spawn. `has_v` = the command carries a `-v` flag (for `printf -v`, which is a
/// mutation, not logging).
pub fn classify_command(name: &str, has_v: bool) -> Cls {
    if PURE.contains(&name)
        || DECL.contains(&name)
        || NEVER_FS.contains(&name)
        || FILTER.contains(&name)
        || MUT_OWNED.contains(&name)
    {
        return Cls::NoEffect; // FILTER fs (Task 5) / MUT_OWNED mutation (Task 8) owned elsewhere
    }
    if name == "echo" {
        return Cls::Effect(EffectKind::Logging, 2);
    }
    if name == "printf" {
        return if has_v {
            Cls::NoEffect // printf -v → mutation.rs owns
        } else {
            Cls::Effect(EffectKind::Logging, 2)
        };
    }
    if FS_ALWAYS.contains(&name) || NET.contains(&name) || DB.contains(&name) {
        return Cls::Effect(EffectKind::NetFsDb, 7);
    }
    if CONCURRENCY.contains(&name) {
        return Cls::Effect(EffectKind::Concurrency, 6);
    }
    if name == "source" || name == "." {
        return Cls::Effect(EffectKind::ProcessControl, 6); // opaque exec (ref: Task 10)
    }
    if DEPLOY.contains(&name) {
        return Cls::Effect(EffectKind::ProcessControl, 6); // known → 0.9 conf
    }
    Cls::Unknown // any other word → a spawn (0.7 conf)
}

/// Classify each `SimpleCommand` in `unit`'s body via [`classify_command`]. Consults
/// `fns` (same-file function names): a command word matching one emits NO command
/// effect — it's a call ref, handled in Task 10 (spec §4 function-vs-command
/// precedence). Task 6 will additionally gate this on wrapper-stripping + resolution
/// mode; here the resolution mode is Normal, so `fns` is consulted directly.
pub fn detect(unit: &FnUnit, fns: &HashSet<String>) -> Vec<Effect> {
    let mut out = Vec::new();
    for site in walk::walk_commands(unit) {
        let Some(name) = command_word(site.sc) else {
            continue;
        };
        if fns.contains(&name) {
            continue; // same-file fn call → ref only (Task 10), no effect
        }
        // Task 5 inserts classify_conditional(&name, &operands) here and, if it returns
        // Some, emits that and `continue`s. Task 4 handles the name-only tri-state:
        match classify_command(&name, has_flag(site.sc, "-v")) {
            Cls::Effect(kind, class) => {
                out.push(mk_effect(kind, class, site.sc, Tier::Heuristic, 0.9, &name))
            }
            Cls::Unknown => out.push(mk_effect(
                EffectKind::ProcessControl,
                6,
                site.sc,
                Tier::Heuristic,
                0.7,
                &name,
            )),
            Cls::NoEffect => {}
        }
    }
    out
}

/// The first literal word of a `SimpleCommand` (the command name itself), or `None` for
/// a bare assignment-only command (`FOO=bar` with no word).
fn command_word(sc: &ast::SimpleCommand) -> Option<String> {
    sc.word_or_name.as_ref().map(|w| w.value.clone())
}

/// `true` if `sc`'s suffix carries an argument `Word` exactly equal to `flag` (e.g.
/// `-v`) — used for the `printf -v` gate.
fn has_flag(sc: &ast::SimpleCommand, flag: &str) -> bool {
    sc.suffix
        .iter()
        .flat_map(|s| s.0.iter())
        .any(|item| matches!(item, ast::CommandPrefixOrSuffixItem::Word(w) if w.value == flag))
}

fn span_of(sc: &ast::SimpleCommand) -> (usize, usize) {
    crate::span(sc).unwrap_or((0, 0))
}

fn mk_effect(
    kind: EffectKind,
    class: u8,
    sc: &ast::SimpleCommand,
    tier: Tier,
    confidence: f64,
    ev: &str,
) -> Effect {
    let (line, col) = span_of(sc);
    Effect {
        kind,
        class,
        discounted_to: None,
        weight: weight_for_class(class),
        line,
        col,
        tier,
        hidden: false,
        contained: false,
        evidence: ev.to_string(),
        discount: None,
        subreason: None,
        confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{functions::collect, parse};
    use fxrank_core::effect::EffectKind;
    use std::collections::HashSet;

    fn kinds_fns(src: &str, fns: &HashSet<String>) -> Vec<(EffectKind, u8)> {
        let prog = parse(src).unwrap();
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.is_script)
            .unwrap();
        detect(&unit, fns)
            .into_iter()
            .map(|e| (e.kind, e.class))
            .collect()
    }
    fn kinds(src: &str) -> Vec<(EffectKind, u8)> {
        kinds_fns(src, &HashSet::new())
    }

    #[test]
    fn classifies_core_command_categories() {
        assert!(kinds("rm -rf /x\n").contains(&(EffectKind::NetFsDb, 7)));
        assert!(kinds("curl http://x\n").contains(&(EffectKind::NetFsDb, 7)));
        assert!(kinds("docker ps\n").contains(&(EffectKind::ProcessControl, 6)));
        assert!(kinds("frobnicate --wat\n").contains(&(EffectKind::ProcessControl, 6))); // unknown => spawn
        assert!(kinds("echo hi\n").contains(&(EffectKind::Logging, 2)));
        assert!(kinds(": ; true\n").is_empty()); // pure builtins → no effect
        // export/cd/set/… are MUT_OWNED: calls.rs emits NOTHING (mutation.rs owns them, Task 8) —
        // this prevents the double-emit. printf -v is likewise NoEffect here.
        assert!(kinds("export FOO=1\n").is_empty());
        assert!(kinds("cd /x\n").is_empty());
        assert!(kinds("printf -v out fmt\n").is_empty());
    }

    #[test]
    fn walk_recurses_into_control_flow_bodies() {
        // a command nested in if/for/while must NOT be invisible
        assert!(kinds("if true; then rm -rf /x; fi\n").contains(&(EffectKind::NetFsDb, 7)));
        assert!(
            kinds("for f in a b; do curl http://$f; done\n").contains(&(EffectKind::NetFsDb, 7))
        );
        assert!(kinds("while read l; do rm -rf /x; done\n").contains(&(EffectKind::NetFsDb, 7)));
        // C-style arithmetic for (ArithmeticForClause) — a distinct CompoundCommand variant
        assert!(
            kinds("for ((i=0;i<3;i++)); do rm -rf /x; done\n").contains(&(EffectKind::NetFsDb, 7))
        );
        assert!(kinds("case $x in a) curl http://y ;; esac\n").contains(&(EffectKind::NetFsDb, 7)));
    }

    #[test]
    fn same_file_function_suppresses_command_classification() {
        // A script defining greet() and calling greet must NOT get a process.control
        // spawn for `greet` — it's a same-file function call.
        let fns: HashSet<String> = ["greet".to_string()].into_iter().collect();
        assert!(kinds_fns("greet\n", &fns).is_empty());
        // but an unrecognized non-function word still spawns
        assert!(kinds_fns("frobnicate\n", &fns).contains(&(EffectKind::ProcessControl, 6)));
    }
}
