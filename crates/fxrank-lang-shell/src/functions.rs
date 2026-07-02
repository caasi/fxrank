//! Function-unit collection for the Shell frontend.
//!
//! A `FnUnit` is either a real shell function (`name() { … }` / `function name { … }`,
//! including nested definitions found anywhere inside another function's body) or a
//! synthetic `<script>` unit standing in for the file's top-level executable statements
//! (skipped when the top level is only function definitions).

use brush_parser::ast;

/// A function-unit's body — either the whole top-level item list of a script (the
/// synthetic `<script>` unit), or a real function's body.
///
/// `Script` holds **all** top-level `CompoundListItem`s unfiltered — a single item can
/// mix a function definition and a command (e.g. `f(){:;} && rm -rf /x`), so filtering
/// at the item level would wrongly drop the command. The shared descent in `walk.rs`
/// skips `Command::Function` nodes itself when walking a `Script` body.
pub enum FnBody<'a> {
    /// All top-level items of the file (the synthetic `<script>` unit).
    Script(Vec<&'a ast::CompoundListItem>),
    /// A real function's body: the compound command plus its own redirect list
    /// (`f(){…} >out`).
    Func(&'a ast::FunctionBody),
}

/// One function (or synthetic `<script>`) unit to be scored.
pub struct FnUnit<'a> {
    /// The function name, or `"<script>"` for the synthetic top-level unit.
    pub symbol: String,
    /// `path:line:col:symbol` — a unique opaque key within a report (spec 005).
    pub id: String,
    /// The source file path, verbatim.
    pub path: String,
    /// 1-based line of the name anchor.
    pub line: usize,
    /// 1-based column of the name anchor.
    pub col: usize,
    /// The unit's body — see [`FnBody`].
    pub body: FnBody<'a>,
    /// `true` for the synthetic `<script>` unit, `false` for a real function.
    pub is_script: bool,
}

/// Collect one `FnUnit` per `FunctionDefinition` in `prog` (both `name() {}` and
/// `function name {}` forms, including nested definitions found anywhere inside another
/// function's body), plus a synthetic `<script>` unit when the top level has executable
/// (non-definition) statements.
pub fn collect<'a>(prog: &'a ast::Program, path: &str) -> Vec<FnUnit<'a>> {
    let mut units = Vec::new();
    let mut has_non_function = false;
    let mut top_items: Vec<&'a ast::CompoundListItem> = Vec::new();

    for complete_command in &prog.complete_commands {
        for item in &complete_command.0 {
            top_items.push(item);
            collect_item(item, path, &mut units);
            if item_has_non_function_command(item) {
                has_non_function = true;
            }
        }
    }

    if has_non_function {
        units.push(FnUnit {
            symbol: "<script>".to_string(),
            id: format!("{path}:1:1:<script>"),
            path: path.to_string(),
            line: 1,
            col: 1,
            body: FnBody::Script(top_items),
            is_script: true,
        });
    }

    units
}

/// Same-file function name set (top-level + nested), for Tasks 4 & 10.
pub fn defined_function_names(prog: &ast::Program) -> std::collections::HashSet<String> {
    collect(prog, "")
        .into_iter()
        .filter(|u| !u.is_script)
        .map(|u| u.symbol)
        .collect()
}

/// `true` if any command in `item` (across every pipeline stage of its `AndOrList`) is
/// not a function definition — i.e. the item has executable content of its own.
fn item_has_non_function_command(item: &ast::CompoundListItem) -> bool {
    item.0.iter().any(|(_, pipeline)| {
        pipeline
            .seq
            .iter()
            .any(|cmd| !matches!(cmd, ast::Command::Function(_)))
    })
}

/// Recurse through a top-level item hunting for `Command::Function` nodes at any nesting
/// depth (inside `if`/`for`/`while`/`case`/brace/subshell/coprocess bodies).
fn collect_item<'a>(item: &'a ast::CompoundListItem, path: &str, units: &mut Vec<FnUnit<'a>>) {
    for (_, pipeline) in &item.0 {
        for cmd in &pipeline.seq {
            collect_command(cmd, path, units);
        }
    }
}

