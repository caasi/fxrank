//! Declaration-vs-hidden mutation model — the shell analog of `mutation.rs` in the other
//! three frontends (spec 029 §7).
//!
//! The organizing question for every write is: **is this an assignment, inside a
//! function, to a name it did not declare `local`?** If yes → hidden write
//! (`global.mutation`/6, escaping). Otherwise → a declaration (top-level: no effect at
//! all, like a Rust `static` declaration; function-local: `local.mutation`/1, contained).
//! `calls.rs` deliberately emits **nothing** for assignment/`cd`/`export`/`unset`/…
//! (its `DECL`/`MUT_OWNED` name lists return `Cls::NoEffect`) — this module is the SOLE
//! owner of every mutation-family effect, so there is never a double-emit between the
//! two detectors.
//!
//! ## Strategy
//!
//! [`detect`] iterates the **full** [`walk::walk`] `Site` stream (not just
//! `Site::Command`) so mutations inside control flow are covered, and each of the three
//! non-`SimpleCommand` write sites gets its own arm: [`Site::Arithmetic`] (`(( x++ ))`,
//! raw-string lvalue scan), [`Site::FnDefine`] (a nested `function` installs into bash's
//! single global function namespace — `global.mutation`/6 on the enclosing unit), and
//! every [`Site::Command`] (the bulk of the model — assignment, `local`/`declare`/
//! `typeset`, `readonly`, `let`, `shift`/`set --`, `cd`/`pushd`/`popd`/`set`/`shopt`/
//! `umask`/`ulimit`, `export`/`unset`, `read`/`mapfile`/`readarray`/`getopts`, `printf
//! -v`, an assignment-prefix `VAR=v cmd`, and `${x:=…}` anywhere in any word).
//!
//! Almost every named-target write funnels through [`classify_plain_write`] — the single
//! declaration-vs-hidden gate (`unit.is_script` → no effect; name in `locals` → contained
//! `local.mutation`/1; else → escaping `global.mutation`/6). `export`/`unset`/`cd`-family/
//! computed-target writes are dedicated branches (spec §7's table has them diverge from
//! the generic ladder).
//!
//! Command substitution (`$()`/backticks) is recursed at the effect level (own half —
//! `calls.rs` recurses its own half separately, avoiding a forward dependency between the
//! two detectors): every result is **forced `contained = true`** (a subshell never
//! escapes to the enclosing scope) and re-anchored to the substitution's word span.

use std::collections::HashSet;

use brush_parser::ParserOptions;
use brush_parser::ast;
use brush_parser::word::{self, Parameter, ParameterExpr, WordPiece, WordPieceWithSource};

use fxrank_core::effect::{Effect, EffectKind, Tier};
use fxrank_core::score::{BoundaryCoverage, apply_boundary_discount, weight_for_class};

use crate::bindings::local_names;
use crate::functions::{FnBody, FnUnit};
use crate::walk::{self, CmdSite, Site};

/// `local`/`declare`/`typeset` (no `-g`/`-x`) / `shift` / `set --` land here.
const DECL_FAMILY: &[&str] = &["local", "declare", "typeset"];

/// Detect mutation effects in `unit`'s own body (the full [`walk::walk`] `Site` stream).
///
/// Returns `(Effect, contained)` pairs, mirroring the Python frontend's tuple return —
/// the `bool` is the containment flag Task 11's `gather` folds into `Effect.contained`
/// (this module leaves that struct field at its default; the tuple is authoritative).
///
/// `top` (the script's top-level binding names, [`crate::bindings::script_top_names`]) is
/// threaded through to the command-substitution recursion below, not consulted for
/// classification itself — the declaration-vs-hidden gate is purely `unit.is_script`
/// (top-level) vs "inside a function" (spec §7's organizing question), not membership in
/// any particular name set.
pub fn detect(unit: &FnUnit, top: &HashSet<String>) -> Vec<(Effect, bool)> {
    let locals = local_names(unit);
    let mut out = Vec::new();

    walk::walk(&unit.body, &mut |site| match site {
        Site::Command(cs) => detect_command_site(&cs, unit.is_script, &locals, top, &mut out),
        Site::Arithmetic(ac, subshell) => {
            if let Some(name) = extract_arith_lvalue(&ac.expr.value) {
                let span = crate::span(ac).unwrap_or((0, 0));
                emit_named_write(
                    &mut out,
                    &name,
                    unit.is_script,
                    &locals,
                    subshell,
                    false,
                    span,
                    "arith:",
                );
            }
        }
        Site::FnDefine(def) => {
            let span = crate::span(&def.fname).unwrap_or((0, 0));
            push_write(
                &mut out,
                EffectKind::GlobalMutation,
                false,
                false,
                Some("fn-define"),
                span,
                format!("fn-define:{}", def.fname.value),
                false,
                false,
            );
        }
        _ => {}
    });

    out
}

