//! TS/JS module map: normalize in-batch file paths to module keys and resolve
//! relative import specifiers against the in-batch set, by path convention
//! (spec 025-3e §5.2). No disk, no tsconfig, no swc.

use std::collections::HashSet;

use fxrank_core::frontend::SourceFile;

use crate::tsconfig::{TsConfig, clean_join};

const TS_EXTS: &[&str] = &[".tsx", ".mts", ".cts", ".ts", ".jsx", ".mjs", ".cjs", ".js"];

pub struct TsModuleMap {
    keys: HashSet<String>,
    /// Precomputed alias table: (pattern, base-joined-target).
    /// Sorted by pattern prefix-length descending (longest-prefix wins, I3).
    /// Empty when built with `build(files)` (no tsconfig).
    aliases: Vec<(String, String)>,
}

impl TsModuleMap {
    pub fn build(files: &[SourceFile]) -> Self {
        let keys = files.iter().map(|f| module_key(&f.path)).collect();
        Self {
            keys,
            aliases: Vec::new(),
        }
    }

    /// Same as `build` plus alias resolution from a parsed tsconfig.
    /// For each `(pattern, targets)`, the first target is taken and joined with
    /// `cfg.base` via `clean_join`. The alias table is sorted by pattern prefix
    /// length descending (longest-prefix wins, I3).
    pub fn build_with_tsconfig(files: &[SourceFile], cfg: &TsConfig) -> Self {
        let keys = files.iter().map(|f| module_key(&f.path)).collect();
        let mut aliases: Vec<(String, String)> = cfg
            .paths
            .iter()
            .filter_map(|(pattern, targets)| {
                let target = targets.first()?;
                let resolved = clean_join(&cfg.base, target);
                Some((pattern.clone(), resolved))
            })
            .collect();
        // Longest-prefix match: sort by the prefix (part before `/*` or the whole pattern)
        // length descending so more-specific patterns win over broader ones.
        aliases.sort_by(|(a, _), (b, _)| {
            let prefix_len = |p: &str| p.strip_suffix("/*").map_or(p.len(), |s| s.len());
            prefix_len(b).cmp(&prefix_len(a))
        });
        Self { keys, aliases }
    }

    pub fn module_of(&self, file_path: &str) -> String {
        module_key(file_path)
    }

    /// Resolve an import specifier to an in-batch module key.
    ///
    /// 1. If `specifier` is non-relative, try the alias table (longest-prefix first).
    ///    A wildcard pattern `P/*` matches a specifier starting with `P/` or equal to `P`,
    ///    capturing the remainder `R`; `R` substitutes `*` in the stored target, producing
    ///    a candidate path run through the same `module_key` + in-batch ladder. An exact
    ///    (non-`*`) pattern must match the whole specifier. First in-batch hit wins.
    ///
    /// 2. If `specifier` is relative (`./x`, `../x`), join with the importer dir and look up.
    ///
    /// Non-relative specifiers not matched by any alias (bare packages, `node:*`, unresolvable
    /// aliases) → `None` (never-guess preserved).
    pub fn resolve_import(&self, importer_file: &str, specifier: &str) -> Option<String> {
        // Step 1: alias resolution for non-relative specifiers.
        if !(specifier.starts_with("./") || specifier.starts_with("../")) {
            return self.resolve_alias(specifier);
        }
        // Step 2: relative resolution.
        let dir = parent_dir(importer_file);
        let candidate = module_key(&normalize_join(dir, specifier));
        if self.keys.contains(&candidate) {
            Some(candidate)
        } else {
            None
        }
    }

    /// Try to resolve a non-relative specifier through the alias table.
    fn resolve_alias(&self, specifier: &str) -> Option<String> {
        for (pattern, target) in &self.aliases {
            if let Some(candidate) = apply_alias(pattern, target, specifier) {
                let key = module_key(&candidate);
                if self.keys.contains(&key) {
                    return Some(key);
                }
                // Alias matched but target not in batch → keep trying next alias.
            }
        }
        None
    }
}

/// Apply a single alias pattern to a specifier, returning the expanded target path if
/// the pattern matches. Returns `None` if the pattern does not match.
///
/// - Wildcard `P/*`: matches specifier `P` (remainder `""`) or `P/R` (remainder `R`);
///   substitutes remainder into the target's `*`.
/// - Exact `P`: matches the whole specifier verbatim; target returned as-is (if target
///   contains `*`, it is left as-is — exact patterns should not have `*` per tsc semantics).
fn apply_alias(pattern: &str, target: &str, specifier: &str) -> Option<String> {
    if let Some(prefix) = pattern.strip_suffix("/*") {
        // Wildcard pattern: match `P` (remainder "") or `P/R` (remainder R).
        let remainder = if specifier == prefix {
            ""
        } else {
            specifier.strip_prefix(&format!("{prefix}/"))?
        };
        // Substitute `*` in target with the remainder.
        let expanded = if let Some(star_pos) = target.find('*') {
            format!(
                "{}{}{}",
                &target[..star_pos],
                remainder,
                &target[star_pos + 1..]
            )
        } else {
            target.to_string()
        };
        Some(expanded)
    } else {
        // Exact pattern: must match the whole specifier.
        if specifier == pattern {
            Some(target.to_string())
        } else {
            None
        }
    }
}

