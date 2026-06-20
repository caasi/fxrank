//! Language selection and span->line resolution for the swc-based frontend.
use swc_common::{BytePos, SourceMap, Span, sync::Lrc};
use swc_ecma_parser::{EsSyntax, Syntax, TsSyntax};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Lang {
    #[default]
    Ts,
    Tsx,
    Js,
}

impl Lang {
    /// Map a file extension (no dot) to a language.
    ///
    /// `.js` / `.mjs` / `.cjs` / `.jsx` all enable JSX (JSX-in-`.js` is common);
    /// `.ts` is TS without JSX; `.tsx` is TS+JSX.
    pub fn from_extension(ext: &str) -> Option<Lang> {
        match ext {
            "ts" => Some(Lang::Ts),
            "tsx" => Some(Lang::Tsx),
            "js" | "mjs" | "cjs" | "jsx" => Some(Lang::Js),
            _ => None,
        }
    }

    /// Return the swc `Syntax` for this language.
    pub fn syntax(self) -> Syntax {
        match self {
            Lang::Ts => Syntax::Typescript(TsSyntax {
                tsx: false,
                ..Default::default()
            }),
            Lang::Tsx => Syntax::Typescript(TsSyntax {
                tsx: true,
                ..Default::default()
            }),
            Lang::Js => Syntax::Es(EsSyntax {
                jsx: true,
                ..Default::default()
            }),
        }
    }
}

/// Resolves swc `Span`s to 1-based line numbers via the `SourceMap`.
pub struct SpanLines {
    cm: Lrc<SourceMap>,
}

impl SpanLines {
    pub fn new(cm: Lrc<SourceMap>) -> Self {
        SpanLines { cm }
    }

    pub fn line(&self, span: Span) -> usize {
        self.cm.lookup_char_pos(span.lo).line
    }

    pub fn line_of(&self, pos: BytePos) -> usize {
        self.cm.lookup_char_pos(pos).line
    }

    /// Resolve a span's start to a 1-based `(line, column)`. The column is the
    /// 1-based **character** column (Unicode scalar count, not byte/UTF-16/display
    /// width): swc's `CharPos` is 0-based, so add 1. Both coordinates come from a
    /// single `lookup_char_pos`, so callers get an anchor-consistent `(line, col)`.
    pub fn line_col(&self, span: Span) -> (usize, usize) {
        let loc = self.cm.lookup_char_pos(span.lo);
        (loc.line, loc.col.0 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_file(src: &str) -> (Lrc<SourceMap>, Lrc<swc_common::SourceFile>) {
        use swc_common::FileName;
        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.new_source_file(FileName::Custom("t".into()).into(), src.to_string());
        (cm, fm)
    }

    #[test]
    fn lang_from_extension() {
        assert_eq!(Lang::from_extension("ts"), Some(Lang::Ts));
        assert_eq!(Lang::from_extension("tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::from_extension("mjs"), Some(Lang::Js));
        assert_eq!(Lang::from_extension("rs"), None);
    }

    #[test]
    fn spanlines_resolves() {
        let (cm, fm) = test_file("a;\nb;\n");
        let lines = SpanLines::new(cm);
        // a span at the start of line 2:
        let pos = swc_common::BytePos(fm.start_pos.0 + 3);
        assert_eq!(lines.line(Span::new(pos, pos)), 2);
    }

    #[test]
    fn line_col_is_one_based_for_line_and_column() {
        // `a` of `ab` is the 5th column (1-based) of line 1 ("let " = 4 chars).
        let (cm, fm) = test_file("let ab = 1;\n");
        let lines = SpanLines::new(cm);
        let pos = swc_common::BytePos(fm.start_pos.0 + 4); // byte offset of `a`
        assert_eq!(lines.line_col(Span::new(pos, pos)), (1, 5));
    }

    #[test]
    fn line_col_counts_characters_not_display_width() {
        // A leading tab is ONE character: the `x` after it is col 2, not col 9.
        let (cm, fm) = test_file("\tx = 1;");
        let lines = SpanLines::new(cm);
        let pos = swc_common::BytePos(fm.start_pos.0 + 1); // byte offset of `x`
        assert_eq!(lines.line_col(Span::new(pos, pos)), (1, 2));
    }
}
