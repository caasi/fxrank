# Spec 025-3e — Precise cross-file module resolution

**Status:** draft v2 (brainstormed 2026-06-25; revised after local review-loop gate — Claude + Codex) ·
**Issue:** #36 · **Parent spec:** `025-cross-file-resolution-and-propagation.md` (this is phase **3e**,
deferred out of 025 §13a) · **Companion:** `docs/cross-file-resolution-guideline.md`. **Precedence:**
spec 001 governs base scoring; spec 025 governs resolution/roots/propagation; **this spec refines 025's
resolver only** (name-based → path-precise). Where this spec and 025 disagree on resolution, this spec
wins; on everything else, 025 stands. Code-vs-spec disagreements resolve to the spec.

## 1. Summary

025 shipped a **name-based** cross-file resolver: `fxrank_core::resolve::resolve_ref` matches a
qualified call by its **last path segment** against in-scope units, resolving to the lone same-named
unit if exactly one exists. This phase replaces that with **path-precise resolution**: each frontend
emits a **canonical fully-qualified path** per unit and an **import-resolved target path** per call
site; the core resolves by **path equality over a neutral index**, not by bare leaf name. This closes
the false-resolve (`std::fs::write` → a lone `Foo::write`; `mod.write()` → a lone `write`;
`fs.readFile()` → a lone `readFile`) across all three frontends with **one shared resolution
algorithm**.

The architecture is the **SCIP/Kythe/LSIF** pattern (validated at GitHub/Google/Microsoft/Sourcegraph
scale): a parser-free core resolves **canonical-path strings** that each language frontend produces.
fxrank already sits on this precedent — this phase only sharpens the *identity* from "leaf name" to
"full path", and adds an **alias index** for re-exports.

## 2. What changes (and what does not)

- **Changes:** resolution precision. `propagated_score` / `propagated_max_class` / `inherited[]`
  become more accurate as false-resolves disappear and real reaches surface. `scope.external_reaches`
  sharpens.
- **Unchanged (hard invariants):**
  - **Own-body output is byte-identical — guaranteed by the type boundary, not by a serialization
    choice.** `UnitRecord` (the pass-1 intermediate, `record.rs`) is **never on the wire**; the
    serialized type is `Hotspot`/`Report` (`model.rs`), and `apply_fold` writes only the propagation
    fields (`propagated_*`, `inherited`, `external_reaches`, `root`) — never the own-body fields. So
    **any** field 3e adds to `UnitRecord` (`canonical_path`, `resolved_target`, `aliases`) cannot
    affect output *by construction*. (This is the same reason per-effect `confidence` never reaches
    the wire — a secondary illustration, not the primary guarantee.) `symbol`, `unit_id`
    (`path:line:col:symbol`), `own_score`, `max_class`, `effects[]`, `risks[]` are all untouched.
  - **`propagated_* ≥ own_*`** (025 §13b F1) still holds.
  - Cycles → existing Tarjan-SCC condensation. Unresolved → existing `external.unresolved` / class-2
    `ReachKind` opaque reach. Both reused verbatim.
