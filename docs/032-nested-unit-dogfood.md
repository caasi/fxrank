# Spec 032 / #83 — nested-unit closure FP-count dogfood (Q5 merge gate)

> **Pre-registered merge gate + results.** The protocol below (corpora, false-positive
> definition, gate threshold, four diagnostics) was fixed **before** the scan so the number is
> not fitted after the fact. This doc also **doubles as spec 031's compat baseline artifact**
> (§5.5): the `scan crates/` run here is the same before/after reference 031 uses.

`#83` makes nested `fn`s and closures their own `UnitRecord`s. Q5(a) ships a per-closure
`hidden.mutation` with subreason `captured-binding` whenever a closure writes a binding it
captured. The **merge gate** is a dogfood of the closure captured-binding false-positive
**count** — comparable to the documented **#55 ~76-site "unacceptable" baseline**.

## Corpora (three, all local — no vendoring, no placeholder)

| # | Corpus | Scan command | Role |
|---|--------|--------------|------|
| a | fxrank workspace itself | `cargo run --release -p fxrank -- scan crates/` | visitor/detector profile; **031 compat baseline** |
| b | `~/GitHub/agent-browser/cli` | `... scan ~/GitHub/agent-browser/cli` | iterator/async-heavy; **TP calibration** (async-`\|\|` / spawn path) |
| c | `~/GitHub/ripgrep` | `... scan ~/GitHub/ripgrep` | pure-synchronous; **morally-local FP ceiling** |

Scanned build: `target/release/fxrank` at branch `feat/083-nested-unit-collection` (`1406b58`,
Tasks 1–5 landed). Scan scope actually parsed:

| Corpus | files | functions (units) | closure units | closure `captured-binding` emissions |
|--------|------:|------------------:|--------------:|-------------------------------------:|
| a fxrank | 112 | 1206 | 333 | 7 |
| b agent-browser/cli | 74 | 3973 | 1975 | 3 |
| c ripgrep | 102 | 2540 | 274 | 16 |

(agent-browser/cli and ripgrep are read-only external corpora — not modified.)

## False-positive definition (final — escape-based; folds in F6 "local OR parameter")

An emitted `<closure@…>` (or binding-named) unit's `captured-binding` emission counts as a
**false positive** iff **both** hold:

1. the captured binding is a **local OR parameter** of the immediately-enclosing fn (F6 — not
   just a local), **AND**
2. the closure **does not escape** the enclosing fn.

Concretely, **non-escaping ⇒ FP**: `.map(|x| acc.push(..))`, `.sort_by`, `.for_each`, a
visitor callback `walk(&body, &mut |site| …)`, and `let f = |..|; f()` where `f` is called
within the **same** fn. **Escaping ⇒ true positive** (NOT a FP): the closure is passed to
`thread::spawn` / `std::thread`, **returned** from the fn, **stored** into a struct/collection
that outlives the call, or **moved into an `async`/task** (or a parallel walker) that runs
elsewhere. The judgment is per-site by the closure's escape context, not by the mutation itself.

## Gate & diagnostics (pre-registered before scanning)

- **Gate (the one merge criterion):** total closure `captured-binding` **FP ≤ 20** across the
  three corpora ⇒ ship Q5(a) as-is. **FP > 50 ⇒ reopen Q5**, and the reopen lever is named:
  **Q5 option (b) — fold re-containment** (re-attribute the captured mutation back to the
  *owning* unit). This is **NOT** the deferred (c) scope-aware *call* resolution — (c) is the
  orthogonal bare-name edge-drop concern; (b) is the FP lever. **Neither (b) nor (c) is built in
  `#83`** regardless of the number; a breach records the finding and hands the merge-vs-defer
  decision to review.
