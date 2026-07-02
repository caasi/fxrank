//! Per-function local-name pre-scan + script-top binding set.
//!
//! The scope model that `mutation.rs` (Task 8) consults for declaration-vs-hidden
//! classification: a write to a name in [`local_names`] is a declared, contained local
//! binding; a write to a name that isn't local and isn't a [`script_top_names`] top-level
//! binding is a captured/global write.

use std::collections::HashSet;

use brush_parser::ast;

use crate::functions::FnUnit;
use crate::walk::{Site, walk};

/// Names declared **local in this function**: `local`, `declare`/`typeset` (without a
/// `-g` flag — that escapes to global scope), and `for`-loop iteration variables.
///
/// **Excluded by construction** (not in the recognized-command set below): `readonly`
/// (declares a constant, not local scope), `select` (no AST variant in brush-parser
/// 0.4.0), and `read`/`mapfile` (deliberate non-local writes to the caller's scope, spec
/// §7).
///
/// Calls the shared [`walk`] descent (Task 2) so `local`s nested inside `if`/`for`/
/// `while`/`case` bodies are found — this must not re-implement the traversal.
pub fn local_names(unit: &FnUnit<'_>) -> HashSet<String> {
    let mut names = HashSet::new();

    walk(&unit.body, &mut |site| match site {
        Site::Command(cs) => {
            let Some(word) = &cs.sc.word_or_name else {
                return;
            };
            if !matches!(word.value.as_str(), "local" | "declare" | "typeset") {
                return;
            }
            if has_global_flag(cs.sc) {
                return;
            }
            names.extend(assigned_names(cs.sc));
        }
        Site::ForVar(name, _) => {
            names.insert(name.to_string());
        }
        _ => {}
    });

    names
}

/// Names assigned at the script's top level: bare `Assignment`s and pure `VAR=val`
/// `SimpleCommand`s (no command word — a temporary env-var prefix on a command, like
/// `FOO=bar ls`, does not persist a binding, so it's excluded by the `word_or_name.is_none()`
/// check below).
pub fn script_top_names(prog: &ast::Program) -> HashSet<String> {
    let mut names = HashSet::new();

    for complete_command in &prog.complete_commands {
        for item in &complete_command.0 {
            for (_, pipeline) in &item.0 {
                for cmd in &pipeline.seq {
                    if let ast::Command::Simple(sc) = cmd {
                        if sc.word_or_name.is_none() {
                            names.extend(assigned_names(sc));
                        }
                    }
                }
            }
        }
    }

    names
}

/// `true` if `sc`'s suffix carries a `-g` short-flag anywhere in a short-flag cluster —
/// `declare -g`/`typeset -g`/`declare -gx`/`typeset -gi` all escape their assignments to
/// global scope, so they are not local names. brush-parser keeps a combined short-flag
/// cluster (`-gx`) as a single atomic `Word`, and bash treats `-g` anywhere in such a
/// cluster as global, so this checks for a `g` character in a short-flag word rather than
/// an exact `-g` match. `declare`/`typeset` have no long options in bash, so excluding
/// `--`-prefixed words is safe (a long option can never carry `-g`'s meaning here).
fn has_global_flag(sc: &ast::SimpleCommand) -> bool {
    let Some(suffix) = &sc.suffix else {
        return false;
    };
    suffix.0.iter().any(|item| {
        matches!(item, ast::CommandPrefixOrSuffixItem::Word(w)
            if w.value.starts_with('-') && !w.value.starts_with("--") && w.value.contains('g'))
    })
}

/// The variable names assigned by `sc`'s prefix + suffix `AssignmentWord` items.
fn assigned_names(sc: &ast::SimpleCommand) -> Vec<String> {
    sc.prefix
        .iter()
        .flat_map(|p| p.0.iter())
        .chain(sc.suffix.iter().flat_map(|s| s.0.iter()))
        .filter_map(|item| match item {
            ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, _word) => {
                Some(assignment_name(&assignment.name))
            }
            _ => None,
        })
        .collect()
}

/// The base variable name of an `AssignmentName` — for `ArrayElementName`, the array's
/// own name (the binding that scope tracking cares about), not the index expression.
fn assignment_name(name: &ast::AssignmentName) -> String {
    match name {
        ast::AssignmentName::VariableName(n) => n.clone(),
        ast::AssignmentName::ArrayElementName(n, _) => n.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{functions::collect, parse};

    #[test]
    fn local_names_include_local_declare_but_not_readonly() {
        let src = "f(){ local a=1; declare b=2; readonly c=3; declare -g d=4; for e in x; do :; done; }\n";
        let prog = parse(src).unwrap();
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "f")
            .unwrap();
        let names = local_names(&unit);
        assert!(names.contains("a") && names.contains("b") && names.contains("e"));
        assert!(!names.contains("c"), "readonly does not create local scope");
        assert!(!names.contains("d"), "declare -g is global, not local");
    }

    #[test]
    fn local_names_finds_locals_nested_in_control_flow() {
        let src = "f(){ if true; then local x=1; fi; for i in a b; do local y=2; done; }\n";
        let prog = parse(src).unwrap();
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "f")
            .unwrap();
        let names = local_names(&unit);
        assert!(
            names.contains("x"),
            "local nested in an if-body must be found"
        );
        assert!(
            names.contains("y"),
            "local nested in a for-body must be found"
        );
        assert!(
            names.contains("i"),
            "the for loop's own iteration var must be found"
        );
    }

    #[test]
    fn local_names_excludes_read_mapfile_select() {
        let src = "f(){ read a; mapfile -t b; typeset c=1; }\n";
        let prog = parse(src).unwrap();
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "f")
            .unwrap();
        let names = local_names(&unit);
        assert!(
            !names.contains("a"),
            "read is a deliberate non-local write, not local scope"
        );
        assert!(names.contains("c"), "typeset without -g is local scope");
    }

    #[test]
    fn local_names_excludes_combined_short_flag_global_cluster() {
        let src = "f(){ declare -gx FOO=1; typeset -gi n=2; }\n";
        let prog = parse(src).unwrap();
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "f")
            .unwrap();
        let names = local_names(&unit);
        assert!(
            !names.contains("FOO"),
            "declare -gx is a combined short-flag cluster containing g — global, not local"
        );
        assert!(
            !names.contains("n"),
            "typeset -gi is a combined short-flag cluster containing g — global, not local"
        );
    }

    #[test]
    fn local_names_finds_array_element_assignment_base_name() {
        let src = "f(){ local arr[0]=1; }\n";
        let prog = parse(src).unwrap();
        let unit = collect(&prog, "x.sh")
            .into_iter()
            .find(|u| u.symbol == "f")
            .unwrap();
        let names = local_names(&unit);
        assert!(
            names.contains("arr"),
            "local arr[0]=1 is an ArrayElementName assignment; its base name arr is the local binding"
        );
    }

    #[test]
    fn script_top_names_collects_bare_and_pure_assignments() {
        let src = "GREETING=hello\nFOO=bar ls\ngreet() { local x=1; }\n";
        let prog = parse(src).unwrap();
        let names = script_top_names(&prog);
        assert!(
            names.contains("GREETING"),
            "a bare top-level VAR=val assignment is a script-top binding"
        );
        assert!(
            !names.contains("FOO"),
            "a temporary env-var prefix on a command (FOO=bar ls) does not persist"
        );
        assert!(
            !names.contains("x"),
            "a function's own local is not a script-top binding"
        );
    }
}
