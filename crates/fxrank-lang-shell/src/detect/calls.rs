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
///
/// Ordering: same-file-fn guard → [`classify_conditional`] (FILTER's file-operand rule)
/// → input-boundary `read`/`mapfile`/`readarray` → [`classify_command`]'s name-only
/// tri-state. Each stage `continue`s on a match so a command is classified exactly once.
pub fn detect(unit: &FnUnit, fns: &HashSet<String>) -> Vec<Effect> {
    let mut out = Vec::new();
    for site in walk::walk_commands(unit) {
        let Some(name) = command_word(site.sc) else {
            continue;
        };
        if fns.contains(&name) {
            continue; // same-file fn call → ref only (Task 10), no effect
        }
        let operands = operand_words(site.sc);

        if let Some((kind, class, ambiguous)) = classify_conditional(&name, &operands) {
            let base = if ambiguous { 0.8 } else { 0.9 };
            let confidence = confidence_for(&name, &operands, base);
            out.push(mk_effect(
                kind,
                class,
                site.sc,
                Tier::Heuristic,
                confidence,
                &name,
            ));
            continue;
        }

        if is_input_reader(&name) {
            // read/mapfile/readarray measure the *input boundary*: class 7 unless stdin
            // is fed by a here-doc/here-string (Task 7 sets `stdin_is_here`; until then
            // bare `read` is always class 7). Recognized regardless (DECL already marks
            // it NoEffect in classify_command), so this always continues.
            if !site.stdin_is_here {
                let confidence = confidence_for(&name, &operands, 0.9);
                out.push(mk_effect(
                    EffectKind::NetFsDb,
                    7,
                    site.sc,
                    Tier::Heuristic,
                    confidence,
                    &name,
                ));
            }
            continue;
        }

        match classify_command(&name, has_flag(site.sc, "-v")) {
            Cls::Effect(kind, class) => {
                let confidence = confidence_for(&name, &operands, 0.9);
                out.push(mk_effect(
                    kind,
                    class,
                    site.sc,
                    Tier::Heuristic,
                    confidence,
                    &name,
                ))
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

/// `true` for the `read`/`mapfile`/`readarray` input-boundary builtins (spec §6): they
/// measure "did this unit consume external input", distinct from `FS_ALWAYS`'s literal
/// filesystem verbs.
fn is_input_reader(name: &str) -> bool {
    matches!(name, "read" | "mapfile" | "readarray")
}

/// A `SimpleCommand`'s argument `Word`s — its suffix's plain `Word` items only (excludes
/// redirects, process substitutions, and `VAR=val` assignment words). This is the operand
/// slice [`classify_conditional`]/[`has_file_operand`] inspect. Task 5 passes these raw
/// operands from `site.sc`; Task 6 switches the call sites in `detect` to the
/// wrapper-peeled `CommandView.args` so `sudo grep -f pats` computes arity against the
/// real inner operands.
fn operand_words(sc: &ast::SimpleCommand) -> Vec<&ast::Word> {
    sc.suffix
        .iter()
        .flat_map(|s| s.0.iter())
        .filter_map(|item| match item {
            ast::CommandPrefixOrSuffixItem::Word(w) => Some(w),
            _ => None,
        })
        .collect()
}

/// Option flags that take a FILE argument, per tool (best-effort, spec §4's stream-filter
/// rule).
fn file_taking_option(name: &str, flag: &str) -> bool {
    matches!(
        (name, flag),
        ("grep", "-f") | ("sed", "-f") | ("awk", "-f") | ("sort", "-o")
    )
}

/// Index of the first POSITIONAL that is a file operand — earlier positionals are the
/// tool's own pattern/program (`grep PAT`, `sed SCRIPT`, `awk PROG`); every other FILTER
/// tool's first positional is already a file.
fn first_file_positional(name: &str) -> usize {
    match name {
        "grep" | "sed" | "awk" => 1,
        _ => 0,
    }
}

/// `Some(effect)` for a `FILTER` command iff it names a real file operand (spec §4's
/// stream-filter rule / pipe containment): reading a *named file* is a durable fs read;
/// a bare stdin stage (`… | grep pat`) is not. Operand-based (NOT `&SimpleCommand`) so a
/// wrapped command can later pass peeled operands (Task 6) — Task 5 passes the raw
/// command's arg `Word`s.
fn classify_conditional(name: &str, args: &[&ast::Word]) -> Option<(EffectKind, u8, bool)> {
    if !FILTER.contains(&name) {
        return None; // tr/others are handled by Task 4's tri-state (NEVER_FS/FS_ALWAYS)
    }
    match has_file_operand(name, args) {
        Some(true) => Some((EffectKind::NetFsDb, 7, false)),
        Some(false) => None,
        None => Some((EffectKind::NetFsDb, 7, true)), // ambiguous ($vars) → emit, but flag it
    }
}

/// `Some(true)` if `args` names a real file operand for `name`: a positional `Word` at
/// index `>= first_file_positional(name)` that isn't a bare (unquoted) variable
/// expansion, or a `-x file` where [`file_taking_option`] recognizes `-x`. `Some(false)`
/// when every candidate positional slot is empty (`grep pat` — only the pattern).
/// `None` when arity can't be decided: a candidate positional exists but every one is a
/// bare `$var`/`${var}` expansion (its runtime value is unknown, so it might expand to a
/// filename or to nothing). Best-effort: option-argument consumption is only recognized
/// for [`file_taking_option`]'s known flags; any other short/long flag is skipped without
/// consuming a following word (an accepted heuristic edge, matching the spec's
/// "operand-vs-flag detection is best-effort" note).
fn has_file_operand(name: &str, args: &[&ast::Word]) -> Option<bool> {
    let threshold = first_file_positional(name);
    let mut positional_idx = 0usize;
    let mut ambiguous = false;
    let mut i = 0;
    while i < args.len() {
        let value = args[i].value.as_str();
        if is_flag(value) {
            if file_taking_option(name, value) && i + 1 < args.len() {
                return Some(true);
            }
            i += 1;
            continue;
        }
        if positional_idx >= threshold {
            if is_bare_var_expansion(value) {
                ambiguous = true;
            } else {
                return Some(true);
            }
        }
        positional_idx += 1;
        i += 1;
    }
    if ambiguous { None } else { Some(false) }
}

/// `true` for a short/long option word (`-f`, `--file`), never for a bare `-` (stdin
/// placeholder, which is a positional, not a flag).
fn is_flag(value: &str) -> bool {
    value.starts_with('-') && value != "-"
}

/// `true` if `value` is a single unquoted variable expansion for its *entire* extent —
/// `$VAR` or `${VAR}` with no surrounding quotes and no other text. `Word::value` is
/// brush-parser's raw source text (quote characters included verbatim), so a quoted
/// `"$VAR"` keeps its `"` and correctly fails this check. Used both to mark a
/// file-operand candidate undecidable (spec §6 ambiguous-file-operand) and to detect an
/// unquoted variable in a destructive command (spec §6 confidence delta).
fn is_bare_var_expansion(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('$') else {
        return false;
    };
    let inner = match rest.strip_prefix('{') {
        Some(braced) => match braced.strip_suffix('}') {
            Some(inner) => inner,
            None => return false,
        },
        None => rest,
    };
    !inner.is_empty() && inner.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// `true` for a `net.fs.db`/7 command whose operands make it destructive (spec §6):
/// `rm` with a recursive short-flag cluster (`-r`/`-rf`/`-R`/…), `chmod -R`/`chown -R`,
/// and the inherently-destructive `dd`/`shred` (no flag needed — both overwrite/wipe by
/// design).
fn is_destructive_fs(name: &str, args: &[&ast::Word]) -> bool {
    match name {
        "rm" => args
            .iter()
            .any(|w| is_short_flag_cluster(&w.value) && w.value.contains(['r', 'R'])),
        "chmod" | "chown" => args
            .iter()
            .any(|w| is_short_flag_cluster(&w.value) && w.value.contains('R')),
        "dd" | "shred" => true,
        _ => false,
    }
}

/// `true` for a short-flag word (`-rf`) as opposed to a long option (`--recursive`) —
/// mirrors `bindings.rs::has_global_flag`'s combined-cluster convention.
fn is_short_flag_cluster(value: &str) -> bool {
    value.starts_with('-') && !value.starts_with("--")
}

/// `true` if any operand is a bare unquoted variable expansion (see
/// [`is_bare_var_expansion`]) — the spec §6 confidence-delta trigger for a destructive fs
/// command.
fn has_unquoted_var_operand(args: &[&ast::Word]) -> bool {
    args.iter().any(|w| is_bare_var_expansion(&w.value))
}

/// Apply the spec §6 unquoted-variable-in-destructive-command delta (−0.1, floored at
/// 0.1) to `base` when `name`/`args` qualify; otherwise `base` unchanged. Centralized here
/// so every `net.fs.db` emission in [`detect`] (FILTER's conditional path, the
/// input-boundary path, and Task 4's name-only match) shares one confidence pipeline —
/// it is a no-op for any non-destructive command.
fn confidence_for(name: &str, args: &[&ast::Word], base: f64) -> f64 {
    if is_destructive_fs(name, args) && has_unquoted_var_operand(args) {
        (base - 0.1_f64).max(0.1)
    } else {
        base
    }
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

    #[test]
    fn stream_filter_rule() {
        // grep with a file operand → fs read; bare (stdin) grep → no fs effect.
        assert!(kinds("grep pat f.txt\n").contains(&(EffectKind::NetFsDb, 7)));
        assert!(kinds("grep pat\n").is_empty());
        // tr never fs, regardless of args
        assert!(kinds("tr a b\n").is_empty());
        // file via option counts
        assert!(kinds("grep -f pats\n").contains(&(EffectKind::NetFsDb, 7)));
        // read from real input is class 7; here-string fed read is NOT (Task 7 wires <<<; here bare read)
        assert!(kinds("read x\n").contains(&(EffectKind::NetFsDb, 7)));
    }

    #[test]
    fn unquoted_var_in_destructive_lowers_effect_confidence() {
        let effect = |src: &str| {
            let prog = parse(src).unwrap();
            let unit = collect(&prog, "x.sh")
                .into_iter()
                .find(|u| u.is_script)
                .unwrap();
            detect(&unit, &HashSet::new())
                .into_iter()
                .find(|e| e.kind == EffectKind::NetFsDb)
                .unwrap()
        };
        // rm -rf $DIR (unquoted var) → the net.fs.db effect confidence is reduced (−0.1) vs a literal path
        assert!(effect("rm -rf $DIR\n").confidence < effect("rm -rf /tmp/x\n").confidence);
    }
}