/// Classify one [`CmdSite`]: an assignment-prefix (`VAR=v cmd`, whenever a command word
/// is present), the per-command-name mutation rules, a `${x:=…}` scan over every word,
/// and the command-substitution recursion (own half).
fn detect_command_site(
    cs: &CmdSite,
    unit_is_script: bool,
    locals: &HashSet<String>,
    top: &HashSet<String>,
    out: &mut Vec<(Effect, bool)>,
) {
    let sc = cs.sc;
    let subshell = cs.subshell;
    let subst = cs.subst;

    match &sc.word_or_name {
        None => {
            // A bare `FOO=bar` (no command word) — a real assignment, not a scoped
            // temporary env prefix.
            for item in prefix_items(sc) {
                if let ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _word) = item {
                    let name = assignment_name(&assignment.name);
                    emit_named_write(
                        out,
                        &name,
                        unit_is_script,
                        locals,
                        subshell,
                        subst,
                        span_of(sc),
                        "assign:",
                    );
                }
            }
        }
        Some(word) => {
            // Assignment prefix (`VAR=v cmd`): a temporary, scoped env for THIS command
            // invocation only — never a script mutation, regardless of which command
            // follows (spec §7's "Assignment prefix" rule).
            for item in prefix_items(sc) {
                if let ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _word) = item {
                    let name = assignment_name(&assignment.name);
                    push_write(
                        out,
                        EffectKind::EnvWrite,
                        subshell,
                        false,
                        None,
                        span_of(sc),
                        format!("prefix-assign:{name}"),
                        false,
                        subst,
                    );
                }
            }

            match word.value.as_str() {
                name if DECL_FAMILY.contains(&name) => {
                    handle_decl_family(sc, unit_is_script, locals, subshell, subst, out)
                }
                "readonly" => handle_readonly(sc, unit_is_script, locals, subshell, subst, out),
                "let" => handle_let(sc, unit_is_script, locals, subshell, subst, out),
                "shift" => push_write(
                    out,
                    EffectKind::LocalMutation,
                    true,
                    false,
                    None,
                    span_of(sc),
                    "shift".to_string(),
                    false,
                    subst,
                ),
                "set" => handle_set(sc, subshell, subst, out),
                "shopt" | "umask" | "ulimit" | "cd" | "pushd" | "popd" => push_write(
                    out,
                    EffectKind::GlobalMutation,
                    subshell,
                    false,
                    None,
                    span_of(sc),
                    format!("mut:{}", word.value),
                    false,
                    subst,
                ),
                "export" => handle_export(sc, subshell, subst, out),
                "unset" => handle_unset(sc, locals, subshell, subst, out),
                name @ ("read" | "mapfile" | "readarray") => {
                    handle_read_targets(sc, name, unit_is_script, locals, subshell, subst, out)
                }
                "printf" => handle_printf_v(sc, unit_is_script, locals, subshell, subst, out),
                "getopts" => handle_getopts(sc, unit_is_script, locals, subshell, subst, out),
                _ => {}
            }
        }
    }

    scan_param_assign_defaults(sc, unit_is_script, locals, subshell, subst, out);
    recurse_command_substitutions(sc, top, out);
}

/// `local`/`declare`/`typeset` (no `-g`): the assigned name(s) go through
/// [`classify_plain_write`] (already `locals`-aware — a `-g` name was excluded from
/// `locals` at the `bindings::local_names` prescan, so it naturally falls through to the
/// escaping `global.mutation` branch with no extra branching here). `-x` (export) is an
/// orthogonal axis from `-g` (scope) — `declare -gx VAR=val` (a common "global +
/// exported" idiom) genuinely crosses the process boundary, so `-x` additionally exports
/// each name (`env.write`/6) regardless of whether `-g` is also present.
fn handle_decl_family(
    sc: &ast::SimpleCommand,
    unit_is_script: bool,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    out: &mut Vec<(Effect, bool)>,
) {
    let has_x = has_short_flag_char(sc, 'x');

    for item in suffix_items(sc) {
        if let ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _word) = item {
            let name = assignment_name(&assignment.name);
            emit_named_write(
                out,
                &name,
                unit_is_script,
                locals,
                subshell,
                subst,
                span_of(sc),
                "decl:",
            );
            if has_x {
                push_write(
                    out,
                    EffectKind::EnvWrite,
                    subshell,
                    false,
                    None,
                    span_of(sc),
                    format!("decl-x:{name}"),
                    false,
                    subst,
                );
            }
        }
    }
}

