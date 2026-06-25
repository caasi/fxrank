//! tsconfig.json (JSONC) loader: extracts compilerOptions.baseUrl + paths for
//! alias resolution. The ONLY disk read in the TS frontend (sanctioned, §9);
//! fxrank-core stays parser-free + I/O-free.

use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct TsConfig {
    /// Effective, CLEANED base directory that `paths` targets resolve against
    /// (config_dir joined with baseUrl if present, else config_dir; `.`/`./`/`""`
    /// collapse to ""). Always populated.
    pub base: String,
    /// (pattern, targets) in declaration order, e.g. ("@/*", ["./*"]).
    pub paths: Vec<(String, Vec<String>)>,
}

/// Parse a JSONC tsconfig string. Pure — no disk, no panic (malformed → empty
/// paths, base = clean_dir(config_dir)).
pub fn parse(jsonc: &str, config_dir: &str) -> TsConfig {
    let value: serde_json::Value = match serde_json::from_str(&strip_jsonc(jsonc)) {
        Ok(v) => v,
        Err(_) => {
            return TsConfig {
                base: clean_dir(config_dir),
                paths: vec![],
            };
        }
    };
    let co = value.get("compilerOptions");
    // Effective base: config_dir + baseUrl (if any), cleaned. C1: clean_dir collapses
    // a `.`/`""`/leading-`./` base so `scan src --project .` resolves (config_dir=".").
    let base = match co.and_then(|c| c.get("baseUrl")).and_then(|b| b.as_str()) {
        Some(b) => clean_join(config_dir, b),
        None => clean_dir(config_dir),
    };
    let mut paths = Vec::new();
    if let Some(obj) = co.and_then(|c| c.get("paths")).and_then(|p| p.as_object()) {
        for (pat, targets) in obj {
            let ts: Vec<String> = targets
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            paths.push((pat.clone(), ts));
        }
    }
    TsConfig { base, paths }
}

pub fn load(project: &Path) -> Result<TsConfig, String> {
    let file = if project.is_dir() {
        project.join("tsconfig.json")
    } else {
        project.to_path_buf()
    };
    let text = std::fs::read_to_string(&file)
        .map_err(|e| format!("could not read tsconfig {}: {e}", file.display()))?;
    let dir = file
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .to_string();
    Ok(parse(&text, &dir))
}

/// Collapse a directory string to the in-batch-key namespace: drop a leading
/// `./`, treat `.`/`""` as the empty (root) base, drop a trailing `/`, and
/// normalize `.`/`..` segments. So `clean_dir(".")=""`, `clean_dir("./src")="src"`,
/// `clean_dir("proj/")="proj"`. (C1: without this, `config_dir="."` poisons every
/// alias with a leading `./` and the relative-invocation case resolves nothing.)
pub fn clean_dir(dir: &str) -> String {
    clean_join(dir, "")
}

/// Join a base dir with a relative segment-string and normalize, collapsing `.`/
/// `..`/empty segments on BOTH sides (unlike module_map's normalize_join, which
/// keeps a `.` base segment — the C1 bug). `clean_join(".","./src")="src"`,
/// `clean_join("proj",".")="proj"`, `clean_join("/abs","./src")="/abs/src"`.
pub fn clean_join(base: &str, rest: &str) -> String {
    let abs = base.starts_with('/');
    let mut segs: Vec<&str> = Vec::new();
    for part in base.split('/').chain(rest.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            other => segs.push(other),
        }
    }
    let joined = segs.join("/");
    if abs { format!("/{joined}") } else { joined }
}

