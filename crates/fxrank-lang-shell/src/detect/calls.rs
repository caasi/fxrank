//! Command classifier core — name → effect (spec 029 §4).
//!
//! `classify_command` is a **tri-state** name classifier so recognized-but-effectless
//! names (`tr`, filter tools, `read`/`mapfile`, declaration builtins, `MUT_OWNED`
//! mutation builtins) do not fall through to the `Unknown → process.control/6` spawn
//! branch. Single ownership across detectors: `FILTER`'s fs verdict is decided by Task
//! 5's `classify_conditional`; `DECL`/`MUT_OWNED`'s mutation is owned by `mutation.rs`
//! (Task 8) — `calls.rs` returns [`Cls::NoEffect`] for both so neither double-emits.
//!
//! [`strip_wrappers`] peels command-prefix wrappers (`sudo`/`command`/`exec`/`builtin`/…)
//! so the classifiers above run against the WRAPPED command, not the wrapper itself. Its
//! `CommandView` is not persisted on `CallSiteRef` — Tasks 5 (via [`classify_conditional`]
//! below), 9, and 10 each re-call `strip_wrappers` on the sites they own.

use std::collections::HashSet;

use brush_parser::ast;

use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::weight_for_class;

use crate::functions::{FnBody, FnUnit};
use crate::walk::{self, CmdSite, Site};

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

/// Command-prefix wrappers that run ANOTHER program without themselves being a
/// resolvable same-file function target: privilege wrappers (`sudo`/`su`/`doas`),
/// environment/process-control wrappers (`env`/`nice`/`nohup`/`exec`), and the
/// name-resolution-bypass builtins (`command`). Deliberately excludes `time` — it is a
/// reserved word carried on `Pipeline.timed` (spec 029), never a `SimpleCommand` word, so
/// there is no word to peel. `exec` additionally earns its own `process.control`/6 (see
/// [`detect`]); `sudo`/`su`/`doas` additionally earn a `PrivilegeEscalation` risk (Task
/// 9, not this module).
const WRAP_EXTERNAL: &[&str] = &[
    "sudo", "su", "doas", "env", "nice", "nohup", "exec", "command",
];

/// Resolution mode of a wrapper-peeled command word (spec 029 §4 function-vs-command
/// precedence, extended for wrappers).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResMode {
    /// No wrapper was peeled — ordinary same-file-function precedence applies.
    Normal,
    /// A [`WRAP_EXTERNAL`] wrapper was peeled — the wrapped word always resolves as an
    /// external command; it can never be a same-file function (`sudo greet` does not
    /// call the local `greet` function).
    FunctionBypass,
    /// `builtin` was peeled — the wrapped word resolves to a shell builtin ONLY. Unlike
    /// [`ResMode::FunctionBypass`], an unrecognized wrapped word is not an external spawn
    /// either: `builtin` never launches a program.
    BuiltinOnly,
}

/// The wrapper-peeled view of a `SimpleCommand`: `head`/`args` name the INNER (wrapped)
/// command and its real operands — wrapper option words are excluded from `args`, so a
/// wrapped stream-filter (`sudo grep -f pats`) computes file-operand arity against the
/// actual `grep` operands, not `sudo`'s. `kinds` is the ordered list of wrapper words
/// peeled (e.g. `["sudo"]`, `["exec"]`, `[]` for an unwrapped command). `prefix_assignments`
/// carries genuine `VAR=val` *prefix* assignments (`FOO=bar sudo cmd` — a real `Assignment`
/// AST node before the command word); an `env`-style in-suffix `VAR=val` (`env FOO=1 cmd`)
/// has no such AST node and is simply consumed as a wrapper option, not tracked here.
pub struct CommandView<'a> {
    pub kinds: Vec<String>,
    pub mode: ResMode,
    pub head: Option<String>,
    pub args: Vec<&'a ast::Word>,
    pub prefix_assignments: Vec<&'a ast::Assignment>,
    pub span: (usize, usize),
}