/// `readonly` never creates local scope (spec §7) — its target name is never in `locals`
/// by construction (`bindings::local_names` doesn't scan it), so it correctly funnels
/// through [`classify_plain_write`] to "declaration" at script level or escaping
/// `global.mutation`/6 inside a function.
fn handle_readonly(
    sc: &ast::SimpleCommand,
    unit_is_script: bool,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    out: &mut Vec<(Effect, bool)>,
) {
    for item in suffix_items(sc) {
        if let ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _word) = item {
            let name = assignment_name(&assignment.name);
            emit_named_write(
                out,
                &name,
                unit_is_script,
                locals,
                subshell,
                subst,
                span_of(sc),
                "readonly:",
            );
        }
    }
}

/// `let x=1` (simple form) parses its operand as an `AssignmentWord` (same grammar path
/// as any `NAME=value`-shaped suffix token); `let x++`/`let "x += 1"` (no top-level `=`)
/// stays a plain `Word` and needs the raw-text [`extract_arith_lvalue`] scan.
fn handle_let(
    sc: &ast::SimpleCommand,
    unit_is_script: bool,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    out: &mut Vec<(Effect, bool)>,
) {
    for item in suffix_items(sc) {
        match item {
            ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _word) => {
                let name = assignment_name(&assignment.name);
                emit_named_write(
                    out,
                    &name,
                    unit_is_script,
                    locals,
                    subshell,
                    subst,
                    span_of(sc),
                    "let:",
                );
            }
            ast::CommandPrefixOrSuffixItem::Word(w) => {
                if let Some(name) = extract_arith_lvalue(&w.value) {
                    emit_named_write(
                        out,
                        &name,
                        unit_is_script,
                        locals,
                        subshell,
                        subst,
                        span_of(sc),
                        "let:",
                    );
                }
            }
            _ => {}
        }
    }
}

/// `set --` is the positional-param mutation (`local.mutation`/1, contained — spec §7);
/// any other `set` invocation (`set -e`, `set -o pipefail`, …) changes shell options →
/// `global.mutation`/6.
fn handle_set(sc: &ast::SimpleCommand, subshell: bool, subst: bool, out: &mut Vec<(Effect, bool)>) {
    let is_dashdash = operand_words(sc).first().is_some_and(|w| w.value == "--");
    if is_dashdash {
        push_write(
            out,
            EffectKind::LocalMutation,
            true,
            false,
            None,
            span_of(sc),
            "set --".to_string(),
            false,
            subst,
        );
    } else {
        push_write(
            out,
            EffectKind::GlobalMutation,
            subshell,
            false,
            None,
            span_of(sc),
            "set".to_string(),
            false,
            subst,
        );
    }
}

/// `export X=…`/`export -n X`: every named target (an `AssignmentWord`, or a bare `Word`
/// for `export X` / `export -n X`'s un-export target) crosses the process boundary →
/// `env.write`/6, unconditionally (not gated by `unit_is_script`/`locals` — exporting is
/// a different axis, "honest cross-process", spec §2).
fn handle_export(
    sc: &ast::SimpleCommand,
    subshell: bool,
    subst: bool,
    out: &mut Vec<(Effect, bool)>,
) {
    for item in suffix_items(sc) {
        let name = match item {
            ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _word) => {
                Some(assignment_name(&assignment.name))
            }
            ast::CommandPrefixOrSuffixItem::Word(w) if !is_flag(&w.value) => Some(w.value.clone()),
            _ => None,
        };
        if let Some(name) = name {
            push_write(
                out,
                EffectKind::EnvWrite,
                subshell,
                false,
                None,
                span_of(sc),
                format!("export:{name}"),
                false,
                subst,
            );
        }
    }
}

/// `unset -f name` (removes a *function*, a global-namespace write) → `global.mutation`/6.
/// `unset x` on a `locals`-declared name stays a contained `local.mutation`/1; any other
/// `unset X` (exported/global) → `env.write`/6 (spec §7).
fn handle_unset(
    sc: &ast::SimpleCommand,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    out: &mut Vec<(Effect, bool)>,
) {
    let removes_fn = has_flag_word(sc, "-f");
    for w in operand_words(sc) {
        if is_flag(&w.value) {
            continue;
        }
        let name = w.value.as_str();
        if removes_fn {
            push_write(
                out,
                EffectKind::GlobalMutation,
                subshell,
                false,
                None,
                span_of(sc),
                format!("unset-f:{name}"),
                false,
                subst,
            );
        } else if locals.contains(name) {
            push_write(
                out,
                EffectKind::LocalMutation,
                true,
                false,
                None,
                span_of(sc),
                format!("unset:{name}"),
                false,
                subst,
            );
        } else {
            push_write(
                out,
                EffectKind::EnvWrite,
                subshell,
                false,
                None,
                span_of(sc),
                format!("unset:{name}"),
                false,
                subst,
            );
        }
    }
}