- **Root model unchanged.** `root` = CLI explicit FILE arg (the agent's observation focus,
  025 §13c / guideline *Roots — the agent's observation focus*). This phase does **not** touch roots —
  see §7.

## 3. Architecture — Shape A (string identity)

```
frontend (language-specific)              core / resolve phase (neutral, shared by 3 langs)
────────────────────────────              ───────────────────────────────────────────────
per UnitRecord:                           CanonicalIndex
  + canonical_path  ───────────┐            primary map:  canonical_path → UnitId
per CallSiteRef:                ├────────►   alias  map:  alias_path     → target_path  (extra keys)
  + resolved_target             │
re-export alias-facts ──────────┘          resolve_ref(ref):  [adopted partition]
  + UnitRecord.aliases: Vec<AliasFact>       1. resolved_target Some → path-equality lookup
                                             2. else follow alias map (transitive, cycle-guarded)
                                             3. hit  → Edge::Resolved(UnitId)
                                             4. miss → qualified ? Edge::Opaque (NO leaf match)
                                                       : unqualified leaf-fallback else None
```

**Invariant preserved:** `fxrank-core` depends on no parser; the core branches only on neutral data
(now: canonical-path strings + alias edges + the existing `qualified`/`first_party` bools). All
language-specific module semantics (Rust `mod`/`#[path]`, TS extension/index ladder, Python package
layout) live in the frontends, which compute the canonical paths. This is the precise reading of "build
the module tree in the resolve phase": the **tree/index + resolution algorithm** are shared and
neutral; the **canonical-path computation** that feeds it is per-frontend.

### 3.1 Why string identity, not graph reachability

The alternative — GitHub **Stack Graphs** / scope-graphs (graph-reachability resolution, ESOP 2015) —
is strictly more powerful (precise alias following, file-incremental) but costs a per-language graph-
construction DSL + a path-finding engine. fxrank does not need it: a partial-view effect profiler
resolves within a bounded corpus and marks the rest opaque, for which string identity is the
well-precedented, low-complexity choice. Stack Graphs is the **documented upgrade path**, not a
requirement (§9).

## 4. Core changes (`fxrank-core`)

### 4.1 Neutral fields

- **`UnitRecord.canonical_path: Vec<String>`** — the unit's fully-qualified path as **interned
  segments** (e.g. `["crate","helpers","write"]`, `["app","util","fetchUser"]`,
  `["pkg","mod","write"]`). **Empty `Vec` ⇒ the frontend could not assign a canonical path** for this
  unit (no crate root in scope, cfg-excluded module, macro-generated, etc.) ⇒ the unit participates in
  resolution only by the degradation rules (§4.3 / §6).
- **`CallSiteRef.resolved_target: Option<Vec<String>>`** — the import-resolved canonical path of the
  callee, as the frontend determined it. `Some(path)` ⇒ frontend resolved the reference against its
  module tree. `None` ⇒ frontend did **not** produce a canonical target (either it has not adopted
  canonical paths, or it attempted and the target is outside the corpus). Disambiguation of these two
  `None` meanings is by the **partition-adoption gate** (§4.3), not by overloading this field.
- **Re-export aliases — a new field `UnitRecord.aliases: Vec<AliasFact>`**, where
  `AliasFact { alias_path: Vec<String>, target: Vec<String> }` is a neutral fact ("this canonical
  alias path names the same definition as this target path"). This **replaces** the currently-unused
  `export: Option<(String, String)>` field (dead today — set to `None` at every construction site and
  read nowhere in `crates/`), so it is a clean field replacement, not a migration. The frontend emits
  one `AliasFact` per re-export it detects; the core does **not** know what a re-export *is*.

Segments are **interned** (one canonical copy per distinct segment string, integer-keyed) so path
equality is integer-vector comparison — the standard symbol-interning optimization. **The interner is
scoped to a single language partition's `CanonicalIndex`** (the CLI builds one index per
`partition_by_language` group, `main.rs`); interned ids are never compared across partitions (the
per-language partition from 025 is unchanged). Interning is an internal core concern; frontends emit
plain `String` segments.

### 4.2 `CanonicalIndex`

- **`CanonicalIndex::from_records`** builds, for one language partition:
  - **primary map** `canonical_path → UnitId` (keyed on the interned segment vector). Units with an
    empty `canonical_path` are not inserted.
  - **alias map** `alias_path → target_path` from every `AliasFact`.
  - an **adopted** flag = `true` iff ≥1 unit in the partition has a non-empty `canonical_path` (used by
    §4.3's gate).
- A `canonical_path` that is **non-unique** (two units claim the same full path — should not happen in
  valid code, but cfg-gated duplicates can) → recorded as **ambiguous**, resolve returns `None` (drop),
  never a silent pick. (025's leaf-collision ambiguity rule, lifted to full paths.)
- **`SymbolIndex` is retained** as the name-based fallback backend for non-adopted partitions (§4.3
  step 0). `CanonicalIndex` **holds** a `SymbolIndex` rather than deleting it, so the 025 path survives
  untouched for frontends that have not yet adopted canonical paths.

### 4.3 `resolve_ref` (path-precise, with a safe degradation gate)

The single most important correctness rule of this phase: **a qualified outward reference must never
fall back to leaf-name matching** — that is exactly the 025 false-resolve (`std::fs::write` →
`Foo::write`). Resolution proceeds:

**Step 0 — partition-adoption gate.** If the partition is **not adopted** (no unit has a
`canonical_path` — e.g. a frontend that has not yet implemented 3e, or a subdirectory scan with no
crate root in scope, §5.1/§6): run the **025 name-based `resolve_ref` unchanged** (leaf lookup for all
refs). This preserves exact pre-3e behaviour for not-yet-migrated frontends and degrades root-less
scans cleanly. Steps 1–3 apply only to an **adopted** partition.

**Step 1 — method early-drop (unchanged):** `RefKind::Method` → `None` (no receiver type; 025 rule).

**Step 2 — canonical resolution:** if `resolved_target == Some(path)`:
  a. primary-map lookup of `path` → `Edge::Resolved(id)`;
  b. else follow the **alias map** transitively (bounded depth, visited-set cycle guard) to a primary
     hit → `Edge::Resolved(id)`;
  c. else → treat as a miss (step 3).

**Step 3 — miss handling (no leaf fallback for qualified):**
  - `resolved_target == None` **or** a Some-path that missed:
    - if `r.qualified` → `Edge::Opaque(ExternalReach{…})` with `ReachKind` from `r.first_party`
      (025 opaque rule kept) — **never a leaf lookup.**
    - else (bare unqualified — `push`, `clone`, a same-scope local helper the frontend left
      uncanonicalized) → a **leaf-name lookup is still permitted** (025 behaviour) for an in-scope local
      definition, else `None`. Unqualified bare names are intra-language noise, not the import surface,
      so leaf matching here cannot resurrect the qualified-call false-resolve.

This makes the dangerous case (a qualified call whose canonical target is unresolvable) go **opaque**,
not leaf-matched — closing T1-3. The ambiguity, cycle, and opaque behaviours are otherwise reused from
025; only the *key* (full path vs leaf) and the *alias hop* are added, and the *leaf fallback is
restricted to unqualified refs in adopted partitions*.

## 5. Frontend changes — canonical-path emitters

Each frontend gains a canonical-path computation. The **resolution algorithm is identical** across
languages (it lives in core, §4); only the per-language *path math* differs. **All path math is over
the in-batch `SourceFile.path` set** (`SourceFile` carries only `{path, text}`) — it is **string-
relative, no disk I/O** — so a target that is not represented in the scanned batch is simply
unresolvable (empty `canonical_path` / `resolved_target = None`), never a filesystem stat. "Filesystem
convention" below means *the conventional shape of those in-batch paths*, not reading the real disk.

### 5.1 Rust (`fxrank-lang-rust`) — the bulk

Reconstruct the crate module tree from the flat `&[SourceFile]` batch. **No `cargo` invocation, no
network, no disk reads** — fxrank stays a pure offline syntactic tool. Rules (authoritative, from The
Rust Reference — *Modules*, *Paths*, *Visibility*, *Namespaces*):

- **Crate root (in-batch filesystem convention):** a scanned file whose in-batch path is named `lib.rs`
  / `main.rs` (or `src/bin/*.rs`, `src/bin/<name>/main.rs`) is a crate root; its items are at module
  path `crate::…` with **no root-module name segment** (the root module is anonymous — `crate::Bar`,
  not `crate::main::Bar`).
- **No crate root in scope (the common case for subdirectory scans, e.g. `scan crates/foo/src/`
  without `lib.rs`/`main.rs` in the batch):** the `crate::` anchor is unknown, so affected files get an
  **empty `canonical_path`** and the partition degrades per §4.3 step 0 (025 name-based) — this is the
  **expected** state for a narrow scan, not an error. If *some* files in the batch have a root and
  others do not, the partition is adopted; the root-less files' qualified refs go opaque (safe
  under-resolution), their unqualified refs leaf-fall-back (§4.3 step 3).
- **Out-of-line `mod foo;`:** resolve to `foo.rs` **or** `foo/mod.rs` under the **owning directory**.
  Owning dir = `dirname(F)` when `F` is a *mod-rs* file (`lib.rs`/`main.rs`/`mod.rs`/`#[path]`-loaded);
  = `dirname(F)/<stem(F)>` when `F` is *non-mod-rs* (`foo.rs` owns `foo/`). Both-present is an error
  (skip/diagnose). Edition delta: pre-1.30 required `mod.rs` to own children; 2018+ allows `foo.rs` to
  own `foo/` — handle both, they coexist.
- **`#[path = "..."]`:** out-of-line → relative to the **declaring file's directory**, overriding the
  `<name>.rs`/`<name>/mod.rs` lookup. Inline-block `#[path]` → sets the **base directory for that
  module's children**; e.g. `#[path = "foo"] mod m { mod n; }` resolves `n` to `foo/n.rs` (or
  `foo/n/mod.rs`). If the `#[path]` target (incl. a `..`-escaping path) is **not in the scanned batch**,
  the loaded module is unresolvable → empty `canonical_path` for anything it would define (degrade).
- **Inline `mod foo { … }`:** adds the segment `foo` with no file; nests by concatenation
  (`crate::a::b::x`). A non-mod-rs file's inline children inject a `<file-stem>/` directory prefix.
- **`use` / path qualifiers:** expand each call's path against the module tree — `crate::`/`self::`/
  `super::`/leading-`::`(extern)/`$crate`, and 2018 bare = relative-or-extern. Follow `use … as …`
  aliases to the target; emit `pub use` re-exports as `AliasFact`s.
- **`cfg`-gated mods** are syntactically present → included in the tree (fxrank is syntactic; matches
  its existing `#[cfg(test)]` handling).

**Config files (Cargo.toml) are out of scope for v1.** `SourceFile` holds only source text the
frontend was handed; `Cargo.toml` is not a `.rs` file and is **not** in the batch. The crate-root
convention above needs no Cargo.toml. The optional `[lib].path`/`[[bin]].path`/`[workspace].members`
enhancements (§7) would require explicitly threading config files into the batch — **never** ad-hoc
disk reads, to preserve offline purity — and are deferred (§9).

**Known syntactic limits (accepted):** cfg-gated *file* mods whose file is excluded from the scan,
macro-generated `mod`s, and `#[path]` targets outside the scanned tree → those units get an empty
`canonical_path` and degrade. Documented, not chased.

### 5.2 TypeScript/JS (`fxrank-lang-ts`)

- **Module identity = in-batch file specifier.** A unit's canonical path = its module specifier
  segments + symbol (normalized from `SourceFile.path`).
- **Import resolution (over in-batch paths):** `import { x } from './foo'` → resolve `./foo` via the
  **extension/index ladder** (`./foo.ts`, `./foo/index.ts`, …) **against the in-batch path set**, +
  `tsconfig.json` `compilerOptions.paths` aliases (`@/*`→`src/*`, `~/*`) *if* a tsconfig is threaded
  into the batch (deferred enhancement; else alias imports are unresolvable → opaque). Resolved target
  = `(resolved module, imported name)`.
- **Barrel re-exports** (`export { x } from './foo'`, `export * from …`) → `AliasFact`s.

### 5.3 Python (`fxrank-lang-python`)

- **Module identity = dotted path** of the file relative to its **package root**, computed over the
  in-batch path set: the nearest ancestor directory (within the batch) *without* an `__init__.py`,
  walking up through `__init__.py` dirs, + symbol.
- **Import resolution:** `from pkg.mod import write` / `import pkg.mod` / relative `from . import x` →
  resolve the dotted/relative module against the in-batch path set; target = `(module dotted path,
  name)`.
- **`__init__.py` re-exports** → `AliasFact`s.

## 6. Graceful degradation (the floor)

Degradation is governed by the §4.3 **partition-adoption gate**, which makes the floor explicit and
safe:

- **Non-adopted partition** (no unit has a `canonical_path`): 025 name-based resolution runs unchanged.
  This covers a frontend that has not yet implemented 3e, stdin fragments, loose single files, and
  subdirectory scans with no crate root in scope. A frontend can therefore adopt canonical paths
  **incrementally** (Rust first) with **zero regression** to the others.
- **Adopted partition, root-less files within it:** units with an empty `canonical_path` contribute no
  primary-map entry; their **qualified** refs resolve only if canonicalized, else go **opaque** (safe —
  never a false leaf-resolve), their **unqualified** refs may leaf-fall-back (025).
- **No errors:** every degradation path yields opaque/None, never a crash or a silent wrong resolve.

This makes each per-frontend emitter independently shippable behind the gate.

## 7. Roots & first_party — what this phase does NOT do, and the one bonus it keeps

- **No pub-visibility-chain roots; no crate-type (bin/lib) root gate.** Both were listed as #36
  bonuses but exist **only** to compute *program-entry* roots, which 025 §13c deliberately removed in
  favour of `root` = CLI explicit file ("the agent's observation focus"). The guideline (*Roots — the
  agent's observation focus* → **History**) is explicit: program-entry detection *"is a different
  concept from `root` — keep the two distinct, don't reconflate them."* These bonuses are therefore
  **dropped from #36** and recorded as a *separate future concept* (§9). No other part of this spec
  depends on them.
- **Workspace-member `first_party` — kept, and partly free (honest limit stated).** With path-precise
  resolution, a first-party sibling unit **in the scanned batch simply resolves** (`Edge::Resolved`) —
  that part is free. But an **out-of-scope sibling crate** (`other_crate::foo`, where `other_crate` is
  a workspace member not in the batch) is **indistinguishable from a third-party crate** (`serde::foo`)
  by syntax alone — the existing `crate::`/`super::`/`self::` frontend tag classifies only
  *intra-crate* references. So **out-of-scope siblings remain classified `ThirdParty`** (under-
  classified, **not** mis-resolved) until the optional `[workspace].members` table is read to tag them
  `FirstPartyOutOfScope`. That table is a lightweight `toml` parse of an in-batch `Cargo.toml` (never a
  `cargo` shell-out) and is **deferred** (§9). Precise resolution removes false-resolves for in-scope
  siblings; it does not, by itself, reclassify out-of-scope ones.

## 8. Testing

- **Core (`resolve.rs` / `CanonicalIndex`):**
  - **The win (the central regression-direction test):** two units `a::write` and `b::write` (leaf-
    ambiguous → *dropped* under 025); a call with `resolved_target = ["a","write"]` resolves to
    `a::write` **only**. Proves path-precision recovers a resolution 025 had to drop.
  - **The false-resolve kill:** an adopted partition with a lone `Foo::write`; a **qualified** call
    `std::fs::write` (`resolved_target = None`, `qualified = true`) → `Edge::Opaque`, **not** Resolved
    to `Foo::write`.
  - full-path collision → ambiguous-drop; alias hop resolves; alias cycle terminates; unqualified
    miss → leaf fallback still works in an adopted partition; **non-adopted partition → 025 name-based
    path unchanged** (all existing 025 `resolve_ref` tests pass verbatim under the gate).
- **Rust module tree:** fixtures for crate-root anonymity (`crate::Bar` not `crate::root::Bar`),
  `foo.rs` vs `foo/mod.rs`, nested-mod directory ownership, `#[path]` (out-of-line + inline-base),
  inline `mod`, `use … as …`, `pub use` re-export, **and the no-root-in-scope subdirectory case →
  empty `canonical_path` → 025 behaviour**. Headline: `mod a { fn write(){} }` + a `std::fs::write()`
  call must **no longer** resolve the stdlib call to `a::write`.
- **TS / Python:** extension/index-ladder and dotted-package fixtures with a same-named decoy in
  another module proving the call resolves to the right one (or stays opaque), not the decoy.
- **End-to-end / dogfood:** `scan crates/` own-body output **byte-identical** to pre-3e (golden);
  `propagated_*` diffs reviewed and explained (each change = a removed false-resolve or a newly
  surfaced real reach). insta snapshots updated with rationale.

## 9. Out of scope (YAGNI) / upgrade path

- **Stack Graphs / scope-graphs** (graph-reachability resolution) — the documented more-powerful
  upgrade; only if file-incremental cross-file resolution or precise multi-hop alias following at scale
  is later needed.
- **Program-entry detection** (Rust pub-chain + crate-type, TS framework-convention entries, Python
  `console_scripts`/`__main__`) — a **distinct concept** from `root` (guideline *Roots … → History*);
  if ever wanted, a new field/notion, not a reconflation of `root`. The abandoned-3b corpus insights
  are preserved in the guideline.
- **Config-file inputs** — `Cargo.toml` (`[lib]`/`[[bin]]`/`[workspace].members`), `tsconfig.json`
  (`paths` aliases), `pyproject.toml`. Adding them requires threading config files into the batch (no
  ad-hoc disk I/O); deferred. Until then: workspace out-of-scope siblings stay `ThirdParty` (§7), TS
  alias imports without an in-batch tsconfig are opaque.
- **`cargo metadata` / full workspace model**, **namespace-complete `(path, namespace)` keying**
  (fxrank units are functions; modules/types are not scored, so leaf-namespace collisions are rare),
  **perfect cross-file multi-hop alias chains**, **type/borrow inference**. All deferred.
- **Cross-language edges** stay out (025): per-language partition unchanged; the canonical index is
  built per language group.
- **Precision misses that degrade to opaque (safe, found in the holistic review).** Neither wrong-resolves;
  both just leave an in-crate edge unresolved (qualified → opaque). Revisit if dogfood shows them biting:
  - **Direct `src/bin/tool.rs` child modules.** A single-file bin root `src/bin/tool.rs` does not register
    `src/bin/tool/` as a source root, so a sibling `src/bin/tool/helper.rs` gets no (or, alongside a
    `src/lib.rs`, a fabricated `crate::bin::tool::helper`) canonical_path → calls to it go opaque. The
    `src/bin/<name>/main.rs` multi-file form IS handled. Fix = register a direct-bin file's own dir as a
    source root.
  - **Inline-submodule calls by bare module name.** `config::load()` calling into an inline `mod config`
    (where `config` is neither import-resolved nor `crate`/`self`/`super`-prefixed) → `resolved_target`
    None → opaque, rather than resolving to the in-file unit.
- **Cross-*crate* canonical collisions under a whole-workspace scan (deferred follow-up, found in Plan 2
  review).** The Rust `canonical_path` is anchored at the literal segment `"crate"` (the crate root is
  anonymous), NOT the actual crate name. So under a multi-crate scan (`scan crates/`), identical paths in
  sibling crates (e.g. `crate::imports::ImportTable::from_file` present in two frontends) collide in the
  one per-language `CanonicalIndex` → `lookup_canonical` ambiguous-drops → that edge is lost. This
  **degrades safely** (None/opaque, never a wrong target) and the pre-3e leaf index dropped the same
  collisions, so it is **not a regression**. Single-crate / sub-directory scans (the common agent
  workflow) are unaffected. Fix = prefix `canonical_path` with the real crate name (from the source-root
  dir, or `Cargo.toml` `[package].name`) instead of literal `"crate"` — pairs with the deferred
  `[workspace].members` config-file work above.

## 10. Decomposition

The paired plan (`docs/superpowers/plans/025-3e-precise-module-resolution.md`, **forthcoming — produced
in the next phase via `writing-plans`**) will decompose this into: core neutral fields +
`CanonicalIndex` + path-precise `resolve_ref` (with the adoption gate) → Rust module-tree emitter → TS
emitter → Python emitter → dogfood/golden verification. Each frontend emitter is independently
shippable behind the §6 degradation floor.