/// Peel leading command-prefix wrappers off `sc` — `sudo`/`su`/`doas`/`env`/`nice`/
/// `nohup`/`exec`/`command` ([`WRAP_EXTERNAL`]) and `builtin` — along with each wrapper's
/// own option flags and (best-effort) `VAR=val`-shaped operands, exposing the wrapped
/// command as [`CommandView::head`]/[`CommandView::args`]. Chained wrappers peel
/// repeatedly (`sudo command rm …`); `mode` is sticky toward the most restrictive verdict
/// seen: `builtin` always yields [`ResMode::BuiltinOnly`], a [`WRAP_EXTERNAL`] word yields
/// [`ResMode::FunctionBypass`] unless `BuiltinOnly` already won.
///
/// Not persisted on `CallSiteRef` — Tasks 5 (below), 9, and 10 each re-call this on the
/// sites they own rather than threading a stored `CommandView` through core.
pub fn strip_wrappers<'a>(sc: &'a ast::SimpleCommand) -> CommandView<'a> {
    let prefix_assignments = sc
        .prefix
        .iter()
        .flat_map(|p| p.0.iter())
        .filter_map(|item| match item {
            ast::CommandPrefixOrSuffixItem::AssignmentWord(a, _) => Some(a),
            _ => None,
        })
        .collect();

    let suffix_words = operand_words(sc);

    let mut kinds = Vec::new();
    let mut mode = ResMode::Normal;
    let mut head = command_word(sc);
    let mut idx = 0usize;

    while let Some(h) = head.clone() {
        let is_builtin = h == "builtin";
        let is_wrap = WRAP_EXTERNAL.contains(&h.as_str());
        if !is_builtin && !is_wrap {
            break;
        }
        kinds.push(h.clone());
        if is_builtin {
            mode = ResMode::BuiltinOnly;
        } else if mode == ResMode::Normal {
            mode = ResMode::FunctionBypass;
        }

        // Peel this wrapper's own option flags and VAR=val-shaped operands (best-effort:
        // no full getopts, matching this module's other option handling) — a
        // value-taking flag ([`wrapper_flag_takes_value`]) additionally consumes its
        // following word so the value is never mistaken for the wrapped command (`sudo -u
        // root rm -rf /x` must not treat `root` as the head).
        while idx < suffix_words.len() {
            let value = suffix_words[idx].value.as_str();
            if is_flag(value) {
                idx += 1;
                if wrapper_flag_takes_value(&h, value) {
                    if let Some(next) = suffix_words.get(idx) {
                        let nv = next.value.as_str();
                        if !is_flag(nv) && !is_assignment_like(nv) {
                            idx += 1;
                        }
                    }
                }
                continue;
            }
            if is_assignment_like(value) {
                idx += 1;
                continue;
            }
            break;
        }

        head = suffix_words.get(idx).map(|w| {
            idx += 1;
            w.value.clone()
        });
    }

    CommandView {
        kinds,
        mode,
        head,
        args: suffix_words[idx..].to_vec(),
        prefix_assignments,
        span: span_of(sc),
    }
}