/// Normalize a file path to a module key: strip a known TS/JS extension, then
/// drop a trailing `/index` segment.
fn module_key(path: &str) -> String {
    let mut stem = path;
    for ext in TS_EXTS {
        if let Some(s) = path.strip_suffix(ext) {
            stem = s;
            break;
        }
    }
    stem.strip_suffix("/index").unwrap_or(stem).to_string()
}

/// Parent directory of a path (no trailing slash). `"src/app.ts"` → `"src"`.
fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Join a base dir with a relative specifier and normalize `.`/`..` segments.
/// `("src/comp", "../util")` → `"src/util"`.
fn normalize_join(base: &str, spec: &str) -> String {
    let mut segs: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for part in spec.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            other => segs.push(other),
        }
    }
    segs.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fxrank_core::frontend::SourceFile;

    fn sf(p: &str) -> SourceFile {
        SourceFile {
            path: p.into(),
            text: String::new(),
        }
    }

    #[test]
    fn module_key_strips_ext_and_index() {
        let files = vec![sf("src/util.ts"), sf("src/foo/index.tsx"), sf("src/a/b.js")];
        let m = TsModuleMap::build(&files);
        assert_eq!(m.module_of("src/util.ts"), "src/util");
        assert_eq!(m.module_of("src/foo/index.tsx"), "src/foo");
        assert_eq!(m.module_of("src/a/b.js"), "src/a/b");
    }

    #[test]
    fn resolve_relative_with_extension_and_index_ladder() {
        let files = vec![
            sf("src/app.ts"),
            sf("src/util.ts"),
            sf("src/comp/index.ts"),
            sf("src/comp/inner.ts"),
        ];
        let m = TsModuleMap::build(&files);
        // ./util from src/app.ts → src/util
        assert_eq!(
            m.resolve_import("src/app.ts", "./util"),
            Some("src/util".into())
        );
        // ./comp from src/app.ts → src/comp (index ladder)
        assert_eq!(
            m.resolve_import("src/app.ts", "./comp"),
            Some("src/comp".into())
        );
        // ../util from src/comp/inner.ts → src/util
        assert_eq!(
            m.resolve_import("src/comp/inner.ts", "../util"),
            Some("src/util".into())
        );
    }

    #[test]
    fn resolve_relative_with_explicit_extension() {
        // ESM/NodeNext code often writes the extension; it must still resolve to
        // the extensionless in-batch key.
        let files = vec![sf("src/app.ts"), sf("src/util.ts")];
        let m = TsModuleMap::build(&files);
        assert_eq!(
            m.resolve_import("src/app.ts", "./util.js"),
            Some("src/util".into())
        );
        assert_eq!(
            m.resolve_import("src/app.ts", "./util.ts"),
            Some("src/util".into())
        );
    }

    #[test]
    fn module_key_handles_esm_extensions() {
        let files = vec![
            sf("src/a.mts"),
            sf("src/b.cts"),
            sf("src/c.mjs"),
            sf("src/d.cjs"),
        ];
        let m = TsModuleMap::build(&files);
        assert_eq!(m.module_of("src/a.mts"), "src/a");
        assert_eq!(m.module_of("src/b.cts"), "src/b");
        assert_eq!(m.module_of("src/c.mjs"), "src/c");
        assert_eq!(m.module_of("src/d.cjs"), "src/d");
    }

    #[test]
    fn non_relative_and_out_of_batch_are_none() {
        let files = vec![sf("src/app.ts"), sf("src/util.ts")];
        let m = TsModuleMap::build(&files);
        assert_eq!(m.resolve_import("src/app.ts", "node:fs"), None); // node builtin
        assert_eq!(m.resolve_import("src/app.ts", "react"), None); // bare package
        assert_eq!(m.resolve_import("src/app.ts", "@/util"), None); // alias (no tsconfig)
        assert_eq!(m.resolve_import("src/app.ts", "./missing"), None); // not in batch
    }

    #[test]
    fn resolves_tsconfig_path_alias_real_shape() {
        use crate::tsconfig::TsConfig;
        let files = vec![
            sf("src/app.ts"),
            sf("src/hooks/use-auth.ts"),
            sf("src/components/btn.ts"),
        ];
        // The real shape: NO baseUrl (base=""), targets with leading `./` → cleaned to "src/*".
        let cfg = TsConfig {
            base: "".into(),
            paths: vec![
                ("@/*".into(), vec!["./src/*".into()]),
                ("@/components/*".into(), vec!["./src/components/*".into()]), // overlap (I3)
            ],
        };
        let m = TsModuleMap::build_with_tsconfig(&files, &cfg);
        assert_eq!(
            m.resolve_import("src/app.ts", "@/hooks/use-auth"),
            Some("src/hooks/use-auth".into())
        );
        // Overlapping prefix: @/components/btn matches both @/* and @/components/* (longest wins) → same key here.
        assert_eq!(
            m.resolve_import("src/app.ts", "@/components/btn"),
            Some("src/components/btn".into())
        );
        assert_eq!(m.resolve_import("src/app.ts", "react"), None); // real package
        assert_eq!(m.resolve_import("src/app.ts", "@/missing"), None); // alias, not in batch → opaque
    }

    #[test]
    fn no_tsconfig_means_aliases_stay_opaque() {
        let files = vec![sf("src/app.ts"), sf("src/hooks/use-auth.ts")];
        let m = TsModuleMap::build(&files); // the unchanged Plan-3 constructor
        assert_eq!(m.resolve_import("src/app.ts", "@/hooks/use-auth"), None);
    }
}