fn collect_command<'a>(cmd: &'a ast::Command, path: &str, units: &mut Vec<FnUnit<'a>>) {
    match cmd {
        ast::Command::Function(def) => {
            push_fn_unit(def, path, units);
            // Recurse into the newly found function's own body to find further nested
            // definitions (e.g. `outer` nested inside `deploy`).
            collect_compound(&def.body.0, path, units);
        }
        ast::Command::Compound(cc, _redirs) => collect_compound(cc, path, units),
        ast::Command::Simple(_) | ast::Command::ExtendedTest(_, _) => {}
    }
}

fn collect_compound<'a>(cc: &'a ast::CompoundCommand, path: &str, units: &mut Vec<FnUnit<'a>>) {
    match cc {
        ast::CompoundCommand::Arithmetic(_) => {}
        ast::CompoundCommand::ArithmeticForClause(afc) => {
            collect_list(&afc.body.list, path, units);
        }
        ast::CompoundCommand::BraceGroup(bg) => collect_list(&bg.list, path, units),
        ast::CompoundCommand::Subshell(sub) => collect_list(&sub.list, path, units),
        ast::CompoundCommand::ForClause(fc) => collect_list(&fc.body.list, path, units),
        ast::CompoundCommand::CaseClause(case_cmd) => {
            for item in &case_cmd.cases {
                if let Some(cmd_list) = &item.cmd {
                    collect_list(cmd_list, path, units);
                }
            }
        }
        ast::CompoundCommand::IfClause(ic) => {
            collect_list(&ic.condition, path, units);
            collect_list(&ic.then, path, units);
            if let Some(elses) = &ic.elses {
                for else_clause in elses {
                    if let Some(cond) = &else_clause.condition {
                        collect_list(cond, path, units);
                    }
                    collect_list(&else_clause.body, path, units);
                }
            }
        }
        ast::CompoundCommand::WhileClause(w) | ast::CompoundCommand::UntilClause(w) => {
            collect_list(&w.0, path, units);
            collect_list(&w.1.list, path, units);
        }
        ast::CompoundCommand::Coprocess(cp) => collect_command(&cp.body, path, units),
    }
}

fn collect_list<'a>(list: &'a ast::CompoundList, path: &str, units: &mut Vec<FnUnit<'a>>) {
    for item in &list.0 {
        collect_item(item, path, units);
    }
}

fn push_fn_unit<'a>(def: &'a ast::FunctionDefinition, path: &str, units: &mut Vec<FnUnit<'a>>) {
    let (line, col) = crate::span(&def.fname).unwrap_or((0, 0));
    let symbol = def.fname.value.clone();
    let id = format!("{path}:{line}:{col}:{symbol}");
    units.push(FnUnit {
        symbol,
        id,
        path: path.to_string(),
        line,
        col,
        body: FnBody::Func(&def.body),
        is_script: false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn units(name: &str) -> Vec<String> {
        let src = std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap();
        let prog = parse(&src).unwrap();
        collect(&prog, name).into_iter().map(|u| u.symbol).collect()
    }

    #[test]
    fn collects_functions_nested_defs_and_script_unit() {
        let syms = units("functions.sh");
        assert!(syms.contains(&"greet".to_string()));
        assert!(syms.contains(&"deploy".to_string()));
        assert!(syms.contains(&"outer".to_string())); // nested def is its own unit
        assert!(syms.contains(&"<script>".to_string())); // top-level GREETING=hello forces it
    }

    #[test]
    fn no_script_unit_when_only_definitions() {
        let src = "greet() { echo hi; }\n";
        let prog = parse(src).unwrap();
        let syms: Vec<_> = collect(&prog, "x.sh")
            .into_iter()
            .map(|u| u.symbol)
            .collect();
        assert!(!syms.contains(&"<script>".to_string()));
        assert_eq!(syms, vec!["greet".to_string()]);
    }
}
