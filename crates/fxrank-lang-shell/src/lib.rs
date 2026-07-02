//! Shell (Bash/POSIX) frontend for fxrank — SPIKE scaffold (Task 0).
//!
//! # Confirmed `brush-parser` 0.4.0 API (spike findings)
//!
//! 1. **Tokenize → parse entry point** (both re-exported at the crate root):
//!    - `brush_parser::tokenize_str(text: &str) -> Result<Vec<Token>, TokenizerError>`
//!    - `brush_parser::parse_tokens(tokens: &[Token], options: &ParserOptions) -> Result<ast::Program, ParseError>`
//!      (2-arg, confirmed — no `SourceInfo` argument in this version).
//!    - `ast::Program { pub complete_commands: Vec<CompleteCommand> }` — the top-level
//!      command list.
//!
//! 2. **Line/col access**: every AST node that carries position info implements
//!    `brush_parser::ast::SourceLocation { fn location(&self) -> Option<SourceSpan> }`
//!    (e.g. `Program`, `Command`, `Pipeline`, `CompoundCommand`, …). A handful of struct
//!    variants (`ArithmeticCommand`, `SubshellCommand`, `ForClauseCommand`, `IfClauseCommand`,
//!    …) additionally carry a `pub loc: SourceSpan` field directly, but `.location()` is the
//!    uniform accessor. `SourceSpan { start: Arc<SourcePosition>, end: Arc<SourcePosition> }`
//!    and `SourcePosition { index: usize /* 0-based */, line: usize /* 1-based */, column:
//!    usize /* 1-based */ }` — so `location()` already yields 1-based line/col directly, no
//!    manual +1 needed. See the [`span`] helper below.
//!
//! 3. **`time` and redirect lists**:
//!    - `time` is *not* a free-standing command; it is carried on the pipeline it applies
//!      to: `Pipeline { pub timed: Option<PipelineTimed>, .. }` where
//!      `PipelineTimed::Timed(SourceSpan)` / `PipelineTimed::TimedWithPosixOutput(SourceSpan)`
//!      (bash `time` vs. POSIX `time -p`).
//!    - Redirect lists on compound commands are a **sibling of the command, not a field on
//!      it**: `Command::Compound(CompoundCommand, Option<RedirectList>)` (also
//!      `Command::ExtendedTest(ExtendedTestExprCommand, Option<RedirectList>)`). A `[ … ]`
//!      block-level redirect (`{ …; } > out.log`) is the second tuple element, not nested
//!      inside `CompoundCommand`.

/// Parse a shell script into a brush-parser AST, or a diagnostic string.
///
/// Never panics: tokenizer and parser errors are both mapped to `Err`.
pub fn parse(text: &str) -> Result<brush_parser::ast::Program, String> {
    let opts = brush_parser::ParserOptions::default();
    let tokens = brush_parser::tokenize_str(text).map_err(|e| e.to_string())?;
    brush_parser::parse_tokens(&tokens, &opts).map_err(|e| e.to_string())
}

/// Map a node's `SourceLocation` to a 1-based `(line, col)` pair, if known.
///
/// `SourceSpan`/`SourcePosition` are already 1-based for `line`/`column` (see the module
/// doc), so this is a direct passthrough over `Option`/`Arc` unwrapping — no offset math.
pub fn span(node: &impl brush_parser::ast::SourceLocation) -> Option<(usize, usize)> {
    node.location()
        .map(|span| (span.start.line, span.start.column))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_simple_script_into_a_program() {
        let prog = parse("echo hi\nfoo() { rm -rf /tmp/x; }\n").expect("should parse");
        assert!(!prog.complete_commands.is_empty());
    }

    #[test]
    fn unparseable_input_is_an_err_not_a_panic() {
        // An obviously broken construct must return Err, never panic.
        let result = parse("if then fi fi )(");
        assert!(result.is_err());
    }

    #[test]
    fn span_reports_a_one_based_line_and_col() {
        let prog = parse("echo hi\n").expect("should parse");
        let (line, col) = span(&prog).expect("program should have a location");
        assert_eq!(line, 1);
        assert_eq!(col, 1);
    }
}
