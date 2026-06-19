// fxrank-lang-ts: TypeScript frontend for FxRank (swc-based)
// This module will be expanded in later tasks.

#[cfg(test)]
mod spike {
    use swc_common::{BytePos, FileName, SourceMap, sync::Lrc};
    use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};

    #[test]
    fn parses_ts_and_resolves_line() {
        let cm: Lrc<SourceMap> = Default::default();
        let src = "function f(): void {\n  fetch('x');\n}\n";
        let fm = cm.new_source_file(FileName::Custom("t.ts".into()).into(), src);
        let lexer = Lexer::new(
            Syntax::Typescript(TsSyntax::default()),
            Default::default(),
            StringInput::from(&*fm),
            None,
        );
        let mut parser = Parser::new_from(lexer);
        let module = parser.parse_module().expect("parse");
        assert!(!module.body.is_empty());
        // span->line: the `fetch` call sits on line 2.
        let line = cm
            .lookup_char_pos(BytePos(src.find("fetch").unwrap() as u32 + fm.start_pos.0))
            .line;
        assert_eq!(line, 2);
    }
}