/// `read`/`mapfile`/`readarray` targets: every non-flag operand word is a write target
/// (best-effort — a recognized value-taking flag, [`value_taking_flag`], consumes its
/// following word so it is never mistaken for a target). A computed target (`read
/// "$name"`) is never below a named one (spec §7) — always `global.mutation`/6 hidden.
fn handle_read_targets(
    sc: &ast::SimpleCommand,
    cmd: &str,
    unit_is_script: bool,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    out: &mut Vec<(Effect, bool)>,
) {
    let words = operand_words(sc);
    let mut i = 0;
    while i < words.len() {
        let value = words[i].value.as_str();
        if is_flag(value) {
            i += 1;
            if value_taking_flag(cmd, value) {
                i += 1;
            }
            continue;
        }
        emit_target(
            out,
            value,
            unit_is_script,
            locals,
            subshell,
            subst,
            span_of(sc),
            &format!("{cmd}:"),
        );
        i += 1;
    }
}

/// `printf -v TARGET`: the word immediately following `-v` is the write target (`printf`
/// with no `-v` is `calls.rs`'s `logging`/2 effect — this function is a no-op then).
fn handle_printf_v(
    sc: &ast::SimpleCommand,
    unit_is_script: bool,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    out: &mut Vec<(Effect, bool)>,
) {
    let words = operand_words(sc);
    let Some(pos) = words.iter().position(|w| w.value == "-v") else {
        return;
    };
    let Some(target) = words.get(pos + 1) else {
        return;
    };
    emit_target(
        out,
        &target.value,
        unit_is_script,
        locals,
        subshell,
        subst,
        span_of(sc),
        "printf-v:",
    );
}

/// `getopts optstring name`: `name` (the second positional) is the write target.
fn handle_getopts(
    sc: &ast::SimpleCommand,
    unit_is_script: bool,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    out: &mut Vec<(Effect, bool)>,
) {
    let words = operand_words(sc);
    let Some(target) = words.get(1) else {
        return;
    };
    emit_target(
        out,
        &target.value,
        unit_is_script,
        locals,
        subshell,
        subst,
        span_of(sc),
        "getopts:",
    );
}

/// `${x:=word}` (and `${x[i]:=word}`) — a parameter-expansion side effect that can appear
/// in ANY word of ANY command, not just assignment/declaration syntax. Reuses
/// [`walk::subst_words`] (the same word set `calls.rs` scans for `$()`/backticks — it
/// already gathers the command word, its arguments, and prefix-assignment VALUE words).
/// `${!ref:=word}` (bang-indirection) is a computed target — never below a named one.
fn scan_param_assign_defaults(
    sc: &ast::SimpleCommand,
    unit_is_script: bool,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    out: &mut Vec<(Effect, bool)>,
) {
    let opts = ParserOptions::default();
    for w in walk::subst_words(sc) {
        let Ok(pieces) = word::parse(&w.value, &opts) else {
            continue;
        };
        scan_pieces(
            &pieces,
            unit_is_script,
            locals,
            subshell,
            subst,
            span_of(sc),
            out,
        );
    }
}

fn scan_pieces(
    pieces: &[WordPieceWithSource],
    unit_is_script: bool,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    span: (usize, usize),
    out: &mut Vec<(Effect, bool)>,
) {
    for piece in pieces {
        match &piece.piece {
            WordPiece::ParameterExpansion(ParameterExpr::AssignDefaultValues {
                parameter,
                indirect,
                ..
            }) => {
                if let Some(name) = parameter_base_name(parameter) {
                    if *indirect {
                        push_write(
                            out,
                            EffectKind::GlobalMutation,
                            subshell,
                            true,
                            Some("indirect-assign"),
                            span,
                            format!("param-assign-default:{name}(indirect)"),
                            true,
                            subst,
                        );
                    } else {
                        emit_named_write(
                            out,
                            name,
                            unit_is_script,
                            locals,
                            subshell,
                            subst,
                            span,
                            "param-assign-default:",
                        );
                    }
                }
            }
            WordPiece::DoubleQuotedSequence(inner)
            | WordPiece::GettextDoubleQuotedSequence(inner) => {
                scan_pieces(inner, unit_is_script, locals, subshell, subst, span, out);
            }
            _ => {}
        }
    }
}

