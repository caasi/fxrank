//! Destructive-fs / privilege-escalation / dynamic-code risk detection for the Shell
//! frontend (spec 029 §6/§9). This is the shell analog of the TS/Python `detect/risk.rs`
//! modules — [`detect`] walks a function's own body for three risk families:
//!
//! - **`DestructiveFs`/5** — the wrapper-peeled inner command is `rm` with a recursive
//!   flag (`-r`/`-rf`/`-R`), `chmod -R`/`chown -R`, or the inherently-destructive
//!   `dd`/`shred`. Shares [`calls::is_destructive_fs`]'s exact recursive-flag rule (the
//!   confidence-delta use in `calls.rs` and the risk-emission use here must not drift).
//! - **`PrivilegeEscalation`/6** — a `sudo`/`su`/`doas` WRAPPER, read from
//!   [`calls::strip_wrappers`]'s peeled `kinds`. Deliberately does **not** inspect the
//!   peeled head word — that is the *wrapped* command, not the privilege escalation
//!   itself (`sudo rm -rf /x` is both a `PrivilegeEscalation`, from the `sudo` wrapper,
//!   AND a `DestructiveFs`, from the inner `rm` — two independent signals on one site).
//! - **`DynamicCode`/7** — `eval` (regardless of its arguments), `source`/`.` with a
//!   COMPUTED (non-literal, containing a `$…` expansion) path argument, and
//!   download-piped-to-shell: a net-fetch pipeline stage (`curl`/`wget`) immediately
//!   followed by a shell-interpreter stage (`sh`/`bash`/`zsh`/`dash`), in either plain
//!   pipeline form (`curl … | sh`) or the command-substitution form
//!   (`sh -c "$(curl …)"` / `` bash -c `curl …` ``).
//!
//! Unquoted-variable confidence deltas are NOT this module's concern — that's a
//! `calls.rs` EFFECT confidence adjustment (Task 5); `RiskFeature` has no confidence
//! field, so this module only ever emits risks, never touches confidence.
//!
//! **`ResMode::BuiltinOnly` consistency (Copilot round-2, refs #15):** [`calls::detect`]
//! already suppresses a command effect for a `builtin foo` whose wrapped word isn't a
//! recognized shell builtin — `builtin` can only invoke a shell builtin, never spawn an
//! external program. This module must agree: `PrivilegeEscalation`, `DestructiveFs`, and
//! the `shell -c "$(download)"` form of `DynamicCode` are all wrapper/inner-command
//! derived, so each is suppressed when [`calls::strip_wrappers`]'s peeled `mode` is
//! [`calls::ResMode::BuiltinOnly`] (a `builtin sudo`/`builtin rm`/`builtin bash -c …` can't
//! actually be privilege escalation, a destructive fs call, or a shell spawn). The plain
//! pipeline form of download-piped-to-shell inherits the same guard through
//! [`simple_head`], which now treats a `BuiltinOnly` stage as headless so `builtin curl`
//! can't satisfy the net-fetch adjacency check either. `eval` and computed-path
//! `source`/`.` are NOT wrapper-dependent (they're bash keywords/builtins in their own
//! right) and are intentionally left unguarded.

use brush_parser::ParserOptions;
use brush_parser::ast;
use brush_parser::word::{self, WordPiece, WordPieceWithSource};

use fxrank_core::effect::{RiskFeature, RiskKind, Tier};
use fxrank_core::score::weight_for_class;

use super::calls::{ResMode, is_destructive_fs, strip_wrappers};
use crate::functions::FnUnit;
use crate::walk::{self, Site};

/// Commands that fetch remote content — the download half of the download-piped-to-shell
/// signal.
const NET_FETCH: &[&str] = &["curl", "wget"];
/// Shell interpreters — the exec half of the download-piped-to-shell signal.
const SHELL_INTERP: &[&str] = &["sh", "bash", "zsh", "dash"];
/// Privilege-escalation wrapper words, as peeled into [`calls::CommandView::kinds`] by
/// [`strip_wrappers`].
const PRIV_WRAPPERS: &[&str] = &["sudo", "su", "doas"];

