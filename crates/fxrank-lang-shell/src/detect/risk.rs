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

use brush_parser::ast;

use fxrank_core::effect::{RiskFeature, RiskKind, Tier};
use fxrank_core::score::weight_for_class;

use super::calls::{is_destructive_fs, strip_wrappers};
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

    if view
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

    if is_destructive_fs(head, &view.args) {
        out.push(mk_risk(RiskKind::DestructiveFs, path, span, head));
    }

    if head == "eval" {
        out.push(mk_risk(RiskKind::DynamicCode, path, span, "eval"));
    }

    if (head == "source" || head == ".") && view.args.first().is_some_and(|w| is_computed_path(w)) {
        out.push(mk_risk(RiskKind::DynamicCode, path, span, head));
    }

    if SHELL_INTERP.contains(&head)
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
/// stage, anywhere in `pipe.seq` (not just a two-stage pipeline — `curl … | tee f | sh`
/// still has the qualifying adjacent pair).
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
/// a non-`Command::Simple` stage (a compound command can't be a `curl`/`sh` stage).
fn simple_head(cmd: &ast::Command) -> (Option<&ast::SimpleCommand>, Option<String>) {
    match cmd {
        ast::Command::Simple(sc) => (Some(sc), strip_wrappers(sc).head),
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

/// `true` for a "computed" path argument — one carrying a `$…` expansion (a variable or
/// command substitution), as opposed to a literal path. Heuristic (spec §9): `source
/// "$dir/x"` is computed, `source ./lib.sh` is not. `pub(crate)` — `detect/refs.rs`
/// (Task 10) reuses this exact rule to decide whether a `source`/`.` site gets an opaque
/// path-keyed ref, so the literal-vs-computed heuristic can't drift between the risk-
/// emission use here and the ref-emission use there.
pub(crate) fn is_computed_path(word: &ast::Word) -> bool {
    word.value.contains('$')
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
}
