// fxrank-lang-ts: TypeScript frontend for FxRank (swc-based)

pub mod coverage;
pub mod detect;
pub mod functions;
pub mod imports;
pub mod module_map;
pub mod react;
pub mod source;
pub mod tsconfig;

use std::collections::{HashMap, HashSet};

use fxrank_core::CorpusProfile;
use fxrank_core::frontend::{Frontend, FrontendOutput, Language, SourceFile};
use fxrank_core::model::{Diagnostic, Hotspot};
use fxrank_core::record::CallSiteRef;

/// TypeScript/JavaScript corpus hygiene.
///
/// `__mocks__` is a directory → placed in `prune_dirs` (channel honesty).
/// Behaviorally identical under the flat union — a bare literal prunes+excludes
/// either way — but this keeps `exclude_file_globs` = "things that never prune a dir".
pub const CORPUS_PROFILE: CorpusProfile = CorpusProfile {
    prune_dirs: &["node_modules", "__mocks__"],
    exclude_file_globs: &[
        "*.min.js",
        "*.min.mjs",
        "*.min.cjs",
        "*.stories.*",
        "mockServiceWorker.js",
        "jest.setup.*",
        "jest.config.*",
    ],
    test_file_globs: &["*.test.*", "*.spec.*", "__tests__"],
    prune_marker_files: &[],
};

use crate::functions::FnUnit;
use crate::imports::ImportTable;
use crate::module_map::TsModuleMap;
use crate::source::{Lang, SpanLines};
use crate::tsconfig::TsConfig;

/// The TypeScript/JavaScript language frontend.
///
/// `TsFrontend { lang }.analyze()` parses each `SourceFile` with the configured
/// `lang` dialect via `functions::parse_module`, builds a `SpanLines` from the
/// same `SourceMap` used for parsing (so effect-line resolution works), then
/// maps each `FnUnit` to a scored `Hotspot` via `detect::analyze_unit`.
/// Un-parseable files become `Diagnostic`s, not panics.
///
/// `lang` is the dialect used for *all* this frontend's sources; the CLI groups
/// sources by resolved `Lang` so each group gets a `TsFrontend` with the right
/// dialect. When `include_tests` is `false` (the default), whole files whose path
/// contains `.test.` or `.spec.` (e.g. `foo.test.ts`, `bar.spec.tsx`) or any
/// path segment equals `__tests__` are skipped; their unit count is tallied in
/// `FrontendOutput::skipped_tests`. JS/TS convention keeps tests in separate
/// files, so skipping is by file path (not by detecting `describe`/`it` inside
/// app code), mirroring the Rust frontend's `skipped_tests` contract.
///
/// When `tsconfig` is `Some(cfg)`, the module map is built with alias resolution
/// from the parsed tsconfig (`paths`/`baseUrl`). When `None` (the default), only
/// relative imports are resolved; non-relative specifiers (aliases) stay opaque.
#[derive(Default)]
pub struct TsFrontend {
    pub lang: Lang,
    pub include_tests: bool,
    pub tsconfig: Option<TsConfig>,
}

impl Frontend for TsFrontend {
    fn language(&self) -> Language {
        Language::Ts
    }

    fn corpus_profile(&self) -> CorpusProfile {
        CORPUS_PROFILE
    }