/// Detect `DestructiveFs`/`PrivilegeEscalation`/`DynamicCode` risk features in `unit`'s
/// own body. `path` is the source file path embedded verbatim in each [`RiskFeature`].
pub fn detect(unit: &FnUnit, path: &str) -> Vec<RiskFeature> {
    let mut out = Vec::new();
    walk::walk(&unit.body, &mut |site| match site {
        Site::Command(cs) => detect_command_site(cs.sc, path, &mut out),
        Site::Pipeline(pipe, _subshell) => detect_pipeline(pipe, path, &mut out),
        _ => {}
    });
    out
}

/// Classify one `SimpleCommand` invocation site: the wrapper-peeled view drives
/// `PrivilegeEscalation` (from `kinds`) and `DestructiveFs`/`eval`/`source`-with-computed-
/// path (from the peeled `head`/`args`); the raw `-c`-argument scan drives the
/// substitution form of download-piped-to-shell.
fn detect_command_site(sc: &ast::SimpleCommand, path: &str, out: &mut Vec<RiskFeature>) {
    let view = strip_wrappers(sc);
    let span = span_of(sc);

    if view.mode != ResMode::BuiltinOnly
        && view
            .kinds
            .iter()
            .any(|k| PRIV_WRAPPERS.contains(&k.as_str()))
    {
        out.push(mk_risk(
            RiskKind::PrivilegeEscalation,
            path,
            span,
            "sudo/su/doas",
        ));
    }

    let Some(head) = view.head.as_deref() else {
        return;
    };

    if view.mode != ResMode::BuiltinOnly && is_destructive_fs(head, &view.args) {
        out.push(mk_risk(RiskKind::DestructiveFs, path, span, head));
    }

    if head == "eval" {
        out.push(mk_risk(RiskKind::DynamicCode, path, span, "eval"));
    }

    if (head == "source" || head == ".") && view.args.first().is_some_and(|w| is_computed_path(w)) {
        out.push(mk_risk(RiskKind::DynamicCode, path, span, head));
    }

    if view.mode != ResMode::BuiltinOnly
        && SHELL_INTERP.contains(&head)
        && let Some(idx) = view.args.iter().position(|w| w.value == "-c")
        && let Some(cword) = view.args.get(idx + 1)
        && walk::subst_programs(cword)
            .iter()
            .any(|(_, prog)| program_has_net_fetch(prog))
    {
        out.push(mk_risk(
            RiskKind::DynamicCode,
            path,
            span,
            "shell -c $(download)",
        ));
    }
}

/// A pipeline's adjacent-stage scan for the plain (non-substitution) form of
/// download-piped-to-shell: a net-fetch stage immediately followed by a shell-interpreter
/// stage, anywhere in `pipe.seq` (not just a two-stage pipeline — a net-fetch stage
/// directly followed by a shell-interpreter stage is caught no matter where in a longer
/// pipeline it occurs, e.g. `curl … | sh | wc`). This is literal-adjacency only —
/// `windows(2)` sees `(curl, tee)` and `(tee, sh)`, so `curl … | tee f | sh` does NOT
/// qualify (a `tee`-relay breaks the adjacent pair); real data-flow tracking through
/// intermediate stages is a Milestone-B item.
fn detect_pipeline(pipe: &ast::Pipeline, path: &str, out: &mut Vec<RiskFeature>) {
    for pair in pipe.seq.windows(2) {
        let [a, b] = pair else { continue };
        let (Some(a_sc), Some(a_head)) = simple_head(a) else {
            continue;
        };
        let (Some(_), Some(b_head)) = simple_head(b) else {
            continue;
        };
        if NET_FETCH.contains(&a_head.as_str()) && SHELL_INTERP.contains(&b_head.as_str()) {
            out.push(mk_risk(
                RiskKind::DynamicCode,
                path,
                span_of(a_sc),
                "curl|sh",
            ));
        }
    }
}

