//! Same-file call refs + `source`/`.` opaque reach + `canonical_path` (spec 025/029 §4/§9).
//!
//! [`canonical_path`] gives every `FnUnit` a path unique **within the file**
//! (`[path, "fn", name]` for a real function, `[path, "<script>"]` for the synthetic
//! script unit) — this is what makes the shell partition "adopted" by the core fold, so
//! resolution runs through the exact `CanonicalIndex` instead of the ambiguity-prone flat
//! `SymbolIndex` (spec 025-3e §4.1).
//!
//! [`refs`] emits one [`CallSiteRef`] per command site that is either a **resolvable
//! same-file function call** (`resolved_target = Some([path,"fn",name])`, first-party, not
//! qualified) or a **`source`/`.` with a literal path argument** (an opaque, path-keyed
//! ref: `qualified: true`, `resolved_target: None`, `base` = the literal path text, so the
//! core fold's reach specifier — `module.unwrap_or(base)` — is the path itself, not the
//! bare word `"source"`). A `source`/`.` with a COMPUTED path (`"$dir/x"`) gets no ref at
//! all — [`super::risk`]'s `DynamicCode` risk (Task 9) plus the own `process.control`/6
//! effect (`calls.rs`) already represent it; a fabricated `base` from unresolved `$…` text
//! would be garbage.
//!
//! Resolution-mode discipline (spec §4): a same-file function call is only recognized
//! under [`ResMode::Normal`] — [`strip_wrappers`] peeling a [`ResMode::FunctionBypass`]
//! wrapper (`sudo`/`command`/…) or [`ResMode::BuiltinOnly`] (`builtin`) means the wrapped
//! word can never resolve to a local function (`sudo greet` does not call this file's
//! `greet`).
//!
//! [`refs`] also recurses into command substitutions (`$()`/backticks) inside a command's
//! words — mirroring `calls.rs::recurse_command_substitutions`'s own half — so a same-file
//! function called ONLY inside a substitution (`x=$(greet)`) still emits a resolvable
//! `CallSiteRef` instead of vanishing (`calls.rs` already suppresses that call's own-body
//! effect on the promise "it's a call ref, handled here").

use std::collections::HashSet;

use brush_parser::ast;

use fxrank_core::record::{CallSiteRef, RefKind};

use super::calls::{ResMode, strip_wrappers};
use super::risk::is_computed_path;
use crate::functions::{FnBody, FnUnit};
use crate::walk;

/// This unit's canonical path, unique within its file: `[path, "fn", name]` for a real
/// function, `[path, "<script>"]` for the synthetic `<script>` unit.
pub fn canonical_path(unit: &FnUnit) -> Vec<String> {
    if unit.is_script {
        vec![unit.path.clone(), "<script>".to_string()]
    } else {
        vec![unit.path.clone(), "fn".to_string(), unit.symbol.clone()]
    }
}

/// Emit a [`CallSiteRef`] for every same-file function call and literal-path `source`/`.`
/// site in `unit`'s body — see the module doc for the two shapes — plus, for each command
/// site, the same two shapes found by recursing into that command's `$()`/backtick
/// substitutions (see [`recurse_command_substitutions`]).
pub fn refs(unit: &FnUnit, fns: &HashSet<String>) -> Vec<CallSiteRef> {
    let mut out = Vec::new();

    for cs in walk::walk_commands(unit) {
        let view = strip_wrappers(cs.sc);
        let (line, col) = crate::span(cs.sc).unwrap_or((0, 0));

        if let Some(head) = view.head.clone() {
            if view.mode == ResMode::Normal && fns.contains(&head) {
                out.push(CallSiteRef {
                    kind: RefKind::Free,
                    base: head.clone(),
                    module: None,
                    line,
                    col,
                    qualified: false,
                    first_party: true,
                    resolved_target: Some(vec![unit.path.clone(), "fn".to_string(), head]),
                });
            } else if (head == "source" || head == ".")
                && let Some(arg) = view.args.first()
                && !is_computed_path(arg)
            {
                out.push(CallSiteRef {
                    kind: RefKind::Free,
                    base: arg.value.clone(),
                    module: None,
                    line,
                    col,
                    qualified: true,
                    first_party: false,
                    resolved_target: None,
                });
            }
        }

        recurse_command_substitutions(cs.sc, unit, fns, &mut out);
    }

    out
}

