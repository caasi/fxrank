/// Parse-position utilities for the Python frontend.
///
/// # BOM invariant
///
/// The frontend strips any leading UTF-8 BOM (`\u{feff}`) exactly once via
/// `strip_bom` at the entry point, then uses that single stripped `&str` for
/// all three consumers: `parse_module`, `tokenize`/`lambda_anchors`, and the
/// pointer-arithmetic in `anchor_of_subslice`. Passing the same buffer to all
/// three keeps every byte offset consistent — libcst's `byte_idx()` values,
/// the token-stream positions, and `SpanIndex` line lookups all agree.
use libcst_native::tokenize;

/// Precomputed line-start byte offsets. The line is found in O(log n) (binary
/// search over the line starts); the 1-based **char** column is then O(line length)
/// (`chars().count()` over the line prefix, so multi-byte chars count as one).
pub struct SpanIndex<'a> {
    src: &'a str,
    line_starts: Vec<usize>, // byte offset of the start of each line (line 1 = index 0)
}

impl<'a> SpanIndex<'a> {
    pub fn new(src: &'a str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        SpanIndex { src, line_starts }
    }

    /// The source buffer this index was built from (for `anchor_of_subslice`).
    pub fn src(&self) -> &'a str {
        self.src
    }

    /// 1-based line, 1-based **char** column for a byte offset (`usize`, matching core).
    pub fn line_col(&self, byte_off: usize) -> (usize, usize) {
        let line_idx = match self.line_starts.binary_search(&byte_off) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx];
        let col_chars = self.src[line_start..byte_off].chars().count();
        (line_idx + 1, col_chars + 1)
    }
}

/// Byte offset of a `&str` that is a subslice of `src` (pointer arithmetic).
///
/// **Precondition:** `sub` MUST point into `src`'s buffer (e.g. a libcst node's
/// borrowed `&str` taken from the same parsed source). Passing an unrelated `&str`
/// yields a meaningless offset. `pub(crate)` so this can't be misused from outside.
pub(crate) fn anchor_of_subslice(src: &str, sub: &str) -> usize {
    sub.as_ptr() as usize - src.as_ptr() as usize
}

/// (line, 1-based char col) of each `lambda` keyword token, in source order.
///
/// Returns `Some(anchors)` on success and `None` if tokenization fails.
///
/// # Precondition
/// `src` MUST be the **same** (BOM-stripped) buffer that `parse_module` already
/// accepted. Tokenizing is a strict subset of parsing, so any `src` that parsed
/// also tokenizes; in practice this function therefore always returns `Some(…)`.
/// The `None` branch exists to close the silent-drop hole that the old
/// `unwrap_or_default()` created: if tokenization ever fails we now propagate the
/// failure to the caller (`PythonFrontend::analyze`) so it can emit a `Diagnostic`
/// and skip the file rather than silently emitting zero anchors and misattributing
/// (or omitting) every lambda.
///
/// # Double-tokenize elimination
/// The caller passes the returned `&[(usize, usize)]` slice directly into
/// `functions::collect`, so tokenization happens **exactly once per file** — the
/// old design called `lambda_anchors` a second time in `PythonFrontend::analyze`
/// for the mismatch guard, producing two tokenizer passes per file.
pub fn lambda_anchors(src: &str) -> Option<Vec<(usize, usize)>> {
    tokenize(src)
        .map(|toks| {
            toks.iter()
                .filter(|t| t.string == "lambda")
                .map(|t| {
                    (
                        t.start_pos.line_number(),
                        t.start_pos.char_column_number() + 1,
                    )
                })
                .collect()
        })
        .ok()
}

/// Strip a leading UTF-8 BOM (`\u{feff}`) from `src`, if present.
///
/// Pass the result consistently to `parse_module`, `anchor_of_subslice`, and
/// `SpanIndex::new` so that all byte offsets are relative to the same buffer.
pub fn strip_bom(src: &str) -> &str {
    src.strip_prefix('\u{feff}').unwrap_or(src)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_counts_chars_not_bytes() {
        let src = "x = 'é'\ndef f():\n    pass\n"; // 'é' is 2 bytes, 1 char
        let idx = SpanIndex::new(src);
        let byte_off = src.find("def").unwrap();
        assert_eq!(idx.line_col(byte_off), (2, 1)); // line 2, char col 1
    }

    #[test]
    fn anchor_of_subslice_is_exact() {
        let src = "def greet():\n    pass\n";
        let name = &src[4..9]; // "greet"
        assert_eq!(anchor_of_subslice(src, name), 4);
    }

    #[test]
    fn lambda_anchors_in_source_order() {
        let src = "a = lambda: 1\nb = lambda y: y\n";
        let anchors = lambda_anchors(src).expect("tokenize must succeed on valid Python");
        assert_eq!(anchors, vec![(1, 5), (2, 5)]); // both `lambda` at char col 5
    }

    #[test]
    fn strip_bom_removes_bom() {
        let with_bom = "\u{feff}def f(): pass\n";
        assert_eq!(strip_bom(with_bom), "def f(): pass\n");
    }

    #[test]
    fn strip_bom_noop_without_bom() {
        let src = "def f(): pass\n";
        assert_eq!(strip_bom(src), src);
    }
}