/// `$()`/backtick command substitutions inside `sc`'s words — text, re-parsed and
/// recursed at the effect level (own half; `calls.rs` recurses its own half separately,
/// avoiding a forward dependency between the two detectors). Every inner effect is
/// **forced `contained = true`** (a subshell never escapes) and re-anchored to the
/// enclosing `Word`'s span, with confidence reduced by 0.1 (spec §6 substitution-context
/// delta). The transient inner `Program` is owned locally and drops at the end of each
/// iteration — no `&'a` leak into the caller.
fn recurse_command_substitutions(
    sc: &ast::SimpleCommand,
    top: &HashSet<String>,
    out: &mut Vec<(Effect, bool)>,
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
            for (mut eff, _contained) in detect(&transient, top) {
                eff.line = line;
                eff.col = col;
                eff.confidence = (eff.confidence - 0.1).max(0.1);
                out.push((eff, true));
            }
        }
    }
}

// ─── the shared declaration-vs-hidden gate ─────────────────────────────────────────────

/// Spec §7's organizing question, reduced to one gate: `unit.is_script` (top-level) → no
/// effect (a declaration, like a Rust `static` declaration); name declared `local` in
/// this function (`locals`) → contained `local.mutation`/1; otherwise (inside a function,
/// not declared `local` here) → escaping `global.mutation`/6 (the shell "spooky action at
/// a distance").
fn classify_plain_write(
    name: &str,
    unit_is_script: bool,
    locals: &HashSet<String>,
) -> Option<(EffectKind, bool)> {
    if unit_is_script {
        None
    } else if locals.contains(name) {
        Some((EffectKind::LocalMutation, true))
    } else {
        Some((EffectKind::GlobalMutation, false))
    }
}

/// Classify a literal (non-computed) named write via [`classify_plain_write`] and push it
/// (a no-op when the gate returns `None` — the top-level-declaration case).
#[allow(clippy::too_many_arguments)]
fn emit_named_write(
    out: &mut Vec<(Effect, bool)>,
    name: &str,
    unit_is_script: bool,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    span: (usize, usize),
    evidence_prefix: &str,
) {
    if let Some((kind, base_contained)) = classify_plain_write(name, unit_is_script, locals) {
        push_write(
            out,
            kind,
            base_contained || subshell,
            false,
            None,
            span,
            format!("{evidence_prefix}{name}"),
            false,
            subst,
        );
    }
}

/// A `read`/`mapfile`/`printf -v`/`getopts` write target: literal (a bare identifier,
/// optionally with an array subscript) funnels through [`emit_named_write`]; anything
/// else (a quoted/expanded computed name) is an indirect write — always
/// `global.mutation`/6 hidden, never below a plain named non-local write (spec §7).
#[allow(clippy::too_many_arguments)]
fn emit_target(
    out: &mut Vec<(Effect, bool)>,
    text: &str,
    unit_is_script: bool,
    locals: &HashSet<String>,
    subshell: bool,
    subst: bool,
    span: (usize, usize),
    evidence_prefix: &str,
) {
    if is_literal_target(text) {
        emit_named_write(
            out,
            text,
            unit_is_script,
            locals,
            subshell,
            subst,
            span,
            evidence_prefix,
        );
    } else {
        push_write(
            out,
            EffectKind::GlobalMutation,
            subshell,
            true,
            Some("indirect-assign"),
            span,
            format!("{evidence_prefix}{text}"),
            true,
            subst,
        );
    }
}

/// Push one mutation `(Effect, contained)` pair. Runs `class` through
/// [`apply_boundary_discount`] with [`BoundaryCoverage::None`] — a no-op (shift 0) kept
/// for auditability so the "no discount channel" is explicit in code, per spec §7.
/// Confidence: a literal-target write is 0.9, a computed/indirect one (`computed`) is
/// 0.5; either way a `-0.1` delta applies when `subst` (a process-substitution site).
/// `Effect.contained` itself is left at its struct default — the returned `bool` is
/// authoritative; Task 11's `gather` sets `Effect.contained` from it.
#[allow(clippy::too_many_arguments)]
fn push_write(
    out: &mut Vec<(Effect, bool)>,
    kind: EffectKind,
    contained: bool,
    hidden: bool,
    subreason: Option<&str>,
    span: (usize, usize),
    evidence: String,
    computed: bool,
    subst: bool,
) {
    let class = apply_boundary_discount(kind.base_class(), BoundaryCoverage::None, contained);
    let mut confidence = if computed { 0.5 } else { 0.9 };
    if subst {
        confidence = (confidence - 0.1_f64).max(0.1);
    }
    let (line, col) = span;
    out.push((
        Effect {
            kind,
            class,
            discounted_to: None,
            weight: weight_for_class(class),
            line,
            col,
            tier: Tier::Heuristic,
            hidden,
            contained: false,
            evidence,
            discount: None,
            subreason: subreason.map(str::to_string),
            confidence,
        },
        contained,
    ));
}