    fn analyze(&self, files: &[SourceFile]) -> FrontendOutput {
        let mut output = FrontendOutput::default();
        let module_map = match &self.tsconfig {
            Some(cfg) => TsModuleMap::build_with_tsconfig(files, cfg),
            None => TsModuleMap::build(files),
        };

        for source in files {
            match functions::parse_module(&source.text, &source.path, self.lang) {
                Err(e) => {
                    // FIXME(diagnostic-UX): swc Error has no Display; Debug output is
                    // verbose — extract just the message in a later pass.
                    output.diagnostics.push(Diagnostic {
                        path: source.path.clone(),
                        parsed: false,
                        error: format!("{e:?}"),
                    });
                }
                Ok((module, cm)) => {
                    // Keep the SourceMap alive through detection: swc spans are
                    // bare BytePos offsets, and SpanLines needs the same cm that
                    // parsed the file to resolve them to line numbers.
                    let lines = SpanLines::new(cm);
                    let imports = ImportTable::from_module(&module);
                    let module_bindings = imports::module_bindings(&module);
                    let units = functions::collect(&module, &source.path, &lines);
                    if !self.include_tests && is_test_file(&source.path) {
                        output.skipped_tests += units.len();
                    } else {
                        analyze_units(
                            &units,
                            &imports,
                            &module_bindings,
                            &lines,
                            &module_map,
                            &mut output.functions,
                            &mut output.records,
                        );

                        // Module-init unit: score the top-level executable
                        // statements as a synthetic `<module>` unit. Emitted
                        // only when the module has ≥1 effect (import-time IO,
                        // effectful top-level call, etc.). A pure module
                        // (imports + function declarations only) produces no
                        // `<module>` entry. This runs AFTER analyze_units so
                        // it does not interfere with the React two-pass.
                        //
                        // `is_root` is set by the CLI for explicit-file entries;
                        // the frontend always emits `false`.
                        if let Some(init_unit) = functions::module_init_unit(&module, &source.path)
                        {
                            let h = detect::analyze_unit(
                                &init_unit,
                                &imports,
                                &lines,
                                &module_bindings,
                            );
                            if !h.effects.is_empty() {
                                let rec = detect::record_from_hotspot(
                                    &init_unit,
                                    &h,
                                    &imports,
                                    &lines,
                                    &[],
                                    &module_map,
                                );
                                output.records.push(rec);
                                output.functions.push(h);
                            }
                        }
                    }
                }
            }
        }

        output
    }
}