/// `true` if `value` is a `NAME=value`-shaped word (identifier chars then `=`) — an
/// `env`-style operand a wrapper consumes as its own (e.g. `env FOO=1 cmd`), not part of
/// the wrapped command's arguments.
fn is_assignment_like(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    for c in chars {
        if c == '=' {
            return true;
        }
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    false
}

/// Classify every effect-producing site in `unit`'s body: [`Site::Command`] (the name
/// classifier core, Tasks 4–6, plus Task 7's own-redirect fs effects and
/// command-substitution recursion), [`Site::Redirect`] (compound-command / function-body
/// redirect lists), and [`Site::Concurrency`] (background `&` / `coproc`, Task 7). Iterates
/// the FULL [`walk::walk`] `Site` stream — not just [`walk::walk_commands`] — so Task 7's
/// structural sites are reachable; other `Site` variants (owned by Tasks 3/8/9) are
/// ignored here.
pub fn detect(unit: &FnUnit, fns: &HashSet<String>) -> Vec<Effect> {
    let mut out = Vec::new();
    walk::walk(&unit.body, &mut |site| match site {
        Site::Command(cs) => detect_command_site(&cs, fns, &mut out),
        Site::Redirect(io, _subshell, fallback) => {
            if let Some(eff) = redirect_effect(io, fallback) {
                out.push(eff);
            }
        }
        Site::Concurrency((line, col)) => out.push(concurrency_effect(line, col)),
        _ => {}
    });
    out
}

/// Classify one [`CmdSite`] via [`strip_wrappers`] + [`classify_command`], then attribute
/// its own redirects (own `CmdSite::redirs`) and recurse into any command substitution in
/// its words. Consults `fns` (same-file function names): a command word matching one emits
/// NO command effect — it's a call ref, handled in Task 10 (spec §4 function-vs-command
/// precedence) — but only when [`ResMode::Normal`]; a wrapped word (`sudo greet`, `command
/// greet`) is never treated as a same-file function call.
///
/// Ordering: `exec`'s own `process.control`/6 (unconditional, since `exec` can appear with
/// no wrapped command) → same-file-fn guard → [`classify_conditional`] (FILTER's
/// file-operand rule) → input-boundary `read`/`mapfile`/`readarray` →
/// [`classify_command`]'s name-only tri-state → own redirects → command-substitution
/// recursion (the last two run regardless of whether a command word was classified, e.g.
/// `exec > f` or a bare `x=$(curl …)` assignment with no command word at all). In
/// [`ResMode::BuiltinOnly`], an unrecognized wrapped word (`Cls::Unknown`) emits NOTHING —
/// `builtin` never spawns an external program, so it must not fall through to the spawn
/// branch. Every effect attributed directly to this site (the classify effect and its own
/// redirects) gets spec §6's `-0.1` substitution-context delta when `site.subst` is set
/// (this site was reached through a process substitution `<(…)`/`>(…)`).
fn detect_command_site(site: &CmdSite, fns: &HashSet<String>, out: &mut Vec<Effect>) {
    let view = strip_wrappers(site.sc);

    if view.kinds.iter().any(|k| k == "exec") {
        push_effect(
            out,
            site.subst,
            mk_effect(
                EffectKind::ProcessControl,
                6,
                site.sc,
                Tier::Heuristic,
                0.9,
                "exec",
            ),
        );
    }

    if let Some(head) = view.head.clone()
        && !(view.mode == ResMode::Normal && fns.contains(&head))
    {
        if let Some((kind, class, ambiguous)) = classify_conditional(&head, &view.args) {
            let base = if ambiguous { 0.8 } else { 0.9 };
            let confidence = confidence_for(&head, &view.args, base);
            push_effect(
                out,
                site.subst,
                mk_effect(kind, class, site.sc, Tier::Heuristic, confidence, &head),
            );
        } else if is_input_reader(&head) {
            // read/mapfile/readarray measure the *input boundary*: class 7 unless stdin is
            // fed by a here-doc/here-string (`stdin_is_here`, Task 7).
            if !site.stdin_is_here {
                let confidence = confidence_for(&head, &view.args, 0.9);
                push_effect(
                    out,
                    site.subst,
                    mk_effect(
                        EffectKind::NetFsDb,
                        7,
                        site.sc,
                        Tier::Heuristic,
                        confidence,
                        &head,
                    ),
                );
            }
        } else {
            match classify_command(&head, has_flag_word(&view.args, "-v")) {
                Cls::Effect(kind, class) => {
                    let confidence = confidence_for(&head, &view.args, 0.9);
                    push_effect(
                        out,
                        site.subst,
                        mk_effect(kind, class, site.sc, Tier::Heuristic, confidence, &head),
                    );
                }
                Cls::Unknown => {
                    if view.mode != ResMode::BuiltinOnly {
                        push_effect(
                            out,
                            site.subst,
                            mk_effect(
                                EffectKind::ProcessControl,
                                6,
                                site.sc,
                                Tier::Heuristic,
                                0.7,
                                &head,
                            ),
                        );
                    }
                }
                Cls::NoEffect => {}
            }
        }
    }

    // This SimpleCommand's own redirects (`cat > out`, `grep pat < in`, …) — command-level
    // redirects on an enclosing compound/function are handled via `Site::Redirect` in
    // `detect` instead.
    for io in site.redirs.iter().copied() {
        if let Some(eff) = redirect_effect(io, span_of(site.sc)) {
            push_effect(out, site.subst, eff);
        }
    }

    recurse_command_substitutions(site.sc, fns, out);
}

/// Push `eff` onto `out`, applying spec §6's `-0.1` substitution-context delta (floored at
/// 0.1) when `subst` is set.
fn push_effect(out: &mut Vec<Effect>, subst: bool, mut eff: Effect) {
    if subst {
        eff.confidence = (eff.confidence - 0.1).max(0.1);
    }
    out.push(eff);
}

/// `$()`/backtick command substitutions inside `sc`'s words (command word, args, and
/// `VAR=val` prefix assignment values — [`walk::subst_words`]) — text, re-parsed and
/// recursed at the effect level (own half; `mutation.rs` recurses its own half separately,
/// Task 8, avoiding a forward dependency between the two detectors). Each inner effect is
/// re-anchored to the enclosing `Word`'s span and has its confidence reduced by 0.1 (spec
/// §6 substitution-context delta). The transient inner `Program` is owned locally and drops
/// at the end of each iteration — no `&'a` leak into the caller.
fn recurse_command_substitutions(
    sc: &ast::SimpleCommand,
    fns: &HashSet<String>,
    out: &mut Vec<Effect>,
) {
    for word in walk::subst_words(sc) {
        for (anchor, inner_prog) in walk::subst_programs(word) {
            let items: Vec<&ast::CompoundListItem> = inner_prog
                .complete_commands
                .iter()
                .flat_map(|cc| cc.0.iter())
                .collect();
            let transient = FnUnit {
                symbol: "<subst>".to_string(),
                id: String::new(),
                path: String::new(),
                line: 0,
                col: 0,
                body: FnBody::Script(items),
                is_script: true,
            };
            let (line, col) = (anchor.start.line, anchor.start.column);
            for mut eff in detect(&transient, fns) {
                eff.line = line;
                eff.col = col;
                eff.confidence = (eff.confidence - 0.1).max(0.1);
                out.push(eff);
            }
        }
    }
}

/// `Some(effect)` for a redirect that names a real file target: an output direction
/// (`>`/`>>`/`>|`/`<>`, plus `&>`/`&>>`) → `net.fs.db`/7 write; an input direction (`<`) →
/// `net.fs.db`/7 read. `None` for fd-dups (`>&1`), here-docs, here-strings, and a process-
/// substitution target (that's a borrowed AST subtree walked in place, not a file operand —
/// spec §4). **Location:** `IoRedirect::location()` is always `None` in brush-parser 0.4.0,
/// so the effect is anchored to the target `Word`'s own span when present, else `fallback`
/// (the enclosing command's span).
fn redirect_effect(io: &ast::IoRedirect, fallback: (usize, usize)) -> Option<Effect> {
    let (kind, class, word, evidence) = classify_redirect(io)?;
    let (line, col) = word
        .loc
        .as_ref()
        .map(|loc| (loc.start.line, loc.start.column))
        .unwrap_or(fallback);
    Some(Effect {
        kind,
        class,
        discounted_to: None,
        weight: weight_for_class(class),
        line,
        col,
        tier: Tier::Heuristic,
        hidden: false,
        contained: false,
        evidence: evidence.to_string(),
        discount: None,
        subreason: None,
        confidence: 0.9,
    })
}

/// The fs-effect verdict for a redirect's `IoFileRedirectKind`/target — see
/// [`redirect_effect`].
fn classify_redirect(io: &ast::IoRedirect) -> Option<(EffectKind, u8, &ast::Word, &'static str)> {
    match io {
        ast::IoRedirect::File(_, kind, ast::IoFileRedirectTarget::Filename(word)) => match kind {
            ast::IoFileRedirectKind::Write
            | ast::IoFileRedirectKind::Append
            | ast::IoFileRedirectKind::Clobber
            | ast::IoFileRedirectKind::ReadAndWrite => {
                Some((EffectKind::NetFsDb, 7, word, "redirect:write"))
            }
            ast::IoFileRedirectKind::Read => Some((EffectKind::NetFsDb, 7, word, "redirect:read")),
            ast::IoFileRedirectKind::DuplicateInput | ast::IoFileRedirectKind::DuplicateOutput => {
                None
            }
        },
        ast::IoRedirect::OutputAndError(word, _append) => {
            Some((EffectKind::NetFsDb, 7, word, "redirect:write"))
        }
        ast::IoRedirect::File(..)
        | ast::IoRedirect::HereDocument(..)
        | ast::IoRedirect::HereString(..) => None,
    }
}

/// A `concurrency`/6 escaping effect for a background `&` launch or a `coproc` (Task 7,
/// [`Site::Concurrency`]) — genuinely outlives the statement, unlike a plain multi-stage
/// pipeline (bounded/joined, no effect of its own; spec §4).
fn concurrency_effect(line: usize, col: usize) -> Effect {
    Effect {
        kind: EffectKind::Concurrency,
        class: 6,
        discounted_to: None,
        weight: weight_for_class(6),
        line,
        col,
        tier: Tier::Heuristic,
        hidden: false,
        contained: false,
        evidence: "&".to_string(),
        discount: None,
        subreason: None,
        confidence: 0.9,
    }
}

/// `true` for the `read`/`mapfile`/`readarray` input-boundary builtins (spec §6): they
/// measure "did this unit consume external input", distinct from `FS_ALWAYS`'s literal
/// filesystem verbs.
fn is_input_reader(name: &str) -> bool {
    matches!(name, "read" | "mapfile" | "readarray")
}

/// A `SimpleCommand`'s argument `Word`s — its suffix's plain `Word` items only (excludes
/// redirects, process substitutions, and `VAR=val` assignment words). [`detect`] consumes
/// this indirectly via [`strip_wrappers`]'s `CommandView.args` (the wrapper-peeled view),
/// so [`classify_conditional`]/[`has_file_operand`] always see the real inner operands —
/// `sudo grep -f pats` computes arity against `grep`'s operands, not `sudo`'s. Also used by
/// `strip_wrappers` itself as the raw suffix-word scan it peels wrapper options from.
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

/// `true` if `flag` takes a following value word for `wrapper` — a minimal per-wrapper
/// table (not a full getopts) covering the common value-taking short flags so
/// [`strip_wrappers`]'s peel loop consumes the value along with the flag instead of
/// mistaking it for the wrapped command word (`sudo -u root rm -rf /x` must peel `root`
/// as `-u`'s value, not treat it as the head). Long `--opt=val` forms already carry their
/// value in one token and need no entry here; `--opt val` separate-value long forms are
/// an accepted best-effort miss (matching this module's other option handling).
fn wrapper_flag_takes_value(wrapper: &str, flag: &str) -> bool {
    match wrapper {
        "sudo" | "su" | "doas" => {
            matches!(flag, "-u" | "-g" | "-p" | "-C" | "-r" | "-t" | "-h" | "-D")
        }
        "nice" => flag == "-n",
        "env" => matches!(flag, "-u" | "-C"),
        _ => false,
    }
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
/// design). `pub(crate)` — `detect/risk.rs` (Task 9) reuses this exact rule for its
/// `DestructiveFs` risk rather than re-deriving it, so the recursive-flag heuristic can't
/// drift between the confidence-delta use here and the risk-emission use there.
pub(crate) fn is_destructive_fs(name: &str, args: &[&ast::Word]) -> bool {
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

/// `true` if `args` carries a `Word` exactly equal to `flag` (e.g. `-v`) — used for the
/// `printf -v` gate. Takes the wrapper-peeled operand slice (`CommandView.args`), not a
/// raw `SimpleCommand`, so a wrapper's own flags never leak into the wrapped command's
/// gate.
fn has_flag_word(args: &[&ast::Word], flag: &str) -> bool {
    args.iter().any(|w| w.value == flag)
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

    #[test]
    fn wrappers_recurse_into_argv() {
        // sudo rm -rf keeps the fs effect (Task 9 adds the PrivilegeEscalation risk)
        assert!(kinds("sudo rm -rf /x\n").contains(&(EffectKind::NetFsDb, 7)));
        // command rm -rf recurses to rm
        assert!(kinds("command rm -rf /x\n").contains(&(EffectKind::NetFsDb, 7)));
        // exec layers its own process.control atop the wrapped command
        let k = kinds("exec rm -rf /x\n");
        assert!(
            k.contains(&(EffectKind::NetFsDb, 7)) && k.contains(&(EffectKind::ProcessControl, 6))
        );
    }

    #[test]
    fn wrapper_value_taking_flag_does_not_swallow_the_wrapped_command() {
        // sudo -u root rm -rf /x: `-u` peels its value `root` too, so `rm` (not `root`)
        // is the wrapped head and its fs effect is found (was silently dropped before
        // the value-taking-flag fix).
        assert!(kinds("sudo -u root rm -rf /x\n").contains(&(EffectKind::NetFsDb, 7)));
        // nice -n 10 curl http://y: `-n` peels its value `10` too, so `curl` is found.
        assert!(kinds("nice -n 10 curl http://y\n").contains(&(EffectKind::NetFsDb, 7)));
    }

    #[test]
    fn wrapper_recurses_into_stream_filter_arity() {
        // sudo grep -f pats → the peeled operands (not sudo's own words) drive the
        // file-operand rule; sudo grep pat → no fs effect (bare pattern, no file operand).
        assert!(kinds("sudo grep -f pats\n").contains(&(EffectKind::NetFsDb, 7)));
        assert!(kinds("sudo grep pat\n").is_empty());
    }

    #[test]
    fn builtin_only_mode_suppresses_unknown_spawn() {
        // `builtin frobnicate` never launches an external program — an unrecognized
        // wrapped word must NOT fall through to the Unknown → process.control spawn.
        assert!(kinds("builtin frobnicate\n").is_empty());
    }

    #[test]
    fn wrapped_word_bypasses_same_file_function_resolution() {
        // `sudo greet`/`command greet` are never a same-file function call, even when a
        // same-named function is defined in this file — the wrapper forces an external
        // resolution (FunctionBypass), so `greet` still classifies as an unknown spawn.
        let fns: HashSet<String> = ["greet".to_string()].into_iter().collect();
        assert!(kinds_fns("sudo greet\n", &fns).contains(&(EffectKind::ProcessControl, 6)));
        assert!(kinds_fns("command greet\n", &fns).contains(&(EffectKind::ProcessControl, 6)));
    }

    #[test]
    fn redirections_and_here_strings() {
        assert!(kinds("cat > out\n").contains(&(EffectKind::NetFsDb, 7))); // output redirect = write
        assert!(kinds("grep pat < in\n").contains(&(EffectKind::NetFsDb, 7))); // input redirect = read
        assert!(kinds("read x <<< \"$s\"\n").is_empty()); // here-string: no fs
    }

    #[test]
    fn command_substitution_recurses_as_subshell() {
        // $() in an AssignmentValue (not an arg Word) — inner curl still counts
        assert!(kinds("x=$(curl http://y)\n").contains(&(EffectKind::NetFsDb, 7)));
    }

    #[test]
    fn process_substitution_inner_command_counts() {
        // <(...) is a borrowed SubshellCommand AST node — walk it; inner curl counts,
        // and the pseudo-file is NOT an fs operand for the outer grep.
        assert!(kinds("grep pat <(curl http://y)\n").contains(&(EffectKind::NetFsDb, 7)));
    }

    #[test]
    fn background_launch_is_concurrency_but_a_plain_pipeline_is_not() {
        assert!(kinds("sleep 1 &\n").contains(&(EffectKind::Concurrency, 6))); // background job escapes
        // a plain multi-stage pipeline is bounded/joined — NO concurrency effect (only the stages' own effects)
        assert!(
            !kinds("a | b\n")
                .iter()
                .any(|(k, _)| *k == EffectKind::Concurrency)
        );
    }
}
