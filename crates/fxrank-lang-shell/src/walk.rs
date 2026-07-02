//! The **single** shared recursive descent over a `FnUnit`'s body.
//!
//! Every later detector (`bindings`, `calls`, `mutation`, `risk`) calls [`walk`] or
//! [`walk_commands`] instead of re-implementing the traversal — this is the root-cause
//! fix for the control-flow-blindness bug (a duplicated, drifted descent per detector).
//!
//! `walk` computes `subshell` context structurally (a `( )` subshell, a background `&`,
//! a multi-stage pipeline, a `coproc`, or a process substitution `<(…)`/`>(…)` all open a
//! new shell context) and emits a [`Site`] for every AST location a detector might care
//! about: simple-command invocations, arithmetic commands, nested function definitions,
//! background jobs, `for`-loop iterator variables, pipelines (for adjacency checks like
//! `curl | sh`), and command-level redirects.
//!
//! Command-substitution (`$()`/backticks) recursion is deliberately **not** part of this
//! structural descent — it happens at the effect level in each detector via
//! [`subst_words`]/[`subst_programs`], so `calls.rs` and `mutation.rs` can each recurse
//! their own half without a forward dependency on one another.

use brush_parser::ast;
use brush_parser::ast::SourceLocation;
use brush_parser::{ParserOptions, SourceSpan};

use crate::functions::FnBody;

/// A `SimpleCommand` invocation site, with the structural context needed to score it.
pub struct CmdSite<'a> {
    /// The invoked command itself.
    pub sc: &'a ast::SimpleCommand,
    /// `true` if this site runs in a subshell (`( )`, `&`, a multi-stage pipeline,
    /// `coproc`, or inside a process substitution) rather than the enclosing shell.
    pub subshell: bool,
    /// This command's own redirects (from its `CommandPrefix`/`CommandSuffix`), not
    /// command-level redirects on an enclosing `Command::Compound`/`ExtendedTest`.
    pub redirs: Vec<&'a ast::IoRedirect>,
    /// `true` if stdin is fed by a here-document or here-string (so a `read`/`mapfile`
    /// consuming it is not file IO).
    pub stdin_is_here: bool,
    /// `true` when this site was reached by descending a **process substitution**
    /// `<(…)`/`>(…)` (distinct from a plain `( )` subshell), so effect-owning detectors
    /// (calls/mutation) apply spec §6's `-0.1` substitution-context confidence delta.
    pub subst: bool,
}