/// The wrapper-peeled `SimpleCommand`/head-word of a pipeline stage, or `(None, None)` for
/// a non-`Command::Simple` stage (a compound command can't be a `curl`/`sh` stage). Also
/// `(Some(sc), None)` when the peeled view is [`ResMode::BuiltinOnly`] — a `builtin curl`
/// stage can never be a real net-fetch/shell-interpreter, so it must not satisfy the
/// adjacency check in [`detect_pipeline`]/[`program_has_net_fetch`].
fn simple_head(cmd: &ast::Command) -> (Option<&ast::SimpleCommand>, Option<String>) {
    match cmd {
        ast::Command::Simple(sc) => {
            let view = strip_wrappers(sc);
            let head = if view.mode == ResMode::BuiltinOnly {
                None
            } else {
                view.head
            };
            (Some(sc), head)
        }
        _ => (None, None),
    }
}

/// `true` if `prog`'s top-level commands contain a [`NET_FETCH`] invocation (`curl`/`wget`,
/// wrapper-peeled) — the inner-program check for the `sh -c "$(curl …)"` substitution form.
fn program_has_net_fetch(prog: &ast::Program) -> bool {
    prog.complete_commands
        .iter()
        .flat_map(|cc| cc.0.iter())
        .flat_map(|item| item.0.iter())
        .flat_map(|(_, pipeline)| pipeline.seq.iter())
        .any(|cmd| {
            simple_head(cmd)
                .1
                .is_some_and(|h| NET_FETCH.contains(&h.as_str()))
        })
}

/// `true` for a "computed" path argument — one carrying a real `$…` expansion (a
/// parameter expansion, command substitution, or arithmetic expression), as opposed to a
/// literal path. Heuristic (spec §9): `source "$dir/x"` is computed, `source ./lib.sh` is
/// not, and — the fix for a Copilot-flagged false positive — `source '$dir/x'`
/// (single-quoted) is ALSO not computed: `Word::value` retains the raw quote characters,
/// so a naive `.contains('$')` on the raw text wrongly flags a literal single-quoted `$`
/// as an expansion. This parses the word into pieces via [`word::parse`] and only counts
/// it computed if an actual expansion piece is present — a `$` inside
/// [`WordPiece::SingleQuotedText`] is literal text, never an expansion. `pub(crate)` —
/// `detect/refs.rs` (Task 10) reuses this exact rule to decide whether a `source`/`.` site
/// gets an opaque path-keyed ref, so the literal-vs-computed heuristic can't drift between
/// the risk-emission use here and the ref-emission use there.
pub(crate) fn is_computed_path(word: &ast::Word) -> bool {
    let opts = ParserOptions::default();
    let Ok(pieces) = word::parse(&word.value, &opts) else {
        // Unparseable text is treated conservatively as literal (not computed) — this
        // mirrors the pre-fix behavior for text that legitimately contains a `$` but
        // can't be re-parsed as a word (an extreme edge case).
        return false;
    };
    pieces_have_expansion(&pieces)
}

/// Recursively `true` if any piece in `pieces` is an actual expansion — a parameter
/// expansion, command substitution (backquoted or `$()`), or arithmetic expression.
/// Double-quoted sequences are descended into (an expansion can be nested inside quotes);
/// `SingleQuotedText`/`Text`/`AnsiCQuotedText`/`TildeExpansion`/`EscapeSequence` are all
/// non-computed by construction.
fn pieces_have_expansion(pieces: &[WordPieceWithSource]) -> bool {
    pieces.iter().any(|p| match &p.piece {
        WordPiece::ParameterExpansion(_)
        | WordPiece::CommandSubstitution(_)
        | WordPiece::BackquotedCommandSubstitution(_)
        | WordPiece::ArithmeticExpression(_) => true,
        WordPiece::DoubleQuotedSequence(inner) | WordPiece::GettextDoubleQuotedSequence(inner) => {
            pieces_have_expansion(inner)
        }
        _ => false,
    })
}

fn span_of(sc: &ast::SimpleCommand) -> (usize, usize) {
    crate::span(sc).unwrap_or((0, 0))
}