// ─── small AST/text helpers ─────────────────────────────────────────────────────────────

fn prefix_items(sc: &ast::SimpleCommand) -> impl Iterator<Item = &ast::CommandPrefixOrSuffixItem> {
    sc.prefix.iter().flat_map(|p| p.0.iter())
}

fn suffix_items(sc: &ast::SimpleCommand) -> impl Iterator<Item = &ast::CommandPrefixOrSuffixItem> {
    sc.suffix.iter().flat_map(|s| s.0.iter())
}

/// A `SimpleCommand`'s plain (non-assignment, non-redirect, non-process-substitution)
/// suffix argument words, in order — mirrors `calls.rs`'s `operand_words`.
fn operand_words(sc: &ast::SimpleCommand) -> Vec<&ast::Word> {
    suffix_items(sc)
        .filter_map(|item| match item {
            ast::CommandPrefixOrSuffixItem::Word(w) => Some(w),
            _ => None,
        })
        .collect()
}

/// The base variable name of an `AssignmentName` — for `ArrayElementName`, the array's
/// own name (mirrors `bindings.rs::assignment_name`; duplicated locally since that helper
/// is private and this module owns a distinct write-classification concern).
fn assignment_name(name: &ast::AssignmentName) -> String {
    match name {
        ast::AssignmentName::VariableName(n) => n.clone(),
        ast::AssignmentName::ArrayElementName(n, _) => n.clone(),
    }
}

/// The base name backing a parameter (used for `${x:=…}`/`${arr[i]:=…}`); `None` for a
/// positional (`$1`) or special (`$?`) parameter — those can't be assignment targets.
fn parameter_base_name(p: &Parameter) -> Option<&str> {
    match p {
        Parameter::Named(n) => Some(n.as_str()),
        Parameter::NamedWithIndex { name, .. } => Some(name.as_str()),
        Parameter::NamedWithAllIndices { name, .. } => Some(name.as_str()),
        Parameter::Positional(_) | Parameter::Special(_) => None,
    }
}

fn span_of(sc: &ast::SimpleCommand) -> (usize, usize) {
    crate::span(sc).unwrap_or((0, 0))
}

fn is_flag(value: &str) -> bool {
    value.starts_with('-') && value != "-"
}

/// `true` for a short-flag word (`-x`, `-gx`) carrying `ch` anywhere in a combined
/// cluster — mirrors `bindings.rs::has_global_flag`, generalized to any flag character.
fn has_short_flag_char(sc: &ast::SimpleCommand, ch: char) -> bool {
    operand_words(sc)
        .iter()
        .any(|w| w.value.starts_with('-') && !w.value.starts_with("--") && w.value.contains(ch))
}

fn has_flag_word(sc: &ast::SimpleCommand, flag: &str) -> bool {
    operand_words(sc).iter().any(|w| w.value == flag)
}

/// `true` if `flag` takes a following value word for `cmd`'s family (a minimal
/// best-effort table, matching `calls.rs::wrapper_flag_takes_value`'s convention) — so
/// [`handle_read_targets`] doesn't mistake a flag's value (`read -p "prompt" name` →
/// `"prompt"`, `mapfile -n 5 arr` → `5`) for a write target.
fn value_taking_flag(cmd: &str, flag: &str) -> bool {
    match cmd {
        "read" => matches!(flag, "-d" | "-n" | "-N" | "-p" | "-t" | "-u"),
        "mapfile" | "readarray" => matches!(flag, "-c" | "-C" | "-d" | "-n" | "-O" | "-s" | "-u"),
        _ => false,
    }
}