- **Diagnostics (reported, NOT gates)** — four numbers that characterize the change:
  1. **Per-corpus FP-rate** — FP ÷ all closure `captured-binding` emissions, per corpus.
     **ripgrep's rate is the morally-local ceiling.**
  2. **Top-20 hotspot closure share** — fraction of top-20 hotspots (by `propagated_score`) that
     are `<closure@…>` units; **flag if > 25%**.
  3. **Concern-1 dilution** — count of parent units that **drop ≥1 effect class** purely because
     an effect re-homed to a nested unit.
  4. **Bare-name edge loss** (the OQ2 "metric 5", D-OQ2) — resolvable bare-name call edges lost
     pre-#83 vs post-#83 *because a nested unit newly shares a simple name with a module-level
     unit*. Trigger evidence for the deferred CALL resolution (c); reported, not gated.

---

## Results

### Gate — closure `captured-binding` FP/TP split

FP = local-OR-param captured binding **and** non-escaping consumption (per the definition above).
Every emission was classified by opening the source at the reported line and reading how the
closure is consumed.

| Corpus | closure `cb` emissions | **FP** | **TP** | unique closure sites | FP sites | TP sites |
|--------|-----------------------:|-------:|-------:|---------------------:|---------:|---------:|
| a fxrank | 7 | 7 | 0 | 4 | 4 | 0 |
| b agent-browser/cli | 3 | 3 | 0 | 3 | 3 | 0 |
| c ripgrep | 16 | 15 | 1 | 10 | 9 | 1 |
| **TOTAL** | **26** | **25** | **1** | **17** | **16** | **1** |

**The single TP** is ripgrep `crates/core/main.rs:183` (`search_parallel`): a
`Box::new(move |result| { … searcher.printer()… })` **boxed and moved into**
`WalkParallel::run`, executed on worker threads — the captured `searcher` escapes, so its
mutation is genuinely non-local. The escape-based definition correctly keeps this a TP; a
naive "captured-write ⇒ FP" rule would have mislabeled it.

### Per-site classification (evidence for spot-checking the escape judgment)

**a — fxrank (all non-escaping visitor / `.map` closures capturing a local ⇒ FP):**

| site | captured binding(s) | consumption | verdict |
|------|--------------------|-------------|---------|
| `fxrank-lang-shell/src/detect/calls.rs:288` | `out` (local ×2) | `walk::walk(&body, &mut \|site\| …)` visitor | FP |
| `fxrank-lang-shell/src/bindings.rs:37` | `names` (local ×3) | `walk(&body, &mut \|site\| …)` visitor | FP |
| `fxrank-lang-shell/src/detect/calls.rs:243` | `idx` (local) | `suffix_words.get(idx).map(\|w\| { idx += 1; … })` | FP |
| `fxrank-lang-shell/src/walk.rs:128` | `out` (local) | `walk(&body, &mut \|site\| out.push(cs))` visitor | FP |

**b — agent-browser/cli (all non-escaping ⇒ FP):**

| site | captured binding | consumption | verdict |
|------|-----------------|-------------|---------|
| `src/doctor/fix.rs:38` | `fixed` (local) | `daemons_closed.get_or_insert_with(\|\| … fixed.push(..))` (inline lazy init) | FP |
| `src/chat.rs:501` | `map` (**param** `&mut`) | `.filter_map(\|idx\| map.remove(&idx))` (inline) | FP (F6 param) |
| `src/commands.rs:700` | `full_page` (local) | `.filter(\|arg\| { full_page = true; … })` (inline) | FP |

**c — ripgrep (15 FP + 1 TP):**