/// String-aware JSONC → JSON: strip `//`/`/* */` comments (NOT inside strings,
/// honoring `\"` escapes) and trailing commas before `}`/`]`. (Task-1 dep decision.)
pub fn strip_jsonc(jsonc: &str) -> String {
    // State machine over chars: tracks in_string (+ escaped), line_comment,
    // block_comment. Emits only code chars; after building the output, a second
    // pass removes trailing commas before `}`/`]`.
    #[derive(PartialEq)]
    enum State {
        Normal,
        InString,
        InStringEscape,
        LineComment,
        BlockComment,
        BlockCommentStar, // inside `/* … */`, just saw a `*`
    }

    let mut out = String::with_capacity(jsonc.len());
    let mut state = State::Normal;
    let mut chars = jsonc.chars().peekable();

    while let Some(ch) = chars.next() {
        match state {
            State::InString => {
                out.push(ch);
                match ch {
                    '\\' => state = State::InStringEscape,
                    '"' => state = State::Normal,
                    _ => {}
                }
            }
            State::InStringEscape => {
                // The escaped char — emit it, return to InString.
                out.push(ch);
                state = State::InString;
            }
            State::LineComment => {
                // Drop chars until newline; emit the newline itself to preserve line structure.
                if ch == '\n' {
                    out.push(ch);
                    state = State::Normal;
                }
                // (carriage return before \n: just drop it too — \r\n handled by the \n above)
            }
            State::BlockComment => {
                if ch == '*' {
                    state = State::BlockCommentStar;
                }
                // drop all other chars
            }
            State::BlockCommentStar => {
                if ch == '/' {
                    state = State::Normal; // end of block comment
                } else if ch == '*' {
                    // stay in BlockCommentStar (e.g. `/***/`)
                } else {
                    state = State::BlockComment;
                }
            }
            State::Normal => {
                match ch {
                    '"' => {
                        out.push(ch);
                        state = State::InString;
                    }
                    '/' => {
                        // Peek at the next char to decide comment vs. divide.
                        match chars.peek() {
                            Some('/') => {
                                chars.next(); // consume second `/`
                                state = State::LineComment;
                            }
                            Some('*') => {
                                chars.next(); // consume `*`
                                state = State::BlockComment;
                            }
                            _ => {
                                out.push(ch);
                            }
                        }
                    }
                    other => {
                        out.push(other);
                    }
                }
            }
        }
    }

    // Second pass: remove trailing commas — a comma followed (possibly by whitespace)
    // by `}` or `]`. Walk the output string right-to-left finding commas to drop.
    remove_trailing_commas(out)
}

/// Remove JSON trailing commas: any `,` that is followed only by whitespace before
/// a `}` or `]`. Operates on a string that has already had comments stripped.
fn remove_trailing_commas(s: String) -> String {
    let bytes = s.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b',' {
            // Look ahead: skip whitespace, check for `}` or `]`.
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r')
            {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'}' || bytes[j] == b']') {
                // Skip the comma (trailing comma before closing bracket).
                i += 1;
                continue;
            }
        }
        result.push(b);
        i += 1;
    }
    // SAFETY: input was valid UTF-8 (came from a &str), we only removed ASCII bytes.
    unsafe { String::from_utf8_unchecked(result) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_shape_no_baseurl_relative_dir() {
        // The REAL omni shape (C2): NO baseUrl, target with leading `./`, and a
        // RELATIVE config_dir "." (the `scan src --project .` case).
        let jsonc = r#"{
            // project config
            "compilerOptions": {
                "paths": {
                    "@/*": ["./src/*"],
                    "@/components/*": ["./src/components/*"], // overlapping prefix (I3)
                },
            },
        }"#;
        let c = parse(jsonc, ".");
        // No baseUrl → base = clean_dir(".") = "" (root namespace, matches in-batch keys).
        assert_eq!(c.base, "");
        assert_eq!(
            c.paths.iter().find(|(k, _)| k == "@/*").unwrap().1,
            vec!["./src/*".to_string()]
        );
    }

    #[test]
    fn baseurl_joined_and_cleaned() {
        let c = parse(
            r#"{"compilerOptions":{"baseUrl":"./src","paths":{"@/*":["./*"]}}}"#,
            "proj",
        );
        assert_eq!(c.base, "proj/src"); // clean_join("proj","./src")
    }

    #[test]
    fn malformed_is_empty_not_error() {
        let c = parse("{ this is not json", "proj");
        assert_eq!(c.base, "proj");
        assert!(c.paths.is_empty());
    }

    #[test]
    fn clean_join_collapses_dot_base() {
        // C1: the bug class — a `.`/`./`/"" base must NOT leak a leading `./`.
        assert_eq!(clean_join(".", "./src"), "src");
        assert_eq!(clean_join("", "src"), "src");
        assert_eq!(clean_join("proj", "."), "proj");
        assert_eq!(clean_join("/abs", "./src"), "/abs/src");
        assert_eq!(clean_join("src/comp", "../util"), "src/util");
    }

    #[test]
    fn strip_jsonc_is_string_aware() {
        // comment markers INSIDE strings must survive; real comments + trailing commas go.
        let out =
            strip_jsonc(r#"{"a":"x // not a comment","b":"/* also not */", /* real */ "c":1,}"#);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], "x // not a comment");
        assert_eq!(v["b"], "/* also not */");
        assert_eq!(v["c"], 1);
    }
}