/// Score every `FnUnit` in one parsed file, routing inline hook-callback arrows
/// into the components that own them (the React two-pass).
///
/// **Pass 1** — find the components (`returns_jsx`), their `useRef`-binding sets,
/// and the inline arrows they pass to built-in hooks (`inherited_callbacks`),
/// keyed by `(line, col)`.
///
/// **Pass 2** — score each unit. An arrow whose `(line, col)` is an inherited
/// callback is **suppressed** as a standalone hotspot; its raw (pre-discount)
/// signals are stashed and later folded into the owning component
/// (`absorb_inherited`). A component's own hotspot is additionally augmented with
/// its render-body React signals (`augment_component`). Emission order matches
/// the input unit order.
fn analyze_units(
    units: &[FnUnit],
    imports: &ImportTable,
    module_bindings: &HashSet<String>,
    lines: &SpanLines,
    module_map: &TsModuleMap,
    out: &mut Vec<Hotspot>,
    records: &mut Vec<fxrank_core::record::UnitRecord>,
) {
    // Pass 1: components, their ref-binding sets, and the inherited arrows.
    let components: Vec<&FnUnit> = units
        .iter()
        .filter(|u| react::returns_jsx(&u.body))
        .collect();
    let comp_refs: HashMap<String, HashSet<String>> = components
        .iter()
        .map(|c| (c.id.clone(), react::ref_bindings(&c.body)))
        .collect();
    // (line, col) of an inline hook arrow -> (owning component id, phase).
    let mut inherited: HashMap<(usize, usize), (String, react::HookPhase)> = HashMap::new();
    for c in &components {
        for ((l, col), phase) in react::inherited_callbacks(&c.body, lines) {
            inherited.insert((l, col), (c.id.clone(), phase));
        }
    }

    // Pass 2: score each unit, routing inherited arrows into their component.
    let mut by_id: HashMap<String, Hotspot> = HashMap::new();
    // id -> &FnUnit for every emitted (non-suppressed) unit, so the final loop
    // can recover the unit to build its record (path/col + own-body refs).
    // Suppressed arrows are never inserted here (they `continue` below), so a
    // record is built iff a Hotspot is pushed → 1:1.
    let mut unit_by_id: HashMap<&str, &FnUnit> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut pending: HashMap<String, Vec<(react::HookPhase, detect::RawSignals)>> = HashMap::new();
    // Outgoing call refs absorbed from suppressed hook-callback arrows, keyed by
    // the owning component id. These are merged into the component's record refs
    // so that cross-file propagation can follow calls made inside hook callbacks
    // (e.g. `useEffect(() => helper())` gives the component an edge to `helper`).
    let mut pending_refs: HashMap<String, Vec<CallSiteRef>> = HashMap::new();
    // Shared empty set used as a borrow fallback when a component has no ref bindings.
    // Avoids cloning the per-component HashSet<String> for every suppressed callback.
    let empty_refs: HashSet<String> = HashSet::new();
    for unit in units {
        // `unit.col` is a real field (Task 4) — NEVER parse it out of `id`.
        let key = (unit.line, unit.col);
        if let Some((comp_id, phase)) = inherited.get(&key).cloned() {
            // Suppress this arrow as a standalone hotspot; stash its raw signals.
            // Thread the owning component's ref-binding set so a `r.current = …`
            // write inside this callback still classifies as ref-cell-write (the
            // arrow alone can't know `r` is a useRef binding from the component).
            let refs = comp_refs.get(comp_id.as_str()).unwrap_or(&empty_refs);
            let raw = detect::raw_signals(unit, imports, lines, module_bindings, refs);
            pending
                .entry(comp_id.clone())
                .or_default()
                .push((phase, raw));
            // Also collect this arrow's outgoing call refs so the component's
            // record carries edges to functions called inside hook callbacks.
            // `refs::extract` uses own-body semantics (stops at nested arrows/fns),
            // which is correct here — the callback IS the body we want.
            // Pass the arrow's file path (same as the component's) + the map so
            // absorbed refs also carry resolved_target.
            let arrow_refs =
                detect::refs::extract(&unit.body, imports, lines, &unit.path, module_map);
            if !arrow_refs.is_empty() {
                pending_refs.entry(comp_id).or_default().extend(arrow_refs);
            }
            continue;
        }
        let mut h = detect::analyze_unit(unit, imports, lines, module_bindings);
        if react::returns_jsx(&unit.body) {
            detect::augment_component(&mut h, unit, lines);
        }
        by_id.insert(unit.id.clone(), h);
        unit_by_id.insert(unit.id.as_str(), unit);
        order.push(unit.id.clone());
    }

    // Fold each component's inherited raw signals in, then recompute.
    for (comp_id, raws) in pending {
        if let Some(h) = by_id.get_mut(&comp_id) {
            detect::absorb_inherited(h, raws);
        }
    }

    for id in order {
        let h = by_id.remove(&id).expect("hotspot present for ordered id");
        // Build the record FROM the final Hotspot (own-data copied, incl. a
        // component's absorbed inherited signals), then push both 1:1.
        // Pass the absorbed arrow refs so cross-file propagation can follow
        // calls made inside hook callbacks (transitive propagation through hooks).
        let unit = unit_by_id
            .get(id.as_str())
            .expect("unit present for ordered id");
        let absorbed_refs = pending_refs.remove(id.as_str()).unwrap_or_default();
        records.push(detect::record_from_hotspot(
            unit,
            &h,
            imports,
            lines,
            &absorbed_refs,
            module_map,
        ));
        out.push(h);
    }
}