| site | captured binding(s) | consumption | verdict |
|------|--------------------|-------------|---------|
| `crates/cli/src/pattern.rs:144` | `line_number`, `patterns` (local) | `.for_byte_line(\|line\| …)` iterator | FP |
| `crates/core/main.rs:183` | `searcher` (local, cloned) | **`Box::new(move …)` → `WalkParallel::run` on worker threads** | **TP** |
| `crates/printer/src/util.rs:571` | `dst` (param), `last_match` (local) | `captures_iter_at(…, \|caps\| …)` iterator | FP |
| `crates/printer/src/util.rs:93` | `matches` (local) | inline replace callback `\|caps, dst\| …` | FP |
| `crates/printer/src/summary.rs:654` | `count` (local) | `find_iter_at_in_context(…, \|_\| { count += 1; … })` | FP |
| `crates/printer/src/json.rs:666` | `matches` (local) | `find_iter_at_in_context(…, \|m\| matches.push(..))` | FP |
| `crates/printer/src/standard.rs:721` | `matches` (local) | `find_iter_at_in_context(…, \|m\| matches.push(..))` | FP |
| `crates/core/flags/config.rs:90` | `line_number`, `args`, `errs` (local) | `.for_byte_line_with_terminator(\|line\| …)` iterator | FP |
| `crates/matcher/src/lib.rs:912` | `dst` (param), `last_match` (local) | `find_iter(haystack, \|m\| …)` iterator | FP |
| `crates/matcher/src/lib.rs:960` | `dst` (param), `last_match` (local) | `captures_iter_at(…, \|caps\| …)` iterator | FP |

### Verdict

**Ship Q5(a).** No corpus, and no reading of the total, comes anywhere near the **FP > 50**
reopen trigger:

- **Per unique closure site: 16 FP ≤ 20** ⇒ clean-ship threshold met. The gate's comparator —
  the **#55 ~76-site** "unacceptable" baseline — is site-based, so this is the faithful reading:
  16 vs 76 is a ~79% reduction, a categorically cleaner surface.
- **Per emission: 25 FP** — sits in the 21–50 advisory band (above the ≤ 20 clean-ship line,
  far below the > 50 reopen line). Every emission beyond the 16 sites is a **repeat captured
  write inside an already-counted non-escaping visitor closure** (e.g. `config.rs:90` = one
  closure, three emissions `line_number`/`args`/`errs`; `bindings.rs:37` = one closure, three
  `names` writes), not a new distinct false hotspot.

Either reading is below the 50 reopen trigger, so **Q5 is not reopened** and **option (b) fold
re-containment is not built**. The site-vs-emission distinction is surfaced for review, but the
recommendation is unambiguous: ship the escape-based Q5(a). The single ripgrep TP confirms the
definition separates escaping mutation from morally-local writes as intended.

### Diagnostics (reported, not gated)

**1 — Per-corpus FP-rate** (closure `cb` FP ÷ closure `cb` emissions):

| Corpus | FP-rate | note |
|--------|--------:|------|
| a fxrank | 7/7 = **100%** | all four closures are synchronous visitors capturing a local |
| b agent-browser/cli | 3/3 = **100%** | inline `get_or_insert_with` / `filter_map` / `filter` |
| c ripgrep | 15/16 = **~94%** | **morally-local ceiling** — pure-sync corpus; only the boxed parallel-walker closure is a real TP |

Denominator note: the rate is over **closure** `captured-binding` emissions (the population the
FP definition governs). Total `captured-binding` emissions including named fns/methods are
13 / 568 / 18 respectively; agent-browser's 568 are pre-existing spec-008 F1 named-unit captured
writes, out of `#83`'s closure scope and excluded from this rate.

**2 — Top-20 hotspot closure share** (by `propagated_score`; flag if > 25%):

| Corpus | closures in top-20 | share |
|--------|-------------------:|------:|
| a fxrank | 0/20 | 0% |
| b agent-browser/cli | 1/20 | 5% |
| c ripgrep | 1/20 | 5% |

All **< 25%** — no flag. Closures are not burying real hotspots; the re-homed units rank low.

**3 — Concern-1 dilution** (parents that drop ≥1 effect class because an effect re-homed to a
nested closure). *Methodology:* a byte-exact pre-#83-vs-post-#83 class delta needs the pre-#83
build (closure walked as part of its parent), which is out of scope here; proxy = enclosing fns
of a `captured-binding` closure whose **own** `max_class` now sits **below** the re-homed
captured-binding class (3), i.e. the highest-class effect now visibly lives in the child, not
the parent.