fn mk_risk(kind: RiskKind, path: &str, span: (usize, usize), evidence: &str) -> RiskFeature {
    let class = kind.class();
    RiskFeature {
        kind,
        class,
        weight: weight_for_class(class),
        path: path.to_string(),
        line: span.0,
        col: span.1,
        evidence: evidence.to_string(),
        tier: Tier::Heuristic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{functions::collect, parse};
    use fxrank_core::effect::RiskKind;

    fn risks(src: &str) -> Vec<RiskKind> {
        let prog = parse(src).unwrap();
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.is_script)
            .unwrap();
        detect(&unit, "x.sh").into_iter().map(|r| r.kind).collect()
    }

    #[test]
    fn detects_shell_risks() {
        assert!(risks("rm -rf /x\n").contains(&RiskKind::DestructiveFs));
        assert!(risks("chmod -R 777 /x\n").contains(&RiskKind::DestructiveFs));
        assert!(risks("sudo rm -rf /x\n").contains(&RiskKind::PrivilegeEscalation));
        assert!(risks("eval \"$cmd\"\n").contains(&RiskKind::DynamicCode));
        assert!(risks("curl http://x | sh\n").contains(&RiskKind::DynamicCode));
        assert!(risks("source \"$dir/x\"\n").contains(&RiskKind::DynamicCode)); // computed path
    }

    #[test]
    fn sudo_rm_rf_yields_both_privilege_and_destructive() {
        // The privilege escalation comes from the `sudo` WRAPPER; the destructive-fs
        // signal comes from the wrapped `rm -rf` — two independent risks on one site.
        let ks = risks("sudo rm -rf /x\n");
        assert!(ks.contains(&RiskKind::PrivilegeEscalation));
        assert!(ks.contains(&RiskKind::DestructiveFs));
    }

    #[test]
    fn literal_source_path_is_not_dynamic_code() {
        assert!(!risks("source ./lib.sh\n").contains(&RiskKind::DynamicCode));
    }

    #[test]
    fn single_quoted_dollar_in_source_path_is_not_dynamic_code() {
        // `$dir` inside single quotes is LITERAL text (no expansion) — a naive
        // `.contains('$')` on the raw quoted text would wrongly flag this as computed.
        assert!(!risks("source '$dir/x'\n").contains(&RiskKind::DynamicCode));
    }

    #[test]
    fn double_quoted_dollar_in_source_path_is_still_dynamic_code() {
        // the real expansion case must keep working after switching to a piece-based scan.
        assert!(risks("source \"$dir/x\"\n").contains(&RiskKind::DynamicCode));
    }

    #[test]
    fn plain_multi_stage_pipeline_without_shell_is_not_dynamic_code() {
        assert!(!risks("curl http://x | grep y\n").contains(&RiskKind::DynamicCode));
    }

    #[test]
    fn download_piped_to_shell_substitution_form() {
        assert!(risks("sh -c \"$(curl http://x)\"\n").contains(&RiskKind::DynamicCode));
    }

    #[test]
    fn download_pipe_caught_inside_if_body() {
        // The pipeline site is surfaced by `walk::walk` even nested in control flow.
        assert!(risks("if true; then curl http://x | sh; fi\n").contains(&RiskKind::DynamicCode));
    }

    // --- ResMode::BuiltinOnly consistency (Copilot round-2, refs #15) ---

    #[test]
    fn builtin_rm_is_not_destructive_fs() {
        // `builtin rm -rf /x` can never invoke the external `rm` — `builtin` only
        // resolves to a shell builtin, and `rm` isn't one.
        assert!(!risks("builtin rm -rf /x\n").contains(&RiskKind::DestructiveFs));
    }

    #[test]
    fn builtin_sudo_rm_yields_neither_privilege_nor_destructive() {
        // The whole site is builtin-only (`sudo` itself can't be a real privilege-escalation
        // wrapper here, nor can the wrapped `rm` be a real destructive-fs spawn).
        let ks = risks("builtin sudo rm -rf /x\n");
        assert!(!ks.contains(&RiskKind::PrivilegeEscalation));
        assert!(!ks.contains(&RiskKind::DestructiveFs));
    }

    #[test]
    fn builtin_bash_c_download_substitution_is_not_dynamic_code() {
        assert!(!risks("builtin bash -c \"$(curl http://y)\"\n").contains(&RiskKind::DynamicCode));
    }

    #[test]
    fn builtin_curl_piped_to_sh_is_not_dynamic_code() {
        // The `builtin curl` stage isn't a real net-fetch — `curl` isn't a shell builtin,
        // so `builtin curl` can't satisfy the download-piped-to-shell adjacency check.
        assert!(!risks("builtin curl http://x | sh\n").contains(&RiskKind::DynamicCode));
    }
}