/// A structural site produced by [`walk`] — a complete set so every consumer (bindings,
/// calls, mutation, risk) works off the one descent.
pub enum Site<'a> {
    /// A `SimpleCommand` invocation (calls/mutation).
    Command(CmdSite<'a>),
    /// An arithmetic command `(( … ))`, with subshell context (mutation, Task 8).
    Arithmetic(&'a ast::ArithmeticCommand, bool),
    /// A `FunctionDefinition` encountered *inside* a function body — mutation.rs charges
    /// the enclosing unit a `fn-define` `global.mutation`/6. Not emitted for a top-level
    /// (`FnBody::Script`) definition (that's its own `FnUnit`, not a `<script>` mutation).
    FnDefine(&'a ast::FunctionDefinition),
    /// A background job (`&`) or `coproc` launch, with a `(line, col)` span
    /// (calls emits `concurrency`/6, Task 7).
    Concurrency((usize, usize)),
    /// A `for`-loop iterator variable, with the clause's `(line, col)` span (bindings
    /// `local_names`, Task 3).
    ForVar(&'a str, (usize, usize)),
    /// A pipeline, with subshell context (risk's `curl|sh` adjacency, Task 9).
    Pipeline(&'a ast::Pipeline, bool),
    /// A command-level redirect (on `Command::Compound`, `Command::ExtendedTest`, or a
    /// function's own redirect list), with subshell context (calls emits fs effects,
    /// Task 7). A `SimpleCommand`'s own redirects stay on `CmdSite::redirs`.
    Redirect(&'a ast::IoRedirect, bool),
}

/// Threaded traversal context. `in_function` is fixed for the whole call (set once from
/// which `FnBody` variant `walk` started from) — it decides whether a nested
/// `Command::Function` is a real `FnDefine` site or a top-level definition to skip.
#[derive(Clone, Copy)]
struct Ctx {
    subshell: bool,
    subst: bool,
    in_function: bool,
}

fn start_of(loc: &SourceSpan) -> (usize, usize) {
    (loc.start.line, loc.start.column)
}

/// The one recursive descent every detector shares.
pub fn walk<'a>(body: &FnBody<'a>, visit: &mut impl FnMut(Site<'a>)) {
    match body {
        FnBody::Func(fb) => {
            let fb: &'a ast::FunctionBody = fb;
            let ctx = Ctx {
                subshell: false,
                subst: false,
                in_function: true,
            };
            if let Some(rl) = &fb.1 {
                for r in &rl.0 {
                    visit(Site::Redirect(r, ctx.subshell));
                    walk_io_redirect_subs(r, ctx, visit);
                }
            }
            walk_compound(&fb.0, ctx, visit);
        }
        FnBody::Script(items) => {
            let ctx = Ctx {
                subshell: false,
                subst: false,
                in_function: false,
            };
            for item in items {
                walk_item(item, ctx, visit);
            }
        }
    }
}

/// Thin filter over [`walk`] collecting only [`Site::Command`] sites, for detectors
/// (calls/risk) that only classify `SimpleCommand`s.
pub fn walk_commands<'a>(unit: &crate::functions::FnUnit<'a>) -> Vec<CmdSite<'a>> {
    let mut out = Vec::new();
    walk(&unit.body, &mut |site| {
        if let Site::Command(cs) = site {
            out.push(cs);
        }
    });
    out
}

fn walk_compound_list<'a>(list: &'a ast::CompoundList, ctx: Ctx, visit: &mut impl FnMut(Site<'a>)) {
    for item in &list.0 {
        walk_item(item, ctx, visit);
    }
}

fn walk_item<'a>(item: &'a ast::CompoundListItem, ctx: Ctx, visit: &mut impl FnMut(Site<'a>)) {
    let is_async = matches!(item.1, ast::SeparatorOperator::Async);

    for (_, pipeline) in &item.0 {
        let pipe_subshell = ctx.subshell || is_async || pipeline.seq.len() > 1;
        visit(Site::Pipeline(pipeline, pipe_subshell));

        let pipe_ctx = Ctx {
            subshell: pipe_subshell,
            ..ctx
        };
        for cmd in &pipeline.seq {
            walk_command(cmd, pipe_ctx, visit);
        }
    }

    if is_async {
        let sp = item.0.location().map(|l| start_of(&l)).unwrap_or((0, 0));
        visit(Site::Concurrency(sp));
    }
}

fn walk_command<'a>(cmd: &'a ast::Command, ctx: Ctx, visit: &mut impl FnMut(Site<'a>)) {
    match cmd {
        ast::Command::Simple(sc) => {
            let (redirs, stdin_is_here) = collect_redirs(sc);
            visit(Site::Command(CmdSite {
                sc,
                subshell: ctx.subshell,
                redirs,
                stdin_is_here,
                subst: ctx.subst,
            }));
            walk_simple_command_process_subs(sc, ctx, visit);
        }
        ast::Command::Compound(cc, redirs) => {
            if let Some(rl) = redirs {
                for r in &rl.0 {
                    visit(Site::Redirect(r, ctx.subshell));
                    walk_io_redirect_subs(r, ctx, visit);
                }
            }
            walk_compound(cc, ctx, visit);
        }
        ast::Command::Function(def) => {
            if ctx.in_function {
                visit(Site::FnDefine(def));
            }
            // A top-level (`FnBody::Script`) definition is skipped entirely — it is its
            // own `FnUnit` from `collect`, not a `<script>` mutation. Its body is not
            // descended here either way (see `functions::collect`'s own recursion).
        }
        ast::Command::ExtendedTest(_, redirs) => {
            if let Some(rl) = redirs {
                for r in &rl.0 {
                    visit(Site::Redirect(r, ctx.subshell));
                    walk_io_redirect_subs(r, ctx, visit);
                }
            }
        }
    }
}

fn walk_compound<'a>(cc: &'a ast::CompoundCommand, ctx: Ctx, visit: &mut impl FnMut(Site<'a>)) {
    match cc {
        ast::CompoundCommand::Arithmetic(a) => visit(Site::Arithmetic(a, ctx.subshell)),
        ast::CompoundCommand::ArithmeticForClause(afc) => {
            walk_compound_list(&afc.body.list, ctx, visit);
        }
        ast::CompoundCommand::BraceGroup(bg) => walk_compound_list(&bg.list, ctx, visit),
        ast::CompoundCommand::Subshell(sub) => {
            let inner = Ctx {
                subshell: true,
                ..ctx
            };
            walk_compound_list(&sub.list, inner, visit);
        }
        ast::CompoundCommand::ForClause(fc) => {
            visit(Site::ForVar(&fc.variable_name, start_of(&fc.loc)));
            walk_compound_list(&fc.body.list, ctx, visit);
        }
        ast::CompoundCommand::CaseClause(case_cmd) => {
            for item in &case_cmd.cases {
                if let Some(cmd_list) = &item.cmd {
                    walk_compound_list(cmd_list, ctx, visit);
                }
            }
        }
        ast::CompoundCommand::IfClause(ic) => {
            walk_compound_list(&ic.condition, ctx, visit);
            walk_compound_list(&ic.then, ctx, visit);
            if let Some(elses) = &ic.elses {
                for else_clause in elses {
                    if let Some(cond) = &else_clause.condition {
                        walk_compound_list(cond, ctx, visit);
                    }
                    walk_compound_list(&else_clause.body, ctx, visit);
                }
            }
        }
        ast::CompoundCommand::WhileClause(w) | ast::CompoundCommand::UntilClause(w) => {
            walk_compound_list(&w.0, ctx, visit);
            walk_compound_list(&w.1.list, ctx, visit);
        }
        ast::CompoundCommand::Coprocess(cp) => {
            visit(Site::Concurrency(start_of(&cp.loc)));
            let inner = Ctx {
                subshell: true,
                ..ctx
            };
            walk_command(&cp.body, inner, visit);
        }
    }
}

/// Gather a `SimpleCommand`'s own redirects (from its prefix + suffix) and whether stdin
/// is fed by a here-document/here-string.
fn collect_redirs(sc: &ast::SimpleCommand) -> (Vec<&ast::IoRedirect>, bool) {
    let mut redirs = Vec::new();
    let mut stdin_is_here = false;

    for item in prefix_suffix_items(sc) {
        if let ast::CommandPrefixOrSuffixItem::IoRedirect(io) = item {
            redirs.push(io);
            match io {
                ast::IoRedirect::HereDocument(fd, _) | ast::IoRedirect::HereString(fd, _)
                    if fd.is_none() || *fd == Some(0) =>
                {
                    stdin_is_here = true;
                }
                _ => {}
            }
        }
    }

    (redirs, stdin_is_here)
}

/// Descend a `SimpleCommand`'s own process substitutions: both bare `<(…)`/`>(…)`
/// arguments (`CommandPrefixOrSuffixItem::ProcessSubstitution`) and process substitutions
/// used as a redirect target (`IoFileRedirectTarget::ProcessSubstitution`, handled via
/// [`walk_io_redirect_subs`]).
fn walk_simple_command_process_subs<'a>(
    sc: &'a ast::SimpleCommand,
    ctx: Ctx,
    visit: &mut impl FnMut(Site<'a>),
) {
    for item in prefix_suffix_items(sc) {
        match item {
            ast::CommandPrefixOrSuffixItem::ProcessSubstitution(_kind, subshell_cmd) => {
                let inner = Ctx {
                    subshell: true,
                    subst: true,
                    in_function: ctx.in_function,
                };
                walk_compound_list(&subshell_cmd.list, inner, visit);
            }
            ast::CommandPrefixOrSuffixItem::IoRedirect(io) => {
                walk_io_redirect_subs(io, ctx, visit);
            }
            _ => {}
        }
    }
}

/// If `io`'s target is a process substitution, descend its inner command list (subshell +
/// subst context). Applies uniformly to a `SimpleCommand`'s own redirects and to
/// command-level redirects (`Command::Compound`/`ExtendedTest`, a function's redirect
/// list).
fn walk_io_redirect_subs<'a>(io: &'a ast::IoRedirect, ctx: Ctx, visit: &mut impl FnMut(Site<'a>)) {
    if let ast::IoRedirect::File(
        _,
        _,
        ast::IoFileRedirectTarget::ProcessSubstitution(_kind, subshell_cmd),
    ) = io
    {
        let inner = Ctx {
            subshell: true,
            subst: true,
            in_function: ctx.in_function,
        };
        walk_compound_list(&subshell_cmd.list, inner, visit);
    }
}

fn prefix_suffix_items(
    sc: &ast::SimpleCommand,
) -> impl Iterator<Item = &ast::CommandPrefixOrSuffixItem> {
    sc.prefix
        .iter()
        .flat_map(|p| p.0.iter())
        .chain(sc.suffix.iter().flat_map(|s| s.0.iter()))
}

/// A `SimpleCommand`'s words that can carry a command substitution: the command word
/// itself, its arguments, and any `VAR=val` prefix assignment values (`x=$(curl …)`).
pub fn subst_words(sc: &ast::SimpleCommand) -> Vec<&ast::Word> {
    let mut words = Vec::new();

    if let Some(prefix) = &sc.prefix {
        for item in &prefix.0 {
            if let ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _word) = item {
                push_assignment_words(assignment, &mut words);
            }
        }
    }

    if let Some(word) = &sc.word_or_name {
        words.push(word);
    }

    if let Some(suffix) = &sc.suffix {
        for item in &suffix.0 {
            match item {
                ast::CommandPrefixOrSuffixItem::Word(w) => words.push(w),
                ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _word) => {
                    push_assignment_words(assignment, &mut words);
                }
                _ => {}
            }
        }
    }

    words
}

fn push_assignment_words<'a>(assignment: &'a ast::Assignment, words: &mut Vec<&'a ast::Word>) {
    match &assignment.value {
        ast::AssignmentValue::Scalar(w) => words.push(w),
        ast::AssignmentValue::Array(items) => {
            for (key, value) in items {
                if let Some(key) = key {
                    words.push(key);
                }
                words.push(value);
            }
        }
    }
}