/// `$()`/backtick command substitutions inside `sc`'s words (command word, args, and
/// `VAR=val` prefix assignment values — [`walk::subst_words`]) — text, re-parsed and
/// recursed at the ref level (own half; `calls.rs` recurses its own half separately, so a
/// same-file function called only inside a substitution, e.g. `x=$(greet)`, still resolves
/// instead of vanishing). Each inner ref is re-anchored to the enclosing `Word`'s span
/// (inner spans are substring-relative to the substitution text, not meaningful on their
/// own — mirrors `calls.rs::recurse_command_substitutions`). The transient inner unit
/// inherits `unit.path` (same file) so a nested `resolved_target` is keyed correctly; it is
/// owned locally and drops at the end of each iteration — no `&'a` leak into the caller.
fn recurse_command_substitutions(
    sc: &ast::SimpleCommand,
    unit: &FnUnit,
    fns: &HashSet<String>,
    out: &mut Vec<CallSiteRef>,
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
                path: unit.path.clone(),
                line: 0,
                col: 0,
                body: FnBody::Script(items),
                is_script: true,
            };
            let (line, col) = (anchor.start.line, anchor.start.column);
            for mut r in refs(&transient, fns) {
                r.line = line;
                r.col = col;
                out.push(r);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        functions::{collect, defined_function_names},
        parse,
    };

    #[test]
    fn same_file_function_call_is_resolved_target() {
        let src = "greet(){ echo hi; }\nmain(){ greet; }\n";
        let prog = parse(src).unwrap();
        let fns = defined_function_names(&prog);
        let main = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "main")
            .unwrap();
        let r = refs(&main, &fns);
        assert_eq!(r.len(), 1);
        assert_eq!(
            r[0].resolved_target,
            Some(vec!["x.sh".into(), "fn".into(), "greet".into()])
        );
        assert!(r[0].first_party && !r[0].qualified);
    }

    #[test]
    fn source_is_opaque_path_keyed_ref() {
        let src = "main(){ source ./lib.sh; }\n";
        let prog = parse(src).unwrap();
        let fns = defined_function_names(&prog);
        let main = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "main")
            .unwrap();
        let r = refs(&main, &fns);
        // opaque + qualified + base is the PATH (so the fold reach is path-keyed, not "source")
        assert!(
            r.iter()
                .any(|x| x.qualified && x.resolved_target.is_none() && x.base == "./lib.sh")
        );
    }

    #[test]
    fn wrapped_command_word_is_not_a_same_file_function() {
        // spec §4: `sudo docker` / `command docker` must NOT resolve to a same-file docker()
        let src = "docker(){ :; }\nmain(){ sudo docker ps; command docker ps; }\n";
        let prog = parse(src).unwrap();
        let fns = defined_function_names(&prog);
        let main = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "main")
            .unwrap();
        let r = refs(&main, &fns);
        assert!(
            !r.iter()
                .any(|x| x.resolved_target
                    == Some(vec!["x.sh".into(), "fn".into(), "docker".into()]))
        );
    }

    #[test]
    fn computed_source_path_emits_no_ref() {
        let src = "main(){ source \"$dir/x\"; }\n";
        let prog = parse(src).unwrap();
        let fns = defined_function_names(&prog);
        let main = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "main")
            .unwrap();
        let r = refs(&main, &fns);
        assert!(r.is_empty());
    }

    #[test]
    fn same_file_function_called_inside_command_substitution_is_resolved() {
        // greet() is called ONLY inside `$(…)` — must still emit a resolvable ref, or the
        // call vanishes entirely (calls.rs already suppresses its own-body effect on the
        // promise this ref exists).
        let src = "greet(){ echo hi; }\nmain(){ x=$(greet); }\n";
        let prog = parse(src).unwrap();
        let fns = defined_function_names(&prog);
        let main = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "main")
            .unwrap();
        let r = refs(&main, &fns);
        assert!(
            r.iter().any(
                |x| x.resolved_target == Some(vec!["x.sh".into(), "fn".into(), "greet".into()])
            )
        );
    }

    #[test]
    fn canonical_path_shapes() {
        let src = "greet(){ :; }\necho hi\n";
        let prog = parse(src).unwrap();
        let units = collect(&prog, "x.sh");
        let greet = units.iter().find(|u| u.symbol == "greet").unwrap();
        let script = units.iter().find(|u| u.is_script).unwrap();
        assert_eq!(
            canonical_path(greet),
            vec!["x.sh".to_string(), "fn".to_string(), "greet".to_string()]
        );
        assert_eq!(
            canonical_path(script),
            vec!["x.sh".to_string(), "<script>".to_string()]
        );
    }
}