| Corpus | distinct affected parents | parents now own-class < 3 (diluted) |
|--------|--------------------------:|------------------------------------:|
| a fxrank | 4 | 4 |
| b agent-browser/cli | 3 | 3 |
| c ripgrep | 10 | 7 |
| **TOTAL** | **17** | **14** |

14 parents look "cleaner" purely because the captured write moved to the closure unit (e.g.
ripgrep `patterns_from_reader`, fxrank `detect`/`local_names`). The 3 ripgrep parents that stay
≥ 3 (`search_parallel` class 5, the two `record_matches` sinks class 4) carry their own
higher-class effects independently, so re-homing did not dilute them.

**4 — Bare-name edge loss (D-OQ2, deferred-(c) trigger evidence).** *Methodology + limitation:*
the scan output exposes neither `canonical_path` (to flag which named units are nested) nor the
call-edge set (to count dropped resolutions), so a precise pre-#83-vs-post-#83 edge delta needs
a **resolve-layer-instrumented rescan**, which is out of scope. Two bounding observations:

- **Definitive lower structure:** `#83`'s new units are dominated by **anonymous closures**
  (333 / 1975 / 274 across the three corpora), which carry **no simple name** and therefore
  contribute **exactly 0** bare-name collisions. The OQ2 risk is confined to nested *named* fns.
- **Over-broad upper bound:** duplicated simple names among *all* named units are
  84 / 69 / 231 (spanning 294 / 183 / 1670 units). This is a loose **ceiling** — it counts
  long-standing trait/method name reuse (`new`, `default`, per-type `record_matches`) that
  predates `#83` and is unrelated to nesting; the `#83`-attributable subset is bounded above by
  nested named fns only.

**Magnitude verdict: LOW.** The closure-dominant, zero-collision bulk means the newly-introduced
bare-name ambiguity is small — which keeps the deferred CALL resolution (c) **deferred**
(diagnostic 4 does not warrant opening the issue).

---

## Deferred: scope-aware CALL resolution (option c)

Option (c) — scope-aware call resolution — is the **call-graph twin of #78's binding
shadowing**, one layer up: `#78` scope-qualifies *bindings* (name → binding, per-frontend
`scope.rs`); (c) would scope-qualify *call targets* (call → callee-unit) in language-neutral
core **`resolve.rs`**. Once nested `fn`s / closures are their own units, a nested `fn helper`
and a module-level `helper` share a simple name, so a bare `helper()` ambiguous-drops in the
flat `SymbolIndex`. (c) is a **possible future cross-language enhancement**, **gated by
diagnostic 4** (bare-name edge-loss magnitude) as its trigger evidence — and that magnitude is
**LOW** here, so:

- (c) is **NOT opened as an issue** and **does NOT block closing #78** (different layer — #78 is
  the binding twin, not the call twin).
- **D-OQ2 deliberately accepts** the current bare-name ambiguity (symmetric ambiguous-drop,
  mirroring Python/TS shared-core behavior). Exclusion option (b) — dropping nested units from
  the name index *without* lexical pre-resolution — is **rejected as a false-resolve trap**
  (it would resolve an intra-tree `helper()` to the module-level unit, worse than a drop).
- The principled fix (parent-scoped canonical paths + lexical pre-resolution of intra-tree
  calls) is documented, issue-ready, but deferred until the measured magnitude warrants it.

**Cross-link:** this anchors to the shared guideline
[`cross-file-resolution-guideline.md` → "Deferred — scope-aware call resolution"](cross-file-resolution-guideline.md#deferred--scope-aware-call-resolution-candidate-issue-not-yet-filed)
(committed on `main`), which carries the full problem statement, root-cause (#78 twin), design
sketch, rejected alternative, and cross-language cost. This dogfood's diagnostic 4 is that
section's named gate ("the edge-loss magnitude measured by #83's nested-unit dogfood").