/// Word-parse `word`'s pieces (including inside double-quoted sequences) and, for each
/// `CommandSubstitution`/`BackquotedCommandSubstitution` piece, tokenize + parse it into a
/// locally-owned inner `Program`, paired with the enclosing `Word`'s span to re-anchor to
/// (inner spans are substring-relative, not meaningful on their own).
pub fn subst_programs(word: &ast::Word) -> Vec<(SourceSpan, ast::Program)> {
    let mut out = Vec::new();

    let Some(anchor) = word.location() else {
        return out;
    };
    let opts = ParserOptions::default();
    let Ok(pieces) = brush_parser::word::parse(&word.value, &opts) else {
        return out;
    };

    collect_subst_pieces(&pieces, &anchor, &mut out);
    out
}

fn collect_subst_pieces(
    pieces: &[brush_parser::word::WordPieceWithSource],
    anchor: &SourceSpan,
    out: &mut Vec<(SourceSpan, ast::Program)>,
) {
    for piece in pieces {
        match &piece.piece {
            brush_parser::word::WordPiece::CommandSubstitution(text)
            | brush_parser::word::WordPiece::BackquotedCommandSubstitution(text) => {
                if let Ok(tokens) = brush_parser::tokenize_str(text) {
                    let opts = ParserOptions::default();
                    if let Ok(prog) = brush_parser::parse_tokens(&tokens, &opts) {
                        out.push((anchor.clone(), prog));
                    }
                }
            }
            brush_parser::word::WordPiece::DoubleQuotedSequence(inner)
            | brush_parser::word::WordPiece::GettextDoubleQuotedSequence(inner) => {
                collect_subst_pieces(inner, anchor, out);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    #[test]
    fn walk_descends_into_if_then_body() {
        let prog = parse("if true; then rm -rf /x; fi\n").expect("should parse");
        let items: Vec<&ast::CompoundListItem> = prog
            .complete_commands
            .iter()
            .flat_map(|cc| cc.0.iter())
            .collect();
        let body = FnBody::Script(items);

        let mut commands = Vec::new();
        walk(&body, &mut |site| {
            if let Site::Command(cs) = site {
                commands.push(cs);
            }
        });

        let found_rm = commands
            .iter()
            .any(|cs| cs.sc.word_or_name.as_ref().map(|w| w.value.as_str()) == Some("rm"));
        assert!(
            found_rm,
            "expected a `rm` command inside the if-then body, got: {:?}",
            commands
                .iter()
                .map(|cs| cs.sc.word_or_name.as_ref().map(|w| w.value.clone()))
                .collect::<Vec<_>>()
        );
    }

    /// Parse `src` and return its top-level items as a synthetic `<script>` [`FnBody`],
    /// mirroring what `functions::collect` hands `walk` for the `<script>` unit.
    fn script_items(prog: &ast::Program) -> Vec<&ast::CompoundListItem> {
        prog.complete_commands
            .iter()
            .flat_map(|cc| cc.0.iter())
            .collect()
    }

    fn command_word<'a>(cs: &CmdSite<'a>) -> Option<&'a str> {
        cs.sc.word_or_name.as_ref().map(|w| w.value.as_str())
    }

    #[test]
    fn walk_visits_elif_condition() {
        // The `elif` clause's *condition* (`rm -rf /x`) is itself a `CompoundList` that
        // must be descended, not just its `then` body.
        let prog = parse("if a; then b; elif rm -rf /x; then d; fi\n").expect("should parse");
        let body = FnBody::Script(script_items(&prog));

        let mut commands = Vec::new();
        walk(&body, &mut |site| {
            if let Site::Command(cs) = site {
                commands.push(cs);
            }
        });

        let found_rm = commands.iter().any(|cs| command_word(cs) == Some("rm"));
        assert!(
            found_rm,
            "expected the elif condition's `rm` to be visited, got: {:?}",
            commands.iter().map(command_word).collect::<Vec<_>>()
        );
    }

    #[test]
    fn walk_coprocess_emits_concurrency_and_descends_body_in_subshell() {
        let prog = parse("coproc { sleep 1; }\n").expect("should parse");
        let body = FnBody::Script(script_items(&prog));

        let mut saw_concurrency = false;
        let mut sleep_subshell = None;
        walk(&body, &mut |site| match site {
            Site::Concurrency(_) => saw_concurrency = true,
            Site::Command(cs) if command_word(&cs) == Some("sleep") => {
                sleep_subshell = Some(cs.subshell);
            }
            _ => {}
        });

        assert!(
            saw_concurrency,
            "expected a Site::Concurrency for the coproc"
        );
        assert_eq!(
            sleep_subshell,
            Some(true),
            "expected the coproc's inner command to be walked in subshell context"
        );
    }

    #[test]
    fn walk_process_substitution_descends_inline_with_subshell_and_subst() {
        // `<(curl …)` must be walked in place (subshell + subst), distinct from the
        // enclosing `grep`'s own (non-subshell) command site.
        let prog = parse("grep pat <(curl http://y)\n").expect("should parse");
        let body = FnBody::Script(script_items(&prog));

        let mut commands = Vec::new();
        walk(&body, &mut |site| {
            if let Site::Command(cs) = site {
                commands.push(cs);
            }
        });

        let curl = commands
            .iter()
            .find(|cs| command_word(cs) == Some("curl"))
            .expect("expected a `curl` command from the process substitution");
        assert!(
            curl.subshell,
            "process-substitution command should run in subshell context"
        );
        assert!(
            curl.subst,
            "process-substitution command should be marked subst"
        );

        let grep = commands
            .iter()
            .find(|cs| command_word(cs) == Some("grep"))
            .expect("expected the outer `grep` command");
        assert!(
            !grep.subshell,
            "the outer grep command is not itself in a subshell"
        );
    }

    #[test]
    fn walk_multi_stage_pipeline_marks_stages_subshell() {
        let prog = parse("a | b\n").expect("should parse");
        let body = FnBody::Script(script_items(&prog));

        let mut commands = Vec::new();
        walk(&body, &mut |site| {
            if let Site::Command(cs) = site {
                commands.push(cs);
            }
        });

        assert_eq!(commands.len(), 2);
        assert!(
            commands.iter().all(|cs| cs.subshell),
            "every stage of a multi-stage pipeline should be subshell, got: {:?}",
            commands.iter().map(|cs| cs.subshell).collect::<Vec<_>>()
        );
    }

    #[test]
    fn walk_lone_command_is_not_subshell() {
        let prog = parse("a\n").expect("should parse");
        let body = FnBody::Script(script_items(&prog));

        let mut commands = Vec::new();
        walk(&body, &mut |site| {
            if let Site::Command(cs) = site {
                commands.push(cs);
            }
        });

        assert_eq!(commands.len(), 1);
        assert!(
            !commands[0].subshell,
            "a lone, non-backgrounded command is not a subshell"
        );
    }

    #[test]
    fn walk_background_job_emits_concurrency_and_subshell_command() {
        let prog = parse("sleep 1 &\n").expect("should parse");
        let body = FnBody::Script(script_items(&prog));

        let mut saw_concurrency = false;
        let mut sleep_subshell = None;
        walk(&body, &mut |site| match site {
            Site::Concurrency(_) => saw_concurrency = true,
            Site::Command(cs) if command_word(&cs) == Some("sleep") => {
                sleep_subshell = Some(cs.subshell);
            }
            _ => {}
        });

        assert!(
            saw_concurrency,
            "expected a Site::Concurrency for the background job"
        );
        assert_eq!(
            sleep_subshell,
            Some(true),
            "expected the backgrounded command to be walked in subshell context"
        );
    }

    #[test]
    fn walk_for_clause_emits_for_var_site() {
        let prog = parse("for i in a b; do :; done\n").expect("should parse");
        let body = FnBody::Script(script_items(&prog));

        let mut for_var = None;
        walk(&body, &mut |site| {
            if let Site::ForVar(name, _) = site {
                for_var = Some(name.to_string());
            }
        });

        assert_eq!(for_var.as_deref(), Some("i"));
    }
}