/// `true` for a bare identifier, optionally with an array subscript (`x`, `arr[0]`) — a
/// literal, auditable write target. Anything else (a quoted/expanded string like
/// `"$var"`) is a computed/indirect target.
fn is_literal_target(text: &str) -> bool {
    let base = text.split('[').next().unwrap_or(text);
    let mut chars = base.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Best-effort scan of a raw arithmetic-expression / bare-`let`-operand string for its
/// assigned lvalue: a leading `NAME` immediately followed by an assignment operator
/// (`=`, `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `++`, `--`), or a prefix
/// `++NAME`/`--NAME`. Multiple comma-separated sub-expressions (`(( i=0, j=1 ))`) only
/// report the first — an accepted heuristic edge (matches this frontend's other
/// best-effort option/operand scans).
fn extract_arith_lvalue(text: &str) -> Option<String> {
    let trimmed = text.trim().trim_matches(|c| c == '"' || c == '\'');

    if let Some(ident) = leading_ident(trimmed) {
        let rest = trimmed[ident.len()..].trim_start();
        let assigns = rest.starts_with("++")
            || rest.starts_with("--")
            || rest.starts_with("+=")
            || rest.starts_with("-=")
            || rest.starts_with("*=")
            || rest.starts_with("/=")
            || rest.starts_with("%=")
            || rest.starts_with("&=")
            || rest.starts_with("|=")
            || rest.starts_with("^=")
            || (rest.starts_with('=') && !rest.starts_with("=="));
        if assigns {
            return Some(ident.to_string());
        }
    }

    let prefixed = trimmed
        .strip_prefix("++")
        .or_else(|| trimmed.strip_prefix("--"))?;
    leading_ident(prefixed.trim_start()).map(str::to_string)
}

fn leading_ident(text: &str) -> Option<&str> {
    let mut end = 0;
    for (i, c) in text.char_indices() {
        if c.is_ascii_alphanumeric() || c == '_' {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let ident = &text[..end];
    ident
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic() || *c == '_')?;
    Some(ident)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bindings::script_top_names, functions::collect, parse};
    use fxrank_core::effect::EffectKind;

    fn muts(src: &str, sym: &str) -> Vec<(EffectKind, u8, bool)> {
        let prog = parse(src).unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == sym)
            .unwrap();
        detect(&unit, &top)
            .into_iter()
            .map(|(e, c)| (e.kind, e.class, c))
            .collect()
    }

    #[test]
    fn declaration_vs_hidden_ladder() {
        // top-level FOO=bar → declaration → no mutation effect
        let prog = parse("FOO=bar\n").unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.is_script)
            .unwrap();
        assert!(detect(&unit, &top).is_empty());
        // local x=1 → local.mutation/1 contained
        assert!(muts("f(){ local x=1; }\n", "f").contains(&(EffectKind::LocalMutation, 1, true)));
        // bare non-local write in a function → global.mutation/6 escaping
        assert!(muts("f(){ y=1; }\n", "f").contains(&(EffectKind::GlobalMutation, 6, false)));
        // export → env.write/6
        assert!(muts("f(){ export Z=1; }\n", "f").contains(&(EffectKind::EnvWrite, 6, false)));
        // cd → global.mutation/6
        assert!(muts("f(){ cd /x; }\n", "f").contains(&(EffectKind::GlobalMutation, 6, false)));
    }

    #[test]
    fn subshell_mutation_is_contained() {
        // cd inside $(...) does not escape → contained
        let v = muts("f(){ x=$(cd /y && pwd); }\n", "f");
        assert!(
            v.iter()
                .any(|(k, _, c)| *k == EffectKind::GlobalMutation && *c)
        );
    }

    #[test]
    fn indirect_write_is_global_hidden_not_below_named() {
        let prog = parse("f(){ printf -v \"$n\" x; }\n").unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "f")
            .unwrap();
        let e = &detect(&unit, &top)[0].0;
        assert_eq!(
            (e.kind, e.class, e.hidden),
            (EffectKind::GlobalMutation, 6, true)
        );
        assert_eq!(e.subreason.as_deref(), Some("indirect-assign"));
    }

    #[test]
    fn nested_function_definition_is_global_mutation() {
        // defining inner() inside outer() installs it into the GLOBAL fn namespace
        let prog = parse("outer(){ inner(){ :; }; }\n").unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "outer")
            .unwrap();
        let effects = detect(&unit, &top);
        let (e, contained) = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::GlobalMutation)
            .expect("expected a GlobalMutation effect for the nested fn-define");
        assert_eq!(e.class, 6);
        assert!(!*contained);
        assert_eq!(e.subreason.as_deref(), Some("fn-define"));
    }

    #[test]
    fn assignment_prefix_is_scoped_env_write() {
        // VAR=v cmd → temporary env for that command (env.write), NOT a script mutation
        let v = muts("f(){ FOO=1 curl http://x; }\n", "f");
        assert!(v.iter().any(|(k, _, _)| *k == EffectKind::EnvWrite));
    }

    #[test]
    fn arithmetic_write_follows_the_same_ladder() {
        // (( x++ )) on a non-local name inside a function → global.mutation/6 escaping
        assert!(muts("f(){ (( x++ )); }\n", "f").contains(&(EffectKind::GlobalMutation, 6, false)));
        // (( y = 1 )) on a declared-local name stays contained
        assert!(muts("f(){ local y=0; (( y = 1 )); }\n", "f").contains(&(
            EffectKind::LocalMutation,
            1,
            true
        )));
        // top-level (( x = 1 )) is a declaration → no effect
        let prog = parse("(( x = 1 ))\n").unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.is_script)
            .unwrap();
        assert!(detect(&unit, &top).is_empty());
    }

    #[test]
    fn let_bare_increment_is_hidden_non_local() {
        assert!(muts("f(){ let x++; }\n", "f").contains(&(EffectKind::GlobalMutation, 6, false)));
    }

    #[test]
    fn readonly_inside_function_is_global_mutation_not_local() {
        // readonly does NOT create local scope, even inside a function
        assert!(muts("f(){ readonly x=1; }\n", "f").contains(&(
            EffectKind::GlobalMutation,
            6,
            false
        )));
    }

    #[test]
    fn declare_x_emits_local_and_env_write() {
        let v = muts("f(){ declare -x FOO=1; }\n", "f");
        assert!(v.contains(&(EffectKind::LocalMutation, 1, true)));
        assert!(v.iter().any(|(k, _, _)| *k == EffectKind::EnvWrite));
    }

    #[test]
    fn declare_g_escapes_regardless_of_x() {
        // -g dominates scope: a global write, not a local one (bindings::local_names
        // already excludes a -g'd name from `locals`).
        assert!(muts("f(){ declare -g x=1; }\n", "f").contains(&(
            EffectKind::GlobalMutation,
            6,
            false
        )));
    }

    #[test]
    fn declare_gx_still_exports() {
        // -g (scope) and -x (export) are orthogonal axes: a combined -gx cluster must
        // still emit the EnvWrite even though the name resolves to a global write, not a
        // local one.
        let v = muts("f(){ declare -gx VAR=1; }\n", "f");
        assert!(v.contains(&(EffectKind::GlobalMutation, 6, false)));
        assert!(v.iter().any(|(k, _, _)| *k == EffectKind::EnvWrite));

        // `typeset -gx n=2` — same orthogonal-axis check under `typeset`. (A bare `-gx n`
        // with no `=` hits the pre-existing, disclosed "no-assignment-operand" gap — see
        // the module's `is_literal_target`/AssignmentWord-only scan — which is unrelated
        // to this export-gate fix and out of scope here.)
        let v = muts("f(){ typeset -gx n=2; }\n", "f");
        assert!(v.iter().any(|(k, _, _)| *k == EffectKind::EnvWrite));
    }

    #[test]
    fn unset_local_is_contained_unset_global_is_env_write() {
        assert!(muts("f(){ local x=1; unset x; }\n", "f").contains(&(
            EffectKind::LocalMutation,
            1,
            true
        )));
        assert!(muts("f(){ unset Y; }\n", "f").contains(&(EffectKind::EnvWrite, 6, false)));
        assert!(muts("f(){ unset -f g; }\n", "f").contains(&(
            EffectKind::GlobalMutation,
            6,
            false
        )));
    }

    #[test]
    fn param_assign_default_follows_the_same_ladder() {
        assert!(muts("f(){ : \"${x:=1}\"; }\n", "f").contains(&(
            EffectKind::GlobalMutation,
            6,
            false
        )));
    }

    #[test]
    fn getopts_target_follows_the_same_ladder() {
        assert!(
            muts("f(){ while getopts \"ab\" opt; do :; done; }\n", "f").contains(&(
                EffectKind::GlobalMutation,
                6,
                false
            ))
        );
    }

    #[test]
    fn confidence_ladder_matches_spec_section_6() {
        // literal-target write → confidence 0.9 (spec §6)
        let prog = parse("f(){ local x=1; }\n").unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "f")
            .unwrap();
        let effects = detect(&unit, &top);
        let (e, _) = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::LocalMutation)
            .expect("expected a LocalMutation effect");
        assert_eq!(e.confidence, 0.9);

        // computed/indirect target → confidence 0.5 (spec §6)
        let prog = parse("f(){ printf -v \"$n\" x; }\n").unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "f")
            .unwrap();
        let effects = detect(&unit, &top);
        let (e, _) = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::GlobalMutation)
            .expect("expected a GlobalMutation effect");
        assert_eq!(e.confidence, 0.5);

        // -0.1 substitution-context delta applied to a mutation recursed out of
        // `x=$(cd /y)`'s inner `cd /y` (a literal-target write, 0.9 base, minus the 0.1
        // substitution delta → 0.8).
        let prog = parse("f(){ x=$(cd /y); }\n").unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "f")
            .unwrap();
        let effects = detect(&unit, &top);
        let (e, _) = effects
            .iter()
            .find(|(e, _)| e.kind == EffectKind::GlobalMutation && e.evidence.starts_with("mut:cd"))
            .expect("expected the recursed `cd` effect from the command substitution");
        assert_eq!(e.confidence, 0.8);
    }

    #[test]
    fn top_level_read_target_is_a_declaration() {
        let prog = parse("read x\n").unwrap();
        let top = script_top_names(&prog);
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.is_script)
            .unwrap();
        assert!(detect(&unit, &top).is_empty());
    }
}
