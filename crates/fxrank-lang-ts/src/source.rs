//! Language selection and span->line resolution for the swc-based frontend.
use swc_common::{BytePos, SourceMap, Span, sync::Lrc};
use swc_ecma_parser::{EsSyntax, Syntax, TsSyntax};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ts,
    Tsx,
    Js,
    Jsx,
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

    /// Parse a `--lang` flag value.
    pub fn from_flag(s: &str) -> Option<Lang> {
        match s {
            "ts" => Some(Lang::Ts),
            "tsx" => Some(Lang::Tsx),
            "js" => Some(Lang::Js),
            "jsx" => Some(Lang::Jsx),
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
            Lang::Jsx => Syntax::Es(EsSyntax {
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
}

#[cfg(test)]
fn test_file(src: &str) -> (Lrc<SourceMap>, Lrc<swc_common::SourceFile>) {
    use swc_common::FileName;
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(FileName::Custom("t".into()).into(), src.to_string());
    (cm, fm)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