/// Return `true` if `path` identifies a test file by convention.
///
/// Delegates to a `CorpusMatcher` built from `CORPUS_PROFILE.test_file_globs`:
/// - `*.test.*` / `*.spec.*` match by base-name glob (e.g. `foo.test.ts`), OR
/// - `__tests__` as a bare literal matches any path segment (e.g. `src/__tests__/foo.ts`).
///
/// Only these two well-established JS/TS conventions are checked. Stdin
/// (`"stdin"`) and ordinary `.ts`/`.js` files are never test files.
pub fn is_test_file(path: &str) -> bool {
    use std::sync::OnceLock;
    static M: OnceLock<fxrank_core::CorpusMatcher> = OnceLock::new();
    M.get_or_init(|| fxrank_core::CorpusMatcher::test_matcher(CORPUS_PROFILE.test_file_globs))
        .matches_test_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_profile_method_returns_const() {
        use fxrank_core::frontend::Frontend;
        let p = TsFrontend::default().corpus_profile();
        assert_eq!(p.prune_dirs, CORPUS_PROFILE.prune_dirs);
        assert_eq!(p.test_file_globs, CORPUS_PROFILE.test_file_globs);
    }

    /// Parse `src` as TSX, run the full `analyze_units` two-pass, and return
    /// `(hotspots, records)`.
    fn analyze_src(src: &str) -> (Vec<Hotspot>, Vec<fxrank_core::record::UnitRecord>) {
        use fxrank_core::frontend::SourceFile;
        let source = SourceFile {
            path: "t.tsx".into(),
            text: src.to_string(),
        };
        let module_map = TsModuleMap::build(&[source]);
        let (module, cm) = functions::parse_module(src, "t.tsx", Lang::Tsx).expect("parse");
        let lines = SpanLines::new(cm);
        let imports = ImportTable::from_module(&module);
        let module_bindings = imports::module_bindings(&module);
        let units = functions::collect(&module, "t.tsx", &lines);
        let mut out = Vec::new();
        let mut records = Vec::new();
        analyze_units(
            &units,
            &imports,
            &module_bindings,
            &lines,
            &module_map,
            &mut out,
            &mut records,
        );
        (out, records)
    }

    #[test]
    fn records_emitted_one_to_one_with_hotspots_and_suppressed_arrow_has_none() {
        // A component passing `() => fetch(...)` to useEffect: the arrow is an
        // inherited hook callback → SUPPRESSED as a standalone hotspot, its fetch
        // effect absorbed into the component. The component is emitted; the arrow
        // is not.
        let src = "import React, { useEffect } from 'react';\n\
                   function FetchData() {\n\
                     useEffect(() => { fetch('/api/data'); }, []);\n\
                     return <div/>;\n\
                   }\n";
        let (out, records) = analyze_src(src);

        // 1:1 — exactly one record per emitted hotspot.
        assert_eq!(
            records.len(),
            out.len(),
            "records must be 1:1 with hotspots; hotspots={:?} records={:?}",
            out.iter().map(|h| &h.id).collect::<Vec<_>>(),
            records.iter().map(|r| &r.unit_id).collect::<Vec<_>>(),
        );

        // The component hotspot exists; the suppressed arrow does NOT.
        let comp = out
            .iter()
            .find(|h| h.symbol == "FetchData")
            .expect("FetchData component hotspot present");
        assert!(
            !out.iter().any(|h| h.id.contains("<arrow@")),
            "suppressed arrow must NOT appear as a hotspot; out={:?}",
            out.iter().map(|h| &h.id).collect::<Vec<_>>(),
        );

        // Records contain the component id but NOT the arrow id.
        assert!(
            records.iter().any(|r| r.unit_id == comp.id),
            "component id must have a record"
        );
        assert!(
            !records.iter().any(|r| r.unit_id.contains("<arrow@")),
            "suppressed arrow must NOT have a record; records={:?}",
            records.iter().map(|r| &r.unit_id).collect::<Vec<_>>(),
        );

        // The component's record carries the absorbed fetch effect (its own-data
        // == the final component Hotspot's).
        let comp_rec = records
            .iter()
            .find(|r| r.unit_id == comp.id)
            .expect("component record present");
        assert_eq!(
            comp_rec.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
            comp.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
            "component record effects must equal the final component Hotspot's (absorbed signals included)"
        );
        assert!(
            comp_rec
                .effects
                .iter()
                .any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
            "component record must carry the absorbed fetch (net.fs.db) effect; effects={:?}",
            comp_rec.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
        );
    }

    /// Finding 2: a suppressed hook-callback arrow's outgoing refs must be merged
    /// into the owning component's record so that cross-file propagation can follow
    /// calls made inside hook callbacks.
    ///
    /// Scenario: `Comp` passes `() => { helper(); }` to `useEffect`. `helper` does
    /// `fetch("x")`. The arrow is suppressed; its call to `helper` must appear in
    /// `Comp`'s record `refs` — enabling the propagation fold to later push
    /// `helper`'s class-7 IO up to `Comp`.
    ///
    /// This test is at the record level (below the cross-file fold). The propagation
    /// fold lives in `fxrank-cli`; what we verify here is the pre-condition: the
    /// record carries the `helper` ref.
    #[test]
    fn hook_callback_refs_routed_into_component_record() {
        // A component with a useEffect callback that calls an in-scope `helper`.
        let src = "import React, { useEffect } from 'react';\n\
                   function Comp() {\n\
                     useEffect(() => { helper(); }, []);\n\
                     return <div/>;\n\
                   }\n\
                   function helper() { fetch('x'); }\n";
        let (out, records) = analyze_src(src);

        // The arrow must be suppressed — only Comp and helper appear as hotspots.
        assert!(
            !out.iter().any(|h| h.id.contains("<arrow@")),
            "suppressed hook-callback arrow must not appear as a hotspot; out={:?}",
            out.iter().map(|h| &h.id).collect::<Vec<_>>(),
        );

        let comp = out
            .iter()
            .find(|h| h.symbol == "Comp")
            .expect("Comp hotspot present");

        let comp_rec = records
            .iter()
            .find(|r| r.unit_id == comp.id)
            .expect("Comp record present");

        // The component's record must carry a ref to `helper` (from the absorbed
        // hook callback) so that the propagation fold can follow the edge.
        assert!(
            comp_rec.refs.iter().any(|r| r.base == "helper"),
            "Comp record must carry a ref to `helper` (absorbed from hook callback); refs={:?}",
            comp_rec.refs.iter().map(|r| &r.base).collect::<Vec<_>>(),
        );

        // Sanity: helper itself is an emitted hotspot with its own fetch effect.
        let helper = out
            .iter()
            .find(|h| h.symbol == "helper")
            .expect("helper hotspot present");
        assert!(
            helper
                .effects
                .iter()
                .any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
            "helper must have a net.fs.db effect from fetch; effects={:?}",
            helper.effects.iter().map(|e| e.kind).collect::<Vec<_>>(),
        );

        // Records 1:1 with hotspots.
        assert_eq!(
            records.len(),
            out.len(),
            "records must be 1:1 with hotspots"
        );
    }

    #[test]
    fn false_resolve_killed_node_fs_not_resolved_to_local_readfile() {
        use fxrank_core::frontend::SourceFile;
        use fxrank_core::graph::Edge;
        use fxrank_core::resolve::{CanonicalIndex, resolve_ref_precise};
        // A project with a lone local `readFile` + a caller using node:fs's fs.readFile.
        let files = vec![SourceFile {
            path: "src/app.ts".into(),
            text: "import fs from 'node:fs';\n\
                   export function readFile() { return 1; }\n\
                   export function caller() { fs.readFile('x', () => {}); }"
                .into(),
        }];
        let out = TsFrontend::default().analyze(&files);
        let idx = CanonicalIndex::from_records(&out.records);
        assert!(idx.adopted(), "TS partition must be adopted");
        let caller = out.records.iter().find(|r| r.symbol == "caller").unwrap();
        let fs_ref = caller
            .refs
            .iter()
            .find(|r| r.base.starts_with("fs"))
            .unwrap();
        let edge = resolve_ref_precise(fs_ref, &idx, &caller.path);
        // `Edge` has no `Debug` derive, so pre-bind the boolean (no `{edge:?}`).
        let is_opaque = matches!(edge, Some(Edge::Opaque(_)));
        assert!(
            is_opaque,
            "node:fs fs.readFile must be opaque, not resolved to a local readFile"
        );
    }

    /// e2e: an `@/util` alias import (tsconfig `@/* → ./src/*`) resolves to the
    /// in-batch callee when `TsFrontend { tsconfig: Some(cfg) }` is used.
    ///
    /// Layout: `src/app.ts` calls `x()` imported from `@/util`; `src/util.ts`
    /// exports `x` which calls `fetch` (net effect, class 7).
    ///
    /// With tsconfig → `load`'s `@/util::x` ref has `resolved_target = Some([…])`,
    /// `CanonicalIndex::resolve_ref_precise` returns `Edge::Resolved`, and `load`
    /// inherits `x`'s net effect.
    ///
    /// Without tsconfig → same ref has no `resolved_target`, the non-relative
    /// specifier is opaque → `Edge::Opaque`.
    #[test]
    fn e2e_at_alias_resolves_with_project_tsconfig() {
        use crate::tsconfig::TsConfig;
        use fxrank_core::frontend::SourceFile;
        use fxrank_core::graph::Edge;
        use fxrank_core::resolve::{CanonicalIndex, resolve_ref_precise};

        let app_src = "import { x } from '@/util';\n\
                       export function load() { x(); }\n";
        let util_src = "export function x() { return fetch('/u'); }\n";

        let files = vec![
            SourceFile {
                path: "src/app.ts".into(),
                text: app_src.into(),
            },
            SourceFile {
                path: "src/util.ts".into(),
                text: util_src.into(),
            },
        ];

        let cfg = TsConfig {
            base: "".into(),
            paths: vec![("@/*".into(), vec!["./src/*".into()])],
        };

        // --- WITH tsconfig: alias resolves ---
        let out = TsFrontend {
            tsconfig: Some(cfg),
            ..Default::default()
        }
        .analyze(&files);
        let idx = CanonicalIndex::from_records(&out.records);
        assert!(
            idx.adopted(),
            "TS partition must be adopted when canonical_paths are set"
        );

        let load_rec = out
            .records
            .iter()
            .find(|r| r.symbol == "load")
            .expect("load record must be present");
        // Find the call ref for the @/util import (module = "@/util").
        let x_ref = load_rec
            .refs
            .iter()
            .find(|r| r.module.as_deref() == Some("@/util"))
            .expect("load must have a ref with module='@/util'");
        let edge = resolve_ref_precise(x_ref, &idx, &load_rec.path);
        let is_resolved = matches!(edge, Some(Edge::Resolved(_)));
        assert!(
            is_resolved,
            "with tsconfig, @/util ref must resolve to Edge::Resolved (got opaque or None)"
        );

        // load must inherit x's net/fetch effect (propagation pre-condition: the
        // record carries the ref, the fold would propagate it; at the record level
        // we verify resolved_target is set and the x hotspot has net effects).
        let x_hotspot = out
            .functions
            .iter()
            .find(|h| h.symbol == "x")
            .expect("x hotspot must be present");
        assert!(
            x_hotspot
                .effects
                .iter()
                .any(|e| e.kind == fxrank_core::effect::EffectKind::NetFsDb),
            "x must have a net.fs.db (fetch) effect; effects={:?}",
            x_hotspot.effects.iter().map(|e| e.kind).collect::<Vec<_>>()
        );

        // --- WITHOUT tsconfig: same import is opaque ---
        let out_no_cfg = TsFrontend::default().analyze(&files);
        let idx_no_cfg = CanonicalIndex::from_records(&out_no_cfg.records);
        let load_rec_no_cfg = out_no_cfg
            .records
            .iter()
            .find(|r| r.symbol == "load")
            .expect("load record must be present");
        let x_ref_no_cfg = load_rec_no_cfg
            .refs
            .iter()
            .find(|r| r.module.as_deref() == Some("@/util"))
            .expect("load must have a ref with module='@/util'");
        let edge_no_cfg = resolve_ref_precise(x_ref_no_cfg, &idx_no_cfg, &load_rec_no_cfg.path);
        let is_opaque = matches!(edge_no_cfg, Some(Edge::Opaque(_)));
        assert!(
            is_opaque,
            "without tsconfig, @/util ref must be Edge::Opaque (got resolved or None)"
        );
    }

    #[test]
    fn is_test_file_characterization() {
        for p in [
            "a.test.ts",
            "a.spec.tsx",
            "x.b.test.js",
            "src/__tests__/a.ts",
            "a/__tests__/b/c.ts",
        ] {
            assert!(is_test_file(p), "expected test file: {p}");
        }
        for p in [
            "app.ts",
            "src/app.tsx",
            "my.test.project/app.ts",
            "testdata.ts",
            "a.contest.ts",
        ] {
            assert!(!is_test_file(p), "expected NON-test file: {p}");
        }
    }
}
